//! Read-only surface adapters for caller identity and the authenticated T3 effect bridge (`BRO-3268`).
//!
//! Resolves who is calling across [`Surface::Paseo`], [`Surface::T3`], and
//! [`Surface::Direct`]:
//! - [`paseo::PaseoRegistry`] ports `server/callers.ts` and `server/permissions.ts`
//!   from the Paseo plugin;
//! - [`t3::T3Registry`] verifies threads against T3's `statev2.sqlite` (or JSON snapshot)
//!   registry and discovers runtime transport/version metadata;
//! - [`DirectRegistry`] verifies bare processes against per-launch tokens issued at
//!   dispatch;
//! - [`SurfaceRegistry`] routes by surface, verifies node and short-lived tuple-bound
//!   [`BridgeCredential`] bindings with HMAC-SHA256, tracks persistent session generations
//!   to refuse ID reuse, refuses cross-surface spoofing, and projects live permission
//!   prompts into [`Need`]s.

pub mod paseo;
pub mod t3;

pub use maestro_core::session::{
    BridgeCredential, BridgeDiscovery, BridgeTransport, CallerClaim, Harness, IdentityRefusal,
    MAX_BRIDGE_CREDENTIAL_TTL_MS, NodeId, PeerCredentials, PermissionPrompt, RefusalReason, SessionError,
    SessionLiveness, SessionRef, SessionSnapshot, SessionUpdate, Surface, VerifiedSession, verified_identity,
};
pub use paseo::{
    PARENT_AGENT_LABEL, PaseoAgent, PaseoRegistry, PermissionLike, describe_permission, permission_request_id,
    resolve_real, within,
};
pub use t3::{T3Registry, T3Thread, is_safe_t3_id};

use maestro_core::need::Need;
use paseo::{check_peer, is_safe_id, short_id};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static SHARED_GENERATIONS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();

fn shared_generations() -> &'static Mutex<HashMap<String, u64>> {
    SHARED_GENERATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut key_block = [0u8; 64];
    if key.len() > 64 {
        let hashed = Sha256::digest(key);
        key_block[..32].copy_from_slice(&hashed);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

/// A registered [`Surface::Direct`] session and its per-launch token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectSession {
    pub id: String,
    pub cwd: String,
    #[serde(default, skip_serializing)]
    pub token: String,
    pub liveness: SessionLiveness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<Harness>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_permissions: Vec<PermissionPrompt>,
}

impl fmt::Debug for DirectSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirectSession")
            .field("id", &self.id)
            .field("cwd", &self.cwd)
            .field("token", &"<redacted>")
            .field("liveness", &self.liveness)
            .field("harness", &self.harness)
            .field("pending_permissions", &self.pending_permissions)
            .finish()
    }
}

/// Registry of per-launch tokens issued for [`Surface::Direct`] sessions.
#[derive(Debug, Default)]
pub struct DirectRegistry {
    sessions: Mutex<HashMap<String, DirectSession>>,
    file_path: Option<PathBuf>,
    expected_uid: Option<u32>,
}

