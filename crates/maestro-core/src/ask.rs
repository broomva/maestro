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

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone, Default, Serialize)]
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

/// One ask as Maestro reports it. Built from the raw row by [`Ask::from_row`],
/// which reads every field the way the plugin does, so a value the plugin
/// tolerates never drops an entry here.
#[derive(Debug, Clone, Serialize)]
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

/// The plugin's `text()`: a trimmed non-empty string, or a number or bool as
/// text. A number reads as JavaScript's `String(n)` would print it.
pub fn text(value: Option<&serde_json::Value>) -> Option<String> {
    match value? {
        serde_json::Value::String(s) => Some(s.trim()).filter(|s| !s.is_empty()).map(str::to_string),
        serde_json::Value::Number(n) => Some(js_number(n)),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// The plugin's `textsOf`: the texts of a list, a lone value being a list of one.
pub fn texts_of(value: Option<&serde_json::Value>) -> Vec<String> {
    match value {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(items)) => items.iter().filter_map(|v| text(Some(v))).collect(),
        Some(one) => text(Some(one)).into_iter().collect(),
    }
}

/// The plugin's `optionsOf`: mapping items with a text `id`; the label defaults to the id.
pub fn options_of(value: Option<&serde_json::Value>) -> Vec<AskOption> {
    const KNOWN: [&str; 6] = ["id", "label", "consequence", "reversible", "recommended", "status_quo"];
    let Some(serde_json::Value::Array(items)) = value else { return Vec::new() };
    items
        .iter()
        .filter_map(|item| {
            let map = item.as_object()?;
            let id = text(map.get("id"))?;
            Some(AskOption {
                label: text(map.get("label")).unwrap_or_else(|| id.clone()),
                id,
                consequence: text(map.get("consequence")),
                reversible: map.get("reversible").and_then(serde_json::Value::as_bool),
                recommended: map.get("recommended") == Some(&serde_json::Value::Bool(true)),
                status_quo: map.get("status_quo") == Some(&serde_json::Value::Bool(true)),
                extra: rest(map, &KNOWN),
            })
        })
        .collect()
}

/// The keys of `map` not in `known`, kept so nothing unread is lost.
fn rest(map: &serde_json::Map<String, serde_json::Value>, known: &[&str]) -> Extra {
    map.iter().filter(|(k, _)| !known.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// A flag the plugin reads as `=== true`: anything but the boolean true is false.
fn is_true(value: Option<&serde_json::Value>) -> bool {
    value == Some(&serde_json::Value::Bool(true))
}

/// A string-valued enum read from text: a known name, else kept verbatim.
fn named<T: serde::de::DeserializeOwned>(value: Option<&serde_json::Value>) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(text(value)?)).ok()
}

/// JavaScript's `Number.prototype.toString` (and so `JSON.stringify`) for a JSON number.
/// Integers past 2^53 are doubles in JavaScript, so they print as the double they round to.
pub fn js_number(n: &serde_json::Number) -> String {
    const SAFE: u64 = 1 << 53;
    if let Some(i) = n.as_i64() {
        if i.unsigned_abs() <= SAFE {
            return i.to_string();
        }
    } else if let Some(u) = n.as_u64()
        && u <= SAFE
    {
        return u.to_string();
    }
    js_double(n.as_f64().unwrap_or(f64::NAN))
}

/// ECMA-262 Number::toString(x) for a finite double: the shortest round-trip
/// digits, in decimal for exponents in [-7, 21) and exponential outside it.
fn js_double(x: f64) -> String {
    if x == 0.0 {
        return "0".into(); // -0 too
    }
    if !x.is_finite() {
        return "null".into(); // JSON.stringify's spelling; JSON numbers cannot hold these
    }
    let sci = format!("{:e}", x.abs()); // shortest digits: `d[.ddd]e<exp>`
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let k = digits.len() as i32;
    let n = exp.parse::<i32>().unwrap_or(0) + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let tail = if k > 1 { format!(".{}", &digits[1..]) } else { String::new() };
        let e = n - 1;
        format!("{}{tail}e{}{}", &digits[..1], if e < 0 { '-' } else { '+' }, e.abs())
    };
    if x < 0.0 { format!("-{body}") } else { body }
}

