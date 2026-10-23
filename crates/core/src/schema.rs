//! The inputs the delegation and economy ports read exactly: a typed schema per input.
//!
//! The Python reads whatever a file holds, and what it does with an unexpected type is Python's
//! semantics on arbitrary input (hash equality of `1` and `True`, `str.strip`'s whitespace, `int()`
//! of any digit string). Parity is claimed inside this schema only. A value outside it is refused
//! before any rule runs, with the field named ([`Fault::Refused`]); the parity harness holds the
//! refusals to its own reading of the same schema (`scripts/parity-delegation.py`, `refusal`).
//!
//! The schema is what the real inputs hold: every `docs/plans/*.graph.yaml`, and
//! `provider_manager.py --json usage` / `--json state` and the coordinator's sample cache. A field
//! not named here is not read by the ports and is not checked. Mappings with arbitrary keys are
//! walked in sorted key order, lists in order, named fields in the order listed here, so the first
//! refusal is the same one in both readers.

use crate::delegation::Fault;
use serde_json::{Map, Value};

/// The words every refusal ends with.
pub const OUTSIDE: &str = "outside the schema Maestro reads exactly";

/// The shape a field may have. Every field may be absent; `null` is allowed where listed.
#[derive(Clone, Copy)]
enum Ty {
    /// A string or null.
    Str,
    /// A JSON integer within i64 (not a bool, not a float) or null.
    Int,
    /// A JSON number (an integer or a float, not a bool) or null.
    Num,
    /// A JSON number, present only as a number (null refused: the Python's `float(None)`).
    NumOnly,
    /// A bool or null.
    Bool,
    /// A string, a number (not a bool) or null: what `parse_ts` reads as a timestamp.
    Time,
    /// A list of strings or null.
    Strs,
}

fn refuse(field: &str, want: &str, got: &Value) -> Fault {
    let got = match got {
        Value::Null => "null",
        Value::Bool(_) => "a bool",
        Value::Number(n) if n.is_f64() => "a float",
        Value::Number(_) => "an integer",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "a mapping",
    };
    Fault::Refused { field: field.to_string(), why: format!("{want}, not {got}") }
}

fn check(field: &str, v: &Value, ty: Ty) -> Result<(), Fault> {
    let ok = match (ty, v) {
        (Ty::NumOnly, Value::Null) => false,
        (_, Value::Null) => true,
        (Ty::Str, Value::String(_)) => true,
        (Ty::Int, Value::Number(n)) => n.is_i64(),
        (Ty::Num | Ty::NumOnly | Ty::Time, Value::Number(_)) => true,
        (Ty::Bool, Value::Bool(_)) => true,
        (Ty::Time, Value::String(_)) => true,
        (Ty::Strs, Value::Array(a)) => {
            for (i, x) in a.iter().enumerate() {
                if !x.is_string() {
                    return Err(refuse(&format!("{field}[{i}]"), "a string", x));
                }
            }
            true
        }
        _ => false,
    };
    if ok {
        return Ok(());
    }
    let want = match ty {
        Ty::Str => "a string",
        Ty::Int => "an integer within 64 bits",
        Ty::Num | Ty::NumOnly => "a number",
        Ty::Bool => "a bool",
        Ty::Time => "a timestamp string or epoch number",
        Ty::Strs => "a list of strings",
    };
    Err(refuse(field, want, v))
}

/// The named fields of a mapping, in the order given.
fn fields(at: &str, m: &Map<String, Value>, spec: &[(&str, Ty)]) -> Result<(), Fault> {
    for (k, ty) in spec {
        if let Some(v) = m.get(*k) {
            check(&format!("{at}.{k}"), v, *ty)?;
        }
    }
    Ok(())
}

/// A mapping or null at `field`; `None` for null or absent.
fn mapping<'a>(field: &str, v: Option<&'a Value>) -> Result<Option<&'a Map<String, Value>>, Fault> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(m)) => Ok(Some(m)),
        Some(other) => Err(refuse(field, "a mapping", other)),
    }
}

/// A list or null at `field`.
fn list<'a>(field: &str, v: Option<&'a Value>) -> Result<&'a [Value], Fault> {
    match v {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(a)) => Ok(a),
        Some(other) => Err(refuse(field, "a list", other)),
    }
}

