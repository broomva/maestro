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
            std::fs::create_dir_all(parent)
                .map_err(|e| FencingError::Storage(format!("create fencing dir: {e}")))?;
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
             CREATE TABLE IF NOT EXISTS registered_targets (
                 resource_id TEXT NOT NULL,
                 argv_digest TEXT NOT NULL,
                 PRIMARY KEY (resource_id, argv_digest)
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
        conn.execute_batch("BEGIN IMMEDIATE;")
            .map_err(|e| FencingError::Storage(e.to_string()))?;
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
        conn.execute_batch("BEGIN IMMEDIATE;")
            .map_err(|e| FencingError::Storage(e.to_string()))?;
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
        let locks_dir = self
            .path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("fencing_locks");
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

    /// Deterministic digest of an effect command's exact `argv` vector for pre-execution target registration.
    pub fn argv_digest(argv: &[String]) -> String {
        let json = serde_json::to_string(argv).unwrap_or_default();
        line_hash(json.as_bytes())
    }

    /// Registers an attested fenced target command (`argv` digest) for `resource_id`, certifying
    /// that this command enforces target-side atomic epoch consumption (`granted_epoch == epoch &&
    /// written_epoch < epoch`) inside its own mutation transaction.
    pub fn register_fenced_target(&self, resource_id: &str, argv: &[String]) -> Result<String, FencingError> {
        if resource_id.is_empty() || argv.is_empty() {
            return Err(FencingError::Storage("resource_id and argv must be non-empty".into()));
        }
        let digest = Self::argv_digest(argv);
        let conn = self.connect()?;
        conn.execute(
            "INSERT OR IGNORE INTO registered_targets (resource_id, argv_digest) VALUES (?1, ?2)",
            params![resource_id, digest],
        )
        .map_err(|e| FencingError::Storage(format!("register fenced target: {e}")))?;
        Ok(digest)
    }

    /// Pre-execution target contract check: refuses any effect command whose exact `argv` digest
    /// has NOT been registered as a fenced target for `resource_id` BEFORE spawning the subprocess.
    pub fn verify_registered_target(&self, resource_id: &str, argv: &[String]) -> Result<(), FencingError> {
        let digest = Self::argv_digest(argv);
        let conn = self.connect()?;
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM registered_targets WHERE resource_id = ?1 AND argv_digest = ?2",
                params![resource_id, digest],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| FencingError::Storage(format!("verify registered target: {e}")))?;
        if found.is_none() {
            return Err(FencingError::Storage(format!(
                "effect argv ({digest}) is not a registered fenced target for resource {resource_id:?}; refusing to execute unfenced command"
            )));
        }
        Ok(())
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
        let err_a = store
            .commit_fenced_write("pr:broomva/workspace#1476", e_a, "op-a", "sha256:aaa", "{}")
            .unwrap_err();
        assert_eq!(
            err_a,
            FencingError::StaleEpoch {
                resource_id: "pr:broomva/workspace#1476".into(),
                incoming: 1,
                granted: 2,
            }
        );

        // (ii) Parent B commits at epoch 2.
        let ok_b = store
            .commit_fenced_write("pr:broomva/workspace#1476", e_b, "op-b", "sha256:bbb", r#"{"ok":true}"#)
            .unwrap();
        assert_eq!(ok_b, FencedWriteOutcome::Committed { epoch: 2 });

        // Duplicate write at epoch 2 with different op/payload MUST be rejected!
        let err_dup = store
            .commit_fenced_write("pr:broomva/workspace#1476", e_b, "op-b2", "sha256:ccc", "{}")
            .unwrap_err();
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
            FencedWriteOutcome::IdempotentReplay {
                epoch: 2,
                cached_result_json: r#"{"ok":true}"#.into(),
            }
        );

        // (iii) Reopen after restart; Parent C gets epoch 3 and succeeds without starvation!
        drop(store);
        let reopened = FencingStore::open(&db).unwrap();
        let e_c = reopened.grant_epoch("pr:broomva/workspace#1476").unwrap();
        assert_eq!(e_c, 3);
        let ok_c = reopened
            .commit_fenced_write("pr:broomva/workspace#1476", e_c, "op-c", "sha256:ddd", r#"{"c":1}"#)
            .unwrap();
        assert_eq!(ok_c, FencedWriteOutcome::Committed { epoch: 3 });
    }
}
