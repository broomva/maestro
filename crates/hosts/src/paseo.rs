//! Paseo caller verification and permission-prompt formatting (`server/callers.ts`,
//! `server/permissions.ts`).
//!
//! Reads `<dir>/<folder>/<agent-id>.json` without ever reading out bearer tokens or
//! persistence secrets. Verifies that the named agent exists, its file's own `id`
//! matches the file name, it is live (neither archived nor closed), and the caller's
//! `cwd` resolves inside the session's registered folder by whole path segments.

use maestro_core::session::{
    CallerClaim, Harness, IdentityRefusal, PeerCredentials, PermissionPrompt, RefusalReason, SessionLiveness,
    SessionRef, Surface, VerifiedSession,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

/// Label key recording which Paseo agent started a child session (`server/labels.ts`).
pub const PARENT_AGENT_LABEL: &str = "paseo.parent-agent-id";

/// Whether `id` is a safe Paseo agent file name (`^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`).
pub fn is_safe_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 {
        return false;
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    bytes[1..].iter().all(|b| b.is_ascii_alphanumeric() || matches!(*b, b'.' | b'_' | b'-'))
}

/// A raw permission prompt entry on a session record (`server/permissions.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionLike {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub input: Option<serde_json::Map<String, Value>>,
}

/// What Maestro reads from a Paseo registry file (`server/callers.ts` `RegistryAgent`).
/// Bearer tokens and `persistence` credentials are never read into this struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaseoAgent {
    pub id: String,
    pub cwd: String,
    pub archived_at: Option<String>,
    pub status: Option<String>,
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_permissions: Vec<PermissionPrompt>,
}

impl PaseoAgent {
    pub fn liveness(&self) -> SessionLiveness {
        if self.archived_at.is_some() {
            SessionLiveness::Archived
        } else if self.status.as_deref() == Some("closed") {
            SessionLiveness::Closed
        } else {
            SessionLiveness::Live
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRegistryFile {
    id: String,
    cwd: String,
    #[serde(default)]
    archived_at: Option<String>,
    #[serde(default)]
    last_status: Option<String>,
    #[serde(default)]
    labels: Option<Value>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    pending_permissions: Option<Vec<Value>>,
}

/// Paseo's on-disk session registry (`~/.paseo/agents/<folder>/<id>.json`).
#[derive(Debug)]
pub struct PaseoRegistry {
    dir: PathBuf,
    found: Mutex<HashMap<String, PathBuf>>,
    expected_uid: Option<u32>,
}

impl PaseoRegistry {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), found: Mutex::new(HashMap::new()), expected_uid: None }
    }

