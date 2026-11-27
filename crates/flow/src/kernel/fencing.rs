//! Resource-scoped durable monotonic fencing epochs for IR v10 external effects.
//!
//! Guarantees linearizable stale-writer rejection across independent parent instances and restarts:
//! 1. **Grant-time target barrier:** `grant_epoch(resource_id)` atomically increments and commits
//!    `granted_epoch` in SQLite `BEGIN IMMEDIATE` BEFORE returning `E` to the caller — invalidating
//!    every prior epoch `< E` even before the new writer executes its first write.
//! 2. **Write-time atomic validation & single-use consumption:** `commit_fenced_write` atomically
//!    checks `incoming_epoch == granted_epoch && incoming_epoch > written_epoch` and sets
//!    `written_epoch = incoming_epoch` in the same transaction, rejecting both older grants and
//!    duplicate/replayed writes at the same epoch (optionally returning the cached idempotent
//!    result only when exact `op_id` + `payload_digest` match).

use crate::log::line_hash;
use rusqlite::{Connection, OptionalExtension, params};
use rustix::fs::{FlockOperation, flock};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Reserved argv[0] prefix for built-in kernel-executed atomic SQLite KV puts.
pub const FENCED_SQLITE_KV_CMD: &str = "maestro:fenced-sqlite-kv";
/// Reserved argv[0] prefix for built-in kernel-executed atomic git-ref CAS updates.
pub const FENCED_GIT_REF_CAS_CMD: &str = "maestro:fenced-git-ref-cas";

/// Built-in kernel-executed atomic fenced actuators for IR v10 `Class::Effect` nodes (D6 option a).
/// Executed directly in Rust under `FencingStore`'s exclusive resource lock with zero external
/// shell, zero mutable wrapper scripts, and zero hash-to-exec TOCTOU window.
/// External non-fencing shell commands cannot run on the fenced path and must instead use D6
/// option (b) post-termination reconciliation (`reconcile_superseded_effect`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FencedActuator {
    /// Atomically writes `(resource_id, key) -> value` in `fenced_kv` inside the SAME SQLite
    /// `BEGIN IMMEDIATE` transaction that checks `granted_epoch == epoch && written_epoch < epoch`
    /// and sets `written_epoch = epoch`.
    SqliteKvPut { key: String, value: String },
    /// Executes `git update-ref <ref_name> <new_oid> <expected_old_oid>` directly (no shell) under
    /// the exclusive per-resource lock, verifying the epoch barrier beforehand, committing
    /// `written_epoch = epoch` + receipt in `fenced_git_refs`, and compensating on commit failure.
    GitRefCas { ref_name: String, expected_old_oid: String, new_oid: String },
}

