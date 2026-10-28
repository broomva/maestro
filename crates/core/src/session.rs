//! Who drives a session, what runs it, and where.
//!
//! Three names, as the fleet ADR (broomva/workspace
//! `docs/adrs/2026-10-19-fleet-runtime-boundaries.html`) fixes them:
//!
//! - a [`Surface`] drives a session: Paseo, T3, or [`Surface::Direct`] for a bare
//!   harness process (`claude -p`, `codex exec`, wave, persist, launchd);
//! - a [`Harness`] executes tools and loads hooks: Claude Code, Codex, Gemini, agy;
//! - a [`NodeId`] names the tailnet machine.
//!
//! Every session Maestro names is namespaced by its surface, so a T3 thread id never
//! stands in for a Paseo agent id. Capabilities split the same way: what a surface can
//! do to a session, and what a harness can do inside one.

use serde::{Deserialize, Serialize};
use std::fmt;

/// What drives a session: starts it, sends into it, stops it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Surface {
    Paseo,
    T3,
    /// A bare harness process with no orchestrator around it: `claude -p`, `codex exec`,
    /// wave, persist, launchd.
    Direct,
    #[serde(untagged)]
    Other(String),
}

impl Surface {
    pub const KNOWN: [Surface; 3] = [Surface::Paseo, Surface::T3, Surface::Direct];

    pub fn as_str(&self) -> &str {
        match self {
            Self::Paseo => "paseo",
            Self::T3 => "t3",
            Self::Direct => "direct",
            Self::Other(s) => s,
        }
    }
}

impl std::str::FromStr for Surface {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "paseo" => Self::Paseo,
            "t3" => Self::T3,
            "direct" => Self::Direct,
            other => Self::Other(other.to_string()),
        })
    }
}

/// What executes tools and loads hooks inside a session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Harness {
    ClaudeCode,
    Codex,
    Gemini,
    Agy,
    #[serde(untagged)]
    Other(String),
}

impl Harness {
    pub const KNOWN: [Harness; 4] = [Harness::ClaudeCode, Harness::Codex, Harness::Gemini, Harness::Agy];

    pub fn as_str(&self) -> &str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Agy => "agy",
            Self::Other(s) => s,
        }
    }
}

impl std::str::FromStr for Harness {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "claude-code" | "claude" => Self::ClaudeCode,
            "codex" => Self::Codex,
            "gemini" => Self::Gemini,
            "agy" => Self::Agy,
            other => Self::Other(other.to_string()),
        })
    }
}

/// A tailnet machine, by its Tailscale stable node ID (`Self.ID` in `tailscale status --json`).
/// The fleet ADR binds a machine by this ID because names change and the WireGuard key
/// rotates on re-authentication.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub String);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// This machine as Maestro reports it: the stable ID, and its MagicDNS label for people.
/// A report, not a proof: under the fleet ADR the change-plane authority, not Maestro,
/// checks a caller's ID through `tailscale whois`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Node {
    /// From `tailscale status --json`: `Self.ID`, and the first label of `Self.DNSName`
    /// (`carloss-macbook-pro.tailf3e897.ts.net.` names `carloss-macbook-pro`). No ID, no node.
    pub fn from_tailscale_status(status: &serde_json::Value) -> Option<Self> {
        let me = status.get("Self")?;
        let id = me.get("ID")?.as_str()?.trim();
        if id.is_empty() {
            // A blank ID is no node; a name alone is not an identity.
            return None;
        }
        let name = me
            .get("DNSName")
            .and_then(|d| d.as_str())
            .and_then(|d| d.split('.').next())
            .map(|l| l.trim().to_lowercase())
            .filter(|l| !l.is_empty());
        Some(Self { id: NodeId(id.to_string()), name })
    }
}

/// `{surface, environment, id}`, e.g. `t3:local/6ff25f7c` or `paseo:-/ae7d9f21`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SessionRef {
    pub surface: Surface,
    /// Surface-specific scope: a T3 environment id, a Paseo daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    pub id: String,
}