impl DirectRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_file(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut sessions = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(rows) = serde_json::from_str::<Vec<DirectSession>>(&text)
        {
            for s in rows {
                sessions.insert(s.id.clone(), s);
            }
        }
        Self { sessions: Mutex::new(sessions), file_path: Some(path), expected_uid: None }
    }

    pub fn with_expected_uid(mut self, uid: u32) -> Self {
        self.expected_uid = Some(uid);
        self
    }

    /// Register or re-issue a Direct session; returns `true` when an existing session ID was replaced.
    pub fn register(&self, session: DirectSession) -> bool {
        let id = session.id.clone();
        let replaced = self.sessions.lock().unwrap().insert(id.clone(), session).is_some();
        if replaced {
            let key = format!("direct:{id}");
            let mut map = shared_generations().lock().unwrap();
            let next = map.get(&key).copied().unwrap_or(0) + 1;
            map.insert(key, next);
        }
        replaced
    }

    pub fn find(&self, id: &str) -> Option<DirectSession> {
        if !is_safe_id(id) {
            return None;
        }
        if let Some(found) = self.sessions.lock().unwrap().get(id).cloned() {
            return Some(found);
        }
        if let Some(path) = &self.file_path
            && let Ok(text) = std::fs::read_to_string(path)
            && let Ok(rows) = serde_json::from_str::<Vec<DirectSession>>(&text)
        {
            return rows.into_iter().find(|s| s.id == id);
        }
        None
    }

    pub fn list_live_sessions(&self) -> Vec<DirectSession> {
        let map = self.sessions.lock().unwrap();
        let mut out: Vec<DirectSession> = map.values().filter(|s| s.liveness.is_live()).cloned().collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub fn verify(&self, claim: &CallerClaim) -> Result<VerifiedSession, IdentityRefusal> {
        if claim.surface != Surface::Direct {
            return Err(IdentityRefusal::new(
                RefusalReason::CrossSurface,
                format!("Surface `{}` cannot be verified by the Direct registry.", claim.surface.as_str()),
            ));
        }
        check_peer(self.expected_uid, claim.peer)?;
        let Some(id) = claim.session_id.as_deref().filter(|s| !s.is_empty()) else {
            return Err(IdentityRefusal::new(
                RefusalReason::MissingSessionId,
                "Direct session claim carried no session id.",
            ));
        };
        let Some(cwd) = claim.cwd.as_deref().filter(|s| !s.is_empty()) else {
            return Err(IdentityRefusal::new(RefusalReason::MissingCwd, "Direct session claim carried no folder."));
        };
        if !Path::new(cwd).is_absolute() {
            return Err(IdentityRefusal::new(
                RefusalReason::RelativeCwd,
                format!("The shell's folder must be an absolute path, not {cwd}."),
            ));
        }
        let session = match (self.find(id), claim.token.as_deref().filter(|t| !t.is_empty())) {
            (Some(s), Some(t))
                if !s.token.is_empty() && Sha256::digest(t.as_bytes()) == Sha256::digest(s.token.as_bytes()) =>
            {
                s
            }
            _ => {
                return Err(IdentityRefusal::new(
                    RefusalReason::InvalidToken,
                    "Direct session id or launch token is invalid.",
                ));
            }
        };
        if !session.liveness.is_live() {
            let short = short_id(id);
            return Err(IdentityRefusal::new(
                RefusalReason::NotLive,
                format!("Session {short} is {}, not live.", session.liveness.as_str()),
            ));
        }
        if !within(Path::new(cwd), Path::new(&session.cwd)) {
            let short = short_id(id);
            return Err(IdentityRefusal::new(
                RefusalReason::OutsideCwd,
                format!(
                    "This shell is in {cwd}, but session {short} runs in {}. Run maestro from inside that folder.",
                    session.cwd
                ),
            ));
        }
        Ok(VerifiedSession::new(
            SessionRef { surface: Surface::Direct, environment: None, id: session.id },
            session.cwd,
            session.harness,
            None,
            session.pending_permissions,
        ))
    }
}

/// Combined read-only surface registry across Paseo, T3, and Direct.
#[derive(Debug, Default)]
pub struct SurfaceRegistry {
    pub paseo: Option<PaseoRegistry>,
    pub t3: Option<T3Registry>,
    pub direct: Option<DirectRegistry>,
    expected_node: Option<NodeId>,
    bridge_secret: Option<Vec<u8>>,
    generations_file: Option<PathBuf>,
    generations: Mutex<HashMap<String, u64>>,
}

impl SurfaceRegistry {
    pub fn new(paseo: Option<PaseoRegistry>, t3: Option<T3Registry>, direct: Option<DirectRegistry>) -> Self {
        Self { paseo, t3, direct, ..Self::default() }
    }

