//! T3 thread identity adapter (`statev2.sqlite` and JSON thread snapshots).
//!
//! Reads T3's `orchestration_v2_projection_threads` / `projection_threads`,
//! `projection_projects`, `orchestration_v2_projection_runs`, and
//! `orchestration_v2_projection_runtime_requests` (or a JSON snapshot of the same shape)
//! to verify a T3 thread's existence, liveness, working directory (`worktreePath` or
//! project `workspace_root`), environment id, and pending permission prompts.

use crate::paseo::{PermissionLike, check_peer, describe_permission, short_id, within};
use maestro_core::session::{
    CallerClaim, Harness, IdentityRefusal, PermissionPrompt, RefusalReason, SessionLiveness, SessionRef, Surface,
    VerifiedSession,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Whether `id` is a plausible T3 thread id (e.g. `mcp:<uuid>` or `<uuid>` or token, with no path separators or NUL).
pub fn is_safe_t3_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 {
        return false;
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    bytes[1..].iter().all(|b| b.is_ascii_alphanumeric() || matches!(*b, b'-' | b'_' | b':' | b'.'))
        && !id.contains("..")
}

/// What Maestro reads of a T3 thread record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct T3Thread {
    pub id: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
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
}

#[derive(Deserialize)]
struct RawT3Thread {
    id: String,
    cwd: String,
    #[serde(default)]
    environment_id: Option<String>,
    #[serde(default)]
    provider: Option<String>,
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
    pending_permissions: Option<Vec<PermissionLike>>,
}

/// T3 thread registry reader (`statev2.sqlite` or JSON snapshot).
#[derive(Debug, Clone)]
pub struct T3Registry {
    root: PathBuf,
    expected_uid: Option<u32>,
}

const SQLITE_READER_PY: &str = r#"
import json, pathlib, sqlite3, sys

db_path = pathlib.Path(sys.argv[1])
thread_id = sys.argv[2]
env_file = db_path.parent / "environment-id"
env_id = env_file.read_text().strip() if env_file.is_file() else None

conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
conn.row_factory = sqlite3.Row
tables = {r["name"] for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'")}

row = None
if "orchestration_v2_projection_threads" in tables:
    row = conn.execute(
        "SELECT thread_id, project_id, default_provider, archived_at, deleted_at, payload_json "
        "FROM orchestration_v2_projection_threads WHERE thread_id = ?",
        (thread_id,),
    ).fetchone()

if row is not None:
    payload = json.loads(row["payload_json"] or "{}")
    cwd = payload.get("worktreePath")
    if not cwd and "projection_projects" in tables:
        proj = conn.execute(
            "SELECT workspace_root FROM projection_projects WHERE project_id = ?",
            (row["project_id"],),
        ).fetchone()
        if proj:
            cwd = proj["workspace_root"]
    run_status = payload.get("status")
    if "orchestration_v2_projection_runs" in tables:
        r = conn.execute(
            "SELECT status FROM orchestration_v2_projection_runs WHERE thread_id = ? ORDER BY ordinal DESC LIMIT 1",
            (thread_id,),
        ).fetchone()
        if r and r["status"]:
            run_status = r["status"]
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
            (thread_id,),
        ):
            if req["kind"] in ("approval", "permission", "tool_approval"):
                rp = json.loads(req["payload_json"] or "{}")
                perms.append({
                    "id": req["runtime_request_id"],
                    "name": rp.get("toolName") or rp.get("name") or req["kind"],
                    "title": rp.get("title"),
                    "description": rp.get("description"),
                    "input": rp.get("input") if isinstance(rp.get("input"), dict) else None,
                })
    if cwd:
        print(json.dumps({
            "id": row["thread_id"],
            "cwd": cwd,
            "environment_id": env_id,
            "provider": row["default_provider"],
            "archived_at": row["archived_at"] or payload.get("archivedAt"),
            "deleted_at": row["deleted_at"] or payload.get("deletedAt"),
            "settled_at": settled_at,
            "status": run_status,
            "parent_id": parent_id,
            "pending_permissions": perms,
        }))
        sys.exit(0)

