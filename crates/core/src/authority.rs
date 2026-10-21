//! Who may do what. Three authority surfaces, from the BRO-3186 contract.
//!
//! 1. Reads: scoped observations for coordinators and dashboards.
//! 2. Agent requests: queue gated work, file a validated ask, request one held action.
//! 3. Owner actions: answer, allow or deny, dispatch, send back, review, undo.
//!
//! A model-visible tool is never proof of the owner. An owner action needs a
//! principal the server can verify; a client-supplied `actor: human` is not one.

use crate::session::SessionRef;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    Read,
    AgentRequest,
    OwnerAction,
}

/// The caller as the server verified it, not as it described itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Principal {
    /// Verified through an owner-only channel (signed token, local app, confirmed host prompt).
    Owner { via: String },
    /// A session whose identity was checked against its surface's registry.
    Agent { session: SessionRef },
    /// Anything unverified, including every MCP caller in phase 0.
    Unverified,
}

impl Principal {
    #[allow(clippy::match_like_matches_macro, reason = "one arm per authority surface reads as the policy table")]
    pub fn permits(&self, needed: Authority) -> bool {
        match (self, needed) {
            (_, Authority::Read) => true,
            (Principal::Agent { .. } | Principal::Owner { .. }, Authority::AgentRequest) => true,
            (Principal::Owner { .. }, Authority::OwnerAction) => true,
            _ => false,
        }
    }
}

/// Verbs only the owner may take. Typed now so no surface can invent a looser one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnerVerb {
    #[serde(rename = "ask.answer")]
    AskAnswer,
    #[serde(rename = "approval.decide")]
    ApprovalDecide,
    #[serde(rename = "work.verdict")]
    WorkVerdict,
    #[serde(rename = "work.dispatch")]
    WorkDispatch,
    #[serde(rename = "verb.undo")]
    VerbUndo,
    #[serde(rename = "fleet.review")]
    FleetReview,
    #[serde(rename = "conductor.decide")]
    ConductorDecide,
    #[serde(rename = "initiatives.review")]
    InitiativesReview,
}

impl OwnerVerb {
    pub const ALL: [OwnerVerb; 8] = [
        Self::AskAnswer,
        Self::ApprovalDecide,
        Self::WorkVerdict,
        Self::WorkDispatch,
        Self::VerbUndo,
        Self::FleetReview,
        Self::ConductorDecide,
        Self::InitiativesReview,
    ];

    /// Whether the verb waits out the undo window before it applies.
    pub fn undoable(self) -> bool {
        !matches!(self, Self::VerbUndo)
    }
}

/// Every owner verb waits this long, then is re-checked before it applies (B4).
pub const UNDO_MS: u64 = 8_000;

/// Standing invariants every Maestro surface enforces. `A1`, `A8` and `B4` are
/// owner answers recorded in the Paseo plugin; the last two come from the
/// BRO-3186 contract and carry descriptive keys, not policy gate ids.
pub const INVARIANTS: &[(&str, &str)] = &[
    ("A1", "Work never reaches Done on its session's say-so; only gates, checks or the owner close it."),
    ("A8", "Loops, routing and agent requests start nothing; dispatch is an owner verb."),
    ("B4", "Every owner verb has an undo window and is re-checked when it applies."),
    ("one-governor", "One governor per scope; never run a second active governor."),
    ("unread-is-not-empty", "An unread source is reported as unread, never as empty."),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unverified_callers_only_read() {
        let p = Principal::Unverified;
        assert!(p.permits(Authority::Read));
        assert!(!p.permits(Authority::AgentRequest));
        assert!(!p.permits(Authority::OwnerAction));
    }

    #[test]
    fn agents_cannot_take_owner_actions() {
        let p = Principal::Agent { session: "t3:-/x".parse().unwrap() };
        assert!(p.permits(Authority::AgentRequest));
        assert!(!p.permits(Authority::OwnerAction));
    }
}
