//! One evaluator for the parity cases of the ported Python (`delegation`, `economy`).
//!
//! A case is `{"fn": name, "args": {...}}`; the result is `{"ok": value}`, `{"err": message}` for
//! an error the Python raises as `ValueError` (its message compared to the byte), or
//! `{"crash": kind}` for one it does not catch (the exception's type name, compared exactly).
//! `scripts/parity-delegation.py` runs the Python's functions on the same cases, writes
//! `fixtures/delegation/golden.json`, and runs this evaluator through `maestro parity-eval`;
//! `crates/core/tests/parity_delegation.rs` holds this evaluator to that golden.

use crate::delegation::{self as d, BriefNode, Disk, Fault, Ref, Tier};
use crate::economy as e;
use serde_json::{Map, Value, json};
use std::collections::HashSet;

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array).map(|a| a.iter().map(crate::py::str_of).collect()).unwrap_or_default()
}

fn s<'a>(args: &'a Value, k: &str) -> &'a str {
    args.get(k).and_then(Value::as_str).unwrap_or("")
}

fn fault(f: Fault) -> Value {
    match f {
        Fault::Value(m) => json!({"err": m}),
        Fault::Crash { kind, .. } => json!({"crash": kind}),
    }
}

fn ts(r: Result<Value, String>) -> Value {
    match r {
        Ok(v) => json!({"ok": v}),
        Err(m) => json!({"err": m}),
    }
}

pub fn eval(case: &Value) -> Value {
    let args = case.get("args").cloned().unwrap_or(Value::Null);
    let empty = Map::new();
    match case.get("fn").and_then(Value::as_str).unwrap_or("") {
        "assess" => {
            let sig = args.get("sig").and_then(Value::as_object).unwrap_or(&empty);
            match d::assess(sig, &strings(args.get("human_paths"))) {
                Ok(a) => json!({"ok": a}),
                Err(f) => fault(f),
            }
        }
        "validate" => match d::validate(&args["graph"], &strings(args.get("human_paths")), None, None, &HashSet::new())
        {
            Ok((errors, nodes)) => {
                json!({"ok": {"errors": errors, "nodes": nodes.iter().map(d::PlanNode::to_json).collect::<Vec<_>>()}})
            }
            Err(f) => fault(f),
        },
        // `validate` with files on disk (`exists`) and bound repo directories (`dirs`): the spec-exists
        // check, spec discovery on disk and the nested-repo refusal.
        "validate_disk" => {
            let files = strings(args.get("files"));
            let dirs: Vec<(String, String)> = args
                .get("dirs")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|p| (crate::py::str_of(&p[0]), crate::py::str_of(&p[1]))).collect())
                .unwrap_or_default();
            let human = strings(args.get("human_paths"));
            let exists = |r: &Ref| files.iter().any(|f| *f == format!("{}:{}", r.repo, r.path));
            let policy_for = |_: &str| Ok(human.clone());
            let disk = Disk { exists: &exists, policy_for: &policy_for };
            match d::validate(&args["graph"], &[], Some(&disk), Some(&dirs), &HashSet::new()) {
                Ok((errors, nodes)) => json!({"ok": {
                    "errors": errors, "nodes": nodes.iter().map(d::PlanNode::to_json).collect::<Vec<_>>()
                }}),
                Err(f) => fault(f),
            }
        }
        "add_hours" => {
            let now = e::parse_ts(args.get("now")).ok().flatten().unwrap_or(0);
            let hours = args.get("hours").and_then(Value::as_f64).unwrap_or(f64::NAN);
            json!({"ok": e::add_hours(now, hours).map(|t| t - now)})
        }
        "brief" => match d::validate(&args["graph"], &[], None, None, &HashSet::new()) {
            Ok((errors, _)) if !errors.is_empty() => json!({"err": errors.join("\n")}),
            Ok((_, nodes)) => {
                let Some(n) = nodes.iter().find(|n| crate::py::str_of(&n.id) == s(&args, "node")) else {
                    return json!({"err": "no node"});
                };
                let done = crate::py::str_of(&n.done);
                let brief = BriefNode {
                    tier: n.tier,
                    spawn_default: n.spawn_default.as_str().and_then(Tier::parse).unwrap_or(n.tier),
                    thinking: n.thinking.as_str().unwrap_or(""),
                    verify: n.verify.as_str().unwrap_or(""),
                    done: &done,
                    inputs: n.inputs.iter().cloned().zip(n.input_refs.iter().cloned().map(Some)).collect(),
                    repo: Some(&n.repo),
                    repo_dirs: &[],
                };
                let ticket = args.get("ticket").and_then(Value::as_str);
                json!({"ok": d::brief_block(&brief, ticket)})
            }
            Err(f) => fault(f),
        },
        "rule_matches" => json!({"ok": d::rule_matches(s(&args, "rule"), s(&args, "path"))}),
        "is_governance" => json!({"ok": d::is_governance(s(&args, "path"), &strings(args.get("human_paths")))}),
        "is_test_path" => json!({"ok": d::is_test_path(s(&args, "path"))}),
        "normpath" => json!({"ok": crate::py::normpath(s(&args, "path"))}),
        "require_human_paths" => match d::require_human_paths(&args["policy"]) {
            Ok(v) => json!({"ok": v}),
            Err(f) => fault(f),
        },
        "tier_of_model" => json!({"ok": d::tier_of_model(s(&args, "model"))}),
        "model_of_provider" => json!({"ok": d::model_of_provider(s(&args, "provider"))}),
        "parse_ts" => ts(e::parse_ts(args.get("value")).map(e::iso)),
        "capacity" => ts((|| {
            let now = e::parse_ts(args.get("now"))?.ok_or("no now")?;
            let rows = args.get("usage").and_then(Value::as_array).cloned().unwrap_or_default();
            let samples = args.get("samples").and_then(Value::as_array).cloned().unwrap_or_default();
            e::capacity(&rows, &samples, now)
        })()),
        "session_gates" => {
            let (handoff, successor) = e::session_gates(
                args.get("context_k").and_then(Value::as_i64),
                args["driven"].as_bool().unwrap_or(false),
            );
            json!({"ok": {"handoff_candidate": handoff, "successor_due": successor}})
        }
        "tier_signals" => {
            let (rows, replan) = e::tier_signals(&args["data"]);
            json!({"ok": {"rows": rows, "replan": replan}})
        }
        other => json!({"crash": format!("unknown fn {other}")}),
    }
}