/// A key JavaScript treats as an array index (`"0"`, `"7"`, not `"07"`): such
/// keys come first in an object, in numeric order, whatever order they were set in.
fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    key.parse::<u32>().ok().filter(|&i| i != u32::MAX)
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

/// `JSON.stringify(canonical(row))` byte for byte. `canonical` sorts keys as
/// JavaScript strings sort (UTF-16 code units), and the object it rebuilds puts
/// array-index keys first in numeric order; numbers print as JavaScript prints them.
fn canonical_json(value: &serde_json::Value, out: &mut String) {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| match (array_index(a), array_index(b)) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.encode_utf16().cmp(b.encode_utf16()),
            });
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
        Value::Number(n) => out.push_str(&js_number(n)),
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
    const KNOWN: [&'static str; 25] = [
        "id",
        "uid",
        "ask",
        "headline",
        "question",
        "class",
        "gates",
        "exhausted",
        "blocking",
        "preanswerable",
        "default",
        "no_default_because",
        "stakes",
        "why_you",
        "options",
        "project",
        "topics",
        "asked_at",
        "origin",
        "answer",
        "answer_option",
        "answer_note",
        "answered_at",
        "answered_by",
        "status",
    ];

    /// Read one ledger entry as the plugin's `decisionOf` reads it: every field
    /// through `text`/`textsOf`/`optionsOf`, flags as `=== true`. The only entry
    /// refused is one the plugin also drops: not a mapping, or without a text `id`.
    pub fn from_row(row: serde_json::Value) -> Result<Ask, String> {
        let map = row.as_object().ok_or("entry is not a mapping")?;
        let id = text(map.get("id")).ok_or("entry has no id")?;
        let t = |k: &str| text(map.get(k));
        let origin = map.get("origin").and_then(serde_json::Value::as_object).map(|o| AskOrigin {
            agent_id: text(o.get("agent_id")),
            provider: text(o.get("provider")),
            cwd: text(o.get("cwd")),
            session: text(o.get("session")),
            extra: rest(o, &["agent_id", "provider", "cwd", "session"]),
        });
        let exhausted = match map.get("exhausted") {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::Array(items)) => items.clone(),
            Some(one) => vec![one.clone()],
        };
        Ok(Ask {
            id,
            uid: t("uid"),
            ask: t("ask"),
            headline: t("headline"),
            question: t("question"),
            class: named(map.get("class")),
            gates: texts_of(map.get("gates")),
            exhausted,
            blocking: is_true(map.get("blocking")),
            preanswerable: is_true(map.get("preanswerable")),
            default: t("default"),
            no_default_because: named(map.get("no_default_because")),
            stakes: t("stakes"),
            why_you: t("why_you"),
            options: options_of(map.get("options")),
            project: t("project"),
            topics: texts_of(map.get("topics")),
            asked_at: t("asked_at"),
            origin,
            answer: t("answer"),
            answer_option: t("answer_option"),
            answer_note: t("answer_note"),
            answered_at: t("answered_at"),
            answered_by: t("answered_by"),
            status: t("status"),
            extra: rest(map, &Self::KNOWN),
            row,
        })
    }

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

