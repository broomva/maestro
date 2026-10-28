//! Read-only surface adapters for caller identity (`BRO-3268`).
//!
//! Resolves who is calling across [`Surface::Paseo`], [`Surface::T3`], and
//! [`Surface::Direct`]:
//! - [`paseo::PaseoRegistry`] ports `server/callers.ts` and `server/permissions.ts`
//!   from the Paseo plugin;
//! - [`t3::T3Registry`] verifies threads against T3's `statev2.sqlite` (or JSON snapshot)
//!   registry;
//! - [`DirectRegistry`] verifies bare processes against per-launch tokens issued at
//!   dispatch;
//! - [`SurfaceRegistry`] routes by surface and refuses cross-surface spoofing (such as a
//!   T3 thread id presented as a Paseo agent id, or vice versa).

pub mod paseo;
pub mod t3;

pub use maestro_core::session::{
    CallerClaim, Harness, IdentityRefusal, PeerCredentials, PermissionPrompt, RefusalReason, SessionLiveness,
    SessionRef, Surface, VerifiedSession,
};
pub use paseo::{PARENT_AGENT_LABEL, PaseoAgent, PaseoRegistry, PermissionLike, describe_permission, within};
pub use t3::{T3Registry, T3Thread};

use maestro_core::need::Need;
use paseo::{check_peer, is_safe_id, short_id};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// A registered [`Surface::Direct`] session and its per-launch token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectSession {
    pub id: String,
    pub cwd: String,
    pub token: String,
    pub liveness: SessionLiveness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<Harness>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_permissions: Vec<PermissionPrompt>,
}

/// Registry of per-launch tokens issued for [`Surface::Direct`] sessions.
#[derive(Debug, Default)]
pub struct DirectRegistry {
    sessions: Mutex<HashMap<String, DirectSession>>,
    expected_uid: Option<u32>,
}

impl DirectRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_expected_uid(mut self, uid: u32) -> Self {
        self.expected_uid = Some(uid);
        self
    }

    pub fn register(&self, session: DirectSession) {
        self.sessions.lock().unwrap().insert(session.id.clone(), session);
    }

    pub fn find(&self, id: &str) -> Option<DirectSession> {
        if !is_safe_id(id) {
            return None;
        }
        self.sessions.lock().unwrap().get(id).cloned()
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
        let Some(session) = self.find(id) else {
            return Err(IdentityRefusal::new(RefusalReason::NotFound, format!("No Direct session has the id {id}.")));
        };
        if !session.liveness.is_live() {
            let short = short_id(id);
            return Err(IdentityRefusal::new(
                RefusalReason::NotLive,
                format!("Session {short} is {}, not live.", session.liveness.as_str()),
            ));
        }
        let Some(token) = claim.token.as_deref().filter(|t| !t.is_empty()) else {
            return Err(IdentityRefusal::new(
                RefusalReason::InvalidToken,
                "Direct session requires its per-launch token.",
            ));
        };
        if token != session.token {
            return Err(IdentityRefusal::new(
                RefusalReason::InvalidToken,
                "Direct session launch token does not match.",
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
}

impl SurfaceRegistry {
    pub fn new(paseo: Option<PaseoRegistry>, t3: Option<T3Registry>, direct: Option<DirectRegistry>) -> Self {
        Self { paseo, t3, direct }
    }

    /// Verify a caller claim against its surface registry, refusing cross-surface spoofing.
    pub fn verify_caller(&self, claim: &CallerClaim) -> Result<VerifiedSession, IdentityRefusal> {
        match &claim.surface {
            Surface::Paseo => {
                if let Some(id) = claim.session_id.as_deref().filter(|s| !s.is_empty()) {
                    let in_t3 = id.starts_with("mcp:") || self.t3.as_ref().is_some_and(|t3| t3.find(id).is_some());
                    let in_paseo = self.paseo.as_ref().is_some_and(|p| p.find(id).is_some());
                    if in_t3 && !in_paseo {
                        return Err(IdentityRefusal::new(
                            RefusalReason::CrossSurface,
                            format!("No Paseo session has the id {id}: {id} is a T3 thread, not a Paseo session."),
                        ));
                    }
                }
                let Some(paseo) = &self.paseo else {
                    return Err(IdentityRefusal::new(
                        RefusalReason::NotFound,
                        "Paseo registry is not configured on this host.",
                    ));
                };
                paseo.verify(claim)
            }
            Surface::T3 => {
                if let Some(id) = claim.session_id.as_deref().filter(|s| !s.is_empty()) {
                    let in_paseo = self.paseo.as_ref().is_some_and(|p| p.find(id).is_some());
                    let in_t3 = self.t3.as_ref().is_some_and(|t3| t3.find(id).is_some());
                    if in_paseo && !in_t3 {
                        return Err(IdentityRefusal::new(
                            RefusalReason::CrossSurface,
                            format!("No T3 thread has the id {id}: {id} is a Paseo session, not a T3 thread."),
                        ));
                    }
                }
                let Some(t3) = &self.t3 else {
                    return Err(IdentityRefusal::new(
                        RefusalReason::NotFound,
                        "T3 registry is not configured on this host.",
                    ));
                };
                t3.verify(claim)
            }
            Surface::Direct => {
                let Some(direct) = &self.direct else {
                    return Err(IdentityRefusal::new(
                        RefusalReason::NotFound,
                        "Direct registry is not configured on this host.",
                    ));
                };
                direct.verify(claim)
            }
            Surface::Other(name) => {
                Err(IdentityRefusal::new(RefusalReason::UnsupportedSurface, format!("Unsupported surface `{name}`.")))
            }
        }
    }

    /// Convert all live permission prompts on a verified session into [`Need`] items.
    pub fn needs_for(&self, session: &VerifiedSession, project: Option<&str>, since: Option<&str>) -> Vec<Need> {
        session.needs(project, since)
    }
}
