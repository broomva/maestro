//! Context packets: what Maestro injects into an agent session so it works
//! inside the governance layer without being told.
//!
//! A packet is built from one snapshot and scoped to the session (its cwd,
//! project or initiative). The same packet renders for every injection
//! strategy; only the envelope changes per harness.

use crate::authority::INVARIANTS;
use crate::need::Need;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

/// How a packet reaches a session. Adapters pick the strongest one their host supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InjectionStrategy {
    /// Harness hook at session start (Claude Code `SessionStart`, Codex/T3 equivalents).
    SessionStart,
    /// Harness hook on each user prompt; keep packets small here.
    UserPrompt,
    /// MCP prompt the agent or user pulls (`maestro_context`).
    McpPrompt,
    /// MCP resource a client may attach (`maestro://context`).
    McpResource,
    /// A message delivered into a running thread by an orchestrator (T3 `thread_send`).
    ThreadMessage,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextScope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initiative: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitiativeBrief {
    pub id: String,
    pub title: String,
    pub status: String,
    pub next_action: String,
    pub driver: String,
}

/// An open ask in scope that does not block a lane: decided by the owner in their own time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskBrief {
    pub key: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopBrief {
    pub id: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPacket {
    pub generated_at: String,
    pub scope: ContextScope,
    /// Total holds on the owner across all scopes; `None` when the ask ledgers could not be read.
    pub needs_you: Option<usize>,
    /// False when some ledgers or work items failed to read: the count is a lower bound.
    pub needs_complete: bool,
    /// Holds relevant to this scope, most relevant first.
    pub needs: Vec<Need>,
    /// Open, non-blocking asks in this scope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_asks: Vec<AskBrief>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initiative: Option<InitiativeBrief>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loops: Vec<LoopBrief>,
    /// Sources that could not be read; the agent must not treat them as empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub degraded: Vec<String>,
}

impl ContextPacket {
    /// Render for a model. `max_needs` bounds the list so per-prompt injection stays small.
    pub fn render_markdown(&self, max_needs: usize) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "## Maestro context");
        let _ = writeln!(out);
        let scope = [
            self.scope.project.as_deref().map(|p| format!("project `{p}`")),
            self.scope.initiative.as_deref().map(|i| format!("initiative `{i}`")),
            self.scope.cwd.as_deref().map(|c| format!("cwd `{c}`")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        if !scope.is_empty() {
            let _ = writeln!(out, "Scope: {}.", scope.join(", "));
        }
        match self.needs_you {
            None => {
                let _ = writeln!(
                    out,
                    "Needs you: unknown. The ask ledgers could not be read; do not assume nothing is held."
                );
            }
            Some(n) => {
                let bound = if self.needs_complete { "" } else { "at least " };
                let _ = writeln!(out, "Needs you: {bound}{n} open hold{} on the owner.", if n == 1 { "" } else { "s" });
            }
        }

        if let Some(init) = &self.initiative {
            let _ = writeln!(out);
            let _ = writeln!(out, "### Initiative `{}`", init.id);
            let _ = writeln!(out, "{} ({}, driver {}).", init.title, init.status, init.driver);
            let _ = writeln!(out, "Next action: {}", init.next_action);
        }

        if !self.needs.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "### Holds in this scope");
            for need in self.needs.iter().take(max_needs) {
                let _ = writeln!(out, "- `{}`: {}", need.key, need.title);
            }
            if self.needs.len() > max_needs {
                let _ = writeln!(out, "- and {} more; run `maestro needs`.", self.needs.len() - max_needs);
            }
            let _ = writeln!(out, "Do not re-ask these. Work that depends on one waits; take the next unblocked lane.");
        }

        if !self.open_asks.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "### Open asks in this scope (not blocking)");
            for a in self.open_asks.iter().take(max_needs) {
                let _ = writeln!(out, "- `{}`: {}", a.key, a.title);
            }
            if self.open_asks.len() > max_needs {
                let _ = writeln!(out, "- and {} more; run `maestro asks`.", self.open_asks.len() - max_needs);
            }
            let _ = writeln!(out, "Already asked: read these before filing a new ask.");
        }

        if !self.loops.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "### Loops needing attention");
            for l in &self.loops {
                let _ = writeln!(out, "- `{}`: {}", l.id, l.note);
            }
        }

        if !self.degraded.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "Unread sources (treat as unknown, not empty): {}.", self.degraded.join(", "));
        }

        let _ = writeln!(out);
        let _ = writeln!(out, "### Standing rules");
        for (id, rule) in INVARIANTS {
            let _ = writeln!(out, "- {id}: {rule}");
        }
        let _ = writeln!(out, "\nQuery more with the `maestro` CLI or the `maestro_*` MCP tools (read-only).");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::need::{NeedKind, NeedSource};

    fn need(i: usize) -> Need {
        Need {
            kind: NeedKind::Ask,
            source: NeedSource::Asks,
            key: format!("ask:l#{i}"),
            title: format!("Question {i}?"),
            since: None,
            ask: None,
            project: None,
            unblocks: vec![],
        }
    }

    #[test]
    fn rendering_bounds_needs_and_names_unread_sources() {
        let packet = ContextPacket {
            generated_at: "now".into(),
            scope: ContextScope { project: Some("maestro".into()), ..Default::default() },
            needs_you: Some(5),
            needs_complete: true,
            needs: (0..5).map(need).collect(),
            open_asks: vec![],
            initiative: None,
            loops: vec![],
            degraded: vec!["fleet".into()],
        };
        let md = packet.render_markdown(2);
        assert!(md.contains("Needs you: 5 open holds"));
        assert!(md.contains("`ask:l#1`"));
        assert!(!md.contains("`ask:l#2`"));
        assert!(md.contains("and 3 more"));
        assert!(md.contains("Unread sources (treat as unknown, not empty): fleet."));
        assert!(md.contains("A8:"));
    }

    #[test]
    fn unread_needs_render_as_unknown_and_partial_as_a_bound() {
        let mut packet = ContextPacket {
            generated_at: "now".into(),
            scope: ContextScope::default(),
            needs_you: None,
            needs_complete: false,
            needs: vec![],
            open_asks: vec![],
            initiative: None,
            loops: vec![],
            degraded: vec!["asks".into()],
        };
        let md = packet.render_markdown(8);
        assert!(md.contains("Needs you: unknown"));
        assert!(!md.contains("Needs you: 0"));
        packet.needs_you = Some(2);
        assert!(packet.render_markdown(8).contains("Needs you: at least 2 open holds"));
    }
}