    /// Construct a `SurfaceRegistry` from environment variables (`MAESTRO_PASEO_AGENTS_DIR`,
    /// `MAESTRO_T3_HOME`, `MAESTRO_DIRECT_REGISTRY`, `MAESTRO_GENERATIONS_FILE`,
    /// `MAESTRO_NODE`, `MAESTRO_BRIDGE_SECRET`).
    pub fn from_env() -> Self {
        let paseo = std::env::var("MAESTRO_PASEO_AGENTS_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|p| PaseoRegistry::new(PathBuf::from(p)));
        let t3 =
            std::env::var("MAESTRO_T3_HOME").ok().filter(|s| !s.is_empty()).map(|p| T3Registry::new(PathBuf::from(p)));
        let direct =
            std::env::var("MAESTRO_DIRECT_REGISTRY").ok().filter(|s| !s.is_empty()).map(DirectRegistry::from_file);
        let mut reg = Self::new(paseo, t3, direct);
        if let Ok(gen_file) = std::env::var("MAESTRO_GENERATIONS_FILE")
            && !gen_file.is_empty()
        {
            reg = reg.with_generations_file(PathBuf::from(gen_file));
        }
        if let Ok(node_raw) = std::env::var("MAESTRO_NODE")
            && let Ok(node) = node_raw.trim().parse::<NodeId>()
        {
            reg = reg.with_expected_node(node);
        }
        if let Ok(secret) = std::env::var("MAESTRO_BRIDGE_SECRET")
            && !secret.is_empty()
        {
            reg = reg.with_bridge_secret(secret.into_bytes());
        }
        reg
    }

    pub fn with_expected_node(mut self, node: NodeId) -> Self {
        if let Some(t3) = self.t3.take() {
            self.t3 = Some(t3.with_expected_node(node.clone()));
        }
        self.expected_node = Some(node);
        self
    }

    pub fn with_bridge_secret(mut self, secret: impl Into<Vec<u8>>) -> Self {
        let bytes = secret.into();
        self.bridge_secret = (!bytes.is_empty()).then_some(bytes);
        self
    }

    pub fn with_generations_file(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(map) = serde_json::from_str::<HashMap<String, u64>>(&text)
        {
            *self.generations.lock().unwrap() = map;
        }
        self.generations_file = Some(path);
        self
    }

    pub fn has_surface(&self, surface: &Surface) -> bool {
        match surface {
            Surface::Paseo => self.paseo.is_some(),
            Surface::T3 => self.t3.is_some(),
            Surface::Direct => self.direct.is_some(),
            Surface::Other(_) => false,
        }
    }

    /// Register or re-issue a Direct session on this registry, bumping its generation if the ID was reused.
    pub fn register_direct(&self, session: DirectSession) -> u64 {
        let id = session.id.clone();
        let replaced = self.direct.as_ref().is_some_and(|direct| direct.register(session));
        if replaced { self.bump_generation(&Surface::Direct, &id) } else { self.generation_of(&Surface::Direct, &id) }
    }

    /// Current generation counter for `(surface, session_id)`.
    pub fn generation_of(&self, surface: &Surface, session_id: &str) -> u64 {
        let key = format!("{}:{session_id}", surface.as_str());
        if let Some(path) = &self.generations_file
            && let Ok(text) = std::fs::read_to_string(path)
            && let Ok(map) = serde_json::from_str::<HashMap<String, u64>>(&text)
            && let Some(val) = map.get(&key).copied()
        {
            return val;
        }
        let local = self.generations.lock().unwrap().get(&key).copied().unwrap_or(0);
        let shared = shared_generations().lock().unwrap().get(&key).copied().unwrap_or(0);
        local.max(shared)
    }

    /// Increment the generation counter when a session ID is re-issued or restarted,
    /// invalidating any earlier [`BridgeCredential`] for that ID.
    pub fn bump_generation(&self, surface: &Surface, session_id: &str) -> u64 {
        let key = format!("{}:{session_id}", surface.as_str());
        let next = self.generation_of(surface, session_id) + 1;
        self.generations.lock().unwrap().insert(key.clone(), next);
        shared_generations().lock().unwrap().insert(key, next);
        if let Some(path) = &self.generations_file {
            let snapshot = self.generations.lock().unwrap().clone();
            if let Ok(encoded) = serde_json::to_string(&snapshot) {
                let _ = std::fs::write(path, encoded);
            }
        }
        next
    }

    fn sign_tuple(
        secret: &[u8],
        session: &SessionRef,
        node: &NodeId,
        cwd: &str,
        issued_at_ms: i64,
        expires_at_ms: i64,
        generation: u64,
    ) -> String {
        let mut msg = Vec::new();
        msg.extend_from_slice(session.to_string().as_bytes());
        msg.push(0);
        msg.extend_from_slice(node.as_str().as_bytes());
        msg.push(0);
        msg.extend_from_slice(cwd.as_bytes());
        msg.push(0);
        msg.extend_from_slice(&issued_at_ms.to_be_bytes());
        msg.extend_from_slice(&expires_at_ms.to_be_bytes());
        msg.extend_from_slice(&generation.to_be_bytes());
        let mac = hmac_sha256(secret, &msg);
        let mut hex = String::with_capacity(64);
        for b in mac {
            let _ = write!(&mut hex, "{b:02x}");
        }
        hex
    }

    /// Issue a short-lived tuple-bound [`BridgeCredential`] for a verified session.
    pub fn issue_bridge_credential(
        &self,
        verified: &VerifiedSession,
        node: NodeId,
        issued_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<BridgeCredential, SessionError> {
        let Some(secret) = self.bridge_secret.as_deref() else {
            return Err(SessionError::Refused(IdentityRefusal::new(
                RefusalReason::InvalidToken,
                "Bridge secret is not configured on this host.",
            )));
        };
        let Some(ttl) = expires_at_ms.checked_sub(issued_at_ms) else {
            return Err(SessionError::StaleCredential {
                session_id: verified.agent_id().to_string(),
                reason: "timestamp subtraction overflowed".to_string(),
            });
        };
        if ttl <= 0 || ttl > MAX_BRIDGE_CREDENTIAL_TTL_MS {
            return Err(SessionError::StaleCredential {
                session_id: verified.agent_id().to_string(),
                reason: "credential lifetime must be > 0 and at most 1 hour".to_string(),
            });
        }
        if let Some(want_node) = &self.expected_node
            && want_node != &node
        {
            return Err(SessionError::WrongNode { expected: want_node.clone(), actual: node });
        }
        let generation = self.generation_of(&verified.session().surface, verified.agent_id());
        let signature = Self::sign_tuple(
            secret,
            verified.session(),
            &node,
            verified.cwd(),
            issued_at_ms,
            expires_at_ms,
            generation,
        );
        Ok(BridgeCredential {
            session: verified.session().clone(),
            node,
            cwd: verified.cwd().to_string(),
            issued_at_ms,
            expires_at_ms,
            generation,
            signature,
        })
    }

    /// Verify a caller claim, returning a typed [`SessionError`] on failure.
    pub fn verify_session(&self, claim: &CallerClaim, now_ms: Option<i64>) -> Result<VerifiedSession, SessionError> {
        if let (Some(want_node), Some(actual_node)) = (&self.expected_node, &claim.node)
            && want_node != actual_node
        {
            return Err(SessionError::WrongNode { expected: want_node.clone(), actual: actual_node.clone() });
        }
        let current_gen = claim.session_id.as_deref().map(|id| self.generation_of(&claim.surface, id)).unwrap_or(0);
        if let Some(presented_gen) = claim.generation
            && presented_gen != current_gen
        {
            return Err(SessionError::IdReused {
                session_id: claim.session_id.clone().unwrap_or_default(),
                expected_generation: current_gen,
                presented_generation: presented_gen,
            });
        }
        if let Some(cred) = &claim.credential {
            let Some(secret) = self.bridge_secret.as_deref() else {
                return Err(SessionError::Refused(IdentityRefusal::new(
                    RefusalReason::InvalidToken,
                    "Bridge secret is not configured on this host.",
                )));
            };
            if cred.session.surface != claim.surface {
                return Err(SessionError::Refused(IdentityRefusal::new(
                    RefusalReason::CrossSurface,
                    format!(
                        "Bridge credential for surface `{}` cannot authenticate `{}`.",
                        cred.session.surface.as_str(),
                        claim.surface.as_str()
                    ),
                )));
            }
            if claim.session_id.as_deref() != Some(cred.session.id.as_str()) {
                return Err(SessionError::Refused(IdentityRefusal::new(
                    RefusalReason::InvalidToken,
                    "Bridge credential session id does not match claim.",
                )));
            }
            if let Some(want_node) = &self.expected_node
                && &cred.node != want_node
            {
                return Err(SessionError::WrongNode { expected: want_node.clone(), actual: cred.node.clone() });
            }
            if let Some(claim_node) = &claim.node
                && &cred.node != claim_node
            {
                return Err(SessionError::WrongNode { expected: cred.node.clone(), actual: claim_node.clone() });
            }
            if cred.generation != current_gen {
                return Err(SessionError::IdReused {
                    session_id: cred.session.id.clone(),
                    expected_generation: current_gen,
                    presented_generation: cred.generation,
                });
            }
            let Some(ttl) = cred.expires_at_ms.checked_sub(cred.issued_at_ms) else {
                return Err(SessionError::StaleCredential {
                    session_id: cred.session.id.clone(),
                    reason: "timestamp overflow".to_string(),
                });
            };
            if ttl <= 0 || ttl > MAX_BRIDGE_CREDENTIAL_TTL_MS {
                return Err(SessionError::StaleCredential {
                    session_id: cred.session.id.clone(),
                    reason: "lifetime outside 1..=3600000 ms".to_string(),
                });
            }
            if let Some(now) = now_ms
                && (now < cred.issued_at_ms || now >= cred.expires_at_ms)
            {
                return Err(SessionError::StaleCredential {
                    session_id: cred.session.id.clone(),
                    reason: format!("now={now} outside [{}, {})", cred.issued_at_ms, cred.expires_at_ms),
                });
            }
            let expected_sig = Self::sign_tuple(
                secret,
                &cred.session,
                &cred.node,
                &cred.cwd,
                cred.issued_at_ms,
                cred.expires_at_ms,
                cred.generation,
            );
            if Sha256::digest(cred.signature.as_bytes()) != Sha256::digest(expected_sig.as_bytes()) {
                return Err(SessionError::Refused(IdentityRefusal::new(
                    RefusalReason::InvalidToken,
                    "Bridge credential signature is invalid.",
                )));
            }
        }

        let verified = self.verify_surface(claim)?;
        if let Some(cred) = &claim.credential
            && !within(Path::new(&cred.cwd), Path::new(verified.cwd()))
        {
            return Err(SessionError::Refused(IdentityRefusal::new(
                RefusalReason::OutsideCwd,
                "Bridge credential folder does not match registered session folder.",
            )));
        }
        let node = claim
            .credential
            .as_ref()
            .map(|c| c.node.clone())
            .or_else(|| claim.node.clone())
            .or_else(|| self.expected_node.clone());
        Ok(verified.with_node_and_generation(node, current_gen))
    }

    fn verify_surface(&self, claim: &CallerClaim) -> Result<VerifiedSession, SessionError> {
        match &claim.surface {
            Surface::Paseo => {
                if let Some(id) = claim.session_id.as_deref().filter(|s| !s.is_empty()) {
                    let in_t3 = id.starts_with("mcp:") || self.t3.as_ref().is_some_and(|t3| t3.find(id).is_some());
                    let in_paseo = self.paseo.as_ref().is_some_and(|p| p.find(id).is_some());
                    if in_t3 && !in_paseo {
                        return Err(IdentityRefusal::new(
                            RefusalReason::CrossSurface,
                            format!("No Paseo session has the id {id}: {id} is a T3 thread, not a Paseo session."),
                        )
                        .into());
                    }
                }
                let Some(paseo) = &self.paseo else {
                    return Err(SessionError::RegistryDisconnected {
                        surface: Surface::Paseo,
                        detail: "Paseo registry is not configured on this host".to_string(),
                    });
                };
                paseo.verify(claim).map_err(Into::into)
            }
            Surface::T3 => {
                if let Some(id) = claim.session_id.as_deref().filter(|s| !s.is_empty()) {
                    let in_paseo = self.paseo.as_ref().is_some_and(|p| p.find(id).is_some());
                    let in_t3 = self.t3.as_ref().is_some_and(|t3| t3.find(id).is_some());
                    if in_paseo && !in_t3 {
                        return Err(IdentityRefusal::new(
                            RefusalReason::CrossSurface,
                            format!("No T3 thread has the id {id}: {id} is a Paseo session, not a T3 thread."),
                        )
                        .into());
                    }
                }
                let Some(t3) = &self.t3 else {
                    return Err(SessionError::RegistryDisconnected {
                        surface: Surface::T3,
                        detail: "T3 registry is not configured on this host".to_string(),
                    });
                };
                t3.verify(claim).map_err(Into::into)
            }
            Surface::Direct => {
                let Some(direct) = &self.direct else {
                    return Err(SessionError::RegistryDisconnected {
                        surface: Surface::Direct,
                        detail: "Direct registry is not configured on this host".to_string(),
                    });
                };
                direct.verify(claim).map_err(Into::into)
            }
            Surface::Other(name) => {
                Err(IdentityRefusal::new(RefusalReason::UnsupportedSurface, format!("Unsupported surface `{name}`."))
                    .into())
            }
        }
    }

    /// Verify a caller claim against its surface registry, refusing cross-surface spoofing,
    /// wrong node, stale credential, ID reuse, or disconnected registries.
    pub fn verify_caller(&self, claim: &CallerClaim) -> Result<VerifiedSession, IdentityRefusal> {
        self.verify_session(claim, None).map_err(SessionError::into_refusal)
    }

    /// Verify a caller claim at `now_ms`, checking bridge credential freshness when supplied.
    pub fn verify_caller_at(&self, claim: &CallerClaim, now_ms: i64) -> Result<VerifiedSession, IdentityRefusal> {
        self.verify_session(claim, Some(now_ms)).map_err(SessionError::into_refusal)
    }

    /// Capture a [`SessionSnapshot`] for a verified caller claim.
    pub fn snapshot_session(&self, claim: &CallerClaim, now_ms: Option<i64>) -> Result<SessionSnapshot, SessionError> {
        let verified = self.verify_session(claim, now_ms)?;
        let discovery = match &claim.surface {
            Surface::T3 => self.t3.as_ref().map(T3Registry::discover),
            _ => None,
        };
        Ok(verified.snapshot(discovery))
    }

    /// Compute [`SessionUpdate`] events between two snapshots of a session.
    pub fn diff_snapshots(before: Option<&SessionSnapshot>, after: Option<&SessionSnapshot>) -> Vec<SessionUpdate> {
        let mut updates = Vec::new();
        match (before, after) {
            (None, Some(curr)) => {
                updates.push(SessionUpdate::Snapshot { snapshot: Box::new(curr.clone()) });
            }
            (Some(prev), Some(curr)) => {
                if prev.liveness != curr.liveness {
                    updates.push(SessionUpdate::LivenessChanged {
                        session: curr.session.clone(),
                        liveness: curr.liveness,
                    });
                }
                if prev.pending_permissions != curr.pending_permissions {
                    updates.push(SessionUpdate::PermissionsChanged {
                        session: curr.session.clone(),
                        pending_permissions: curr.pending_permissions.clone(),
                    });
                }
                if let (Some(d1), Some(d2)) = (&prev.discovery, &curr.discovery)
                    && (d1.connected != d2.connected || d1.version != d2.version)
                {
                    updates.push(SessionUpdate::BridgeStateChanged {
                        surface: curr.session.surface.clone(),
                        connected: d2.connected,
                        version: Some(d2.version.clone()),
                    });
                }
            }
            (Some(prev), None) => {
                updates.push(SessionUpdate::Ended { session: prev.session.clone() });
            }
            (None, None) => {}
        }
        updates
    }

    /// Convert all live permission prompts on a verified session into [`Need`] items.
    pub fn needs_for(&self, session: &VerifiedSession, project: Option<&str>, since: Option<&str>) -> Vec<Need> {
        session.needs(project, since)
    }

    /// Enumerate all live permission prompts across configured Paseo, T3, and Direct registries
    /// and convert them into [`Need`] items.
    pub fn live_permission_needs(&self, project: Option<&str>, since: Option<&str>) -> Vec<Need> {
        let mut out = Vec::new();
        if let Some(paseo) = &self.paseo {
            for agent in paseo.list_live_agents() {
                let sref = SessionRef { surface: Surface::Paseo, environment: None, id: agent.id };
                for prompt in &agent.pending_permissions {
                    out.push(prompt.to_need(&sref, project.map(str::to_string), since.map(str::to_string)));
                }
            }
        }
        if let Some(t3) = &self.t3 {
            for thread in t3.list_live_threads() {
                let sref = SessionRef { surface: Surface::T3, environment: thread.environment_id, id: thread.id };
                for prompt in &thread.pending_permissions {
                    out.push(prompt.to_need(&sref, project.map(str::to_string), since.map(str::to_string)));
                }
            }
        }
        if let Some(direct) = &self.direct {
            for session in direct.list_live_sessions() {
                let sref = SessionRef { surface: Surface::Direct, environment: None, id: session.id };
                for prompt in &session.pending_permissions {
                    out.push(prompt.to_need(&sref, project.map(str::to_string), since.map(str::to_string)));
                }
            }
        }
        out
    }
}