impl fmt::Display for SessionRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}/{}", self.surface.as_str(), self.environment.as_deref().unwrap_or("-"), self.id)
    }
}

impl std::str::FromStr for SessionRef {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (surface, rest) = s.split_once(':').ok_or_else(|| format!("expected surface:env/id, got `{s}`"))?;
        let (env, id) = match rest.split_once('/') {
            Some((env, id)) => (Some(env).filter(|e| *e != "-").map(str::to_string), id),
            None => (None, rest),
        };
        if id.is_empty() {
            return Err(format!("empty session id in `{s}`"));
        }
        let Ok(surface) = surface.parse();
        Ok(Self { surface, environment: env, id: id.into() })
    }
}

/// What a surface can do to a session. Maestro plans against capabilities, never names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceCapabilities {
    /// Start a new session with a brief.
    pub launch: bool,
    /// Deliver a message into a running session.
    pub send: bool,
    pub interrupt: bool,
    pub archive: bool,
    /// Run a prompt on a schedule bound to a session.
    pub schedule: bool,
}

/// What a harness can do inside a session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessCapabilities {
    /// Add context at session start or per prompt (hooks or equivalent).
    pub inject_context: bool,
    /// Serve MCP tools to the session.
    pub mcp: bool,
}

/// Surface capabilities as observed on 2026-10-18. Adapters confirm at runtime; this is the plan.
pub fn surface_capabilities(surface: &Surface) -> SurfaceCapabilities {
    match surface {
        Surface::T3 | Surface::Paseo => {
            SurfaceCapabilities { launch: true, send: true, interrupt: true, archive: true, schedule: true }
        }
        // A bare process is started and scheduled (launchd, persist), never reached once running.
        Surface::Direct => SurfaceCapabilities { launch: true, schedule: true, ..Default::default() },
        Surface::Other(_) => SurfaceCapabilities::default(),
    }
}

/// Harness capabilities as observed on 2026-10-21: each known harness has a registration that
/// runs `maestro mcp`. An unknown harness is promised nothing. Adapters confirm at runtime.
pub fn harness_capabilities(harness: &Harness) -> HarnessCapabilities {
    match harness {
        // Codex loads SessionStart and UserPromptSubmit hooks (`~/.codex/hooks.json`).
        Harness::ClaudeCode | Harness::Codex => HarnessCapabilities { inject_context: true, mcp: true },
        Harness::Gemini | Harness::Agy => HarnessCapabilities { mcp: true, ..Default::default() },
        Harness::Other(_) => HarnessCapabilities::default(),
    }
}

/// Peer credentials read from a local socket where the platform provides them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerCredentials {
    pub pid: u32,
    pub uid: u32,
}

/// What a caller claims about the session it is speaking from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallerClaim {
    pub surface: Surface,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<PeerCredentials>,
}

impl CallerClaim {
    pub fn paseo(session_id: Option<impl Into<String>>, cwd: Option<impl Into<String>>) -> Self {
        Self {
            surface: Surface::Paseo,
            session_id: session_id.map(Into::into),
            cwd: cwd.map(Into::into),
            token: None,
            peer: None,
        }
    }

    pub fn t3(session_id: Option<impl Into<String>>, cwd: Option<impl Into<String>>) -> Self {
        Self {
            surface: Surface::T3,
            session_id: session_id.map(Into::into),
            cwd: cwd.map(Into::into),
            token: None,
            peer: None,
        }
    }

    pub fn direct(
        session_id: Option<impl Into<String>>,
        cwd: Option<impl Into<String>>,
        token: Option<impl Into<String>>,
    ) -> Self {
        Self {
            surface: Surface::Direct,
            session_id: session_id.map(Into::into),
            cwd: cwd.map(Into::into),
            token: token.map(Into::into),
            peer: None,
        }
    }

    pub fn with_peer(mut self, peer: PeerCredentials) -> Self {
        self.peer = Some(peer);
        self
    }
}

