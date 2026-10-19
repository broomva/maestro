//! Ticks: one bounded fire of a standing loop, bracketed by ledger receipts.
//!
//! A tick opens with `loop.tick.started`, works its due tiers, and closes with
//! `loop.tick.ended` caused by the start event. Both live in the hash-chained
//! `~/.broomva/ledger/loops/ticks.jsonl` envelope (`broomva-event.v1`).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The `broomva-event.v1` envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEvent {
    pub v: u32,
    pub id: String,
    pub ts: String,
    pub stream: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub actor: String,
    pub subject: String,
    #[serde(default)]
    pub refs: serde_json::Value,
    #[serde(default)]
    pub cause: Option<String>,
    #[serde(default)]
    pub data: serde_json::Value,
    /// Hash of the previous event; `null` on the chain's first event.
    #[serde(default)]
    pub prev: Option<String>,
}

pub const TICK_STARTED: &str = "loop.tick.started";
pub const TICK_ENDED: &str = "loop.tick.ended";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TickOutcome {
    Ok,
    Partial,
    Failed,
}

/// Cadence tiers a coordinator tick works when due.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tier {
    #[serde(rename = "every")]
    Every,
    #[serde(rename = "hourly")]
    Hourly,
    #[serde(rename = "4h")]
    FourHourly,
    #[serde(rename = "daily")]
    Daily,
}

impl Tier {
    pub const ALL: [Tier; 4] = [Self::Every, Self::Hourly, Self::FourHourly, Self::Daily];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Every => "every",
            Self::Hourly => "hourly",
            Self::FourHourly => "4h",
            Self::Daily => "daily",
        }
    }

    /// Minimum seconds between runs. `every` is due on each fire.
    pub fn period_secs(self) -> i64 {
        match self {
            Self::Every => 0,
            Self::Hourly => 3_600,
            Self::FourHourly => 4 * 3_600,
            Self::Daily => 24 * 3_600,
        }
    }
}

/// Tiers due at `now`, given each tier's last run (unix seconds). A tier never run is due.
pub fn due_tiers(last_run: &BTreeMap<Tier, i64>, now: i64) -> Vec<Tier> {
    Tier::ALL
        .into_iter()
        .filter(|tier| match last_run.get(tier) {
            None => true,
            Some(&at) => now - at >= tier.period_secs(),
        })
        .collect()
}

/// A start event paired with its end, if one was written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tick {
    pub loop_id: String,
    pub actor: String,
    pub started_at: String,
    pub start_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<TickOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calls: Option<u64>,
}

impl Tick {
    pub fn is_closed(&self) -> bool {
        self.ended_at.is_some()
    }
}

/// Pair starts with ends by `cause`. Events need not be sorted; output is in start order.
pub fn pair_ticks(events: &[LedgerEvent]) -> Vec<Tick> {
    let ends: BTreeMap<&str, &LedgerEvent> =
        events.iter().filter(|e| e.kind == TICK_ENDED).filter_map(|e| e.cause.as_deref().map(|c| (c, e))).collect();
    events
        .iter()
        .filter(|e| e.kind == TICK_STARTED)
        .map(|start| {
            let end = ends.get(start.id.as_str());
            let data = end.map(|e| &e.data);
            Tick {
                loop_id: start.subject.strip_prefix("loop:").unwrap_or(&start.subject).to_string(),
                actor: start.actor.clone(),
                started_at: start.ts.clone(),
                start_id: start.id.clone(),
                ended_at: end.map(|e| e.ts.clone()),
                outcome: data.and_then(|d| d.get("outcome")).and_then(|o| serde_json::from_value(o.clone()).ok()),
                summary: data.and_then(|d| d.get("summary")).and_then(|s| s.as_str()).map(str::to_string),
                tiers: data
                    .and_then(|d| d.get("tiers"))
                    .and_then(|t| serde_json::from_value(t.clone()).ok())
                    .unwrap_or_default(),
                calls: data.and_then(|d| d.get("calls")).and_then(|c| c.as_u64()),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(id: &str, kind: &str, cause: Option<&str>, data: serde_json::Value) -> LedgerEvent {
        LedgerEvent {
            v: 1,
            id: id.into(),
            ts: format!("ts-{id}"),
            stream: "loops/ticks".into(),
            kind: kind.into(),
            actor: "agent:x".into(),
            subject: "loop:coordinator-tick".into(),
            refs: json!({}),
            cause: cause.map(str::to_string),
            data,
            prev: Some("sha256:0".into()),
        }
    }

    #[test]
    fn pairs_by_cause_and_leaves_open_ticks_open() {
        let events = vec![
            event("1", TICK_STARTED, None, json!({})),
            event("2", TICK_STARTED, None, json!({})),
            event("3", TICK_ENDED, Some("1"), json!({"outcome":"ok","summary":"s","tiers":["every"],"calls":4})),
        ];
        let ticks = pair_ticks(&events);
        assert_eq!(ticks.len(), 2);
        assert_eq!(ticks[0].loop_id, "coordinator-tick");
        assert_eq!(ticks[0].outcome, Some(TickOutcome::Ok));
        assert_eq!(ticks[0].calls, Some(4));
        assert!(!ticks[1].is_closed());
    }

    #[test]
    fn due_tiers_respect_periods() {
        let now = 100_000;
        let last =
            BTreeMap::from([(Tier::Every, now), (Tier::Hourly, now - 3_599), (Tier::FourHourly, now - 4 * 3_600)]);
        assert_eq!(due_tiers(&last, now), vec![Tier::Every, Tier::FourHourly, Tier::Daily]);
    }
}
