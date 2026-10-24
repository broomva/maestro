//! An ask's v2 fields (summary, details, tradeoffs, sources) as the plugin reads them: a port of
//! `richOf` in maestro-paseo `server/rich-ask.ts` and its helpers in `shared/rich-ask.ts`. A value
//! the plugin drops (over its cap, the wrong type, a malformed source) is not shown here either.

use super::sources::{self, Source};
use maestro_core::ask::Ask;
use maestro_core::ask::js_string;
use maestro_core::js::JsValue;
use serde_json::Value;

const SUMMARY_MAX: usize = 700;
const DETAILS_MAX: usize = 8000;
const CELL_MAX: usize = 160;
const LABEL_MAX: usize = 60;
const TRADEOFF_ROWS_MAX: usize = 8;
/// `TRADEOFF_AXES`, in its order.
pub(crate) const AXES: [&str; 4] = ["cost", "risk", "time", "reversibility"];

/// The whitespace both readers agree on (`WS`): space, tab, newline, carriage return, form feed, vertical tab.
fn ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{0C}' | '\u{0B}')
}

/// `trimWs`
pub(crate) fn trim_ws(s: &str) -> &str {
    s.trim_matches(ws)
}

/// `flatLength`: trimmed, runs of whitespace as one, in code points.
fn flat_length(s: &str) -> usize {
    let mut n = 0;
    let mut in_run = false;
    for c in trim_ws(s).chars() {
        if ws(c) {
            if !in_run {
                n += 1;
            }
            in_run = true;
        } else {
            n += 1;
            in_run = false;
        }
    }
    n
}

/// `words(value, max)`: a string, trimmed, non-empty and at most `max` long, else nothing.
pub(crate) fn words(v: Option<&Value>, max: usize) -> Option<String> {
    let t = trim_ws(v?.as_str()?);
    (!t.is_empty() && flat_length(t) <= max).then(|| t.to_string())
}

/// One tradeoff row: for one option, or a free row with its own label; at least one cell.
pub(crate) struct Tradeoff {
    pub option: Option<String>,
    pub label: Option<String>,
    /// `(axis, text)` for each axis the row fills, in [`AXES`] order.
    pub cells: Vec<(&'static str, String)>,
}

pub(crate) struct Rich {
    pub summary: Option<String>,
    pub details: Option<String>,
    pub tradeoffs: Vec<Tradeoff>,
    pub sources: Vec<Source>,
}

pub(crate) fn of(ask: &Ask) -> Rich {
    let get = |k: &str| ask.extra.get(k);
    let details = get("details")
        .and_then(Value::as_str)
        .filter(|d| !trim_ws(d).is_empty())
        .map(|d| d.chars().take(DETAILS_MAX).collect());
    Rich {
        summary: words(get("summary"), SUMMARY_MAX),
        details,
        tradeoffs: tradeoffs(ask),
        sources: sources::of(get("sources")),
    }
}

/// `tradeoffsOf`: rows naming exactly one of an option of this ask or a free label, with at
/// least one cell; at most eight.
fn tradeoffs(ask: &Ask) -> Vec<Tradeoff> {
    // From the row as read (JavaScript values), not its JSON copy, where `.inf` becomes null.
    let text = |v: Option<&JsValue>, max| words(v.map(JsValue::to_json).as_ref(), max);
    ask.row
        .get("tradeoffs")
        .and_then(JsValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(JsValue::as_object)
        .filter_map(|row| {
            // `String(entry.option)` for a string or a number, as written: `.inf` is `Infinity`.
            let option = match row.get("option") {
                Some(JsValue::Str(s)) => Some(s.clone()),
                Some(JsValue::Num(x)) => Some(js_string(*x)),
                _ => None,
            };
            let label = text(row.get("label"), LABEL_MAX);
            if option.is_none() == label.is_none() {
                return None;
            }
            if option.as_ref().is_some_and(|o| !ask.options.iter().any(|x| &x.id == o)) {
                return None;
            }
            let cells: Vec<_> = AXES.iter().filter_map(|axis| Some((*axis, text(row.get(*axis), CELL_MAX)?))).collect();
            (!cells.is_empty()).then_some(Tradeoff { option, label, cells })
        })
        .take(TRADEOFF_ROWS_MAX)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn words_trims_and_caps_as_the_plugin_measures() {
        assert_eq!(words(Some(&json!("  a  b \n")), 3).as_deref(), Some("a  b"));
        assert_eq!(words(Some(&json!("ab cd")), 4), None);
        assert_eq!(words(Some(&json!("   ")), 10), None);
        assert_eq!(words(Some(&json!(5)), 10), None, "a number is not a cell");
        assert_eq!(flat_length("a\u{00a0}b"), 3, "a no-break space is not whitespace to either reader");
        assert_eq!(flat_length("🙂🙂"), 2, "code points, not UTF-16 units");
    }

    #[test]
    fn a_non_finite_option_reads_as_javascript_writes_it() {
        let mut ask: Ask = serde_json::from_value(json!({
            "id": "a", "options": [{ "id": "Infinity", "label": "Inf" }],
            "tradeoffs": [{ "option": 0, "cost": "c" }]
        }))
        .unwrap();
        // YAML's `.inf`, as the reader holds it.
        let rows = ask.row.as_object_mut().unwrap().get_mut("tradeoffs").unwrap();
        let JsValue::Arr(rows) = rows else { panic!("tradeoffs is a list") };
        let JsValue::Obj(row) = &mut rows[0] else { panic!("a row is a mapping") };
        row.insert("option".into(), JsValue::Num(f64::INFINITY));
        let got = of(&ask).tradeoffs;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].option.as_deref(), Some("Infinity"));
    }