if "projection_threads" in tables:
    prow = conn.execute(
        "SELECT thread_id, project_id, worktree_path, archived_at, deleted_at, settled_at "
        "FROM projection_threads WHERE thread_id = ?",
        (thread_id,),
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
        if "projection_thread_sessions" in tables:
            srow = conn.execute(
                "SELECT provider_name, status FROM projection_thread_sessions WHERE thread_id = ?",
                (thread_id,),
            ).fetchone()
            if srow:
                provider = srow["provider_name"]
                status = srow["status"]
        if cwd:
            print(json.dumps({
                "id": prow["thread_id"],
                "cwd": cwd,
                "environment_id": env_id,
                "provider": provider,
                "archived_at": prow["archived_at"],
                "deleted_at": prow["deleted_at"],
                "settled_at": prow["settled_at"],
                "status": status,
                "parent_id": None,
                "pending_permissions": [],
            }))
            sys.exit(0)

sys.exit(1)
"#;

impl T3Registry {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), expected_uid: None }
    }

    pub fn with_expected_uid(mut self, uid: u32) -> Self {
        self.expected_uid = Some(uid);
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
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
            .enumerate()
            .map(|(idx, p)| {
                let (title, detail) = describe_permission(&p, Some(&raw.cwd));
                let request_id = p.id.filter(|s| !s.is_empty()).unwrap_or_else(|| format!("t3-perm-{}", idx + 1));
                PermissionPrompt { request_id, title, detail }
            })
            .collect();
        T3Thread {
            id: raw.id,
            cwd: raw.cwd,
            environment_id: raw.environment_id,
            provider: raw.provider,
            archived_at: raw.archived_at,
            deleted_at: raw.deleted_at,
            settled_at: raw.settled_at,
            status: raw.status,
            parent_id: raw.parent_id,
            pending_permissions,
        }
    }

    /// Look up `id` in T3's thread registry (`statev2.sqlite` or `threads.json`).
    pub fn find(&self, id: &str) -> Option<T3Thread> {
        if !is_safe_t3_id(id) {
            return None;
        }
        if let Some(json_file) = self.json_path() {
            let text = std::fs::read_to_string(json_file).ok()?;
            let rows: Vec<RawT3Thread> = serde_json::from_str(&text).ok()?;
            return rows.into_iter().find(|raw| raw.id == id).map(Self::from_raw);
        }
        if let Some(db_file) = self.sqlite_path() {
            let out = Command::new("python3")
                .arg("-P")
                .arg("-c")
                .arg(SQLITE_READER_PY)
                .arg(&db_file)
                .arg(id)
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let raw: RawT3Thread = serde_json::from_slice(&out.stdout).ok()?;
            if raw.id != id {
                return None;
            }
            return Some(Self::from_raw(raw));
        }
        None
    }

    /// Verify a caller claim against T3's thread registry.
    pub fn verify(&self, claim: &CallerClaim) -> Result<VerifiedSession, IdentityRefusal> {
        if claim.surface != Surface::T3 {
            return Err(IdentityRefusal::new(
                RefusalReason::CrossSurface,
                format!("Surface `{}` cannot be verified by the T3 registry.", claim.surface.as_str()),
            ));
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
        let Some(thread) = self.find(thread_id) else {
            return Err(IdentityRefusal::new(RefusalReason::NotFound, format!("No T3 thread has the id {thread_id}.")));
        };
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
        let harness = thread.harness();
        Ok(VerifiedSession::new(
            SessionRef { surface: Surface::T3, environment: thread.environment_id, id: thread.id },
            thread.cwd,
            harness,
            thread.parent_id,
            thread.pending_permissions,
        ))
    }
}
