//! Work items and their lifecycle.
//!
//! `OrchState` is the stored state, kept identical to the Paseo plugin's
//! `ORCH_STATES` so `_work.md` files stay readable by every Maestro.
//! `WorkState` is the presentation vocabulary from the Broomva agentic-work
//! extension; its labels are exact and shared by every surface.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchState {
    Proposed,
    Reviewing,
    Triggered,
    Running,
    Blocked,
    Review,
    Done,
    Canceled,
}

impl OrchState {
    pub const ALL: [OrchState; 8] = [
        Self::Proposed,
        Self::Reviewing,
        Self::Triggered,
        Self::Running,
        Self::Blocked,
        Self::Review,
        Self::Done,
        Self::Canceled,
    ];

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Canceled)
    }

    /// Legal transitions, from `LEGAL` in `maestro-paseo/shared/states.ts`.
    pub fn successors(self) -> &'static [OrchState] {
        use OrchState::*;
        match self {
            Proposed => &[Reviewing, Triggered, Running, Canceled],
            Reviewing => &[Triggered, Running, Canceled],
            Triggered => &[Running, Canceled],
            Running => &[Review, Blocked, Done],
            Blocked => &[Running, Triggered, Canceled, Done],
            Review => &[Done, Running, Canceled, Blocked],
            Done | Canceled => &[],
        }
    }

    pub fn can_transition(self, to: OrchState) -> bool {
        self.successors().contains(&to)
    }

    pub fn as_str(self) -> &'static str {
        use OrchState::*;
        match self {
            Proposed => "proposed",
            Reviewing => "reviewing",
            Triggered => "triggered",
            Running => "running",
            Blocked => "blocked",
            Review => "review",
            Done => "done",
            Canceled => "canceled",
        }
    }
}

impl std::str::FromStr for OrchState {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL.into_iter().find(|state| state.as_str() == s).ok_or_else(|| format!("unknown work state `{s}`"))
    }
}

/// Canonical work states. Labels are exact: copy, components and automation share them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WorkState {
    #[serde(rename = "Queued")]
    Queued,
    #[serde(rename = "Running")]
    Running,
    #[serde(rename = "Stuck")]
    Stuck,
    #[serde(rename = "Needs you")]
    NeedsYou,
    #[serde(rename = "Done")]
    Done,
    #[serde(rename = "Standing")]
    Standing,
}

impl WorkState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Running",
            Self::Stuck => "Stuck",
            Self::NeedsYou => "Needs you",
            Self::Done => "Done",
            Self::Standing => "Standing",
        }
    }

    /// Standing is not a stored state: it is `running` plus the quiet flag.
    pub fn of(state: OrchState, quiet: bool) -> Self {
        use OrchState::*;
        match state {
            Proposed | Reviewing | Triggered => Self::Queued,
            Running if quiet => Self::Standing,
            Running => Self::Running,
            Blocked => Self::Stuck,
            Review => Self::NeedsYou,
            Done | Canceled => Self::Done,
        }
    }
}

impl fmt::Display for WorkState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateMode {
    Human,
    Auto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkKind {
    Question,
    Task,
    Project,
    Initiative,
    Routine,
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Approve,
    Revise,
    Block,
}

/// `_work.md` frontmatter. Unknown keys are kept in `extra`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkFrontmatter {
    pub id: String,
    pub kind: WorkKind,
    pub state: OrchState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<GateMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// A work item as Maestro reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItem {
    /// Folder name under the work root; stable and human-readable.
    pub slug: String,
    pub title: String,
    pub work_state: WorkState,
    #[serde(flatten)]
    pub front: WorkFrontmatter,
    /// Timestamp of the last line in `_events.jsonl`, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states_have_no_successors() {
        for state in OrchState::ALL {
            assert_eq!(state.is_terminal(), state.successors().is_empty(), "{state:?}");
        }
    }

    #[test]
    fn done_is_reachable_only_from_running_blocked_and_review() {
        let into_done: Vec<_> = OrchState::ALL.into_iter().filter(|s| s.can_transition(OrchState::Done)).collect();
        assert_eq!(into_done, vec![OrchState::Running, OrchState::Blocked, OrchState::Review]);
    }

    #[test]
    fn labels_are_the_canonical_vocabulary() {
        let labels: Vec<_> = [
            WorkState::Queued,
            WorkState::Running,
            WorkState::Stuck,
            WorkState::NeedsYou,
            WorkState::Done,
            WorkState::Standing,
        ]
        .iter()
        .map(|s| s.label())
        .collect();
        assert_eq!(labels, ["Queued", "Running", "Stuck", "Needs you", "Done", "Standing"]);
        assert_eq!(serde_json::to_string(&WorkState::NeedsYou).unwrap(), "\"Needs you\"");
    }

    #[test]
    fn standing_is_quiet_running_only() {
        assert_eq!(WorkState::of(OrchState::Running, true), WorkState::Standing);
        assert_eq!(WorkState::of(OrchState::Blocked, true), WorkState::Stuck);
        assert_eq!(WorkState::of(OrchState::Review, false), WorkState::NeedsYou);
    }

    #[test]
    fn state_round_trips_through_str() {
        for state in OrchState::ALL {
            assert_eq!(state.as_str().parse::<OrchState>().unwrap(), state);
        }
    }
}
