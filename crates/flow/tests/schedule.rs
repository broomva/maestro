//! BRO-3633 PR 3b1: `start`, and the read-only scheduler. Fixture `Completed`, `Chosen`,
//! `Claimed` and `Heartbeat` events go through the storage API, as their writers will. The clock
//! is passed in: no test sleeps.

mod common;

use common::*;
use jiff::{SignedDuration, Timestamp};
use maestro_flow::kernel::events::{Chosen, Claimed, Completed, Heartbeat, KernelEvent, Started, VerifyRecord, digest};
use maestro_flow::kernel::ir::{Class, Input};
use maestro_flow::kernel::{CompiledFlow, Next, Reason, RepeatState, State, Status, compile, next, start, status};
use maestro_flow::{InstanceId, Ledger, NewEvent, Options, Reader};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const VERIFY: &str = include_str!("../flows/verify.yaml");

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    ledger: Ledger,
    flow: CompiledFlow,
    params: Value,
}

fn fx(yaml: &str) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    let flow = compile(yaml).unwrap_or_else(|e| panic!("{e}"));
    Fx { _tmp: tmp, root, ledger, flow, params: Value::Null }
}

fn iter(pairs: &[(&str, u32)]) -> maestro_flow::kernel::Iteration {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

impl Fx {
    fn begin(&mut self, params: Value) -> InstanceId {
        self.params = params.clone();
        start(&mut self.ledger, &self.flow, params).unwrap().instance
    }

    /// Appends `ev` at the head; returns when the storage layer stamped it.
    fn append(&mut self, fi: &InstanceId, ev: NewEvent) -> Timestamp {
        let mut guard = self.ledger.lock().unwrap();
        let seq = guard.log(fi).unwrap().seq();
        guard.append(fi, seq, ev).unwrap().event.at.parse().unwrap()
    }

    fn complete(&mut self, fi: &InstanceId, node: &str, at: &[(&str, u32)], outputs: Value) -> Timestamp {
        self.receipt(fi, node, at, outputs, None)
    }

    fn observe(&mut self, fi: &InstanceId, node: &str, at: &[(&str, u32)], exit: i32) -> Timestamp {
        self.receipt(fi, node, at, Value::Null, Some(exit))
    }

    fn receipt(
        &mut self,
        fi: &InstanceId,
        node: &str,
        at: &[(&str, u32)],
        outputs: Value,
        exit: Option<i32>,
    ) -> Timestamp {
        let verify = exit.map(|exit| VerifyRecord {
            argv: vec!["x".into()],
            exit,
            stdout_sha256: "sha256:00".into(),
            ran_at: "2026-11-07T00:00:00Z".into(),
        });
        // An observation records the digests of the params it declares as inputs, as `complete` will.
        let inputs = match self.flow.ir().nodes.get(node).map(|n| &n.class) {
            Some(Class::Observe { inputs, .. }) => inputs
                .iter()
                .filter_map(|i| match i {
                    Input::Param { name } => Some((i.key(), digest(&self.params[name.as_str()]))),
                    Input::Node { .. } => None,
                })
                .collect(),
            _ => Default::default(),
        };
        let c = Completed {
            node: node.into(),
            iteration: iter(at),
            attempt: 1,
            inputs,
            outputs,
            verify,
            performer: None,
            tool_use_id: None,
        };
        self.append(fi, c.to_event().unwrap())
    }

    fn choose(&mut self, fi: &InstanceId, node: &str, at: &[(&str, u32)], edge: &str) {
        let c = Chosen { node: node.into(), iteration: iter(at), attempt: 1, edge: edge.into(), tool_use_id: None };
        self.append(fi, c.to_event().unwrap());
    }

    fn claim(&mut self, fi: &InstanceId, node: &str, attempt: u32, lease_until: Timestamp) -> Timestamp {
        let c = Claimed {
            node: node.into(),
            iteration: iter(&[]),
            attempt,
            lease_until: lease_until.to_string(),
            token_sha256: "sha256:00".into(),
            inputs: Default::default(),
            performer: None,
            tool_use_id: None,
        };
        self.append(fi, c.to_event().unwrap())
    }

    fn status(&self, fi: &InstanceId, now: Timestamp) -> Status {
        let reader = Reader::new(&self.root, Options::default()).unwrap();
        status(&reader.lock().unwrap(), fi, now).unwrap()
    }

    fn next(&self, fi: &InstanceId, now: Timestamp) -> Next {
        let reader = Reader::new(&self.root, Options::default()).unwrap();
        next(&reader.lock().unwrap(), fi, now).unwrap()
    }

    fn try_status(&self, fi: &InstanceId) -> Result<Status, String> {
        let reader = Reader::new(&self.root, Options::default()).unwrap();
        status(&reader.lock().unwrap(), fi, Timestamp::now()).map_err(|e| e.code().to_string())
    }
}

fn state(s: &Status, node: &str) -> State {
    s.nodes[node].state
}

fn unmet(s: &Status, node: &str) -> Vec<Reason> {
    s.nodes[node].unmet.clone()
}

fn needs(node: &str) -> Reason {
    Reason::Needs { node: node.into() }
}

fn enabled(n: &Next) -> Vec<&str> {
    n.enabled.iter().map(|e| e.node.as_str()).collect()
}

fn mins(m: i64) -> SignedDuration {
    SignedDuration::from_mins(m)
}

#[test]
fn start_pins_the_ir_and_refuses_params_that_do_not_fit() {
    let mut f = fx(VERIFY);
    for (params, why) in [
        (json!({ "repo": ".", "head": "abc" }), "a required param is missing"),
        (json!({ "repo": ".", "head": "abc", "check": "make", "colour": "blue" }), "an unknown param"),
        (json!(["repo"]), "not an object"),
    ] {
        let err = start(&mut f.ledger, &f.flow, params).unwrap_err();
        assert_eq!(err.code(), "invalid_params", "{why}");
    }
    assert!(f.ledger.lock().unwrap().instances().unwrap().is_empty(), "a refused start writes nothing");

    let fi = f.begin(json!({ "repo": ".", "head": "abc", "check": "make check" }));
    let log = f.ledger.lock().unwrap().log(&fi).unwrap();
    assert_eq!(log.events.len(), 1);
    assert_eq!(log.events[0].kind, "Started");
    let started: Started = serde_json::from_value(log.events[0].body.clone()).unwrap();
    assert_eq!(&started.ir, f.flow.ir(), "the instance keeps the IR it started with");
    assert_eq!(started.ir_hash, f.flow.hash());
    assert_eq!(started.flow, "verify@1");
}

#[test]
fn a_node_is_not_ready_until_every_need_is_completed() {
    let mut f = fx(
        "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x]}\n  b: {class: pure, run: [x]}\n  c: {class: pure, run: [x], needs: [a, b]}\n",
    );
    let fi = f.begin(json!({}));
    let now = Timestamp::now();
    let s = f.status(&fi, now);
    assert_eq!((state(&s, "a"), state(&s, "b"), state(&s, "c")), (State::Ready, State::Ready, State::Blocked));
    assert_eq!(unmet(&s, "c"), vec![needs("a"), needs("b")]);
    assert_eq!(enabled(&f.next(&fi, now)), vec!["a", "b"]);

    f.complete(&fi, "a", &[], json!({}));
    let s = f.status(&fi, now);
    assert_eq!(state(&s, "a"), State::Done);
    assert_eq!(state(&s, "c"), State::Blocked);
    assert_eq!(unmet(&s, "c"), vec![needs("b")]);

    f.complete(&fi, "b", &[], json!({}));
    assert_eq!(state(&f.status(&fi, now), "c"), State::Ready);
    assert_eq!(enabled(&f.next(&fi, now)), vec!["c"]);
}

#[test]
fn a_fork_join_waits_for_every_branch() {
    let mut f = fx(
        "flow: f\nversion: 1\nnodes:\n  x: {class: pure, run: [x]}\n  y: {class: pure, run: [x]}\n  z: {class: pure, run: [x]}\n  join: {class: work, verify: [x], needs: [strata]}\nforks:\n  strata: {all: [x, y, z]}\n",
    );
    let fi = f.begin(json!({}));
    let now = Timestamp::now();
    assert_eq!(enabled(&f.next(&fi, now)), vec!["x", "y", "z"]);
    f.complete(&fi, "x", &[], json!({}));
    f.complete(&fi, "y", &[], json!({}));
    let s = f.status(&fi, now);
    assert_eq!(state(&s, "join"), State::Blocked);
    assert_eq!(unmet(&s, "join"), vec![needs("z")]);
    f.complete(&fi, "z", &[], json!({}));
    assert_eq!(enabled(&f.next(&fi, now)), vec!["join"]);
}

const SWITCH: &str = "flow: f
version: 1
nodes:
  depth: {class: choice, edges: {quick: {guard: 'true'}, deep: {guard: '.params != null'}}, performer: agent:researcher}
  skim: {class: work, verify: [x], switch: {on: depth, case: quick}}
  dig: {class: work, verify: [x], switch: {on: depth, case: deep}, performer: agent:researcher, skill: deep-research, routing: {provider: codex}}
  summary: {class: work, verify: [x], needs: [skim]}
";

#[test]
fn a_switch_follows_only_the_chosen_edge() {
    let mut f = fx(SWITCH);
    let fi = f.begin(json!({}));
    let now = Timestamp::now();
    let s = f.status(&fi, now);
    assert_eq!(state(&s, "depth"), State::Ready);
    assert_eq!((state(&s, "skim"), state(&s, "dig")), (State::Blocked, State::Blocked));
    assert_eq!(unmet(&s, "dig"), vec![needs("depth")]);

    f.choose(&fi, "depth", &[], "deep");
    let s = f.status(&fi, now);
    assert_eq!(state(&s, "depth"), State::Done);
    assert_eq!(state(&s, "dig"), State::Ready);
    assert_eq!(state(&s, "skim"), State::Blocked);
    assert_eq!(
        unmet(&s, "skim"),
        vec![Reason::Switch { on: "depth".into(), took: Some("deep".into()), case: "quick".into() }]
    );
    assert_eq!(unmet(&s, "summary"), vec![Reason::Unreachable { node: "skim".into() }]);
    let n = f.next(&fi, now);
    assert_eq!(enabled(&n), vec!["dig"]);
    let launch = &n.enabled[0].launch;
    assert_eq!(launch.performer.as_deref(), Some("agent:researcher"));
    assert_eq!(launch.skill.as_deref(), Some("deep-research"));
    assert_eq!(launch.routing.get("provider").map(String::as_str), Some("codex"));
}

#[test]
fn a_recorded_edge_the_choice_does_not_declare_fails_the_node() {
    let mut f = fx(SWITCH);
    let fi = f.begin(json!({}));
    f.choose(&fi, "depth", &[], "sideways");
    let s = f.status(&fi, Timestamp::now());
    assert_eq!(state(&s, "depth"), State::Failed);
    assert_eq!(unmet(&s, "depth"), vec![Reason::EdgeUndeclared { edge: "sideways".into() }]);
    assert_eq!((state(&s, "skim"), state(&s, "dig")), (State::Blocked, State::Blocked));
}

const ROUNDS: &str = "flow: f
version: 1
nodes:
  review: {class: work, verify: [x]}
  fix: {class: work, verify: [x], needs: [review]}
  merge: {class: effect, needs: [rounds], reconcile: [x]}
  escalate: {class: human, ask: The review did not converge. Merge anyway?}
  announce: {class: work, verify: [x], needs: [merge]}
  waive: {class: work, verify: [x], needs: [escalate]}
  report: {class: work, verify: [x], needs: [fix]}
repeats:
  rounds: {body: [review, fix], max: 3, until: '.nodes.review.outputs.verdict == \"pass\"', on_exhausted: escalate}
";

#[test]
fn a_receipt_from_one_iteration_never_satisfies_the_next() {
    let mut f = fx(ROUNDS);
    let fi = f.begin(json!({}));
    let now = Timestamp::now();
    f.complete(&fi, "review", &[("rounds", 0)], json!({ "verdict": "fail" }));
    f.complete(&fi, "fix", &[("rounds", 0)], json!({}));
    let s = f.status(&fi, now);
    assert_eq!(s.repeats["rounds"].iteration, 1);
    assert_eq!(s.nodes["review"].iteration, iter(&[("rounds", 1)]));
    assert_eq!(state(&s, "review"), State::Ready);
    assert_eq!(state(&s, "fix"), State::Blocked, "iteration 0's review does not satisfy iteration 1's fix");
    assert_eq!(unmet(&s, "fix"), vec![needs("review")]);
    // A node outside the repeat that needs a body node waits for the repeat to finish.
    assert_eq!(unmet(&s, "report"), vec![Reason::RepeatRunning { repeat: "rounds".into() }]);
    // Another receipt for iteration 0, written late, changes nothing in iteration 1.
    f.complete(&fi, "review", &[("rounds", 0)], json!({ "verdict": "fail" }));
    assert_eq!(state(&f.status(&fi, now), "fix"), State::Blocked);

    f.complete(&fi, "review", &[("rounds", 1)], json!({ "verdict": "pass" }));
    let s = f.status(&fi, now);
    assert_eq!(state(&s, "fix"), State::Ready);
    assert_eq!(state(&s, "merge"), State::Blocked);
    f.complete(&fi, "fix", &[("rounds", 1)], json!({}));
    let s = f.status(&fi, now);
    assert_eq!(s.repeats["rounds"].state, RepeatState::Done, "until held in iteration 1");
    assert_eq!(state(&s, "merge"), State::Ready);
    assert_eq!(state(&s, "escalate"), State::Blocked);
    assert_eq!(unmet(&s, "escalate"), vec![Reason::RepeatFinished { repeat: "rounds".into() }]);
    assert_eq!(unmet(&s, "waive"), vec![Reason::Unreachable { node: "escalate".into() }], "it will never run");
    // Outside the repeat, a body node is seen at the repeat's final iteration.
    assert_eq!(state(&s, "report"), State::Ready);
}

#[test]
fn a_repeat_that_reaches_max_is_exhausted_and_routes_to_on_exhausted() {
    let mut f = fx(&ROUNDS.replace(", until: '.nodes.review.outputs.verdict == \"pass\"'", ""));
    let fi = f.begin(json!({}));
    let now = Timestamp::now();
    for i in 0..2 {
        f.complete(&fi, "review", &[("rounds", i)], json!({ "verdict": "pass" }));
        f.complete(&fi, "fix", &[("rounds", i)], json!({}));
    }
    let s = f.status(&fi, now);
    assert_eq!(s.repeats["rounds"].state, RepeatState::Running);
    assert_eq!(unmet(&s, "escalate"), vec![Reason::RepeatRunning { repeat: "rounds".into() }]);
    f.complete(&fi, "review", &[("rounds", 2)], json!({ "verdict": "pass" }));
    f.complete(&fi, "fix", &[("rounds", 2)], json!({}));

    let s = f.status(&fi, now);
    assert_eq!(s.repeats["rounds"].state, RepeatState::Exhausted, "no until: max iterations exhaust it");
    assert_eq!((state(&s, "review"), state(&s, "fix")), (State::Exhausted, State::Exhausted));
    assert_eq!(state(&s, "merge"), State::Blocked);
    assert_eq!(unmet(&s, "merge"), vec![Reason::RepeatExhausted { repeat: "rounds".into() }]);
    assert_eq!(unmet(&s, "announce"), vec![Reason::Unreachable { node: "merge".into() }], "it will never run");
    assert_eq!(unmet(&s, "report"), vec![Reason::RepeatExhausted { repeat: "rounds".into() }]);
    assert_eq!(unmet(&f.status(&fi, now), "waive"), vec![needs("escalate")]);
    assert_eq!(state(&s, "escalate"), State::Waiting, "on_exhausted is the route, here to the owner");
    assert_eq!(
        s.nodes["escalate"].waits,
        vec![Reason::Owner { ask: "The review did not converge. Merge anyway?".into() }]
    );
}

#[test]
fn a_branch_not_taken_does_not_hold_up_a_repeat() {
    let mut f = fx("flow: f
version: 1
nodes:
  pick: {class: choice, edges: {quick: {guard: 'true'}, deep: {guard: 'true'}}}
  skim: {class: work, verify: [x], switch: {on: pick, case: quick}}
  dig: {class: work, verify: [x], switch: {on: pick, case: deep}}
  after: {class: work, verify: [x], needs: [tries]}
repeats:
  tries: {body: [pick, skim, dig], max: 2, until: '.nodes.pick.value == \"quick\"'}
");
    let fi = f.begin(json!({}));
    let now = Timestamp::now();
    f.choose(&fi, "pick", &[("tries", 0)], "quick");
    f.complete(&fi, "skim", &[("tries", 0)], json!({}));
    let s = f.status(&fi, now);
    assert_eq!(s.repeats["tries"].state, RepeatState::Done, "dig was not taken, so iteration 0 is settled");
    assert_eq!(state(&s, "after"), State::Ready);
}

fn verify_params(surface: bool) -> Value {
    let mut p = json!({ "repo": "/repo", "head": "9f2c", "check": "make check" });
    if surface {
        p["surface"] = json!("cli");
    }
    p
}

#[test]
fn a_stale_observation_blocks_every_dependent() {
    // V1 and V2: pin carries fresh_for; while it is stale, check and exercise are blocked.
    let mut f = fx(VERIFY);
    let fi = f.begin(verify_params(true));
    let t = f.observe(&fi, "pin", &[], 0);
    let s = f.status(&fi, t + mins(9));
    assert_eq!(state(&s, "pin"), State::Done);
    assert_eq!(state(&s, "check"), State::Ready);
    let s = f.status(&fi, t + mins(11));
    assert_eq!(state(&s, "pin"), State::Stale);
    assert_eq!(unmet(&s, "pin"), vec![Reason::Expired { age_secs: 660, fresh_for_secs: 600 }]);
    assert_eq!(state(&s, "check"), State::Blocked);
    assert_eq!(unmet(&s, "check"), vec![Reason::Stale { node: "pin".into() }]);
    assert!(f.next(&fi, t + mins(11)).enabled.is_empty());

    // Further down, past a node that is done: exercise is blocked too.
    f.complete(&fi, "check", &[], json!({}));
    let s = f.status(&fi, t + mins(9));
    assert_eq!(state(&s, "exercise"), State::Ready);
    let s = f.status(&fi, t + mins(11));
    assert_eq!(state(&s, "check"), State::Done);
    assert_eq!(state(&s, "exercise"), State::Blocked);
    assert_eq!(unmet(&s, "exercise"), vec![Reason::Stale { node: "pin".into() }]);

    // A fresh observation unblocks them.
    let t2 = f.observe(&fi, "pin", &[], 0);
    assert_eq!(state(&f.status(&fi, t2 + mins(1)), "exercise"), State::Ready);
}

#[test]
fn an_observation_whose_input_changed_is_stale() {
    let mut f = fx(
        "flow: f\nversion: 1\nnodes:\n  push: {class: effect, reconcile: [x]}\n  ci: {class: observe, run: [x], needs: [push], inputs: [push], exit_map: {0: green}}\n  land: {class: work, verify: [x], needs: [ci]}\n",
    );
    let fi = f.begin(json!({}));
    let now = Timestamp::now();
    f.complete(&fi, "push", &[], json!({ "sha": "aaa" }));
    let ci = |sha: &str| Completed {
        node: "ci".into(),
        iteration: iter(&[]),
        attempt: 1,
        inputs: [("push".to_string(), digest(&json!({ "sha": sha })))].into(),
        outputs: Value::Null,
        verify: Some(VerifyRecord { argv: vec![], exit: 0, stdout_sha256: String::new(), ran_at: String::new() }),
        performer: None,
        tool_use_id: None,
    };
    f.append(&fi, ci("aaa").to_event().unwrap());
    let s = f.status(&fi, now);
    assert_eq!(state(&s, "ci"), State::Done);
    assert_eq!(state(&s, "land"), State::Ready);

    f.complete(&fi, "push", &[], json!({ "sha": "bbb" }));
    let s = f.status(&fi, now);
    assert_eq!(state(&s, "ci"), State::Stale);
    assert_eq!(unmet(&s, "ci"), vec![Reason::InputChanged { input: "push".into() }]);
    assert_eq!(unmet(&s, "land"), vec![Reason::Stale { node: "ci".into() }]);
}

#[test]
fn an_exit_status_outside_the_exit_map_fails_closed() {
    let mut f = fx(VERIFY);
    let fi = f.begin(verify_params(true));
    let t = f.observe(&fi, "pin", &[], 7);
    let s = f.status(&fi, t);
    assert_eq!(state(&s, "pin"), State::Failed);
    assert_eq!(unmet(&s, "pin"), vec![Reason::ExitUnmapped { exit: Some(7) }]);
    assert_eq!(state(&s, "check"), State::Blocked);
}

#[test]
fn a_switch_on_an_observed_value_follows_it() {
    // V2: a check never runs against a head that has moved.
    let mut f = fx(VERIFY);
    let fi = f.begin(verify_params(true));
    let t = f.observe(&fi, "pin", &[], 1);
    let s = f.status(&fi, t);
    assert_eq!(state(&s, "pin"), State::Done);
    assert_eq!(
        unmet(&s, "check"),
        vec![Reason::Switch { on: "pin".into(), took: Some("moved".into()), case: "at_head".into() }]
    );
    assert_eq!(unmet(&s, "exercise"), vec![Reason::Unreachable { node: "check".into() }]);
}

#[test]
fn exercise_applies_only_when_a_surface_is_declared() {
    // V4.
    let mut f = fx(VERIFY);
    let fi = f.begin(verify_params(false));
    let t = f.observe(&fi, "pin", &[], 0);
    f.complete(&fi, "check", &[], json!({}));
    let s = f.status(&fi, t);
    assert_eq!(state(&s, "exercise"), State::Blocked);
    assert_eq!(unmet(&s, "exercise"), vec![Reason::ParamAbsent { param: "surface".into() }]);

    let fi = f.begin(verify_params(true));
    let t = f.observe(&fi, "pin", &[], 0);
    f.complete(&fi, "check", &[], json!({}));
    let n = f.next(&fi, t);
    assert_eq!(enabled(&n), vec!["exercise"]);
    assert_eq!(n.enabled[0].launch.skill.as_deref(), Some("dogfood"));
    assert_eq!(n.enabled[0].launch.performer.as_deref(), Some("agent:verifier"));
}

#[test]
fn claims_leases_retry_and_cancel_decide_waiting_ready_and_failed() {
    let mut f = fx(
        "flow: f\nversion: 1\nnodes:\n  ci: {class: work, verify: [x], retry: {attempts: 2}, wait: {external: CI checks, heartbeat: 5m}}\n  long: {class: work, verify: [x], cancel: {after: 30m}}\n",
    );
    let fi = f.begin(json!({}));
    let t = f.claim(&fi, "ci", 1, Timestamp::now() + mins(10));
    let s = f.status(&fi, t);
    assert_eq!(state(&s, "ci"), State::Waiting);
    assert!(matches!(s.nodes["ci"].waits[0], Reason::Claimed { attempt: 1, .. }));
    assert_eq!(s.nodes["ci"].waits[1], Reason::External { waits_for: "CI checks".into(), heartbeat_secs: 300 });

    // The lease passed: the node is released, and the next claim is attempt 2.
    let later = t + mins(20);
    let n = f.next(&fi, later);
    assert_eq!(enabled(&n), vec!["ci", "long"]);
    assert_eq!(n.enabled[0].attempt, 2);
    // A second attempt that lapses uses the two attempts retry allows.
    f.claim(&fi, "ci", 2, later + mins(1));
    let s = f.status(&fi, later + mins(5));
    assert_eq!(state(&s, "ci"), State::Failed);
    assert_eq!(unmet(&s, "ci"), vec![Reason::AttemptsUsed { attempts: 2 }]);

    // cancel.after ends an attempt whatever its heartbeats say.
    let t = f.claim(&fi, "long", 1, Timestamp::now() + mins(10));
    let hb =
        Heartbeat { node: "long".into(), iteration: iter(&[]), attempt: 1, lease_until: (t + mins(60)).to_string() };
    f.append(&fi, hb.to_event().unwrap());
    assert_eq!(state(&f.status(&fi, t + mins(20)), "long"), State::Waiting, "the heartbeat extended the lease");
    let s = f.status(&fi, t + mins(31));
    assert_eq!(state(&s, "long"), State::Ready, "cancelled after 30m, so the node is released");
}

#[test]
fn replay_is_deterministic() {
    let mut f = fx(ROUNDS);
    let fi = f.begin(json!({}));
    f.complete(&fi, "review", &[("rounds", 0)], json!({ "verdict": "fail", "notes": ["a", "b"] }));
    f.complete(&fi, "fix", &[("rounds", 0)], json!({}));
    let now = Timestamp::now();
    let a = serde_json::to_vec(&f.status(&fi, now)).unwrap();
    let b = serde_json::to_vec(&f.status(&fi, now)).unwrap();
    assert_eq!(a, b, "the same log and clock give byte-identical status");
    let a = serde_json::to_vec(&f.next(&fi, now)).unwrap();
    let b = serde_json::to_vec(&f.next(&fi, now)).unwrap();
    assert_eq!(a, b, "and byte-identical next");
}

/// A flow whose every command, guard and reconcile would create `marker` if anything ran it.
fn armed(marker: &Path) -> String {
    let m = marker.display();
    format!(
        "flow: armed
version: 1
nodes:
  pin: {{class: observe, run: [touch, '{m}'], fresh_for: 1m, exit_map: {{0: ok}}}}
  check: {{class: pure, run: [touch, '{m}'], needs: [pin]}}
  build: {{class: work, verify: [sh, -c, 'touch {m}'], needs: [check]}}
  push: {{class: effect, reconcile: [touch, '{m}'], preconditions: ['true'], needs: [build]}}
  pick: {{class: choice, edges: {{a: {{guard: 'true'}}}}, needs: [push]}}
  ask: {{class: human, ask: 'touch {m}', needs: [pick]}}
"
    )
}

#[test]
fn replay_executes_nothing_and_reads_write_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("executed");
    let mut f = fx(&armed(&marker));
    let fi = f.begin(json!({}));
    let t = f.observe(&fi, "pin", &[], 0);
    f.complete(&fi, "check", &[], json!({}));
    f.complete(&fi, "build", &[], json!({}));
    f.complete(&fi, "push", &[], json!({}));
    f.choose(&fi, "pick", &[], "a");

    let before = snapshot(&f.root);
    for now in [t, t + mins(5)] {
        let s = f.status(&fi, now);
        let n = f.next(&fi, now);
        assert_eq!(state(&s, "ask"), if now == t { State::Waiting } else { State::Blocked });
        assert!(n.enabled.is_empty());
    }
    assert!(!marker.exists(), "replay ran a command");
    assert_eq!(snapshot(&f.root), before, "a read wrote under the flows root");
}

#[test]
fn an_instance_that_does_not_replay_is_an_error_never_an_empty_flow() {
    let mut f =
        fx("flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x]}\n  b: {class: pure, run: [x], needs: [a]}\n");
    let started = |f: &Fx| Started {
        flow: f.flow.name(),
        ir_hash: f.flow.hash().to_string(),
        ir: f.flow.ir().clone(),
        params: json!({}),
    };
    let fresh = |f: &mut Fx, name: &str, first: NewEvent| {
        let fi = id(name);
        f.ledger.lock().unwrap().append(&fi, 0, first).unwrap();
        fi
    };

    // An event kind the kernel does not define.
    let fi = f.begin(json!({}));
    f.append(&fi, NewEvent::new("Teleported", json!({ "node": "a" })));
    assert_eq!(f.try_status(&fi).unwrap_err(), "malformed_instance");

    // An event naming a node the flow does not declare.
    let fi = f.begin(json!({}));
    f.complete(&fi, "ghost", &[], json!({}));
    assert_eq!(f.try_status(&fi).unwrap_err(), "malformed_instance");

    // A first event that is not Started, even with a Started body.
    let body = started(&f).to_event().unwrap().body;
    let fi = fresh(&mut f, "fi_not_started", NewEvent::new("Claimed", body));
    assert_eq!(f.try_status(&fi).unwrap_err(), "malformed_instance");

    // A pinned IR that does not match its hash.
    let mut s = started(&f);
    s.ir.nodes.remove("b");
    let fi = fresh(&mut f, "fi_hash", s.to_event().unwrap());
    assert_eq!(f.try_status(&fi).unwrap_err(), "malformed_instance");

    // A pinned IR that no longer validates, however its hash was made.
    let mut s = started(&f);
    s.ir.nodes.get_mut("a").unwrap().needs.push(maestro_flow::kernel::ir::Need::Node { node: "b".into() });
    s.ir_hash = maestro_flow::kernel::ir::content_hash(&s.ir).unwrap();
    let fi = fresh(&mut f, "fi_cycle", s.to_event().unwrap());
    assert_eq!(f.try_status(&fi).unwrap_err(), "malformed_instance");

    // And the untouched pin replays.
    let first = started(&f).to_event().unwrap();
    let fi = fresh(&mut f, "fi_ok", first);
    assert_eq!(state(&f.try_status(&fi).unwrap(), "a"), State::Ready);
}
