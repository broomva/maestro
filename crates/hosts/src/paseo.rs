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

use crate::read::{self, Read};

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

/// The one rule for a pending permission record (BRO-3863), for every surface: Paseo agent files,
/// T3 snapshot rows and T3 runtime requests alike. It reads keys by presence and type, never by
/// truthiness. The tool name is `toolName` when that key is present, else `name` when that key is
/// present; the first present key governs, and it must hold a non-blank string, else the record
/// is unread (`""`, `"   "`, `0`, `false`, `null`, `[]` all are). A record with neither key is a
/// prompt named by its `kind` (a T3 runtime request's); one with neither key nor kind is unread.
/// `id`, `title`, `description` and `input` are optional and typed: a wrong type is ignored.
pub(crate) fn permission_record(
    record: &serde_json::Map<String, Value>,
    kind: Option<&str>,
) -> Result<PermissionLike, String> {
    let name = if let Some(v) = record.get("toolName") {
        tool_name(v, "toolName")?
    } else if let Some(v) = record.get("name") {
        tool_name(v, "name")?
    } else {
        match kind.map(str::trim).filter(|k| !k.is_empty()) {
            Some(k) => k.to_string(),
            None => return Err("names no tool".to_string()),
        }
    };
    let text = |key: &str| record.get(key).and_then(Value::as_str).map(str::to_string);
    Ok(PermissionLike {
        id: text("id"),
        name,
        title: text("title"),
        description: text("description"),
        input: record.get("input").and_then(Value::as_object).cloned(),
    })
}

fn tool_name(value: &Value, key: &str) -> Result<String, String> {
    match value.as_str().filter(|s| !s.trim().is_empty()) {
        Some(name) => Ok(name.to_string()),
        None => Err(format!("has a tool name that is not a non-blank string (`{key}`)")),
    }
}

/// A session's pending permission records, each a prompt or an unread record with why, never
/// dropped (BRO-3863): a record that is not an object is unread, any other reads by
/// [`permission_record`]. Records carry no kind, so one naming no tool is unread. Identity is
/// unaffected: a verified session carries the prompts that decoded, and its lookup's outcome does
/// not depend on them; only the owner's permission listing reports the unread ones.
pub(crate) fn decode_permissions(raw: Vec<Value>, cwd: &str, prefix: &str) -> (Vec<PermissionPrompt>, Vec<String>) {
    let mut prompts = Vec::new();
    let mut unread = Vec::new();
    for (i, value) in raw.iter().enumerate() {
        let Some(record) = value.as_object() else {
            unread.push(format!("permission record {i} is not an object"));
            continue;
        };
        match permission_record(record, None) {
            Ok(p) => prompts.push(prompt_of(&p, cwd, prefix)),
            Err(why) => unread.push(format!("permission record {i} {why}")),
        }
    }
    (prompts, unread)
}

/// One pending T3 runtime request, raw, as the SQLite reader emits it: the reader does not
/// interpret its payload (BRO-3863 round 11).
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawRuntimeRequest {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub payload_json: Option<String>,
}

/// A T3 thread's pending runtime requests, by the same rule: a payload that is NULL, blank, not
/// JSON or not an object is unread; any other reads by [`permission_record`] with the request's
/// kind, so a payload naming no tool (`{}`) is a prompt named by its kind. The request's own id
/// is the prompt's when it is non-blank, else the payload's `id`.
pub(crate) fn decode_runtime_requests(
    raw: Vec<RawRuntimeRequest>,
    thread: &str,
    cwd: &str,
    prefix: &str,
) -> (Vec<PermissionPrompt>, Vec<String>) {
    let mut prompts = Vec::new();
    let mut unread = Vec::new();
    for req in raw {
        let what = format!("permission request {} of thread {thread}", req.id.as_deref().unwrap_or("-"));
        let payload = match req.payload_json.as_deref().filter(|s| !s.trim().is_empty()) {
            None => Err("is empty"),
            Some(text) => match serde_json::from_str::<Value>(text) {
                Err(_) => Err("does not parse"),
                Ok(Value::Object(record)) => Ok(record),
                Ok(_) => Err("is not an object"),
            },
        };
        let decoded =
            payload.map_err(str::to_string).and_then(|record| permission_record(&record, req.kind.as_deref()));
        match decoded {
            Ok(mut p) => {
                if let Some(id) = req.id.as_deref().filter(|s| !s.trim().is_empty()) {
                    p.id = Some(id.to_string());
                }
                prompts.push(prompt_of(&p, cwd, prefix));
            }
            Err(why) => unread.push(format!("{what} {why}")),
        }
    }
    (prompts, unread)
}

