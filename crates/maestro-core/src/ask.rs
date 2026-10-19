//! Asks: questions only the owner can answer, held in `.control/asks/*.yaml`.
//!
//! The ledger format is owned by `scripts/ask_ledger.py`, which stays the
//! validator of record. These types read it faithfully and keep every unknown
//! key, so nothing Maestro rewrites can lose a field.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub type Extra = BTreeMap<String, serde_json::Value>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskClass {
    Credential,
    Authority,
    Decision,
    Artifact,
    Scope,
    External,
    Idea,
    #[serde(untagged)]
    Other(String),
}

/// Why an ask may not carry a default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoDefaultReason {
    Irreversible,
    ExternallyVisible,
    SpendsMoney,
    #[serde(untagged)]
    Other(String),
}

/// The ladder an agent climbs before it may ask. Each `exhausted` entry starts with one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rung {
    Read,
    Preauth,
    Delegate,
    Reroute,
    Loop,
    Default,
}

impl Rung {
    pub const ALL: [Rung; 6] = [Self::Read, Self::Preauth, Self::Delegate, Self::Reroute, Self::Loop, Self::Default];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Preauth => "preauth",
            Self::Delegate => "delegate",
            Self::Reroute => "reroute",
            Self::Loop => "loop",
            Self::Default => "default",
        }
    }

    /// The rung an `exhausted` entry names. YAML reads `- read: did x` as a
    /// one-key map, so an entry is either `"rung: ..."` text or `{rung: ...}`.
    pub fn of_entry(entry: &serde_json::Value) -> Option<Rung> {
        let head = match entry {
            serde_json::Value::String(s) => s.split(':').next()?.trim().to_string(),
            serde_json::Value::Object(map) if map.len() == 1 => map.keys().next()?.trim().to_string(),
            _ => return None,
        };
        Self::ALL.into_iter().find(|r| r.as_str() == head)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskOption {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consequence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reversible: Option<bool>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub recommended: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub status_quo: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AskOrigin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ask {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    /// The one-line ask; older ledgers may carry only `question`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<AskClass>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exhausted: Vec<serde_json::Value>,
    #[serde(default)]
    pub blocking: bool,
    #[serde(default)]
    pub preanswerable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_default_because: Option<NoDefaultReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stakes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why_you: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<AskOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<AskOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_option: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Statuses that close an ask, from `ANSWERED_STATUSES` in `ask_ledger.py`.
pub const ANSWERED_STATUSES: [&str; 7] =
    ["resolved", "archived", "rerouted", "closed", "answered", "withdrawn", "defaulted"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskState {
    Open,
    Answered,
    Archived,
}

impl Ask {
    pub fn state(&self) -> AskState {
        match self.status.as_deref() {
            Some("archived" | "withdrawn" | "closed") => AskState::Archived,
            Some(s) if ANSWERED_STATUSES.contains(&s) => AskState::Answered,
            _ if self.answered_at.is_some() || self.answer.is_some() => AskState::Answered,
            _ => AskState::Open,
        }
    }

    /// Best one-line title: headline, then ask, then question, then id.
    pub fn title(&self) -> &str {
        self.headline.as_deref().or(self.ask.as_deref()).or(self.question.as_deref()).unwrap_or(&self.id)
    }

    /// Distinct rungs recorded in `exhausted`.
    pub fn rungs(&self) -> Vec<Rung> {
        let mut out = Vec::new();
        for rung in self.exhausted.iter().filter_map(Rung::of_entry) {
            if !out.contains(&rung) {
                out.push(rung);
            }
        }
        out
    }

    pub fn recommended(&self) -> Option<&AskOption> {
        self.options.iter().find(|o| o.recommended)
    }

    /// Content hash used to refuse an answer made against a stale copy.
    /// Answer fields are excluded so the hash names the question, not its reply.
    pub fn hash(&self) -> String {
        let mut value = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if let Some(map) = value.as_object_mut() {
            for key in
                ["answer", "answer_option", "answer_note", "answer_other", "answered_at", "answered_by", "status"]
            {
                map.remove(key);
            }
        }
        // serde_json maps are ordered (BTreeMap), so this serialization is canonical.
        let bytes = serde_json::to_vec(&value).unwrap_or_default();
        let digest = Sha256::digest(&bytes);
        let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
        format!("sha256:{hex}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskLedger {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lanes: Vec<String>,
    #[serde(default)]
    pub asks: Vec<Ask>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Stable address of one ask: the ledger file name and the ask's uid or id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AskRef {
    pub ledger: String,
    pub id: String,
}

impl std::fmt::Display for AskRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}#{}", self.ledger, self.id)
    }
}

/// Lifecycle of an answer after the owner gives it. Answering never implies execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerStage {
    /// Written, but not yet on main.
    Unlanded,
    /// On main, not yet delivered to anyone who can act.
    Unrouted,
    /// Delivered and acted on.
    Acted,
    Superseded,
    /// Answered before routing existed.
    Before,
    /// Held for the conductor to assign.
    Conductor,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(yaml: &str) -> Ask {
        serde_json::from_value(serde_json::from_str::<serde_json::Value>(yaml).unwrap()).unwrap()
    }

    #[test]
    fn state_follows_status_then_answer_fields() {
        assert_eq!(ask(r#"{"id":"a"}"#).state(), AskState::Open);
        assert_eq!(ask(r#"{"id":"a","answered_at":"x"}"#).state(), AskState::Answered);
        assert_eq!(ask(r#"{"id":"a","status":"resolved"}"#).state(), AskState::Answered);
        assert_eq!(ask(r#"{"id":"a","status":"withdrawn"}"#).state(), AskState::Archived);
    }

    #[test]
    fn rungs_are_distinct_and_prefix_parsed() {
        let a = ask(r#"{"id":"a","exhausted":["read: x",{"read":"y"},"default: z","nope",{"standing-grant":"w"}]}"#);
        assert_eq!(a.rungs(), vec![Rung::Read, Rung::Default]);
    }

    #[test]
    fn hash_ignores_answer_but_not_question() {
        let open = ask(r#"{"id":"a","ask":"q?"}"#);
        let answered = ask(r#"{"id":"a","ask":"q?","answer":"yes","status":"resolved"}"#);
        let changed = ask(r#"{"id":"a","ask":"other?"}"#);
        assert_eq!(open.hash(), answered.hash());
        assert_ne!(open.hash(), changed.hash());
    }

    #[test]
    fn unknown_keys_survive_a_round_trip() {
        let a = ask(r#"{"id":"a","liveness":{"kind":"pr-open","ref":"x#1"}}"#);
        let back = serde_json::to_value(&a).unwrap();
        assert_eq!(back["liveness"]["kind"], "pr-open");
    }

    #[test]
    fn unknown_class_is_kept() {
        let a = ask(r#"{"id":"a","class":"novel"}"#);
        assert_eq!(a.class, Some(AskClass::Other("novel".into())));
    }
}
