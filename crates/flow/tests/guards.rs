//! BRO-3633 PR 3b1: the bounded guard evaluator writers call. Replay never evaluates a guard.

use maestro_flow::kernel::evaluate;
use serde_json::json;
use std::time::{Duration, Instant};

fn code(r: Result<bool, maestro_flow::kernel::GuardRefusal>) -> String {
    match r {
        Ok(b) => b.to_string(),
        Err(e) => e.code().to_string(),
    }
}

#[test]
fn a_guard_gives_a_boolean_verdict_or_a_coded_refusal() {
    let state = json!({ "nodes": { "review": { "outputs": { "verdict": "pass" } } } });
    let limit = Duration::from_secs(5);
    assert_eq!(code(evaluate(".nodes.review.outputs.verdict == \"pass\"", &state, limit)), "true");
    assert_eq!(code(evaluate(".nodes.review.outputs.verdict == \"fail\"", &state, limit)), "false");
    assert_eq!(code(evaluate(".nodes", &state, limit)), "guard_not_boolean");
    assert_eq!(code(evaluate("empty", &state, limit)), "guard_not_boolean");
    assert_eq!(code(evaluate("error(\"no\")", &state, limit)), "guard_error");
}

#[test]
fn guards_that_would_not_end_are_refused_before_they_run() {
    let started = Instant::now();
    let state = json!({ "nodes": { "a": { "outputs": { "n": 1e12 } } } });
    for guard in ["def f: f; f", "last(range(1e15)) > 0", "[range(0; .nodes.a.outputs.n)] | length > 0"] {
        assert_eq!(code(evaluate(guard, &state, Duration::from_secs(60))), "invalid_guard", "{guard}");
    }
    assert!(started.elapsed() < Duration::from_secs(5), "refused without running");
}

#[test]
fn a_guard_past_its_limit_gives_no_verdict() {
    // Allowed filters, bounded by the input, but far more work than the limit allows.
    let input = json!((0..40).collect::<Vec<_>>());
    let slow = "[.[] as $a | .[] as $b | .[] as $c | .[] as $d | 1] | length > 0";
    let started = Instant::now();
    assert_eq!(code(evaluate(slow, &input, Duration::from_millis(20))), "guard_timeout");
    assert!(started.elapsed() < Duration::from_secs(2), "the caller got its answer at the limit");
}