fn is_hex_oid(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl FencedActuator {
    /// Parses a `Class::Effect.run` argv into a built-in kernel actuator if `argv[0]` uses a
    /// `maestro:fenced-*` command; returns `Ok(None)` for external commands and `Err(...)` for
    /// malformed built-in actuator invocations.
    pub fn parse(argv: &[String]) -> Result<Option<Self>, String> {
        let Some((cmd, args)) = argv.split_first() else {
            return Ok(None);
        };
        if cmd == FENCED_SQLITE_KV_CMD {
            if args.len() != 2 || args[0].is_empty() {
                return Err(format!("{FENCED_SQLITE_KV_CMD} requires exactly 2 arguments: <non-empty key> <value>"));
            }
            return Ok(Some(Self::SqliteKvPut { key: args[0].clone(), value: args[1].clone() }));
        }
        if cmd == FENCED_GIT_REF_CAS_CMD {
            if args.len() != 3 {
                return Err(format!(
                    "{FENCED_GIT_REF_CAS_CMD} requires exactly 3 arguments: <refs/...> <expected_old_oid> <new_oid>"
                ));
            }
            let (ref_name, expected_old_oid, new_oid) = (&args[0], &args[1], &args[2]);
            if !ref_name.starts_with("refs/")
                || ref_name.contains("..")
                || ref_name.bytes().any(|b| b <= 0x20 || b == 0x7f)
            {
                return Err(format!("invalid git ref name for {FENCED_GIT_REF_CAS_CMD}: {ref_name:?}"));
            }
            if !is_hex_oid(expected_old_oid) {
                return Err(format!(
                    "expected_old_oid must be a 40- or 64-char lowercase hex OID, got {expected_old_oid:?}"
                ));
            }
            if !is_hex_oid(new_oid) || new_oid.bytes().all(|b| b == b'0') {
                return Err(format!("new_oid must be a non-zero 40- or 64-char lowercase hex OID, got {new_oid:?}"));
            }
            return Ok(Some(Self::GitRefCas {
                ref_name: ref_name.clone(),
                expected_old_oid: expected_old_oid.clone(),
                new_oid: new_oid.clone(),
            }));
        }
        if cmd.starts_with("maestro:fenced-") {
            return Err(format!("unknown built-in fenced actuator {cmd:?}"));
        }
        Ok(None)
    }
}

/// Exclusive OS lock guard for a single fenced resource (`<fencing_dir>/fencing_locks/<sha>.lock`),
/// serializing `(verify_epoch_current -> external mutation -> commit_fenced_write)` on the host.
pub struct ResourceLockGuard {
    _file: File,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FencingError {
    #[error("fencing store error: {0}")]
    Storage(String),
    #[error("stale fencing epoch {incoming} for {resource_id}: current granted_epoch is {granted}")]
    StaleEpoch { resource_id: String, incoming: u64, granted: u64 },
    #[error("fencing epoch {incoming} for {resource_id} was already consumed (written_epoch={written})")]
    EpochAlreadyConsumed { resource_id: String, incoming: u64, written: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FencedWriteOutcome {
    Committed { epoch: u64 },
    IdempotentReplay { epoch: u64, cached_result_json: String },
}

/// Durable SQLite WAL store for resource-scoped `fencing_epoch: u64` allocation and validation.
#[derive(Debug, Clone)]
pub struct FencingStore {
    path: PathBuf,
}

impl FencingStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, FencingError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| FencingError::Storage(format!("create fencing dir: {e}")))?;
        }
        let store = Self { path };
        let conn = store.connect()?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=15000;
             BEGIN IMMEDIATE;
             CREATE TABLE IF NOT EXISTS resource_epochs (
                 resource_id TEXT PRIMARY KEY,
                 granted_epoch INTEGER NOT NULL,
                 written_epoch INTEGER NOT NULL,
                 last_op_id TEXT,
                 last_payload_digest TEXT,
                 last_result_json TEXT
             );
             CREATE TABLE IF NOT EXISTS fenced_kv (
                 resource_id TEXT NOT NULL,
                 key TEXT NOT NULL,
                 value TEXT NOT NULL,
                 epoch INTEGER NOT NULL,
                 op_id TEXT NOT NULL,
                 PRIMARY KEY (resource_id, key)
             );
             CREATE TABLE IF NOT EXISTS fenced_git_refs (
                 resource_id TEXT NOT NULL,
                 ref_name TEXT NOT NULL,
                 old_oid TEXT NOT NULL,
                 new_oid TEXT NOT NULL,
                 epoch INTEGER NOT NULL,
                 op_id TEXT NOT NULL,
                 PRIMARY KEY (resource_id, ref_name)
             );
             COMMIT;",
        )
        .map_err(|e| FencingError::Storage(format!("init fencing schema: {e}")))?;
        Ok(store)
    }

    fn connect(&self) -> Result<Connection, FencingError> {
        let conn = Connection::open(&self.path)
            .map_err(|e| FencingError::Storage(format!("open sqlite {}: {e}", self.path.display())))?;
        conn.busy_timeout(std::time::Duration::from_secs(15))
            .map_err(|e| FencingError::Storage(format!("set busy_timeout: {e}")))?;
        Ok(conn)
    }

    /// Grant-time barrier (standalone): acquires the exclusive per-resource OS `flock(LOCK_EX)`
    /// and atomically advances `granted_epoch` on `resource_id` BEFORE returning the strictly
    /// greater epoch `E` to the caller. Because `write.rs` holds the same resource lock across
    /// `(grant/verify -> external command -> commit_fenced_write)`, no newer epoch can be granted
    /// while an active effect command is executing.
    pub fn grant_epoch(&self, resource_id: &str) -> Result<u64, FencingError> {
        if resource_id.is_empty() {
            return Err(FencingError::Storage("resource_id must be non-empty".into()));
        }
        let lock = self.acquire_resource_lock(resource_id)?;
        self.grant_epoch_locked(&lock, resource_id)
    }

    /// Grant-time barrier when the caller already holds [`ResourceLockGuard`] for `resource_id`.
    pub fn grant_epoch_locked(&self, _lock: &ResourceLockGuard, resource_id: &str) -> Result<u64, FencingError> {
        if resource_id.is_empty() {
            return Err(FencingError::Storage("resource_id must be non-empty".into()));
        }
        let conn = self.connect()?;
        conn.execute_batch("BEGIN IMMEDIATE;").map_err(|e| FencingError::Storage(e.to_string()))?;
        let res = (|| -> Result<u64, FencingError> {
            let current: Option<i64> = conn
                .query_row(
                    "SELECT granted_epoch FROM resource_epochs WHERE resource_id = ?1",
                    params![resource_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| FencingError::Storage(e.to_string()))?;
            let next = match current {
                Some(g) => (g as u64).checked_add(1).ok_or_else(|| FencingError::Storage("epoch overflow".into()))?,
                None => 1,
            };
            if current.is_some() {
                conn.execute(
                    "UPDATE resource_epochs SET granted_epoch = ?1 WHERE resource_id = ?2",
                    params![next as i64, resource_id],
                )
                .map_err(|e| FencingError::Storage(e.to_string()))?;
            } else {
                conn.execute(
                    "INSERT INTO resource_epochs (resource_id, granted_epoch, written_epoch) VALUES (?1, ?2, 0)",
                    params![resource_id, next as i64],
                )
                .map_err(|e| FencingError::Storage(e.to_string()))?;
            }
            Ok(next)
        })();
        match res {
            Ok(epoch) => {
                conn.execute_batch("COMMIT;").map_err(|e| FencingError::Storage(e.to_string()))?;
                Ok(epoch)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    /// Write-time atomic check and single-use consumption without an auxiliary SQLite mutation.
    pub fn commit_fenced_write(
        &self,
        resource_id: &str,
        incoming_epoch: u64,
        op_id: &str,
        payload_digest: &str,
        result_json: &str,
    ) -> Result<FencedWriteOutcome, FencingError> {
        self.commit_fenced_write_with_tx(resource_id, incoming_epoch, op_id, payload_digest, result_json, |_| Ok(()))
    }

    /// Write-time atomic check, protected mutation callback, and single-use consumption inside
    /// ONE SQLite `BEGIN IMMEDIATE` transaction.
    ///
    /// Requires `incoming_epoch == granted_epoch && incoming_epoch > written_epoch`.
    /// Runs `mutate_tx(&conn)` ONLY after verifying epoch validity and BEFORE committing
    /// `written_epoch = incoming_epoch`; if `mutate_tx` returns `Err`, the entire transaction
    /// rolls back so the epoch is not spuriously consumed and no partial mutation commits.
    /// If `incoming_epoch == written_epoch` AND exact `(op_id, payload_digest)` match, returns
    /// `IdempotentReplay` without invoking `mutate_tx` a second time.
    pub fn commit_fenced_write_with_tx<F>(
        &self,
        resource_id: &str,
        incoming_epoch: u64,
        op_id: &str,
        payload_digest: &str,
        result_json: &str,
        mutate_tx: F,
    ) -> Result<FencedWriteOutcome, FencingError>
    where
        F: FnOnce(&Connection) -> Result<(), FencingError>,
    {
        let conn = self.connect()?;
        conn.execute_batch("BEGIN IMMEDIATE;").map_err(|e| FencingError::Storage(e.to_string()))?;
        let res = (|| -> Result<FencedWriteOutcome, FencingError> {
            struct EpochRow {
                granted: i64,
                written: i64,
                last_op: Option<String>,
                last_digest: Option<String>,
                last_res: Option<String>,
            }
            let row: Option<EpochRow> = conn
                .query_row(
                    "SELECT granted_epoch, written_epoch, last_op_id, last_payload_digest, last_result_json
                     FROM resource_epochs WHERE resource_id = ?1",
                    params![resource_id],
                    |r| {
                        Ok(EpochRow {
                            granted: r.get(0)?,
                            written: r.get(1)?,
                            last_op: r.get(2)?,
                            last_digest: r.get(3)?,
                            last_res: r.get(4)?,
                        })
                    },
                )
                .optional()
                .map_err(|e| FencingError::Storage(e.to_string()))?;
            let EpochRow { granted: granted_i, written: written_i, last_op, last_digest, last_res } =
                row.ok_or_else(|| FencingError::StaleEpoch {
                    resource_id: resource_id.to_string(),
                    incoming: incoming_epoch,
                    granted: 0,
                })?;
            let granted = granted_i as u64;
            let written = written_i as u64;
            if incoming_epoch != granted {
                return Err(FencingError::StaleEpoch {
                    resource_id: resource_id.to_string(),
                    incoming: incoming_epoch,
                    granted,
                });
            }
            if incoming_epoch <= written {
                if incoming_epoch == written
                    && last_op.as_deref() == Some(op_id)
                    && last_digest.as_deref() == Some(payload_digest)
                    && let Some(cached) = last_res
                {
                    return Ok(FencedWriteOutcome::IdempotentReplay {
                        epoch: incoming_epoch,
                        cached_result_json: cached,
                    });
                }
                return Err(FencingError::EpochAlreadyConsumed {
                    resource_id: resource_id.to_string(),
                    incoming: incoming_epoch,
                    written,
                });
            }
            mutate_tx(&conn)?;
            conn.execute(
                "UPDATE resource_epochs
                 SET written_epoch = ?1, last_op_id = ?2, last_payload_digest = ?3, last_result_json = ?4
                 WHERE resource_id = ?5",
                params![incoming_epoch as i64, op_id, payload_digest, result_json, resource_id],
            )
            .map_err(|e| FencingError::Storage(e.to_string()))?;
            Ok(FencedWriteOutcome::Committed { epoch: incoming_epoch })
        })();
        match res {
            Ok(outcome) => {
                conn.execute_batch("COMMIT;").map_err(|e| FencingError::Storage(e.to_string()))?;
                Ok(outcome)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    /// Acquires an exclusive host-level `flock(LOCK_EX)` lock for `resource_id`, serializing
    /// `(verify_epoch_current -> external effect command -> commit_fenced_write)` so no stale
    /// worker can spawn an external mutation after a newer epoch has been granted.
    pub fn acquire_resource_lock(&self, resource_id: &str) -> Result<ResourceLockGuard, FencingError> {
        let locks_dir = self.path.parent().unwrap_or_else(|| Path::new(".")).join("fencing_locks");
        std::fs::create_dir_all(&locks_dir).map_err(|e| FencingError::Storage(e.to_string()))?;
        let digest = line_hash(resource_id.as_bytes()).replace(':', "_");
        let lock_path = locks_dir.join(format!("{digest}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| FencingError::Storage(e.to_string()))?;
        flock(&file, FlockOperation::LockExclusive)
            .map_err(|e| FencingError::Storage(format!("resource flock failed: {e}")))?;
        Ok(ResourceLockGuard { _file: file })
    }

    /// Built-in kernel actuator 1: executes a resource-fenced key/value write inside the EXACT
    /// SAME SQLite `BEGIN IMMEDIATE` transaction that validates `granted_epoch == incoming_epoch
    /// && written_epoch < incoming_epoch` and commits `written_epoch = incoming_epoch`.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_atomic_sqlite_kv(
        &self,
        resource_id: &str,
        incoming_epoch: u64,
        op_id: &str,
        payload_digest: &str,
        result_json: &str,
        key: &str,
        value: &str,
    ) -> Result<FencedWriteOutcome, FencingError> {
        let res_owned = resource_id.to_string();
        let key_owned = key.to_string();
        let val_owned = value.to_string();
        let op_owned = op_id.to_string();
        self.commit_fenced_write_with_tx(resource_id, incoming_epoch, op_id, payload_digest, result_json, move |conn| {
            conn.execute(
                "INSERT INTO fenced_kv (resource_id, key, value, epoch, op_id)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(resource_id, key) DO UPDATE SET
                       value = excluded.value,
                       epoch = excluded.epoch,
                       op_id = excluded.op_id",
                params![res_owned, key_owned, val_owned, incoming_epoch as i64, op_owned],
            )
            .map_err(|e| FencingError::Storage(format!("fenced_kv write failed: {e}")))?;
            Ok(())
        })
    }

    /// Reads the current committed `(value, epoch, op_id)` from `fenced_kv`.
    pub fn read_fenced_kv(&self, resource_id: &str, key: &str) -> Result<Option<(String, u64, String)>, FencingError> {
        let conn = self.connect()?;
        let row: Option<(String, i64, String)> = conn
            .query_row(
                "SELECT value, epoch, op_id FROM fenced_kv WHERE resource_id = ?1 AND key = ?2",
                params![resource_id, key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| FencingError::Storage(format!("read fenced_kv: {e}")))?;
        Ok(row.map(|(v, ep, op)| (v, ep as u64, op)))
    }

    /// Built-in kernel actuator 2: executes a direct `git update-ref` compare-and-swap under the
    /// exclusive per-resource lock (`_rlock`), verifying `incoming_epoch` BEFORE touching git,
    /// committing `written_epoch = incoming_epoch` + `fenced_git_refs` receipt in SQLite, and
    /// rolling back the git ref if the SQLite commit fails (zero shell, zero external scripts).
    #[allow(clippy::too_many_arguments)]
    pub fn execute_atomic_git_ref_cas(
        &self,
        _rlock: &ResourceLockGuard,
        repo: &Path,
        resource_id: &str,
        incoming_epoch: u64,
        op_id: &str,
        payload_digest: &str,
        result_json: &str,
        ref_name: &str,
        expected_old_oid: &str,
        new_oid: &str,
    ) -> Result<FencedWriteOutcome, FencingError> {
        // 1. Check idempotent replay first so a retried op_id+digest never re-runs update-ref.
        struct EpochRow {
            granted: i64,
            written: i64,
            last_op: Option<String>,
            last_dig: Option<String>,
            last_res: Option<String>,
        }
        {
            let conn = self.connect()?;
            let row = conn
                .query_row(
                    "SELECT granted_epoch, written_epoch, last_op_id, last_payload_digest, last_result_json
                     FROM resource_epochs WHERE resource_id = ?1",
                    params![resource_id],
                    |r| {
                        Ok(EpochRow {
                            granted: r.get(0)?,
                            written: r.get(1)?,
                            last_op: r.get(2)?,
                            last_dig: r.get(3)?,
                            last_res: r.get(4)?,
                        })
                    },
                )
                .optional()
                .map_err(|e| FencingError::Storage(format!("read epoch row: {e}")))?;
            if let Some(EpochRow { granted, written, last_op, last_dig, last_res }) = row {
                let granted = granted as u64;
                let written = written as u64;
                if incoming_epoch == written
                    && last_op.as_deref() == Some(op_id)
                    && last_dig.as_deref() == Some(payload_digest)
                {
                    return Ok(FencedWriteOutcome::IdempotentReplay {
                        epoch: incoming_epoch,
                        cached_result_json: last_res.unwrap_or_else(|| "null".to_string()),
                    });
                }
                if incoming_epoch != granted {
                    return Err(FencingError::StaleEpoch {
                        resource_id: resource_id.to_string(),
                        incoming: incoming_epoch,
                        granted,
                    });
                }
                if incoming_epoch <= written {
                    return Err(FencingError::EpochAlreadyConsumed {
                        resource_id: resource_id.to_string(),
                        incoming: incoming_epoch,
                        written,
                    });
                }
            } else {
                return Err(FencingError::StaleEpoch {
                    resource_id: resource_id.to_string(),
                    incoming: incoming_epoch,
                    granted: 0,
                });
            }
        }

        // 2. Validate ref format and commit existence directly via git (fixed trusted PATH, no shell).
        let git_path = maestro_store::tree::git_path();
        let chk = Command::new("git")
            .env("PATH", git_path)
            .args(["check-ref-format", ref_name])
            .current_dir(repo)
            .output()
            .map_err(|e| FencingError::Storage(format!("git check-ref-format spawn: {e}")))?;
        if !chk.status.success() {
            return Err(FencingError::Storage(format!("invalid git ref format: {ref_name:?}")));
        }
        let rev = Command::new("git")
            .env("PATH", git_path)
            .args(["rev-parse", "--verify", "--quiet", &format!("{new_oid}^{{commit}}")])
            .current_dir(repo)
            .output()
            .map_err(|e| FencingError::Storage(format!("git rev-parse spawn: {e}")))?;
        if !rev.status.success() {
            return Err(FencingError::Storage(format!("target commit {new_oid} does not exist in {}", repo.display())));
        }

        // 3. Atomic compare-and-swap ref update in git (fails closed if current OID != expected_old_oid).
        let upd = Command::new("git")
            .env("PATH", git_path)
            .args(["update-ref", "--", ref_name, new_oid, expected_old_oid])
            .current_dir(repo)
            .output()
            .map_err(|e| FencingError::Storage(format!("git update-ref spawn: {e}")))?;
        if !upd.status.success() {
            return Err(FencingError::Storage(format!(
                "git_ref_cas_failed: git update-ref {ref_name} {new_oid} {expected_old_oid} failed: {}",
                String::from_utf8_lossy(&upd.stderr).trim()
            )));
        }

        // 4. Commit written_epoch + durable receipt in SQLite; compensate git ref on failure.
        let res_owned = resource_id.to_string();
        let ref_owned = ref_name.to_string();
        let old_owned = expected_old_oid.to_string();
        let new_owned = new_oid.to_string();
        let op_owned = op_id.to_string();
        let commit_res = self.commit_fenced_write_with_tx(
            resource_id,
            incoming_epoch,
            op_id,
            payload_digest,
            result_json,
            move |conn| {
                conn.execute(
                    "INSERT INTO fenced_git_refs (resource_id, ref_name, old_oid, new_oid, epoch, op_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(resource_id, ref_name) DO UPDATE SET
                       old_oid = excluded.old_oid,
                       new_oid = excluded.new_oid,
                       epoch = excluded.epoch,
                       op_id = excluded.op_id",
                    params![res_owned, ref_owned, old_owned, new_owned, incoming_epoch as i64, op_owned],
                )
                .map_err(|e| FencingError::Storage(format!("fenced_git_refs write failed: {e}")))?;
                Ok(())
            },
        );
        if let Err(err) = commit_res {
            let is_zero = expected_old_oid.bytes().all(|b| b == b'0');
            let _ = if is_zero {
                Command::new("git")
                    .env("PATH", git_path)
                    .args(["update-ref", "-d", ref_name, new_oid])
                    .current_dir(repo)
                    .output()
            } else {
                Command::new("git")
                    .env("PATH", git_path)
                    .args(["update-ref", "--", ref_name, expected_old_oid, new_oid])
                    .current_dir(repo)
                    .output()
            };
            return Err(err);
        }
        commit_res
    }

    /// Pre-mutation fencing check: verifies `incoming_epoch == granted_epoch && incoming_epoch > written_epoch`
    /// BEFORE spawning an external effect command so stale workers never mutate the target.
    pub fn verify_epoch_current(&self, resource_id: &str, incoming_epoch: u64) -> Result<(), FencingError> {
        let conn = self.connect()?;
        let row: Option<(i64, i64)> = conn
            .query_row(
                "SELECT granted_epoch, written_epoch FROM resource_epochs WHERE resource_id = ?1",
                params![resource_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| FencingError::Storage(e.to_string()))?;
        let (granted_i, written_i) = row.ok_or_else(|| FencingError::StaleEpoch {
            resource_id: resource_id.to_string(),
            incoming: incoming_epoch,
            granted: 0,
        })?;
        let granted = granted_i as u64;
        let written = written_i as u64;
        if incoming_epoch != granted {
            return Err(FencingError::StaleEpoch {
                resource_id: resource_id.to_string(),
                incoming: incoming_epoch,
                granted,
            });
        }
        if incoming_epoch <= written {
            return Err(FencingError::EpochAlreadyConsumed {
                resource_id: resource_id.to_string(),
                incoming: incoming_epoch,
                written,
            });
        }
        Ok(())
    }

    /// Verifies that the target itself atomically checked and consumed `incoming_epoch` inside
    /// its own mutation transaction (`written_epoch == incoming_epoch` with matching `op_id` and
    /// `payload_digest`), closing any controller-crash divergence window between external mutation
    /// and epoch consumption.
    pub fn verify_target_consumed(
        &self,
        resource_id: &str,
        incoming_epoch: u64,
        op_id: &str,
        payload_digest: &str,
    ) -> Result<(), FencingError> {
        let conn = self.connect()?;
        let row: Option<(i64, i64, Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT granted_epoch, written_epoch, last_op_id, last_payload_digest
                 FROM resource_epochs WHERE resource_id = ?1",
                params![resource_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(|e| FencingError::Storage(e.to_string()))?;
        let (granted_i, written_i, last_op, last_digest) = row.ok_or_else(|| FencingError::StaleEpoch {
            resource_id: resource_id.to_string(),
            incoming: incoming_epoch,
            granted: 0,
        })?;
        let granted = granted_i as u64;
        let written = written_i as u64;
        if incoming_epoch != granted {
            return Err(FencingError::StaleEpoch {
                resource_id: resource_id.to_string(),
                incoming: incoming_epoch,
                granted,
            });
        }
        if written != incoming_epoch
            || last_op.as_deref() != Some(op_id)
            || last_digest.as_deref() != Some(payload_digest)
        {
            return Err(FencingError::Storage(format!(
                "target did not atomically consume fencing epoch {incoming_epoch} for {resource_id} (written_epoch={written}, last_op={last_op:?})"
            )));
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_parent_grant_barrier_single_use_and_restart_survival() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("fencing.sqlite");
        let store = FencingStore::open(&db).unwrap();

        // (i) Parent A gets epoch 1 and pauses; Parent B gets epoch 2 (advancing granted_epoch=2 BEFORE B writes).
        let e_a = store.grant_epoch("pr:broomva/workspace#1476").unwrap();
        assert_eq!(e_a, 1);
        let e_b = store.grant_epoch("pr:broomva/workspace#1476").unwrap();
        assert_eq!(e_b, 2);

        // Parent A wakes and tries to write with epoch 1 while B has NOT written yet -> MUST be rejected!
        let err_a =
            store.commit_fenced_write("pr:broomva/workspace#1476", e_a, "op-a", "sha256:aaa", "{}").unwrap_err();
        assert_eq!(
            err_a,
            FencingError::StaleEpoch { resource_id: "pr:broomva/workspace#1476".into(), incoming: 1, granted: 2 }
        );

        // (ii) Parent B commits at epoch 2.
        let ok_b = store
            .commit_fenced_write("pr:broomva/workspace#1476", e_b, "op-b", "sha256:bbb", r#"{"ok":true}"#)
            .unwrap();
        assert_eq!(ok_b, FencedWriteOutcome::Committed { epoch: 2 });

        // Duplicate write at epoch 2 with different op/payload MUST be rejected!
        let err_dup =
            store.commit_fenced_write("pr:broomva/workspace#1476", e_b, "op-b2", "sha256:ccc", "{}").unwrap_err();
        assert_eq!(
            err_dup,
            FencingError::EpochAlreadyConsumed {
                resource_id: "pr:broomva/workspace#1476".into(),
                incoming: 2,
                written: 2,
            }
        );

        // Exact same (op_id, payload_digest) returns cached idempotent replay without second mutation.
        let replay = store
            .commit_fenced_write("pr:broomva/workspace#1476", e_b, "op-b", "sha256:bbb", r#"{"ok":true}"#)
            .unwrap();
        assert_eq!(
            replay,
            FencedWriteOutcome::IdempotentReplay { epoch: 2, cached_result_json: r#"{"ok":true}"#.into() }
        );

        // (iii) Reopen after restart; Parent C gets epoch 3 and succeeds without starvation!
        drop(store);
        let reopened = FencingStore::open(&db).unwrap();
        let e_c = reopened.grant_epoch("pr:broomva/workspace#1476").unwrap();
        assert_eq!(e_c, 3);
        let ok_c =
            reopened.commit_fenced_write("pr:broomva/workspace#1476", e_c, "op-c", "sha256:ddd", r#"{"c":1}"#).unwrap();
        assert_eq!(ok_c, FencedWriteOutcome::Committed { epoch: 3 });
    }
}
