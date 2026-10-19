//! Harness-neutral identity for agent sessions.
//!
//! A T3 thread id cannot stand in for a Paseo agent id. Every session Maestro
//! names is namespaced by its host, so routing, caller checks and fleet census
//! never confuse two harnesses.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Host {
    Paseo,
    T3,
    ClaudeCode,
    Codex,
    Gemini,
    #[serde(untagged)]
    Other(String),
}

impl Host {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Paseo => "paseo",
            Self::T3 => "t3",
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Other(s) => s,
        }
    }
}

impl std::str::FromStr for Host {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "paseo" => Self::Paseo,
            "t3" => Self::T3,
            "claude-code" | "claude" => Self::ClaudeCode,
            "codex" => Self::Codex,
            "gemini" => Self::Gemini,
            other => Self::Other(other.to_string()),
        })
    }
}

/// `{host, environment, id}`, e.g. `t3:local/6ff25f7c` or `paseo:-/ae7d9f21`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SessionRef {
    pub host: Host,
    /// Host-specific scope: a T3 environment id, a Paseo daemon, a machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    pub id: String,
}

impl fmt::Display for SessionRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}/{}", self.host.as_str(), self.environment.as_deref().unwrap_or("-"), self.id)
    }
}

impl std::str::FromStr for SessionRef {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (host, rest) = s.split_once(':').ok_or_else(|| format!("expected host:env/id, got `{s}`"))?;
        let (env, id) = match rest.split_once('/') {
            Some((env, id)) => (Some(env).filter(|e| *e != "-").map(str::to_string), id),
            None => (None, rest),
        };
        if id.is_empty() {
            return Err(format!("empty session id in `{s}`"));
        }
        Ok(Self { host: host.parse().unwrap_or(Host::Other(host.into())), environment: env, id: id.into() })
    }
}

/// What a harness adapter can do. Maestro plans against capabilities, never host names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostCapabilities {
    /// Start a new session with a brief.
    pub launch: bool,
    /// Deliver a message into a running session.
    pub send: bool,
    pub interrupt: bool,
    pub archive: bool,
    /// Run a prompt on a schedule bound to a session.
    pub schedule: bool,
    /// Add context at session start or per prompt (hooks or equivalent).
    pub inject_context: bool,
    /// Serve MCP tools to the session.
    pub mcp: bool,
}

/// Capabilities as observed on 2026-10-18. Adapters confirm at runtime; this is the plan.
pub fn known_capabilities(host: &Host) -> HostCapabilities {
    match host {
        Host::T3 => HostCapabilities {
            launch: true,
            send: true,
            interrupt: true,
            archive: true,
            schedule: true,
            inject_context: true,
            mcp: true,
        },
        Host::Paseo => HostCapabilities {
            launch: true,
            send: true,
            interrupt: true,
            archive: true,
            schedule: true,
            inject_context: true,
            mcp: true,
        },
        Host::ClaudeCode => HostCapabilities {
            launch: true,
            send: false,
            interrupt: false,
            archive: false,
            schedule: true,
            inject_context: true,
            mcp: true,
        },
        Host::Codex | Host::Gemini => HostCapabilities { launch: true, mcp: true, ..Default::default() },
        Host::Other(_) => HostCapabilities { mcp: true, ..Default::default() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ref_round_trips() {
        for s in ["t3:local/6ff25f7c", "paseo:-/ae7d9f21", "claude-code:-/abc"] {
            let parsed: SessionRef = s.parse().unwrap();
            assert_eq!(parsed.to_string(), s);
        }
        let bare: SessionRef = "codex:xyz".parse().unwrap();
        assert_eq!(bare.to_string(), "codex:-/xyz");
    }

    #[test]
    fn hosts_do_not_alias() {
        let a: SessionRef = "t3:-/x".parse().unwrap();
        let b: SessionRef = "paseo:-/x".parse().unwrap();
        assert_ne!(a, b);
    }
}
