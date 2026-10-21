//! The loop registry: `.broomva/loops/*.yaml`, one row per standing loop.
//!
//! The registry is intent. What actually runs is observed on its substrate;
//! the difference between them is drift.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type Extra = BTreeMap<String, serde_json::Value>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubstrateKind {
    Launchd,
    PaseoSchedule,
    PaseoHeartbeat,
    MaestroLoop,
    GhaSchedule,
    SessionCron,
    P9,
    T3Schedule,
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Substrate {
    pub kind: SubstrateKind,
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Desired {
    Active,
    Paused,
    Retired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopScope {
    Broomva,
    Sri,
    External,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoopContract {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub produces: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eval: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kill: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopSpec {
    pub id: String,
    pub title: String,
    pub purpose: String,
    pub owner: String,
    pub scope: LoopScope,
    pub substrate: Substrate,
    pub cadence: String,
    pub review_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initiative: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desired: Option<Desired>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<LoopContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl LoopSpec {
    /// A row without `desired` is active, as `loop_ledger.py` reads it.
    pub fn desired(&self) -> Desired {
        self.desired.unwrap_or(Desired::Active)
    }
}

/// Drift classes, from `DRIFT_CLASSES` in `maestro-paseo/shared/loops.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftClass {
    PausedButRunning,
    Orphaned,
    ActiveButAbsent,
    BoundSessionGone,
    DuplicateBinding,
    TargetChanged,
    PromptMismatch,
    StateLost,
    TickMissing,
    TickUnclosed,
    Failing,
    Missed,
    SkippedBusy,
    Expiring,
    Unregistered,
}

/// Signals Maestro can prove from the registry and the ticks ledger alone. They are
/// not drift classes: the plugin's classes need substrate observations (fires,
/// sessions, schedules) that arrive with surface adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    /// An active row's `review_by` date has passed.
    ReviewOverdue,
    /// The contract names tick receipts and the ledger's tail holds none for this loop.
    NoTickReceipts,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notice {
    pub kind: NoticeKind,
    pub loop_id: String,
    pub detail: String,
}

/// A started tick with no end is unclosed after max(this, 2 x the row's median tick length),
/// measured from its start (`TICK_CLOSE_MS` in `maestro-paseo/server/loops-model.ts`).
pub const TICK_CLOSE_SECS: i64 = 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Drift {
    pub class: DriftClass,
    pub loop_id: String,
    pub detail: String,
}