const SIGNALS: [(&str, Ty); 12] = [
    ("kind", Ty::Str),
    ("paths", Ty::Strs),
    ("est_loc", Ty::Int),
    ("files", Ty::Int),
    ("repos", Ty::Int),
    ("public_api", Ty::Bool),
    ("one_way_door", Ty::Bool),
    ("spec", Ty::Str),
    ("failed_attempts", Ty::Int),
    ("observed_tier", Ty::Str),
    ("allow_apex", Ty::Bool),
    ("mode", Ty::Str),
];

const NODE: [(&str, Ty); 12] = [
    ("id", Ty::Str),
    ("kind", Ty::Str),
    ("repo", Ty::Str),
    ("done", Ty::Str),
    ("tier", Ty::Str),
    ("verify", Ty::Str),
    ("thinking", Ty::Str),
    ("spawn_default", Ty::Str),
    ("reason", Ty::Str),
    ("inputs", Ty::Strs),
    ("produces", Ty::Strs),
    ("depends_on", Ty::Strs),
];

/// `assess`'s signals, at `at` (`signals`, or a node's `nodes[i].signals`).
pub fn signals(at: &str, sig: &Map<String, Value>) -> Result<(), Fault> {
    fields(at, sig, &SIGNALS)
}

/// A plan graph: `repo`, `repos` (name -> directory string or `{path, policy}`), and `nodes`, each
/// a mapping of string fields, string lists and `signals`.
pub fn graph(g: &Value) -> Result<(), Fault> {
    let Some(g) = mapping("graph", Some(g))? else {
        return Err(refuse("graph", "a mapping", &Value::Null));
    };
    fields("graph", g, &[("repo", Ty::Str)])?;
    if let Some(repos) = mapping("graph.repos", g.get("repos"))? {
        for (name, r) in repos {
            let at = format!("graph.repos.{name}");
            match r {
                Value::Null | Value::String(_) => {}
                Value::Object(m) => fields(&at, m, &[("path", Ty::Str), ("policy", Ty::Str)])?,
                other => return Err(refuse(&at, "a directory string or a mapping", other)),
            }
        }
    }
    for (i, n) in list("graph.nodes", g.get("nodes"))?.iter().enumerate() {
        let at = format!("graph.nodes[{i}]");
        let Value::Object(n) = n else { return Err(refuse(&at, "a mapping", n)) };
        fields(&at, n, &NODE)?;
        let at = format!("{at}.signals");
        if let Some(sig) = mapping(&at, n.get("signals"))? {
            signals(&at, sig)?;
        }
    }
    Ok(())
}

/// A repo's `.control/policy.yaml`: `auto_merge.rules`, each a mapping with string `action` and
/// `path_touched`.
pub fn policy(p: &Value) -> Result<(), Fault> {
    let Some(p) = mapping("policy", Some(p))? else { return Ok(()) };
    let Some(am) = mapping("policy.auto_merge", p.get("auto_merge"))? else { return Ok(()) };
    for (i, r) in list("policy.auto_merge.rules", am.get("rules"))?.iter().enumerate() {
        let at = format!("policy.auto_merge.rules[{i}]");
        let Value::Object(r) = r else { return Err(refuse(&at, "a mapping", r)) };
        fields(&at, r, &[("action", Ty::Str), ("path_touched", Ty::Str)])?;
    }
    Ok(())
}

/// `provider_manager.py --json usage` rows.
pub fn usage(rows: &[Value]) -> Result<(), Fault> {
    for (i, r) in rows.iter().enumerate() {
        let at = format!("usage[{i}]");
        let Value::Object(r) = r else { return Err(refuse(&at, "a mapping", r)) };
        fields(
            &at,
            r,
            &[
                ("id", Ty::Str),
                ("email", Ty::Str),
                ("fiveHourUtil", Ty::Num),
                ("sevenDayUtil", Ty::Num),
                ("fiveHourResetsAt", Ty::Time),
                ("sevenDayResetsAt", Ty::Time),
                ("numbersAgeSeconds", Ty::Num),
                ("isActive", Ty::Bool),
                ("isRateLimited", Ty::Bool),
                ("stale", Ty::Bool),
                ("needsLogin", Ty::Bool),
                ("hasStoredCredentials", Ty::Bool),
            ],
        )?;
    }
    Ok(())
}

