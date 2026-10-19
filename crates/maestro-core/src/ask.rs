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
    /// The entry exactly as the ledger holds it. State and hash read this, never the typed copy.
    #[serde(skip)]
    pub row: serde_json::Value,
}

/// Keys whose presence means the ask was answered, from `OUTCOME_KEYS` in
/// `maestro-paseo/server/control-asks.ts`. The plugin is the answer writer, so
/// its reading is the one every surface must share.
pub const OUTCOME_KEYS: [&str; 6] = ["answer", "answered", "resolution", "outcome", "answered_at", "defaulted_at"];

/// Status prefixes that close an ask as answered (`TERMINAL_STATUS` in the plugin).
pub const TERMINAL_STATUS_PREFIXES: [&str; 6] =
    ["resolved", "answered", "closed", "rerouted", "withdrawn", "defaulted"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskState {
    Open,
    Answered,
    Archived,
}

/// The plugin's `text()`: a trimmed non-empty string, or a number or bool as text.
fn text(value: Option<&serde_json::Value>) -> Option<String> {
    match value? {
        serde_json::Value::String(s) => Some(s.trim()).filter(|s| !s.is_empty()).map(str::to_string),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// An ask row's state, ported from the plugin's `stateOf`.
pub fn state_of(row: &serde_json::Value) -> AskState {
    let status = text(row.get("status")).unwrap_or_default().to_lowercase();
    if text(row.get("archived_at")).is_some() || status.starts_with("archived") {
        return AskState::Archived;
    }
    if OUTCOME_KEYS.iter().any(|k| text(row.get(*k)).is_some())
        || TERMINAL_STATUS_PREFIXES.iter().any(|p| status.starts_with(p))
    {
        return AskState::Answered;
    }
    AskState::Open
}

/// `JSON.stringify(canonical(row))`: keys sorted at every depth, compact.
fn canonical_json(value: &serde_json::Value, out: &mut String) {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                canonical_json(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical_json(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// The plugin's `entryHash`: the first 16 hex of sha256 over the canonical row.
/// It covers the whole entry, answers included: an entry changed since it was
/// read is not answered. This is the hash the plugin's answer path rechecks.
pub fn entry_hash(row: &serde_json::Value) -> String {
    let mut s = String::new();
    canonical_json(row, &mut s);
    let digest = Sha256::digest(s.as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

impl Ask {
    /// The row as written. Built from the typed fields when the ask was not read from a ledger.
    fn row_value(&self) -> std::borrow::Cow<'_, serde_json::Value> {
        if self.row.is_object() {
            std::borrow::Cow::Borrowed(&self.row)
        } else {
            std::borrow::Cow::Owned(serde_json::to_value(self).unwrap_or_default())
        }
    }

    pub fn state(&self) -> AskState {
        state_of(&self.row_value())
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

    /// See [`entry_hash`].
    pub fn hash(&self) -> String {
        entry_hash(&self.row_value())
    }

    /// The handle the plugin addresses an ask by: its uid, else its id.
    pub fn handle(&self) -> &str {
        self.uid.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    #[serde(default, skip_deserializing)]
    pub asks: Vec<Ask>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl AskLedger {
    /// Parse a ledger document. The asks list is `asks`, or the corpus's other
    /// spelling `open_asks` (the plugin's `asksKey`). An entry that does not
    /// parse is returned as `(index, reason)` and the rest are kept.
    pub fn from_value(mut doc: serde_json::Value) -> Result<(AskLedger, Vec<(usize, String)>), String> {
        let map = doc.as_object_mut().ok_or("ledger is not a mapping")?;
        let key = if map.get("asks").is_some_and(|v| v.is_array()) {
            Some("asks")
        } else if map.get("open_asks").is_some_and(|v| v.is_array()) {
            Some("open_asks")
        } else {
            None
        };
        let rows = key.and_then(|k| map.remove(k)).and_then(|v| v.as_array().cloned()).unwrap_or_default();
        let mut ledger: AskLedger = serde_json::from_value(doc).map_err(|e| e.to_string())?;
        let mut errors = Vec::new();
        for (i, row) in rows.into_iter().enumerate() {
            if !row.is_object() {
                errors.push((i, "entry is not a mapping".to_string()));
                continue;
            }
            match serde_json::from_value::<Ask>(row.clone()) {
                Ok(mut ask) => {
                    ask.row = row;
                    ledger.asks.push(ask);
                }
                Err(e) => errors.push((i, e.to_string())),
            }
        }
        Ok((ledger, errors))
    }
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
    use serde_json::json;

    fn ask(v: serde_json::Value) -> Ask {
        let mut a: Ask = serde_json::from_value(v.clone()).unwrap();
        a.row = v;
        a
    }

    #[test]
    fn state_matches_the_plugin_rules() {
        assert_eq!(state_of(&json!({"id":"a"})), AskState::Open);
        assert_eq!(state_of(&json!({"id":"a","answered_at":"x"})), AskState::Answered);
        assert_eq!(state_of(&json!({"id":"a","resolution":"done"})), AskState::Answered);
        assert_eq!(state_of(&json!({"id":"a","outcome":"merged"})), AskState::Answered);
        assert_eq!(state_of(&json!({"id":"a","answered":true})), AskState::Answered);
        assert_eq!(state_of(&json!({"id":"a","defaulted_at":"x"})), AskState::Answered);
        assert_eq!(state_of(&json!({"id":"a","status":"Resolved-but-lane-still-parked"})), AskState::Answered);
        assert_eq!(state_of(&json!({"id":"a","status":"withdrawn"})), AskState::Answered);
        assert_eq!(state_of(&json!({"id":"a","archived_at":"x","answer":"y"})), AskState::Archived);
        assert_eq!(state_of(&json!({"id":"a","status":"archived-by-owner"})), AskState::Archived);
        assert_eq!(state_of(&json!({"id":"a","answer":"   "})), AskState::Open, "blank text is no answer");
    }

    #[test]
    fn rungs_are_distinct_and_prefix_parsed() {
        let a = ask(json!({"id":"a","exhausted":["read: x",{"read":"y"},"default: z","nope",{"standing-grant":"w"}]}));
        assert_eq!(a.rungs(), vec![Rung::Read, Rung::Default]);
    }

    #[test]
    fn hash_covers_the_whole_entry_in_key_order() {
        let a = json!({"id":"a","ask":"q?","nested":{"z":1,"b":[{"y":2,"x":1}]}});
        let reordered = json!({"nested":{"b":[{"x":1,"y":2}],"z":1},"ask":"q?","id":"a"});
        assert_eq!(entry_hash(&a), entry_hash(&reordered));
        assert_ne!(entry_hash(&a), entry_hash(&json!({"id":"a","ask":"q?","nested":{"z":1,"b":[]}})));
        let answered = json!({"id":"a","ask":"q?","resolution":"x","nested":{"z":1,"b":[{"y":2,"x":1}]}});
        assert_ne!(entry_hash(&a), entry_hash(&answered), "an answered entry is not the one you read");
        assert_eq!(entry_hash(&a).len(), 16);
    }

    #[test]
    fn hash_reads_the_raw_row_not_typed_defaults() {
        // `blocking` defaults to false in the typed copy; the raw row never had it.
        let a = ask(json!({"id":"a"}));
        assert_eq!(a.hash(), entry_hash(&json!({"id":"a"})));
    }

    #[test]
    fn ledger_reads_open_asks_and_keeps_bad_entries_out() {
        let (l, errors) =
            AskLedger::from_value(json!({"arc":"x","open_asks":[{"id":"a"}, "oops", {"no_id": true}]})).unwrap();
        assert_eq!(l.asks.len(), 1);
        assert_eq!(l.asks[0].row, json!({"id":"a"}));
        assert_eq!(errors.iter().map(|(i, _)| *i).collect::<Vec<_>>(), [1, 2]);
        let (l, _) = AskLedger::from_value(json!({"asks":[{"id":"b"}],"open_asks":[{"id":"c"}]})).unwrap();
        assert_eq!(l.asks[0].id, "b", "asks wins when both exist");
    }

    #[test]
    fn unknown_keys_survive_a_round_trip() {
        let a = ask(json!({"id":"a","liveness":{"kind":"pr-open","ref":"x#1"}}));
        assert_eq!(serde_json::to_value(&a).unwrap()["liveness"]["kind"], "pr-open");
    }

    #[test]
    fn unknown_class_is_kept() {
        assert_eq!(ask(json!({"id":"a","class":"novel"})).class, Some(AskClass::Other("novel".into())));
    }
}
