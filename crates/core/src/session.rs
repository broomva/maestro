//! Who drives a session, what runs it, and where.
//!
//! Three names, as the fleet ADR (`docs/adrs/2026-10-19-fleet-runtime-boundaries.html`)
//! fixes them:
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

/// A tailnet machine.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub String);

impl NodeId {
    /// The machine's MagicDNS label from `tailscale status --json`: the first label of
    /// `Self.DNSName` (`carloss-macbook-pro.tailf3e897.ts.net.` names `carloss-macbook-pro`).
    /// The OS hostname is not used: it can differ from the tailnet name.
    pub fn from_tailscale_status(status: &serde_json::Value) -> Option<Self> {
        let label = status.get("Self")?.get("DNSName")?.as_str()?.split('.').next()?.trim();
        (!label.is_empty()).then(|| Self(label.to_lowercase()))
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
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

/// Harness capabilities as observed on 2026-10-18 (agy: its MCP config registers Maestro,
/// 2026-10-19). Adapters confirm at runtime; this is the plan.
pub fn harness_capabilities(harness: &Harness) -> HarnessCapabilities {
    match harness {
        Harness::ClaudeCode => HarnessCapabilities { inject_context: true, mcp: true },
        Harness::Codex | Harness::Gemini | Harness::Agy | Harness::Other(_) => {
            HarnessCapabilities { mcp: true, ..Default::default() }
        }
    }
}

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
    fn node_is_the_tailnet_label() {
        let status = serde_json::json!({ "Self": { "HostName": "Carlos’s MacBook Pro", "DNSName": "Carloss-MacBook-Pro.tailf3e897.ts.net." } });
        assert_eq!(NodeId::from_tailscale_status(&status), Some(NodeId("carloss-macbook-pro".into())));
        for absent in [
            serde_json::json!({}),
            serde_json::json!({ "Self": { "DNSName": "" } }),
            serde_json::json!({ "Self": { "DNSName": 3 } }),
        ] {
            assert_eq!(NodeId::from_tailscale_status(&absent), None, "{absent}");
        }
    }

    #[test]
    fn every_known_harness_serves_mcp_and_only_claude_code_injects() {
        // Each of the four is registered to run `maestro mcp`; only Claude Code's hooks inject.
        for h in Harness::KNOWN {
            let c = harness_capabilities(&h);
            assert!(c.mcp, "{h:?} mcp");
            assert_eq!(c.inject_context, h == Harness::ClaudeCode, "{h:?} inject_context");
        }
    }

    #[test]
    fn only_orchestrating_surfaces_reach_a_running_session() {
        for s in Surface::KNOWN {
            let c = surface_capabilities(&s);
            let orchestrated = matches!(s, Surface::Paseo | Surface::T3);
            assert_eq!(c.send, orchestrated, "{s:?} send");
            assert_eq!(c.interrupt, orchestrated, "{s:?} interrupt");
            assert_eq!(c.archive, orchestrated, "{s:?} archive");
            assert!(c.launch, "{s:?} launch");
        }
    }
}