/// The coordinator's sample cache lines: `{at, accounts: {name: {5h, 7d, reset_7d, ...}}}`.
pub fn samples(samples: &[Value]) -> Result<(), Fault> {
    for (i, s) in samples.iter().enumerate() {
        let at = format!("samples[{i}]");
        let Value::Object(s) = s else { return Err(refuse(&at, "a mapping", s)) };
        fields(&at, s, &[("at", Ty::Time)])?;
        let Some(accounts) = mapping(&format!("{at}.accounts"), s.get("accounts"))? else { continue };
        for (name, a) in accounts {
            let at = format!("{at}.accounts.{name}");
            let Some(a) = mapping(&at, Some(a))? else { continue };
            fields(
                &at,
                a,
                &[
                    ("5h", Ty::Num),
                    ("7d", Ty::Num),
                    ("reset_5h", Ty::Time),
                    ("reset_7d", Ty::Time),
                    ("age_s", Ty::Num),
                    ("active", Ty::Bool),
                    ("limited", Ty::Bool),
                    ("stale", Ty::Bool),
                    ("needs_login", Ty::Bool),
                ],
            )?;
        }
    }
    Ok(())
}

/// `provider_manager.py --json state`: the fields `gate_bounds` reads.
pub fn state(st: &Map<String, Value>) -> Result<(), Fault> {
    if let Some(vg) = mapping("state.versionGate", st.get("versionGate"))? {
        fields("state.versionGate", vg, &[("automaticSwitching", Ty::Str)])?;
    }
    if let Some(cfg) = mapping("state.config", st.get("config"))? {
        fields(
            "state.config",
            cfg,
            &[
                ("autoBalance", Ty::Bool),
                ("staleNumbersMinutes", Ty::NumOnly),
                ("standbyMax", Ty::NumOnly),
                ("standbyWeeklyMax", Ty::NumOnly),
            ],
        )?;
    }
    fields("state", st, &[("holdUntil", Ty::Time)])?;
    for (i, p) in list("state.pendingSignals", st.get("pendingSignals"))?.iter().enumerate() {
        let at = format!("state.pendingSignals[{i}]");
        let Value::Object(p) = p else { return Err(refuse(&at, "a mapping", p)) };
        fields(&at, p, &[("at", Ty::Num)])?;
    }
    if let Some(probes) = mapping("state.probes", st.get("probes"))? {
        for (id, p) in probes {
            let at = format!("state.probes.{id}");
            let Value::Object(p) = p else { return Err(refuse(&at, "a mapping", p)) };
            fields(&at, p, &[("result", Ty::Str), ("at", Ty::Num)])?;
        }
    }
    Ok(())
}

/// Maestro's economy.json: `{tiers: {tier: {runs, approved, firstPass, ...}}}`.
pub fn tiers(data: &Value) -> Result<(), Fault> {
    let Some(data) = mapping("data", Some(data))? else { return Ok(()) };
    let Some(tiers) = mapping("data.tiers", data.get("tiers"))? else { return Ok(()) };
    for (t, v) in tiers {
        let at = format!("data.tiers.{t}");
        let Some(v) = mapping(&at, Some(v))? else { continue };
        fields(
            &at,
            v,
            &[
                ("runs", Ty::Int),
                ("raisedRuns", Ty::Int),
                ("approved", Ty::Int),
                ("raises", Ty::Int),
                ("escalations", Ty::Int),
                ("unmeasured", Ty::Int),
                ("firstPass", Ty::Num),
                ("tokensPerApproved", Ty::Num),
            ],
        )?;
    }
    Ok(())
}

/// `json.loads`, refusing an integer literal no 64-bit integer holds: serde_json reads one as a
/// float where Python keeps an int, so its type and every message naming it would differ.
pub fn parse_json(text: &str) -> Result<Value, String> {
    if let Some(lit) = wide_integer(text) {
        return Err(format!("the integer {lit} does not fit 64 bits ({OUTSIDE})"));
    }
    serde_json::from_str(text).map_err(|e| e.to_string())
}