    #[test]
    fn caps_follow_rich_of() {
        let ask = |extra: serde_json::Value| -> Ask {
            let mut row = json!({ "id": "a", "options": [{ "id": "x", "label": "X" }] });
            row.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            serde_json::from_value(row).unwrap()
        };
        assert!(of(&ask(json!({ "summary": "s".repeat(700) }))).summary.is_some());
        assert!(of(&ask(json!({ "summary": "s".repeat(701) }))).summary.is_none());
        let d = of(&ask(json!({ "details": "d".repeat(8001) }))).details.unwrap();
        assert_eq!(d.chars().count(), 8000);
        let rows: Vec<_> = (0..9).map(|i| json!({ "option": "x", "cost": format!("c{i}") })).collect();
        assert_eq!(of(&ask(json!({ "tradeoffs": rows }))).tradeoffs.len(), 8);
    }

    #[test]
    fn tradeoff_rows_follow_tradeoffs_of() {
        let ask: Ask = serde_json::from_value(json!({
            "id": "a",
            "options": [{ "id": "x", "label": "X" }, { "id": "7", "label": "Seven" }],
            "tradeoffs": [
                { "option": "x", "cost": "c" },
                { "option": 7, "risk": "r" },
                { "option": 7.0, "time": "t7" },
                { "option": "nope", "cost": "c" },
                { "option": "x", "label": "both", "cost": "c" },
                { "label": "free", "time": "t" },
                { "label": "x".repeat(61), "time": "too long a label" },
                { "label": "no cells" },
                { "option": "x", "cost": 5 },
                { "option": "x", "cost": "x".repeat(161) }
            ]
        }))
        .unwrap();
        let rows = of(&ask).tradeoffs;
        let got: Vec<_> = rows.iter().map(|r| (r.option.clone(), r.label.clone(), r.cells.clone())).collect();
        assert_eq!(
            got,
            vec![
                (Some("x".into()), None, vec![("cost", "c".into())]),
                (Some("7".into()), None, vec![("risk", "r".into())]),
                (Some("7".into()), None, vec![("time", "t7".into())]),
                (None, Some("free".into()), vec![("time", "t".into())]),
            ]
        );
    }
}