/// Whether a registered session is live or has ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLiveness {
    Live,
    Archived,
    Closed,
    Settled,
}

impl SessionLiveness {
    pub fn is_live(self) -> bool {
        matches!(self, Self::Live)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Archived => "archived",
            Self::Closed => "closed",
            Self::Settled => "settled",
        }
    }
}

/// A live permission prompt held by a session on its surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionPrompt {
    pub request_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl PermissionPrompt {
    /// Turn a live permission prompt into a [`crate::need::Need`] for the owner.
    pub fn to_need(&self, session: &SessionRef, project: Option<String>, since: Option<String>) -> crate::need::Need {
        let title = match &self.detail {
            Some(detail) if !detail.is_empty() => format!("{}: {detail}", self.title),
            _ => self.title.clone(),
        };
        crate::need::Need {
            kind: crate::need::NeedKind::Approval,
            source: crate::need::NeedSource::Approvals,
            key: format!("approval:{session}#{}", self.request_id),
            title,
            since,
            ask: None,
            project,
            unblocks: vec![session.to_string()],
        }
    }
}

/// A session that a surface registry has verified as live and matching its folder and credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedSession {
    session: SessionRef,
    cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    harness: Option<Harness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_permissions: Vec<PermissionPrompt>,
}

impl VerifiedSession {
    /// Construct a verified session record. Intended for surface adapters in `maestro-hosts`.
    pub fn new(
        session: SessionRef,
        cwd: impl Into<String>,
        harness: Option<Harness>,
        parent_id: Option<String>,
        pending_permissions: Vec<PermissionPrompt>,
    ) -> Self {
        Self { session, cwd: cwd.into(), harness, parent_id, pending_permissions }
    }

    pub fn session(&self) -> &SessionRef {
        &self.session
    }

    pub fn agent_id(&self) -> &str {
        &self.session.id
    }

    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    pub fn harness(&self) -> Option<&Harness> {
        self.harness.as_ref()
    }

    pub fn parent_id(&self) -> Option<&str> {
        self.parent_id.as_deref()
    }

    pub fn pending_permissions(&self) -> &[PermissionPrompt] {
        &self.pending_permissions
    }

    pub fn principal(&self) -> crate::authority::Principal {
        crate::authority::Principal::Agent { session: self.session.clone() }
    }

    pub fn into_principal(self) -> crate::authority::Principal {
        crate::authority::Principal::Agent { session: self.session }
    }

    /// Convert all live permission prompts on this session into [`crate::need::Need`] items.
    pub fn needs(&self, project: Option<&str>, since: Option<&str>) -> Vec<crate::need::Need> {
        self.pending_permissions
            .iter()
            .map(|prompt| prompt.to_need(&self.session, project.map(str::to_string), since.map(str::to_string)))
            .collect()
    }
}

/// Why a surface registry refused a caller claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    MissingSessionId,
    MissingCwd,
    RelativeCwd,
    NotFound,
    NotLive,
    OutsideCwd,
    CrossSurface,
    PeerMismatch,
    InvalidToken,
    UnsupportedSurface,
}

/// A refused caller identity check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityRefusal {
    pub reason: RefusalReason,
    pub message: String,
}

impl IdentityRefusal {
    pub fn new(reason: RefusalReason, message: impl Into<String>) -> Self {
        Self { reason, message: message.into() }
    }
}

impl fmt::Display for IdentityRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for IdentityRefusal {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ref_round_trips() {
        for s in ["t3:local/6ff25f7c", "paseo:-/ae7d9f21", "direct:-/abc"] {
            let parsed: SessionRef = s.parse().unwrap();
            assert_eq!(parsed.to_string(), s);
        }
        let bare: SessionRef = "direct:xyz".parse().unwrap();
        assert_eq!(bare.to_string(), "direct:-/xyz");
    }