impl<'de> Deserialize<'de> for Ask {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ask::from_row(serde_json::Value::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Default, Serialize)]
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
        // The ledger's own fields read leniently too: a stray type there must not hide every ask.
        let mut ledger = AskLedger {
            arc: text(map.get("arc")),
            ticket: text(map.get("ticket")),
            pr: map.get("pr").filter(|v| !v.is_null()).cloned(),
            tz: text(map.get("tz")),
            opened: text(map.get("opened")),
            lanes: texts_of(map.get("lanes")),
            asks: Vec::new(),
            extra: rest(map, &["arc", "ticket", "pr", "tz", "opened", "lanes"]),
        };
        let mut errors = Vec::new();
        for (i, row) in rows.into_iter().enumerate() {
            match Ask::from_row(row) {
                Ok(ask) => ledger.asks.push(ask),
                Err(e) => errors.push((i, e)),
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
        Ask::from_row(v).unwrap()
    }

    #[test]
    fn fields_read_as_the_plugin_reads_them() {
        // Every value here once failed the whole entry; the plugin keeps it.
        let a = ask(json!({
            "id": 42, "blocking": "true", "preanswerable": true, "gates": "lane-a",
            "options": [{"id": "go"}, {"label": "no id"}, "text", {"id": 7, "label": " ", "recommended": "yes"}],
            "answer": {"text": "a map"}, "status": 3, "exhausted": "read: looked",
        }));
        assert_eq!(a.id, "42");
        assert!(!a.blocking, "the plugin reads blocking as === true");
        assert!(a.preanswerable);
        assert_eq!(a.gates, ["lane-a"]);
        let opts: Vec<_> = a.options.iter().map(|o| (o.id.as_str(), o.label.as_str(), o.recommended)).collect();
        assert_eq!(opts, [("go", "go", false), ("7", "7", false)]);
        assert_eq!(a.answer, None, "a map is no text");
        assert_eq!(a.status.as_deref(), Some("3"));
        assert_eq!(a.rungs(), vec![Rung::Read]);
        assert_eq!(a.state(), AskState::Open, "state reads the raw row");
        assert!(Ask::from_row(json!({"id": [1]})).is_err());
        assert!(Ask::from_row(json!("x")).is_err());
    }

    #[test]
    fn ledger_fields_never_hide_the_asks() {
        let (l, errors) = AskLedger::from_value(json!({"arc": 9, "lanes": "one", "asks": [{"id": "a"}]})).unwrap();
        assert!(errors.is_empty());
        assert_eq!((l.arc.as_deref(), l.lanes.as_slice(), l.asks.len()), (Some("9"), &["one".to_string()][..], 1));
    }

    #[test]
    fn numbers_print_as_javascript_prints_them() {
        let cases = [
            (json!(2.0), "2"),
            (json!(1e3), "1000"),
            (json!(-0.0), "0"),
            (json!(0.5), "0.5"),
            (json!(1.5e-7), "1.5e-7"),
            (json!(0.000001), "0.000001"),
            (json!(1e21), "1e+21"),
            (json!(123456789012345680000.0), "123456789012345680000"),
            (json!(12), "12"),
            (json!(9007199254740993u64), "9007199254740992"),
            (json!(-9007199254740993i64), "-9007199254740992"),
        ];
        for (v, want) in cases {
            let mut out = String::new();
            canonical_json(&v, &mut out);
            assert_eq!(out, want, "{v}");
        }
        assert_eq!(text(Some(&json!(1.0))).as_deref(), Some("1"));
    }

    #[test]
    fn keys_order_as_a_rebuilt_javascript_object() {
        let mut out = String::new();
        canonical_json(&json!({"b": 1, "10": 2, "2": 3, "07": 4, "a": 5, "\u{ff61}": 6, "\u{1f600}": 7}), &mut out);
        // Index keys first, numerically; then UTF-16 order, where an astral char (surrogates 0xD83D…) sorts before U+FF61.
        assert_eq!(out, "{\"2\":3,\"10\":2,\"07\":4,\"a\":5,\"b\":1,\"\u{1f600}\":7,\"\u{ff61}\":6}");
    }

    #[test]
    fn hash_matches_the_plugin_on_a_known_row() {
        // node -e 'const c=v=>Array.isArray(v)?v.map(c):v&&typeof v==="object"?Object.fromEntries(Object.keys(v).sort().map(k=>[k,c(v[k])])):v;
        //   console.log(require("crypto").createHash("sha256").update(JSON.stringify(c(ROW))).digest("hex").slice(0,16))'
        let row = json!({"id": "a", "n": 2.0, "big": 1e3, "10": "x", "2": "y"});
        let mut s = String::new();
        canonical_json(&row, &mut s);
        assert_eq!(s, r#"{"2":"y","10":"x","big":1000,"id":"a","n":2}"#);
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