    pub fn with_expected_uid(mut self, uid: u32) -> Self {
        self.expected_uid = Some(uid);
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Look up `id` in the Paseo registry (`Callers.find`).
    pub fn find(&self, id: &str) -> Option<PaseoAgent> {
        if !is_safe_id(id) {
            return None;
        }
        let known = self.found.lock().unwrap().get(id).cloned();
        if let Some(path) = known {
            if let Some(agent) = Self::read_file(&path, id) {
                return Some(agent);
            }
            self.found.lock().unwrap().remove(id);
        }
        let entries = std::fs::read_dir(&self.dir).ok()?;
        let mut folders: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        folders.sort();
        for folder in folders {
            let path = folder.join(format!("{id}.json"));
            if let Some(agent) = Self::read_file(&path, id) {
                self.found.lock().unwrap().insert(id.to_string(), path);
                return Some(agent);
            }
        }
        None
    }

    fn read_file(path: &Path, expected_id: &str) -> Option<PaseoAgent> {
        let text = std::fs::read_to_string(path).ok()?;
        let parsed: RawRegistryFile = serde_json::from_str(&text).ok()?;
        if parsed.id != expected_id {
            return None;
        }
        let parent = parsed
            .labels
            .as_ref()
            .and_then(|labels| labels.get(PARENT_AGENT_LABEL))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let pending_permissions = parsed
            .pending_permissions
            .unwrap_or_default()
            .into_iter()
            .filter_map(|raw| serde_json::from_value::<PermissionLike>(raw).ok())
            .map(|raw| {
                let (title, detail) = describe_permission(&raw, Some(&parsed.cwd));
                let request_id = permission_request_id(&raw, "perm");
                PermissionPrompt { request_id, title, detail }
            })
            .collect();
        Some(PaseoAgent {
            id: parsed.id,
            cwd: parsed.cwd,
            archived_at: parsed.archived_at,
            status: parsed.last_status,
            parent,
            provider: parsed.provider,
            pending_permissions,
        })
    }

    /// Verify a caller claim against Paseo's registry (`Callers.verify`).
    pub fn verify(&self, claim: &CallerClaim) -> Result<VerifiedSession, IdentityRefusal> {
        if claim.surface != Surface::Paseo {
            return Err(IdentityRefusal::new(
                RefusalReason::CrossSurface,
                format!("Surface `{}` cannot be verified by the Paseo registry.", claim.surface.as_str()),
            ));
        }
        check_peer(self.expected_uid, claim.peer)?;
        let Some(agent_id) = claim.session_id.as_deref().filter(|s| !s.is_empty()) else {
            return Err(IdentityRefusal::new(
                RefusalReason::MissingSessionId,
                "Maestro cannot tell which session is asking: PASEO_AGENT_ID is not set in this shell. Run maestro from a Paseo session.",
            ));
        };
        let Some(cwd) = claim.cwd.as_deref().filter(|s| !s.is_empty()) else {
            return Err(IdentityRefusal::new(
                RefusalReason::MissingCwd,
                "Maestro cannot tell where this shell is: the request carried no folder. Update the maestro command (bin/maestro.ts) to this plugin's version.",
            ));
        };
        if !Path::new(cwd).is_absolute() {
            return Err(IdentityRefusal::new(
                RefusalReason::RelativeCwd,
                format!("The shell's folder must be an absolute path, not {cwd}."),
            ));
        }
        let Some(agent) = self.find(agent_id) else {
            return Err(IdentityRefusal::new(
                RefusalReason::NotFound,
                format!("No Paseo session has the id {agent_id}."),
            ));
        };
        let liveness = agent.liveness();
        if !liveness.is_live() {
            let short = short_id(agent_id);
            return Err(IdentityRefusal::new(
                RefusalReason::NotLive,
                format!("Session {short} is {}, not live.", liveness.as_str()),
            ));
        }
        if !within(Path::new(cwd), Path::new(&agent.cwd)) {
            let short = short_id(agent_id);
            return Err(IdentityRefusal::new(
                RefusalReason::OutsideCwd,
                format!(
                    "This shell is in {cwd}, but session {short} runs in {}. Run maestro from inside that folder.",
                    agent.cwd
                ),
            ));
        }
        let harness = agent.provider.as_deref().map(|p| p.parse::<Harness>().unwrap());
        Ok(VerifiedSession::new(
            SessionRef { surface: Surface::Paseo, environment: None, id: agent.id },
            agent.cwd,
            harness,
            agent.parent,
            agent.pending_permissions,
        ))
    }

    /// Whether `child` is a live session `parent` directly started (`Callers.started`).
    pub fn started(&self, parent: &str, child: &str) -> bool {
        self.find(child).is_some_and(|agent| agent.parent.as_deref() == Some(parent) && agent.liveness().is_live())
    }
}

pub(crate) fn check_peer(expected_uid: Option<u32>, peer: Option<PeerCredentials>) -> Result<(), IdentityRefusal> {
    if let Some(cred) = peer {
        if cred.pid == 0 {
            return Err(IdentityRefusal::new(RefusalReason::PeerMismatch, "Peer credentials carried pid 0."));
        }
        if let Some(want_uid) = expected_uid
            && cred.uid != want_uid
        {
            return Err(IdentityRefusal::new(
                RefusalReason::PeerMismatch,
                format!("Peer uid {} does not match registry owner uid {want_uid}.", cred.uid),
            ));
        }
    } else if let Some(want_uid) = expected_uid {
        return Err(IdentityRefusal::new(
            RefusalReason::PeerMismatch,
            format!("Peer credentials (uid {want_uid}) are required on this socket."),
        ));
    }
    Ok(())
}

pub(crate) fn short_id(id: &str) -> &str {
    let end = id.char_indices().nth(8).map_or(id.len(), |(idx, _)| idx);
    &id[..end]
}

/// Resolve a path on disk (`realpath`), falling back to a lexically normalized path when absent.
pub fn resolve_real(path: &Path) -> PathBuf {
    let normalized = lexical_normalize(path);
    std::fs::canonicalize(&normalized).unwrap_or(normalized)
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Whether `inner` is `outer` or inside it by whole path segments (`server/callers.ts` `within`).
/// `/work/ab` is not inside `/work/a`.
pub fn within(inner: &Path, outer: &Path) -> bool {
    let inside = resolve_real(inner);
    let outside = resolve_real(outer);
    inside.starts_with(&outside)
}

fn strip_macos_private(path: &str) -> &str {
    if path.starts_with("/private/") {
        return &path["/private".len()..];
    }
    path
}

fn relative_within(base: &Path, target: &Path) -> Option<String> {
    let base_norm = lexical_normalize(base);
    let target_norm = lexical_normalize(target);
    let rel = target_norm.strip_prefix(&base_norm).ok()?;
    if rel.as_os_str().is_empty() {
        return None;
    }
    Some(rel.to_string_lossy().into_owned())
}

fn short_path(path: &str, cwd: Option<&str>) -> String {
    let Some(cwd) = cwd.filter(|c| !c.is_empty()) else {
        return path.to_string();
    };
    let p = Path::new(path);
    if !p.is_absolute() {
        return path.to_string();
    }
    if let Some(rel) = relative_within(Path::new(cwd), p) {
        return rel;
    }
    if let Some(rel) = relative_within(Path::new(strip_macos_private(cwd)), Path::new(strip_macos_private(path))) {
        return rel;
    }
    path.to_string()
}

fn tool_verb(name: &str) -> Option<&'static str> {
    match name.to_ascii_lowercase().as_str() {
        "write" => Some("Write a file"),
        "edit" | "multiedit" => Some("Edit a file"),
        "notebookedit" => Some("Edit a notebook"),
        "bash" | "shell" | "exec" => Some("Run a command"),
        "webfetch" => Some("Fetch a page"),
        "websearch" => Some("Search the web"),
        "read" => Some("Read a file"),
        _ => None,
    }
}

fn input_field(input: Option<&serde_json::Map<String, Value>>, keys: &[&str]) -> Option<String> {
    let map = input?;
    for key in keys {
        if let Some(val) = map.get(*key).and_then(Value::as_str) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Port of `describePermission` (`server/permissions.ts`).
pub fn describe_permission(req: &PermissionLike, cwd: Option<&str>) -> (String, Option<String>) {
    let title = req
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| tool_verb(&req.name).map(str::to_string))
        .unwrap_or_else(|| req.name.clone());
    if let Some(desc) = req.description.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return (title, Some(desc.to_string()));
    }
    if let Some(cmd) = input_field(req.input.as_ref(), &["command", "cmd"]) {
        let chars: Vec<char> = cmd.chars().collect();
        let detail = if chars.len() > 200 {
            let prefix: String = chars[..199].iter().collect();
            format!("{prefix}…")
        } else {
            cmd
        };
        return (title, Some(detail));
    }
    if let Some(path) = input_field(req.input.as_ref(), &["file_path", "path", "notebook_path"]) {
        return (title, Some(short_path(&path, cwd)));
    }
    let detail = input_field(req.input.as_ref(), &["url", "query"]);
    (title, detail)
}

/// Derive a stable request id for a permission prompt, using the source `id` when present
/// and otherwise hashing the prompt's full fields (`name`, `title`, `description`, `input`).
pub fn permission_request_id(raw: &PermissionLike, prefix: &str) -> String {
    if let Some(id) = raw.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return id.to_string();
    }
    let mut hasher = Sha256::new();
    hasher.update(raw.name.as_bytes());
    hasher.update([0]);
    if let Some(title) = &raw.title {
        hasher.update(title.as_bytes());
    }
    hasher.update([0]);
    if let Some(desc) = &raw.description {
        hasher.update(desc.as_bytes());
    }
    hasher.update([0]);
    if let Some(input) = &raw.input
        && let Ok(bytes) = serde_json::to_vec(input)
    {
        hasher.update(&bytes);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(16);
    for b in &digest[..8] {
        let _ = write!(&mut hex, "{b:02x}");
    }
    format!("{prefix}-{hex}")
}
