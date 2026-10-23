//! The delegation and economy ports against the Python's own output.
//!
//! `fixtures/delegation/golden.json` is what the workspace's `scripts/delegation.py` and
//! `scripts/fleet_economy.py` return on each case, written by `scripts/parity-delegation.py
//! --write` (which also fails when the golden is not the Python's output). This test evaluates
//! every case with the Rust port and requires the same result: equal JSON of equal types, the
//! same message for an error the Python raises as `ValueError`, and a crash of the same exception
//! kind where it crashes.

use maestro_core::parity::eval;
use serde_json::Value;

/// Equal values of equal JSON types: an integer is not a float.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            x.is_f64() == y.is_f64() && (x.as_i64(), x.as_u64(), x.as_f64()) == (y.as_i64(), y.as_u64(), y.as_f64())
        }
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

/// A crash is `{"crash": <Python exception type>}` on both sides, so it is compared like any value.
fn agrees(want: &Value, got: &Value) -> bool {
    same(want, got)
}

fn golden() -> Vec<Value> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/delegation/golden.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("the golden")).expect("golden JSON")
}

/// Every case of one function agrees with the Python. Split by function, so a mutant names the
/// predicate whose cases it broke.
fn check(function: &str) {
    let cases: Vec<Value> = golden().into_iter().filter(|c| c["fn"] == function).collect();
    assert!(!cases.is_empty(), "no golden cases for {function}");
    let bad: Vec<String> = cases
        .iter()
        .filter_map(|c| {
            let got = eval(c);
            (!agrees(&c["want"], &got)).then(|| format!("{}\n  python: {}\n  rust:   {}", c["args"], c["want"], got))
        })
        .collect();
    assert!(
        bad.is_empty(),
        "{} of {} {function} cases differ from the Python:\n{}",
        bad.len(),
        cases.len(),
        bad[..bad.len().min(5)].join("\n")
    );
}

#[test]
fn assess_matches_the_python() {
    check("assess");
}

#[test]
fn graph_validation_matches_the_python() {
    check("validate");
}

#[test]
fn graph_validation_on_disk_matches_the_python() {
    check("validate_disk");
}

#[test]
fn briefs_match_the_python() {
    check("brief");
}

#[test]
fn path_rules_match_the_python() {
    for f in ["rule_matches", "is_governance", "is_test_path", "normpath", "require_human_paths"] {
        check(f);
    }
}

#[test]
fn capacity_matches_the_python() {
    check("capacity");
}

#[test]
fn resume_gate_matches_the_python() {
    check("resume_gate");
}

#[test]
fn gate_bounds_match_the_python() {
    check("gate_bounds");
}

#[test]
fn timestamps_match_the_python() {
    check("parse_ts");
    check("add_hours");
}

#[test]
fn session_gates_and_tier_signals_match_the_python() {
    check("session_gates");
    check("tier_signals");
}

#[test]
fn the_golden_covers_every_rung() {
    // A golden that lost its variety would pass trivially: pin what it exercises.
    let g = golden();
    let ok = |f: &str, p: &dyn Fn(&Value) -> bool| g.iter().filter(|c| c["fn"] == f).any(|c| p(&c["want"]["ok"]));
    for tier in ["mechanical", "standard", "frontier", "apex"] {
        assert!(ok("assess", &|a| a["executor"] == tier), "no assess case reaches {tier}");
    }
    for c in ["trivial", "bounded", "substantive", "governance"] {
        assert!(ok("assess", &|a| a["complexity"] == c), "no assess case is {c}");
    }
    for m in ["normal", "conserve", "critical", "read_only"] {
        assert!(ok("capacity", &|a| a["cadence"]["mode"] == m), "no capacity case is {m}");
    }
    for b in ["measured", "window_average", "insufficient_samples", "no_burn"] {
        assert!(ok("capacity", &|a| a["basis"] == b), "no capacity case has basis {b}");
    }
    assert!(ok("validate", &|v| v["errors"].as_array().is_some_and(Vec::is_empty)), "no valid graph");
}

/// Every JSON input of the golden the CLI reads, Python's `json.loads` reads to the same value,
/// types strict (`scripts/parity-delegation.py`, `json_differential`); where Python raises or reads
/// NaN or an infinity, the CLI refuses.
#[test]
fn the_cli_reads_json_as_python_or_refuses() {
    let golden: Vec<Value> =
        serde_json::from_str(include_str!("../../../fixtures/delegation/json-golden.json")).unwrap();
    fn same(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => x.is_f64() == y.is_f64() && x.as_f64() == y.as_f64(),
            (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
            (Value::Object(x), Value::Object(y)) => {
                x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
            }
            _ => a == b,
        }
    }
    let (mut read, mut bad) = (0, Vec::new());
    for g in &golden {
        let text = g["text"].as_str().unwrap();
        let Ok(v) = maestro_core::schema::parse_json(text) else { continue };
        read += 1;
        if !g["python"].get("value").is_some_and(|w| same(w, &v)) {
            bad.push(format!("{text:?}: python {}, rust {v}", g["python"]));
        }
    }
    assert!(golden.len() > 3000 && read > 500, "{read} of {} read", golden.len());
    assert!(bad.is_empty(), "{} misread:\n{}", bad.len(), bad[..bad.len().min(10)].join("\n"));
}
