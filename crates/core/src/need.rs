//! Needs: holds that require the owner. The "Needs you" count is this list and nothing else.
//!
//! Open decisions, health notices, quiet sessions and work are not
//! interchangeable counts; only a held approval, a blocking ask, or an owner
//! step becomes a need.

use crate::ask::AskRef;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedKind {
    Ask,
    Approval,
    OwnerStep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedSource {
    Work,
    Asks,
    Reviews,
    Answers,
    Approvals,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Need {
    pub kind: NeedKind,
    pub source: NeedSource,
    /// Hold key, e.g. `ask:<ledger>#<id>` or `work:<id>`.
    pub key: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<AskRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// What the owner's answer unblocks, when the source says.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unblocks: Vec<String>,
}
