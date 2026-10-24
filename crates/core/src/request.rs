//! Agent requests: the second authority surface (spec §Authority; BRO-3267, BRO-2916).
//!
//! An agent may file a validated ask, queue owner-gated work, request one exact held action,
//! close its session with a receipt, hand back, or escalate. None of them starts anything (A8):
//! queued work waits for the owner's dispatch and a requested action waits for the owner's
//! decision. Every request names its [`SessionRef`]; until a surface adapter verifies callers
//! (node `host-identity`, BRO-3268) every caller is [`crate::authority::Principal::Unverified`],
//! so every request is recorded as from an unverified caller.
//!
//! What an unverified request may change: only [`RequestKind::AskFile`] is applied, because an
//! ask is itself a hold on the owner. Its origin records the unverified session and carries no
//! `agent_id`, so no owner answer is routed into a session on a caller's word. Every other
//! request is recorded and held.

use crate::authority::Principal;
use crate::session::SessionRef;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RequestKind {
    #[serde(rename = "ask.file")]
    AskFile,
    #[serde(rename = "work.queue")]
    WorkQueue,
    #[serde(rename = "action.request")]
    ActionRequest,
    #[serde(rename = "session.close")]
    SessionClose,
    #[serde(rename = "handback")]
    Handback,
    #[serde(rename = "escalate")]
    Escalate,
}

impl RequestKind {
    pub const ALL: [RequestKind; 6] =
        [Self::AskFile, Self::WorkQueue, Self::ActionRequest, Self::SessionClose, Self::Handback, Self::Escalate];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AskFile => "ask.file",
            Self::WorkQueue => "work.queue",
            Self::ActionRequest => "action.request",
            Self::SessionClose => "session.close",
            Self::Handback => "handback",
            Self::Escalate => "escalate",
        }
    }

    /// What this request does for `caller`: applied, or recorded and held for the owner.
    pub fn disposition(self, caller: &Principal) -> Disposition {
        match (self, caller) {
            // An ask holds the owner by construction; filing one starts nothing.
            (Self::AskFile, _) => Disposition::Applied,
            // Queued work and requested actions wait on an owner verb whoever asks (A8).
            (Self::WorkQueue | Self::ActionRequest, _) => Disposition::Held,
            (Self::SessionClose | Self::Handback | Self::Escalate, Principal::Unverified) => Disposition::Held,
            (Self::SessionClose | Self::Handback | Self::Escalate, _) => Disposition::Recorded,
        }
    }
}

/// What became of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// Done: an ask was written to its ledger.
    Applied,
    /// Recorded for the owner; nothing changed and nothing started.
    Held,
    /// Recorded from a verified session; it changes nothing by itself.
    Recorded,
}

/// One line of the requests journal (`$BROOMVA_HOME/ledger/requests/requests.jsonl`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestRecord {
    /// Always `agent.request`.
    pub kind: String,
    /// 16 random hex.
    pub id: String,
    /// RFC 3339, UTC.
    pub at: String,
    pub request: RequestKind,
    pub session: SessionRef,
    /// Whether the session was verified against its surface's registry.
    pub verified: bool,
    pub disposition: Disposition,
    /// The request's arguments as given.
    pub payload: serde_json::Value,
    /// Where an applied request landed (an ask's ledger and id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_ask_is_applied_for_an_unverified_caller() {
        for kind in RequestKind::ALL {
            let d = kind.disposition(&Principal::Unverified);
            assert_eq!(d == Disposition::Applied, kind == RequestKind::AskFile, "{}", kind.as_str());
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
        }
    }

    #[test]
    fn queued_work_and_actions_wait_on_the_owner_whoever_asks() {
        let agent = Principal::Agent { session: "t3:-/x".parse().unwrap() };
        assert_eq!(RequestKind::WorkQueue.disposition(&agent), Disposition::Held);
        assert_eq!(RequestKind::ActionRequest.disposition(&agent), Disposition::Held);
        assert_eq!(RequestKind::Handback.disposition(&agent), Disposition::Recorded);
    }
}
