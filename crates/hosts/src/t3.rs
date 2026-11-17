//! T3 thread identity adapter (`statev2.sqlite` and JSON thread snapshots).
//!
//! Reads T3's `orchestration_v2_projection_threads` / `projection_threads`,
//! `projection_projects`, `orchestration_v2_projection_runs`, and
//! `orchestration_v2_projection_runtime_requests` (or a JSON snapshot of the same shape)
//! to verify a T3 thread's existence, liveness, working directory (`worktreePath` or
//! project `workspace_root`), environment id, and pending permission prompts.

use crate::paseo::{PermissionLike, check_peer, describe_permission, permission_request_id, short_id, within};
use maestro_core::session::{
    BridgeDiscovery, BridgeTransport, CallerClaim, Harness, IdentityRefusal, NodeId, PermissionPrompt, RefusalReason,
    SessionLiveness, SessionRef, Surface, VerifiedSession,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The longest T3 thread id Maestro reads (BRO-3846): live delegated ids run 129 to 206 bytes.
pub const MAX_T3_ID: usize = 512;

/// Whether `id` is a plausible T3 thread id: `mcp:<uuid>`, `<uuid>`, or a percent-encoded composite
/// such as `mcp:launch:abc%3Adef`. A leading alphanumeric, then alphanumerics and `-_:.%`, at most
/// [`MAX_T3_ID`] bytes, never `/`, `..` or NUL. Ids are opaque: never percent-decoded, used only as
/// SQL parameters and in comparisons, never as a path, so `%2F` is just three bytes.
pub fn is_safe_t3_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_T3_ID {
        return false;
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    if id.contains('/') || id.contains('\0') || id.contains("..") {
        return false;
    }
    bytes[1..].iter().all(|b| b.is_ascii_alphanumeric() || matches!(*b, b'-' | b'_' | b':' | b'.' | b'%'))
}

/// What Maestro reads of a T3 thread record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct T3Thread {
    pub id: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_permissions: Vec<PermissionPrompt>,
}

impl T3Thread {
    pub fn liveness(&self) -> SessionLiveness {
        if self.deleted_at.is_some()
            || self.status.as_deref() == Some("closed")
            || self.status.as_deref() == Some("deleted")
        {
            SessionLiveness::Closed
        } else if self.archived_at.is_some() || self.status.as_deref() == Some("archived") {
            SessionLiveness::Archived
        } else if self.settled_at.is_some() && self.status.as_deref() != Some("running") {
            SessionLiveness::Settled
        } else {
            SessionLiveness::Live
        }
    }

    pub fn harness(&self) -> Option<Harness> {
        let raw = self.provider.as_deref()?;
        Some(match raw {
            "claudeAgent" | "claude-code" | "claude" => Harness::ClaudeCode,
            "codex" => Harness::Codex,
            "gemini-vertex" | "gemini" => Harness::Gemini,
            "antigravity" | "agy" => Harness::Agy,
            other => Harness::Other(other.to_string()),
        })
    }

    pub fn incarnation(&self) -> Option<String> {
        let created = self.created_at.as_deref().filter(|s| !s.is_empty());
        let run = self.run_id.as_deref().filter(|s| !s.is_empty());
        if created.is_none() && run.is_none() {
            return None;
        }
        Some(format!(
            "t3:{}:{}:{}:{}:{}",
            self.cwd,
            self.environment_id.as_deref().unwrap_or("-"),
            self.provider.as_deref().unwrap_or("-"),
            created.unwrap_or("-"),
            run.unwrap_or("-"),
        ))
    }
}