    #[test]
    fn surfaces_do_not_alias() {
        let a: SessionRef = "t3:-/x".parse().unwrap();
        let b: SessionRef = "paseo:-/x".parse().unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn a_harness_is_not_a_surface() {
        // A harness name in surface position stays unknown: a bare Claude Code process
        // is `direct`, and Maestro never guesses that from the harness.
        let r: SessionRef = "claude-code:-/abc".parse().unwrap();
        assert_eq!(r.surface, Surface::Other("claude-code".into()));
        assert_eq!(surface_capabilities(&r.surface), SurfaceCapabilities::default());
    }

    #[test]
    fn names_round_trip_through_serde() {
        for s in Surface::KNOWN.iter().chain([&Surface::Other("wave".into())]) {
            let v = serde_json::to_value(s).unwrap();
            assert_eq!(v, serde_json::json!(s.as_str()));
            assert_eq!(&serde_json::from_value::<Surface>(v).unwrap(), s);
        }
        for h in Harness::KNOWN.iter().chain([&Harness::Other("aider".into())]) {
            let v = serde_json::to_value(h).unwrap();
            assert_eq!(v, serde_json::json!(h.as_str()));
            assert_eq!(&serde_json::from_value::<Harness>(v).unwrap(), h);
            assert_eq!(&h.as_str().parse::<Harness>().unwrap(), h);
        }
    }

    #[test]
    fn node_is_the_stable_id_named_by_its_dns_label() {
        let status = serde_json::json!({ "Self": { "ID": "nwPbDUaNzo11CNTRL", "HostName": "Carlos’s MacBook Pro", "DNSName": "Carloss-MacBook-Pro.tailf3e897.ts.net." } });
        let node = Node::from_tailscale_status(&status).unwrap();
        assert_eq!(node.id, NodeId("nwPbDUaNzo11CNTRL".into()));
        assert_eq!(node.name.as_deref(), Some("carloss-macbook-pro"));
        let nameless = serde_json::json!({ "Self": { "ID": "n1", "DNSName": "" } });
        assert_eq!(Node::from_tailscale_status(&nameless), Some(Node { id: NodeId("n1".into()), name: None }));
        for absent in [
            serde_json::json!({}),
            serde_json::json!({ "Self": { "DNSName": "a.ts.net." } }),
            serde_json::json!({ "Self": { "ID": " ", "DNSName": "a.ts.net." } }),
            serde_json::json!({ "Self": { "ID": 3 } }),
        ] {
            assert_eq!(Node::from_tailscale_status(&absent), None, "{absent}");
        }
    }

    #[test]
    fn harness_capabilities_are_the_observed_ones() {
        let table = [
            (Harness::ClaudeCode, true, true),
            (Harness::Codex, true, true),
            (Harness::Gemini, false, true),
            (Harness::Agy, false, true),
            (Harness::Other("aider".into()), false, false),
        ];
        for (h, inject_context, mcp) in table {
            assert_eq!(harness_capabilities(&h), HarnessCapabilities { inject_context, mcp }, "{h:?}");
        }
    }

    #[test]
    fn surface_capabilities_are_the_observed_ones() {
        let all = SurfaceCapabilities { launch: true, send: true, interrupt: true, archive: true, schedule: true };
        let table = [
            (Surface::Paseo, all.clone()),
            (Surface::T3, all),
            (Surface::Direct, SurfaceCapabilities { launch: true, schedule: true, ..Default::default() }),
            (Surface::Other("wave".into()), SurfaceCapabilities::default()),
        ];
        for (s, caps) in table {
            assert_eq!(surface_capabilities(&s), caps, "{s:?}");
        }
    }

    #[test]
    fn names_parse_to_their_variants() {
        for s in Surface::KNOWN {
            assert_eq!(s.as_str().parse::<Surface>().unwrap(), s);
            let r: SessionRef = format!("{}:-/x", s.as_str()).parse().unwrap();
            assert_eq!(r.surface, s);
        }
        for h in Harness::KNOWN {
            assert_eq!(h.as_str().parse::<Harness>().unwrap(), h);
        }
        assert_eq!("claude".parse::<Harness>().unwrap(), Harness::ClaudeCode);
    }
}