fn prompt_of(p: &PermissionLike, cwd: &str, prefix: &str) -> PermissionPrompt {
    let (title, detail) = describe_permission(p, Some(cwd));
    let request_id = permission_request_id(p, prefix);
    PermissionPrompt { request_id, title, detail }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_permissions: Vec<PermissionPrompt>,
    /// Pending permission records that did not decode, each with why (BRO-3863): never dropped,
    /// so the owner's permission listing reports them unread.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unread_permissions: Vec<String>,
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

    pub fn incarnation(&self) -> Option<String> {
        let started = self.started_at.as_deref().filter(|s| !s.is_empty());
        let handle = self.handle_id.as_deref().filter(|s| !s.is_empty());
        if started.is_none() && handle.is_none() {
            return None;
        }
        Some(format!(
            "paseo:{}:{}:{}:{}:{}:{}",
            self.cwd,
            self.provider.as_deref().unwrap_or("-"),
            self.parent.as_deref().unwrap_or("-"),
            started.unwrap_or("-"),
            self.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".to_string()),
            handle.unwrap_or("-"),
        ))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPersistence {
    #[serde(default, alias = "session_id")]
    session_id: Option<String>,
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
    #[serde(default, alias = "started_at")]
    started_at: Option<String>,
    #[serde(default, alias = "created_at")]
    created_at: Option<String>,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    persistence: Option<RawPersistence>,
    #[serde(default)]
    pending_permissions: Option<Vec<Value>>,
}

/// Paseo's on-disk session registry (`~/.paseo/agents/<folder>/<id>.json`).
#[derive(Debug)]
pub struct PaseoRegistry {
    dir: PathBuf,
    found: Mutex<HashMap<String, PathBuf>>,
    expected_uid: Option<u32>,
    disconnected: bool,
}

impl PaseoRegistry {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), found: Mutex::new(HashMap::new()), expected_uid: None, disconnected: false }
    }

    pub fn with_expected_uid(mut self, uid: u32) -> Self {
        self.expected_uid = Some(uid);
        self
    }

    pub fn with_disconnected(mut self, disconnected: bool) -> Self {
        self.disconnected = disconnected;
        self
    }

    pub fn is_connected(&self) -> bool {
        !self.disconnected && matches!(read::is_dir(&self.dir), Read::Present(true))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Look up `id` in the Paseo registry (`Callers.find`).
    pub fn find(&self, id: &str) -> Option<PaseoAgent> {
        self.lookup(id).ok().flatten()
    }

    /// Agent `id`, or why the registry did not read (BRO-3846), by the [`read`](crate::read) rule:
    /// an agents folder, an entry of it, or the agent's own file that does not read (or does not
    /// parse, or names another agent) fails identity closed, never reads as "no such agent".
    fn lookup(&self, id: &str) -> Result<Option<PaseoAgent>, IdentityRefusal> {
        let unread =
            |why: String| IdentityRefusal::new(RefusalReason::RegistryDisconnected, format!("Paseo registry: {why}."));
        if self.disconnected || !is_safe_id(id) {
            return Ok(None);
        }
        let known = self.found.lock().ok().and_then(|f| f.get(id).cloned());
        if let Some(path) = known {
            if let Read::Present(Ok(agent)) = Self::agent_at(&path, id) {
                return Ok(Some(agent));
            }
            if let Ok(mut f) = self.found.lock() {
                f.remove(id);
            }
        }
        let folders = match read::dir_entries(&self.dir) {
            Read::Present(folders) => folders,
            Read::Absent => return Ok(None),
            Read::Unreadable(why) => return Err(unread(why)),
        };
        for folder in folders {
            let folder = match folder {
                Read::Present(folder) => folder,
                Read::Absent => continue,
                Read::Unreadable(why) => return Err(unread(why)),
            };
            let path = folder.join(format!("{id}.json"));
            match Self::agent_at(&path, id) {
                Read::Present(Ok(agent)) => {
                    if let Ok(mut f) = self.found.lock() {
                        f.insert(id.to_string(), path);
                    }
                    return Ok(Some(agent));
                }
                Read::Present(Err(_)) | Read::Absent => {}
                Read::Unreadable(why) => return Err(unread(why)),
            }
        }
        Ok(None)
    }

    /// Enumerate all live Paseo agents in the registry.
    pub fn list_live_agents(&self) -> Vec<PaseoAgent> {
        self.list_live_report().0
    }

    /// The live agents, and what could not be read (BRO-3846): a disconnected registry, an agents
    /// folder or agent folder that does not read, or an agent file that does not read, does not
    /// parse, or names another agent than its file. The listing feeds the owner's permission
    /// prompts, so none of these may look like an empty, complete list. Files that are no agent
    /// record (not `*.json`, or not an agent id) are not read failures.
    pub fn list_live_report(&self) -> (Vec<PaseoAgent>, Vec<(String, String)>) {
        self.list_with(&read::dir_entries)
    }

    /// [`Self::list_live_report`] over `list_dir` (tests inject an entry that does not read).
    fn list_with(
        &self,
        list_dir: &dyn Fn(&Path) -> Read<Vec<Read<PathBuf>>>,
    ) -> (Vec<PaseoAgent>, Vec<(String, String)>) {
        let mut unread = Vec::new();
        let at = |p: &Path| format!("paseo registry {}", p.display());
        if self.disconnected {
            unread.push((at(&self.dir), "the Paseo registry is disconnected".to_string()));
            return (Vec::new(), unread);
        }
        let folders = match list_dir(&self.dir) {
            Read::Present(folders) => folders,
            Read::Absent => {
                unread.push((at(&self.dir), "the agents folder does not read: it is not there".to_string()));
                return (Vec::new(), unread);
            }
            Read::Unreadable(why) => {
                unread.push((at(&self.dir), format!("the agents folder does not read: {why}")));
                return (Vec::new(), unread);
            }
        };
        let mut out = Vec::new();
        for folder in folders {
            let folder = match folder {
                Read::Present(folder) => folder,
                Read::Absent => continue,
                Read::Unreadable(why) => {
                    unread.push((at(&self.dir), why));
                    continue;
                }
            };
            // An entry that is no folder holds no agents; one whose kind cannot be told is unread.
            match read::is_dir(&folder) {
                Read::Present(true) => {}
                Read::Present(false) | Read::Absent => continue,
                Read::Unreadable(why) => {
                    unread.push((at(&folder), why));
                    continue;
                }
            }
            let paths = match list_dir(&folder) {
                Read::Present(paths) => paths,
                Read::Absent => continue,
                Read::Unreadable(why) => {
                    unread.push((at(&folder), format!("the agent folder does not read: {why}")));
                    continue;
                }
            };
            for path in paths {
                let path = match path {
                    Read::Present(path) => path,
                    Read::Absent => continue,
                    Read::Unreadable(why) => {
                        unread.push((at(&folder), why));
                        continue;
                    }
                };
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if !is_safe_id(stem) {
                    continue;
                }
                match Self::agent_at(&path, stem) {
                    Read::Present(Ok(agent)) if agent.liveness().is_live() => {
                        for why in &agent.unread_permissions {
                            unread.push((at(&path), why.clone()));
                        }
                        out.push(agent);
                    }
                    Read::Present(Ok(_)) | Read::Absent => {}
                    Read::Present(Err(why)) | Read::Unreadable(why) => unread.push((at(&path), why)),
                }
            }
        }
        (out, unread)
    }

    /// The agent file at `path` by the [`read`](crate::read) rule: absent when it is not there;
    /// unreadable, with why, when it does not read. A file that reads but does not parse, or names
    /// another agent, is `Present(Err(why))`: identity takes it as no such agent (the plugin's own
    /// rule, `callers-golden.json`), the listing as an unread entry.
    fn agent_at(path: &Path, expected_id: &str) -> Read<Result<PaseoAgent, String>> {
        match read::read_text(path) {
            Read::Absent => Read::Absent,
            Read::Unreadable(why) => Read::Unreadable(format!("the agent file does not read: {why}")),
            Read::Present(text) => Read::Present(Self::parse_agent(&text, expected_id)),
        }
    }

    fn parse_agent(text: &str, expected_id: &str) -> Result<PaseoAgent, String> {
        let parsed: RawRegistryFile =
            serde_json::from_str(text).map_err(|e| format!("the agent file does not parse: {e}"))?;
        if parsed.id != expected_id {
            return Err(format!("the agent file does not name agent {expected_id}"));
        }
        let parent = parsed
            .labels
            .as_ref()
            .and_then(|labels| labels.get(PARENT_AGENT_LABEL))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let (pending_permissions, unread_permissions) =
            decode_permissions(parsed.pending_permissions.unwrap_or_default(), &parsed.cwd, "perm");
        Ok(PaseoAgent {
            id: parsed.id,
            cwd: parsed.cwd,
            archived_at: parsed.archived_at,
            status: parsed.last_status,
            parent,
            provider: parsed.provider,
            started_at: parsed.started_at.or(parsed.created_at),
            pid: parsed.pid,
            handle_id: parsed.persistence.and_then(|p| p.session_id),
            pending_permissions,
            unread_permissions,
        })
    }

    /// Verify a caller claim against Paseo's registry (`Callers.verify`).
    pub fn verify(&self, claim: &CallerClaim) -> Result<VerifiedSession, IdentityRefusal> {
        self.verify_with_reader(claim, &crate::attribution::SystemProcesses)
    }

    /// Verify a caller claim against Paseo's registry, attesting `worker_pgid` from `agent.pid`
    /// through `reader` when the agent's live process group is readable.
    pub fn verify_with_reader(
        &self,
        claim: &CallerClaim,
        reader: &dyn crate::attribution::ProcessReader,
    ) -> Result<VerifiedSession, IdentityRefusal> {
        if claim.surface != Surface::Paseo {
            return Err(IdentityRefusal::new(
                RefusalReason::CrossSurface,
                format!("Surface `{}` cannot be verified by the Paseo registry.", claim.surface.as_str()),
            ));
        }
        if self.disconnected {
            return Err(IdentityRefusal::new(RefusalReason::RegistryDisconnected, "Paseo registry is disconnected."));
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
        let Some(agent) = self.lookup(agent_id)? else {
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
        let Some(incarnation) = agent.incarnation() else {
            return Err(IdentityRefusal::new(
                RefusalReason::InvalidToken,
                format!("Session {} lacks an authoritative incarnation marker.", agent.id),
            ));
        };
        let harness = agent.provider.as_deref().map(|p| p.parse::<Harness>().unwrap());
        let owner_marker = agent
            .handle_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|handle_id| format!("paseo-owner:{}:{handle_id}", agent.id));
        let mut verified = VerifiedSession::new(
            SessionRef { surface: Surface::Paseo, environment: None, id: agent.id },
            agent.cwd,
            harness,
            agent.parent,
            agent.pending_permissions,
        )
        .with_incarnation(incarnation);
        if let Some(marker) = owner_marker {
            verified = verified.with_owner_marker(marker);
        }
        if let Some(pgid) = agent.pid.and_then(|pid| crate::attribution::attest_worker_pgid(reader, pid)) {
            verified = verified.with_worker_pgid(pgid);
        }
        if let Some(cg) = agent.pid.and_then(|pid| crate::attribution::attest_worker_cgroup(reader, pid)) {
            verified = verified.with_worker_cgroup(cg);
        }
        Ok(verified)
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

/// Resolve a path on disk (`realpath`) BEFORE any lexical fallback.
///
/// If `path` exists on disk, `std::fs::canonicalize` resolves symlinks and `..` in true
/// filesystem order (so `allowed/link/..` where `link -> outside/deep` resolves to `outside`).
/// If `path` has a non-existent suffix, every component after the longest existing ancestor
/// must be a normal component (`Component::Normal`): any `..` after a missing segment or
/// broken symlink returns `None` (fail closed).
pub fn resolve_real(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    if let Ok(canon) = std::fs::canonicalize(path) {
        return Some(canon);
    }
    let mut cur = path;
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    loop {
        let file_name = cur.file_name()?;
        if std::fs::symlink_metadata(cur).is_ok() {
            return None;
        }
        let comps: Vec<Component<'_>> = Path::new(file_name).components().collect();
        if comps.len() != 1 || !matches!(comps[0], Component::Normal(_)) {
            return None;
        }
        tail.push(file_name);
        cur = cur.parent()?;
        if let Ok(mut base) = std::fs::canonicalize(cur) {
            for part in tail.into_iter().rev() {
                base.push(part);
            }
            return Some(base);
        }
    }
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
/// `/work/ab` is not inside `/work/a`, and `allowed/link/..` where `link -> outside/deep` is
/// resolved through the filesystem (`outside`) and refused.
pub fn within(inner: &Path, outer: &Path) -> bool {
    let (Some(inside), Some(outside)) = (resolve_real(inner), resolve_real(outer)) else {
        return false;
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    /// BRO-3863 round 11: a runtime request's own id is its prompt's when non-blank; a blank one
    /// gives way to the payload's `id`, and with neither the prompt's id is derived.
    #[test]
    fn a_runtime_request_is_known_by_its_own_id_else_its_payloads() {
        let req = |id: Option<&str>, payload: &str| RawRuntimeRequest {
            id: id.map(str::to_string),
            kind: Some("approval".to_string()),
            payload_json: Some(payload.to_string()),
        };
        let raw = vec![
            req(Some("req-1"), r#"{"id": "p-1", "toolName": "Deploy"}"#),
            req(Some("  "), r#"{"id": "p-2", "toolName": "Deploy"}"#),
            req(None, r#"{"toolName": "Deploy"}"#),
        ];
        let (prompts, unread) = decode_runtime_requests(raw, "mcp:a", "/w", "t3-perm");
        assert!(unread.is_empty(), "{unread:?}");
        let ids: Vec<&str> = prompts.iter().map(|p| p.request_id.as_str()).collect();
        assert_eq!(ids[..2], ["req-1", "p-2"]);
        assert!(ids[2].starts_with("t3-perm-"), "{ids:?}");
    }

    /// Codex's joint round 7: a directory entry whose read fails is an unread item of the listing,
    /// never an agent silently left out. A real `read_dir` cannot be made to fail one entry, so the
    /// lister is injected; the agent beside it is still listed.
    #[test]
    fn a_directory_entry_that_does_not_read_is_unread() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let agent = serde_json::json!({ "id": "agent-a", "cwd": "/w", "lastStatus": "running" });
        std::fs::write(work.join("agent-a.json"), agent.to_string()).unwrap();
        let registry = PaseoRegistry::new(tmp.path());
        let failing = |dir: &Path| match read::dir_entries(dir) {
            Read::Present(mut entries) => {
                entries.push(Read::Unreadable(format!("an entry of {} does not read: injected", dir.display())));
                Read::Present(entries)
            }
            other => other,
        };
        let (agents, unread) = registry.list_with(&failing);
        assert_eq!(agents.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["agent-a"]);
        // One at the agents folder, one at the agent folder: each entry that did not read.
        assert_eq!(unread.len(), 2, "{unread:?}");
        assert!(unread.iter().all(|(_, why)| why.contains("injected")), "{unread:?}");
        let (_, clean) = registry.list_live_report();
        assert!(clean.is_empty(), "{clean:?}");
    }
}