#[derive(Deserialize)]
struct RawT3Thread {
    id: String,
    cwd: String,
    #[serde(default)]
    environment_id: Option<String>,
    #[serde(default)]
    node: Option<NodeId>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default, alias = "createdAt")]
    created_at: Option<String>,
    #[serde(default, alias = "runId", alias = "session_id", alias = "sessionId")]
    run_id: Option<String>,
    #[serde(default)]
    archived_at: Option<String>,
    #[serde(default)]
    deleted_at: Option<String>,
    #[serde(default)]
    settled_at: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    pending_permissions: Option<Vec<Value>>,
    /// The thread's active provider-thread row (`active_provider_thread_id`).
    #[serde(default)]
    active_provider_thread_id: Option<String>,
    /// The thread's provider-thread rows, active or retired, each with its native id
    /// (`payload_json.$.nativeThreadRef.nativeId`), as `orchestration_v2_projection_provider_threads`
    /// holds them.
    #[serde(default)]
    provider_threads: Vec<RawProviderThread>,
}

#[derive(Deserialize)]
struct RawProviderThread {
    provider_thread_id: String,
    #[serde(default)]
    native_id: Option<String>,
}

/// What one thread variable resolved to (BRO-3846).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Exactly one thread, neither archived nor deleted.
    Thread(T3Thread),
    /// No thread, more than one, or one archived or deleted: why.
    Unresolved(String),
}

/// T3 thread registry reader (`statev2.sqlite` or JSON snapshot).
#[derive(Debug, Clone)]
pub struct T3Registry {
    root: PathBuf,
    expected_uid: Option<u32>,
    expected_node: Option<NodeId>,
    disconnected: bool,
}

const SQLITE_READER_PY: &str = r#"
import json, pathlib, sqlite3, sys