/// The first integer literal (no fraction, no exponent) outside i64 and u64, outside strings.
fn wide_integer(text: &str) -> Option<&str> {
    let b = text.as_bytes();
    let (mut i, mut in_str) = (0, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            match c {
                b'\\' => i += 1,
                b'"' => in_str = false,
                _ => {}
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_str = true;
            i += 1;
            continue;
        }
        if c == b'-' || c.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            let float = i < b.len() && matches!(b[i], b'.' | b'e' | b'E');
            while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'.' | b'+' | b'-')) {
                i += 1;
            }
            let lit = &text[start..i];
            if !float && lit.parse::<i64>().is_err() && lit.parse::<u64>().is_err() && lit != "-" {
                return Some(lit);
            }
            continue;
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn field(r: Result<(), Fault>) -> String {
        match r {
            Err(Fault::Refused { field, .. }) => field,
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn the_real_shapes_are_inside() {
        let g = json!({"repo": "w", "repos": {"w": ".", "b": {"path": "../b", "policy": "x"}}, "nodes": [
            {"id": "a", "done": "true", "inputs": ["x"], "signals": {"est_loc": 10, "public_api": true, "paths": ["a.rs"]}}]});
        assert!(graph(&g).is_ok());
        assert!(usage(&[json!({"fiveHourUtil": 1.5, "sevenDayResetsAt": null, "numbersAgeSeconds": 3})]).is_ok());
        assert!(
            state(json!({"holdUntil": 1.5, "pendingSignals": [], "probes": {"x": {"at": 1.0}}}).as_object().unwrap())
                .is_ok()
        );
    }

    #[test]
    fn outside_is_refused_with_the_field_named() {
        let g = |n: Value| json!({"repo": "w", "nodes": [{"id": "a"}, n]});
        assert_eq!(field(graph(&g(json!({"id": 1})))), "graph.nodes[1].id");
        assert_eq!(field(graph(&g(json!({"id": true})))), "graph.nodes[1].id");
        assert_eq!(field(graph(&g(json!({"id": "b", "done": ["x"]})))), "graph.nodes[1].done");
        assert_eq!(field(graph(&g(json!({"id": "b", "depends_on": {"a": 1}})))), "graph.nodes[1].depends_on");
        assert_eq!(field(graph(&g(json!({"id": "b", "depends_on": ["a", 1]})))), "graph.nodes[1].depends_on[1]");
        assert_eq!(
            field(graph(&g(json!({"id": "b", "signals": {"est_loc": "300"}})))),
            "graph.nodes[1].signals.est_loc"
        );
        assert_eq!(field(graph(&g(json!({"id": "b", "signals": {"est_loc": 3.0}})))), "graph.nodes[1].signals.est_loc");
        assert_eq!(field(graph(&g(json!({"id": "b", "signals": {"files": true}})))), "graph.nodes[1].signals.files");
        assert_eq!(field(graph(&g(json!({"id": "b", "signals": {"mode": ["x"]}})))), "graph.nodes[1].signals.mode");
        assert_eq!(field(graph(&g(json!("x")))), "graph.nodes[1]");
        assert_eq!(field(graph(&json!({"repo": 7, "nodes": []}))), "graph.repo");
        assert_eq!(field(graph(&json!({"repos": ["w"]}))), "graph.repos");
        assert_eq!(field(usage(&[json!({"fiveHourUtil": true})])), "usage[0].fiveHourUtil");
        assert_eq!(field(usage(&[json!({"isActive": 1})])), "usage[0].isActive");
        assert_eq!(field(samples(&[json!({"accounts": {"a": {"7d": "8"}}})])), "samples[0].accounts.a.7d");
        assert_eq!(
            field(state(json!({"config": {"standbyMax": null}}).as_object().unwrap())),
            "state.config.standbyMax"
        );
        assert_eq!(field(tiers(&json!({"tiers": {"standard": {"runs": "10"}}}))), "data.tiers.standard.runs");
    }

    #[test]
    fn a_wide_integer_literal_is_refused() {
        assert!(parse_json("[100000000000000000000]").is_err());
        assert!(parse_json("[-9223372036854775809]").is_err());
        assert!(parse_json("[18446744073709551615, -9223372036854775808, 1e300, \"100000000000000000000\"]").is_ok());
        assert!(parse_json(r#"{"a\"1": 100000000000000000000.0}"#).is_ok());
    }
}
