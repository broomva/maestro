//! Fleet reconcile reports, `~/.local/state/fleet-reconcile/<scope>/ticks/NNNNN/report.json`.
//!
//! The fleet skill owns census, classification and agent mail. Maestro reads
//! its reports and never acts in its place.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetReport {
    pub v: u32,
    pub tick: u64,
    pub ts: String,
    pub scope: String,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub dry_run: Option<bool>,
    #[serde(default)]
    pub phase: Option<u32>,
    #[serde(default)]
    pub class_counts: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub sessions: Vec<serde_json::Value>,
    #[serde(default)]
    pub asks_open: Vec<serde_json::Value>,
    #[serde(default)]
    pub drift: Vec<serde_json::Value>,
}

/// What Maestro shows per scope: the latest report, condensed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetSummary {
    pub scope: String,
    pub tick: u64,
    pub ts: String,
    pub mode: Option<String>,
    pub dry_run: Option<bool>,
    pub sessions: usize,
    pub asks_open: usize,
    pub drift: usize,
    pub class_counts: BTreeMap<String, serde_json::Value>,
}

impl From<&FleetReport> for FleetSummary {
    fn from(r: &FleetReport) -> Self {
        Self {
            scope: r.scope.clone(),
            tick: r.tick,
            ts: r.ts.clone(),
            mode: r.mode.clone(),
            dry_run: r.dry_run,
            sessions: r.sessions.len(),
            asks_open: r.asks_open.len(),
            drift: r.drift.len(),
            class_counts: r.class_counts.clone(),
        }
    }
}