def main():
    mode = sys.argv[1]
    db_path = pathlib.Path(sys.argv[2])
    target_id = sys.argv[3] if len(sys.argv) > 3 else ""

    env_file = db_path.parent / "environment-id"
    env_id = env_file.read_text().strip() if env_file.is_file() else None

    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    tables = {r["name"] for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'")}

    def build_thread(row):
        try:
            payload = json.loads(row["payload_json"] or "{}")
        except Exception:
            payload = {}
        if not isinstance(payload, dict):
            payload = {}
        cwd = payload.get("worktreePath")
        if not cwd and "projection_projects" in tables:
            proj = conn.execute(
                "SELECT workspace_root FROM projection_projects WHERE project_id = ?",
                (row["project_id"],),
            ).fetchone()
            if proj:
                cwd = proj["workspace_root"]
        if not isinstance(cwd, str) or not cwd:
            return None
        run_status = payload.get("status")
        latest_run_id = None
        if "orchestration_v2_projection_runs" in tables:
            r = conn.execute(
                "SELECT run_id, status FROM orchestration_v2_projection_runs WHERE thread_id = ? ORDER BY ordinal DESC LIMIT 1",
                (row["thread_id"],),
            ).fetchone()
            if r:
                if r["status"]:
                    run_status = r["status"]
                latest_run_id = r["run_id"]
        settled_at = payload.get("settledAt")
        unsettled_at = payload.get("unsettledAt")
        if settled_at and unsettled_at and unsettled_at > settled_at:
            settled_at = None
        parent_id = None
        lineage = payload.get("lineage") or payload.get("forkedFrom")
        if isinstance(lineage, dict):
            parent_id = lineage.get("parentThreadId") or lineage.get("threadId")
        perms = []
        if "orchestration_v2_projection_runtime_requests" in tables:
            for req in conn.execute(
                "SELECT runtime_request_id, kind, status, payload_json "
                "FROM orchestration_v2_projection_runtime_requests "
                "WHERE thread_id = ? AND status = 'pending' ORDER BY created_at ASC",
                (row["thread_id"],),
            ):
                if req["kind"] in ("approval", "permission", "tool_approval"):
                    try:
                        rp = json.loads(req["payload_json"] or "{}")
                    except Exception:
                        continue
                    if not isinstance(rp, dict):
                        continue
                    name = rp.get("toolName") or rp.get("name") or req["kind"]
                    if not isinstance(name, str) or not name.strip():
                        continue
                    req_id = req["runtime_request_id"]
                    if not isinstance(req_id, str) or not req_id.strip():
                        req_id = rp.get("id") if isinstance(rp.get("id"), str) else None
                    perms.append({
                        "id": req_id,
                        "name": name,
                        "title": rp.get("title") if isinstance(rp.get("title"), str) else None,
                        "description": rp.get("description") if isinstance(rp.get("description"), str) else None,
                        "input": rp.get("input") if isinstance(rp.get("input"), dict) else None,
                    })
        created_at = row["created_at"] if "created_at" in row.keys() and row["created_at"] else payload.get("createdAt")
        run_marker = latest_run_id or payload.get("runId") or payload.get("sessionId")
        return {
            "id": row["thread_id"],
            "cwd": cwd,
            "environment_id": env_id,
            "node": payload.get("node") if isinstance(payload.get("node"), str) else None,
            "provider": row["default_provider"],
            "created_at": created_at,
            "run_id": run_marker if isinstance(run_marker, str) and run_marker else None,
            "archived_at": row["archived_at"] or payload.get("archivedAt"),
            "deleted_at": row["deleted_at"] or payload.get("deletedAt"),
            "settled_at": settled_at,
            "status": run_status,
            "parent_id": parent_id,
            "pending_permissions": perms,
        }

    if mode == "native":
        # The threads whose ACTIVE provider-thread row carries this native id (a Codex
        # CODEX_THREAD_ID, or a Stop hook's session_id). A retired provider row never attributes.
        out = []
        if "orchestration_v2_projection_threads" in tables and "orchestration_v2_projection_provider_threads" in tables:
            rows = conn.execute(
                "SELECT t.thread_id AS thread_id, p.payload_json AS payload_json "
                "FROM orchestration_v2_projection_threads t "
                "JOIN orchestration_v2_projection_provider_threads p "
                "ON p.provider_thread_id = t.active_provider_thread_id AND p.thread_id = t.thread_id"
            )
            for r in rows:
                try:
                    payload = json.loads(r["payload_json"] or "{}")
                except Exception:
                    continue
                ref = payload.get("nativeThreadRef") if isinstance(payload, dict) else None
                if isinstance(ref, dict) and ref.get("nativeId") == target_id and r["thread_id"] not in out:
                    out.append(r["thread_id"])
        print(json.dumps(out))
        sys.exit(0)

    if mode == "list":
        out = []
        if "orchestration_v2_projection_threads" in tables:
            for row in conn.execute(
                "SELECT rowid AS _rowid, thread_id, project_id, default_provider, created_at, archived_at, deleted_at, payload_json "
                "FROM orchestration_v2_projection_threads ORDER BY updated_at DESC"
            ):
                built = build_thread(row)
                if built is not None:
                    out.append(built)
        print(json.dumps(out))
        sys.exit(0)

    if "orchestration_v2_projection_threads" in tables:
        row = conn.execute(
            "SELECT rowid AS _rowid, thread_id, project_id, default_provider, created_at, archived_at, deleted_at, payload_json "
            "FROM orchestration_v2_projection_threads WHERE thread_id = ?",
            (target_id,),
        ).fetchone()
        if row is not None:
            built = build_thread(row)
            if built is not None:
                print(json.dumps(built))
                sys.exit(0)

    if "projection_threads" in tables:
        pcols = {r["name"] for r in conn.execute("PRAGMA table_info(projection_threads)")}
        created_sel = "created_at" if "created_at" in pcols else "NULL AS created_at"
        prow = conn.execute(
            f"SELECT rowid AS _rowid, thread_id, project_id, worktree_path, {created_sel}, archived_at, deleted_at, settled_at "
            "FROM projection_threads WHERE thread_id = ?",
            (target_id,),
        ).fetchone()
        if prow is not None:
            cwd = prow["worktree_path"]
            if not cwd and "projection_projects" in tables:
                proj = conn.execute(
                    "SELECT workspace_root FROM projection_projects WHERE project_id = ?",
                    (prow["project_id"],),
                ).fetchone()
                if proj:
                    cwd = proj["workspace_root"]
            provider = None
            status = None
            session_id = None
            if "projection_thread_sessions" in tables:
                scols = {r["name"] for r in conn.execute("PRAGMA table_info(projection_thread_sessions)")}
                sid_sel = "session_id" if "session_id" in scols else ("run_id AS session_id" if "run_id" in scols else "NULL AS session_id")
                srow = conn.execute(
                    f"SELECT provider_name, status, {sid_sel} FROM projection_thread_sessions WHERE thread_id = ?",
                    (target_id,),
                ).fetchone()
                if srow:
                    provider = srow["provider_name"]
                    status = srow["status"]
                    session_id = srow["session_id"]
            if cwd:
                print(json.dumps({
                    "id": prow["thread_id"],
                    "cwd": cwd,
                    "environment_id": env_id,
                    "node": None,
                    "provider": provider,
                    "created_at": prow["created_at"],
                    "run_id": session_id if isinstance(session_id, str) and session_id else None,
                    "archived_at": prow["archived_at"],
                    "deleted_at": prow["deleted_at"],
                    "settled_at": prow["settled_at"],
                    "status": status,
                    "parent_id": None,
                    "pending_permissions": [],
                }))
                sys.exit(0)

    sys.exit(3)

try:
    main()
except SystemExit:
    raise
except Exception as exc:
    sys.stderr.write(f"sqlite_error: {exc}\n")
    sys.exit(2)
"#;

impl T3Registry {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), expected_uid: None, expected_node: None, disconnected: false }
    }

    pub fn with_expected_uid(mut self, uid: u32) -> Self {
        self.expected_uid = Some(uid);
        self
    }

    pub fn with_expected_node(mut self, node: NodeId) -> Self {
        self.expected_node = Some(node);
        self
    }

    pub fn with_disconnected(mut self, disconnected: bool) -> Self {
        self.disconnected = disconnected;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_connected(&self) -> bool {
        !self.disconnected && (self.json_path().is_some() || self.sqlite_path().is_some())
    }

    /// Discover T3 runtime version, transport, environment id, and connection state.
    pub fn discover(&self) -> BridgeDiscovery {
        let userdata = if self.root.join("userdata").is_dir() {
            self.root.join("userdata")
        } else if self.sqlite_path().and_then(|p| p.parent().map(Path::to_path_buf)).is_some() {
            self.sqlite_path().unwrap().parent().unwrap().to_path_buf()
        } else {
            self.root.clone()
        };

        let env_id = std::fs::read_to_string(userdata.join("environment-id"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let runtime_json = std::fs::read_to_string(userdata.join("server-runtime.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok());
        let service_json = std::fs::read_to_string(self.root.join("runtime/service-state.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok());

        let version = runtime_json
            .as_ref()
            .and_then(|v| v.get("serverVersion"))
            .and_then(Value::as_str)
            .or_else(|| service_json.as_ref().and_then(|v| v.get("activeVersion")).and_then(Value::as_str))
            .unwrap_or("0.1.3")
            .to_string();

        let endpoint = std::env::var("T3_ACP_MCP_ENDPOINT").ok().filter(|s| !s.is_empty()).or_else(|| {
            runtime_json
                .as_ref()
                .and_then(|v| v.get("origin"))
                .and_then(Value::as_str)
                .map(|origin| format!("{}/mcp", origin.trim_end_matches('/')))
        });

        let transport = if endpoint.is_some() && self.sqlite_path().is_some() {
            BridgeTransport::T3McpBridge
        } else if self.sqlite_path().is_some() {
            BridgeTransport::T3SqliteReadonly
        } else {
            BridgeTransport::T3JsonSnapshot
        };

        BridgeDiscovery {
            transport,
            version,
            endpoint,
            environment_id: env_id,
            node: self.expected_node.clone(),
            connected: self.is_connected(),
        }
    }

    fn sqlite_path(&self) -> Option<PathBuf> {
        [self.root.clone(), self.root.join("statev2.sqlite"), self.root.join("userdata/statev2.sqlite")]
            .into_iter()
            .find(|candidate| {
                candidate.is_file() && candidate.extension().is_some_and(|ext| ext == "sqlite" || ext == "db")
            })
    }

    fn json_path(&self) -> Option<PathBuf> {
        [self.root.clone(), self.root.join("threads.json")]
            .into_iter()
            .find(|candidate| candidate.is_file() && candidate.extension().is_some_and(|ext| ext == "json"))
    }

    fn from_raw(raw: RawT3Thread) -> T3Thread {
        let pending_permissions = raw
            .pending_permissions
            .unwrap_or_default()
            .into_iter()
            .filter_map(|val| serde_json::from_value::<PermissionLike>(val).ok())
            .map(|p| {
                let (title, detail) = describe_permission(&p, Some(&raw.cwd));
                let request_id = permission_request_id(&p, "t3-perm");
                PermissionPrompt { request_id, title, detail }
            })
            .collect();
        T3Thread {
            id: raw.id,
            cwd: raw.cwd,
            environment_id: raw.environment_id,
            node: raw.node,
            provider: raw.provider,
            created_at: raw.created_at,
            run_id: raw.run_id,
            archived_at: raw.archived_at,
            deleted_at: raw.deleted_at,
            settled_at: raw.settled_at,
            status: raw.status,
            parent_id: raw.parent_id,
            pending_permissions,
        }
    }

    /// The environment id this host's T3 registry reports (`userdata/environment-id`), the
    /// canonical form of a `local` or `-` T3 session (BRO-3846).
    pub fn environment_id(&self) -> Option<String> {
        self.discover().environment_id
    }

    /// The threads whose active provider-thread row carries `native_id`, read from the registry.
    fn native_threads(&self, native_id: &str) -> Result<Vec<String>, IdentityRefusal> {
        let unreadable = |detail: String| IdentityRefusal::new(RefusalReason::RegistryUnavailable, detail);
        if self.disconnected {
            return Err(unreadable("T3 registry is disconnected.".to_string()));
        }
        if let Some(json_file) = self.json_path() {
            let text = std::fs::read_to_string(json_file)
                .map_err(|e| unreadable(format!("T3 JSON registry could not be read: {e}.")))?;
            let rows: Vec<RawT3Thread> =
                serde_json::from_str(&text).map_err(|e| unreadable(format!("T3 JSON registry is malformed: {e}.")))?;
            let mut out: Vec<String> = Vec::new();
            for raw in rows {
                let Some(active) = raw.active_provider_thread_id.as_deref() else { continue };
                let hit = raw
                    .provider_threads
                    .iter()
                    .any(|p| p.provider_thread_id == active && p.native_id.as_deref() == Some(native_id));
                if hit && !out.contains(&raw.id) {
                    out.push(raw.id);
                }
            }
            return Ok(out);
        }
        if let Some(db_file) = self.sqlite_path() {
            let out = Command::new("python3")
                .arg("-I")
                .arg("-c")
                .arg(SQLITE_READER_PY)
                .arg("native")
                .arg(&db_file)
                .arg(native_id)
                .output()
                .map_err(|e| unreadable(format!("T3 SQLite reader failed to launch: {e}.")))?;
            if out.status.code() != Some(0) {
                let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return Err(unreadable(format!("T3 SQLite database is unreadable: {detail}")));
            }
            return serde_json::from_slice(&out.stdout)
                .map_err(|e| unreadable(format!("T3 SQLite reader returned invalid JSON: {e}.")));
        }
        Err(unreadable(format!("No T3 registry database or snapshot at {}.", self.root.display())))
    }

    /// Resolves a thread variable to its thread (BRO-3846). `native` is false for
    /// `T3CODE_THREAD_ID`, which names a thread row; true for `CODEX_THREAD_ID` or a Stop hook's
    /// `session_id`, which name a provider's native thread and resolve through the thread's active
    /// provider row only. Liveness is `archived_at` and `deleted_at` unset; `settled_at` and the run
    /// status are ignored. A registry that does not read is `registry_unavailable`, never a fallback.
    pub fn resolve(&self, value: &str, native: bool) -> Result<Resolution, IdentityRefusal> {
        let thread_id = if native {
            if value.is_empty() || value.len() > MAX_T3_ID || value.contains('\0') {
                return Ok(Resolution::Unresolved(format!("{} is not a native thread id", short_id(value))));
            }
            let found = self.native_threads(value)?;
            match found.as_slice() {
                [] => {
                    return Ok(Resolution::Unresolved(format!(
                        "no T3 thread's active provider row is {}",
                        short_id(value)
                    )));
                }
                [one] => one.clone(),
                many => {
                    return Ok(Resolution::Unresolved(format!(
                        "{} active provider rows carry {}",
                        many.len(),
                        short_id(value)
                    )));
                }
            }
        } else {
            value.to_string()
        };
        if !is_safe_t3_id(&thread_id) {
            return Ok(Resolution::Unresolved(format!("{} is not a T3 thread id", short_id(&thread_id))));
        }
        if self.disconnected {
            return Err(IdentityRefusal::new(RefusalReason::RegistryUnavailable, "T3 registry is disconnected."));
        }
        let thread =
            self.lookup(&thread_id).map_err(|r| IdentityRefusal::new(RefusalReason::RegistryUnavailable, r.message))?;
        Ok(match thread {
            None => Resolution::Unresolved(format!("no T3 thread has the id {}", short_id(&thread_id))),
            Some(t) if t.archived_at.is_some() => {
                Resolution::Unresolved(format!("thread {} is archived", short_id(&t.id)))
            }
            Some(t) if t.deleted_at.is_some() => {
                Resolution::Unresolved(format!("thread {} is deleted", short_id(&t.id)))
            }
            Some(t) => Resolution::Thread(t),
        })
    }

    fn lookup(&self, id: &str) -> Result<Option<T3Thread>, IdentityRefusal> {
        if !is_safe_t3_id(id) {
            return Ok(None);
        }
        if let Some(json_file) = self.json_path() {
            let text = std::fs::read_to_string(json_file).map_err(|e| {
                IdentityRefusal::new(
                    RefusalReason::RegistryDisconnected,
                    format!("T3 JSON registry could not be read: {e}."),
                )
            })?;
            let rows: Vec<RawT3Thread> = serde_json::from_str(&text).map_err(|e| {
                IdentityRefusal::new(
                    RefusalReason::RegistryDisconnected,
                    format!("T3 JSON registry is malformed: {e}."),
                )
            })?;
            return Ok(rows.into_iter().find(|raw| raw.id == id).map(Self::from_raw));
        }
        if let Some(db_file) = self.sqlite_path() {
            let out = Command::new("python3")
                .arg("-I")
                .arg("-c")
                .arg(SQLITE_READER_PY)
                .arg("get")
                .arg(&db_file)
                .arg(id)
                .output()
                .map_err(|e| {
                    IdentityRefusal::new(
                        RefusalReason::RegistryDisconnected,
                        format!("T3 SQLite reader failed to launch: {e}."),
                    )
                })?;
            match out.status.code() {
                Some(0) => {
                    let raw: RawT3Thread = serde_json::from_slice(&out.stdout).map_err(|e| {
                        IdentityRefusal::new(
                            RefusalReason::RegistryDisconnected,
                            format!("T3 SQLite reader returned invalid JSON: {e}."),
                        )
                    })?;
                    if raw.id != id {
                        return Ok(None);
                    }
                    return Ok(Some(Self::from_raw(raw)));
                }
                Some(3) => return Ok(None),
                _ => {
                    return Err(IdentityRefusal::new(
                        RefusalReason::RegistryDisconnected,
                        format!("T3 SQLite database is unreadable: {}", String::from_utf8_lossy(&out.stderr).trim()),
                    ));
                }
            }
        }
        Err(IdentityRefusal::new(
            RefusalReason::RegistryDisconnected,
            format!("No T3 registry database or snapshot at {}.", self.root.display()),
        ))
    }

    /// Look up `id` in T3's thread registry (`statev2.sqlite` or `threads.json`).
    pub fn find(&self, id: &str) -> Option<T3Thread> {
        if !self.is_connected() {
            return None;
        }
        self.lookup(id).ok().flatten()
    }

    /// Enumerate all live T3 threads in the registry.
    pub fn list_live_threads(&self) -> Vec<T3Thread> {
        if !self.is_connected() {
            return Vec::new();
        }
        if let Some(json_file) = self.json_path() {
            let Ok(text) = std::fs::read_to_string(json_file) else {
                return Vec::new();
            };
            let Ok(rows) = serde_json::from_str::<Vec<RawT3Thread>>(&text) else {
                return Vec::new();
            };
            return rows.into_iter().map(Self::from_raw).filter(|t| t.liveness().is_live()).collect();
        }
        if let Some(db_file) = self.sqlite_path() {
            let Ok(out) =
                Command::new("python3").arg("-I").arg("-c").arg(SQLITE_READER_PY).arg("list").arg(&db_file).output()
            else {
                return Vec::new();
            };
            if !out.status.success() {
                return Vec::new();
            }
            let Ok(rows) = serde_json::from_slice::<Vec<RawT3Thread>>(&out.stdout) else {
                return Vec::new();
            };
            return rows.into_iter().map(Self::from_raw).filter(|t| t.liveness().is_live()).collect();
        }
        Vec::new()
    }

    /// Verify the thread a claiming process's own thread variable resolved to (BRO-3846). Called
    /// from `SurfaceRegistry::verify_session` for a T3 claim with an attribution and no credential,
    /// so the node, environment, generation and cross-surface checks there still apply. It needs
    /// no folder: the variable, not a cwd, picks the thread. Liveness is attribution's (`archived_at`
    /// and `deleted_at` unset). It sets the same owner marker [`T3Registry::verify`] sets.
    pub fn verify_attributed(
        &self,
        thread_id: &str,
        node: Option<&NodeId>,
    ) -> Result<VerifiedSession, IdentityRefusal> {
        if self.disconnected {
            return Err(IdentityRefusal::new(RefusalReason::RegistryDisconnected, "T3 registry is disconnected."));
        }
        let Some(thread) = self.lookup(thread_id)? else {
            return Err(IdentityRefusal::new(
                RefusalReason::AttributionUnresolved,
                format!("No T3 thread has the id {}.", short_id(thread_id)),
            ));
        };
        let presented = node.or(self.expected_node.as_ref());
        for (want, actual) in [(thread.node.as_ref(), presented), (self.expected_node.as_ref(), node)] {
            if let (Some(want), Some(actual)) = (want, actual)
                && want != actual
            {
                return Err(IdentityRefusal::new(
                    RefusalReason::WrongNode,
                    format!("Thread {} is bound to node {want}, not {actual}.", short_id(thread_id)),
                ));
            }
        }
        if thread.archived_at.is_some() || thread.deleted_at.is_some() {
            return Err(IdentityRefusal::new(
                RefusalReason::AttributionUnresolved,
                format!("Thread {} is archived or deleted.", short_id(thread_id)),
            ));
        }
        let Some(incarnation) = thread.incarnation() else {
            return Err(IdentityRefusal::new(
                RefusalReason::InvalidToken,
                format!("Thread {thread_id} lacks an authoritative incarnation marker."),
            ));
        };
        Ok(Self::verified(thread, incarnation))
    }

    /// A verified T3 session for `thread`, with its owner marker (ADR flow-ledger decision 6): fixed
    /// across turns and provider switches, new when the thread is recreated. The environment lives
    /// in the SessionRef, not here. No `created_at`, no marker: writes are then `no_owner_marker`.
    fn verified(thread: T3Thread, incarnation: String) -> VerifiedSession {
        let harness = thread.harness();
        let owner_marker = thread
            .created_at
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|created_at| format!("t3-owner:{}:{created_at}", thread.id));
        let verified = VerifiedSession::new(
            SessionRef { surface: Surface::T3, environment: thread.environment_id, id: thread.id },
            thread.cwd,
            harness,
            thread.parent_id,
            thread.pending_permissions,
        )
        .with_incarnation(incarnation);
        match owner_marker {
            Some(marker) => verified.with_owner_marker(marker),
            None => verified,
        }
    }

    /// Verify a caller claim against T3's thread registry.
    pub fn verify(&self, claim: &CallerClaim) -> Result<VerifiedSession, IdentityRefusal> {
        if claim.surface != Surface::T3 {
            return Err(IdentityRefusal::new(
                RefusalReason::CrossSurface,
                format!("Surface `{}` cannot be verified by the T3 registry.", claim.surface.as_str()),
            ));
        }
        if self.disconnected {
            return Err(IdentityRefusal::new(RefusalReason::RegistryDisconnected, "T3 registry is disconnected."));
        }
        check_peer(self.expected_uid, claim.peer)?;
        let Some(thread_id) = claim.session_id.as_deref().filter(|s| !s.is_empty()) else {
            return Err(IdentityRefusal::new(
                RefusalReason::MissingSessionId,
                "Maestro cannot tell which T3 thread is asking: no thread id was supplied.",
            ));
        };
        let Some(cwd) = claim.cwd.as_deref().filter(|s| !s.is_empty()) else {
            return Err(IdentityRefusal::new(
                RefusalReason::MissingCwd,
                "Maestro cannot tell where this shell is: the request carried no folder.",
            ));
        };
        if !Path::new(cwd).is_absolute() {
            return Err(IdentityRefusal::new(
                RefusalReason::RelativeCwd,
                format!("The shell's folder must be an absolute path, not {cwd}."),
            ));
        }
        let Some(thread) = self.lookup(thread_id)? else {
            return Err(IdentityRefusal::new(RefusalReason::NotFound, format!("No T3 thread has the id {thread_id}.")));
        };
        let presented = claim.node.as_ref().or(self.expected_node.as_ref());
        for (want, actual) in [(thread.node.as_ref(), presented), (self.expected_node.as_ref(), claim.node.as_ref())] {
            if let (Some(want), Some(actual)) = (want, actual)
                && want != actual
            {
                return Err(IdentityRefusal::new(
                    RefusalReason::WrongNode,
                    format!("Thread {thread_id} is bound to node {want}, not {actual}."),
                ));
            }
        }
        let liveness = thread.liveness();
        if !liveness.is_live() {
            let short = short_id(thread_id);
            return Err(IdentityRefusal::new(
                RefusalReason::NotLive,
                format!("Thread {short} is {}, not live.", liveness.as_str()),
            ));
        }
        if !within(Path::new(cwd), Path::new(&thread.cwd)) {
            let short = short_id(thread_id);
            return Err(IdentityRefusal::new(
                RefusalReason::OutsideCwd,
                format!(
                    "This shell is in {cwd}, but thread {short} runs in {}. Run maestro from inside that folder.",
                    thread.cwd
                ),
            ));
        }
        let Some(incarnation) = thread.incarnation() else {
            return Err(IdentityRefusal::new(
                RefusalReason::InvalidToken,
                format!("Thread {thread_id} lacks an authoritative incarnation marker."),
            ));
        };
        Ok(Self::verified(thread, incarnation))
    }
}
