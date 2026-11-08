//! BRO-3633 PR 3b1: `start`, and the read-only scheduler. Fixture `Completed`, `Chosen`,
//! `Claimed`, `Heartbeat` and `Guarded` events go through the storage API, as their writers
//! will; a fixture receipt records the input digests `next` names for it, as `complete` will. The
//! clock is passed in: no test sleeps.

mod common;

use common::*;
use jiff::{SignedDuration, Timestamp};
use maestro_flow::kernel::events::{
    Chosen, Claimed, Completed, Guarded, Heartbeat, KernelEvent, Refused, Started, VerifyRecord,
};
use maestro_flow::kernel::ir::{IR_VERSION, Need, content_hash};
use maestro_flow::kernel::{
    CompiledFlow, Iteration, Next, Reason, RepeatState, State, Status, compile, next, start, status,
};
use maestro_flow::{InstanceId, Ledger, NewEvent, Options, Reader};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const VERIFY: &str = include_str!("../flows/verify.yaml");

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    ledger: Ledger,
    flow: CompiledFlow,
}

fn fx(yaml: &str) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    let flow = compile(yaml).unwrap_or_else(|e| panic!("{e}"));
    Fx { _tmp: tmp, root, ledger, flow }
}

fn iter(pairs: &[(&str, u32)]) -> Iteration {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

impl Fx {
    fn begin(&mut self, params: Value) -> InstanceId {
        start(&mut self.ledger, &self.flow, params).unwrap().instance
    }

    /// Appends `ev` at the head; returns when the storage layer stamped it.
    fn append(&mut self, fi: &InstanceId, ev: NewEvent) -> Timestamp {
        let mut guard = self.ledger.lock().unwrap();
        let seq = guard.log(fi).unwrap().seq();
        guard.append(fi, seq, ev).unwrap().event.at.parse().unwrap()
    }

    /// The input digests `status` names for `node` now.
    fn inputs(&self, fi: &InstanceId, node: &str) -> BTreeMap<String, String> {
        self.status(fi, Timestamp::now()).nodes.get(node).map(|n| n.inputs.clone()).unwrap_or_default()
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
        let c = Completed {
            node: node.into(),
            iteration: iter(at),
            attempt: 1,
            inputs: self.inputs(fi, node),
            outputs,
            verify,
            performer: None,
            tool_use_id: None,
        };
        self.append(fi, c.to_event().unwrap())
    }

    fn choose(&mut self, fi: &InstanceId, node: &str, at: &[(&str, u32)], edge: &str) {
        let inputs = self.inputs(fi, node);
        let c =
            Chosen { node: node.into(), iteration: iter(at), attempt: 1, edge: edge.into(), inputs, tool_use_id: None };
        self.append(fi, c.to_event().unwrap());
    }

    fn verdict(&mut self, fi: &InstanceId, repeat: &str, iteration: u32, holds: bool) {
        self.append(fi, Guarded { repeat: repeat.into(), iteration, holds }.to_event().unwrap());
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

    fn heartbeat(&mut self, fi: &InstanceId, node: &str, attempt: u32, lease_until: Timestamp) {
        let h = Heartbeat { node: node.into(), iteration: iter(&[]), attempt, lease_until: lease_until.to_string() };
        self.append(fi, h.to_event().unwrap());
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

fn unreachable(node: &str) -> Reason {
    Reason::Unreachable { node: node.into() }
}

fn enabled(n: &Next) -> Vec<&str> {
    n.enabled.iter().map(|e| e.node.as_str()).collect()
}

/// The reasons replay set events aside for, as `(subject, reason)`.
fn rejected(s: &Status) -> Vec<(String, String)> {
    s.rejected.iter().map(|r| (r.subject.clone(), r.reason.clone())).collect()
}

fn rej(subject: &str, reason: &str) -> (String, String) {
    (subject.to_string(), reason.to_string())
}

fn mins(m: i64) -> SignedDuration {
    SignedDuration::from_mins(m)
}

#[test]
fn start_pins_the_ir_and_refuses_params_that_do_not_fit() {
    let mut f = fx(VERIFY);
    for (params, why) in [
        (json!({ "repo": ".", "head": "abc" }), "a required param is missing"),
        (json!({ "repo": ".", "head": null, "check": "make" }), "a required param is null"),
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

const CHAIN: &str = "flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x]}\n  b: {class: pure, run: [x], needs: [a]}\n  c: {class: pure, run: [x], needs: [b]}\n";

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
    assert_eq!(unmet(&s, "c"), vec![needs("b")]);

    f.complete(&fi, "b", &[], json!({}));
    assert_eq!(state(&f.status(&fi, now), "c"), State::Ready);
    assert_eq!(enabled(&f.next(&fi, now)), vec!["c"]);
}

#[test]
fn a_receipt_for_a_node_that_is_not_enabled_counts_for_nothing() {
    let mut f = fx(CHAIN);
    let fi = f.begin(json!({}));
    f.complete(&fi, "b", &[], json!({}));
    let s = f.status(&fi, Timestamp::now());
    assert_eq!((state(&s, "a"), state(&s, "b"), state(&s, "c")), (State::Ready, State::Blocked, State::Blocked));
    assert_eq!(rejected(&s), vec![rej("b", "not_enabled")]);
    // In order, it counts.
    f.complete(&fi, "a", &[], json!({}));
    f.complete(&fi, "b", &[], json!({}));
    assert_eq!(state(&f.status(&fi, Timestamp::now()), "c"), State::Ready);
    // A second receipt for a node that is done counts for nothing.
    f.complete(&fi, "a", &[], json!({ "again": true }));
    let s = f.status(&fi, Timestamp::now());
    assert_eq!(rejected(&s).last(), Some(&rej("a", "already_done")));
    assert_eq!(state(&s, "c"), State::Ready, "the second receipt did not change a's digest");
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
    assert_eq!(unmet(&s, "dig"), vec![needs("depth")]);

    f.choose(&fi, "depth", &[], "deep");
    let s = f.status(&fi, now);
    assert_eq!(state(&s, "depth"), State::Done);
    assert_eq!(state(&s, "dig"), State::Ready);
    assert_eq!(
        unmet(&s, "skim"),
        vec![Reason::Switch { on: "depth".into(), took: Some("deep".into()), case: "quick".into() }]
    );
    assert_eq!(unmet(&s, "summary"), vec![unreachable("skim")]);
    let n = f.next(&fi, now);
    assert_eq!(enabled(&n), vec!["dig"]);
    let launch = &n.enabled[0].launch;
    assert_eq!(launch.performer.as_deref(), Some("agent:researcher"));
    assert_eq!(launch.skill.as_deref(), Some("deep-research"));
    assert_eq!(launch.routing.get("provider").map(String::as_str), Some("codex"));
}

#[test]
fn a_receipt_or_choice_off_the_chosen_branch_counts_for_nothing() {
    let mut f = fx(SWITCH);
    let fi = f.begin(json!({}));
    f.choose(&fi, "depth", &[], "sideways");
    f.complete(&fi, "depth", &[], json!({}));
    f.choose(&fi, "skim", &[], "quick");
    f.choose(&fi, "depth", &[], "deep");
    f.complete(&fi, "skim", &[], json!({}));
    f.choose(&fi, "depth", &[], "quick");
    let s = f.status(&fi, Timestamp::now());
    assert_eq!(
        rejected(&s),
        vec![
            rej("depth", "undeclared_edge"),
            rej("depth", "wrong_class"),
            rej("skim", "wrong_class"),
            rej("skim", "not_enabled"),
            rej("depth", "already_done"),
        ]
    );
    assert_eq!(state(&s, "dig"), State::Ready, "deep stays the chosen edge");
    assert_eq!(state(&s, "skim"), State::Blocked);
    assert_eq!(unmet(&s, "summary"), vec![unreachable("skim")]);
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
fn a_receipt_from_one_iteration_never_satisfies_another() {
    let mut f = fx(ROUNDS);
    let fi = f.begin(json!({}));
    let now = Timestamp::now();
    // A receipt for a future iteration is not pre-filled.
    f.complete(&fi, "review", &[("rounds", 1)], json!({ "verdict": "pass" }));
    f.complete(&fi, "review", &[("rounds", 0)], json!({ "verdict": "fail" }));
    f.verdict(&fi, "rounds", 0, false);
    f.complete(&fi, "fix", &[("rounds", 0)], json!({}));
    let s = f.status(&fi, now);
    assert_eq!(rejected(&s), vec![rej("review", "not_current_iteration"), rej("rounds", "not_settled")]);
    assert!(s.repeats["rounds"].awaiting_verdict, "iteration 0 settled; its until verdict is due");
    assert_eq!(s.repeats["rounds"].iteration, 0);
    f.verdict(&fi, "rounds", 0, false);
    let s = f.status(&fi, now);
    assert_eq!(s.repeats["rounds"].iteration, 1);
    assert_eq!(s.nodes["review"].iteration, iter(&[("rounds", 1)]));
    assert_eq!(state(&s, "review"), State::Ready);
    assert_eq!(state(&s, "fix"), State::Blocked, "iteration 0's review does not satisfy iteration 1's fix");
    assert_eq!(unmet(&s, "fix"), vec![needs("review")]);
    assert_eq!(unmet(&s, "report"), vec![Reason::RepeatRunning { repeat: "rounds".into() }]);

    // A late iteration-0 receipt, even a passing one, cannot rewrite the repeat.
    f.complete(&fi, "review", &[("rounds", 0)], json!({ "verdict": "pass" }));
    f.verdict(&fi, "rounds", 0, true);
    let s = f.status(&fi, now);
    assert_eq!(s.repeats["rounds"].iteration, 1);
    assert_eq!(s.repeats["rounds"].state, RepeatState::Running);
    assert_eq!(state(&s, "merge"), State::Blocked);
    assert_eq!(rejected(&s)[2..], [rej("review", "not_current_iteration"), rej("rounds", "not_current_iteration")]);

    f.complete(&fi, "review", &[("rounds", 1)], json!({ "verdict": "pass" }));
    f.complete(&fi, "fix", &[("rounds", 1)], json!({}));
    f.verdict(&fi, "rounds", 1, true);
    let s = f.status(&fi, now);
    assert_eq!(s.repeats["rounds"].state, RepeatState::Done, "the verdict for iteration 1 held");
    assert_eq!(state(&s, "merge"), State::Ready);
    assert_eq!(unmet(&s, "escalate"), vec![Reason::RepeatFinished { repeat: "rounds".into() }]);
    assert_eq!(unmet(&s, "waive"), vec![unreachable("escalate")], "it will never run");
    // Outside the repeat, a body node is seen at the repeat's final iteration.
    assert_eq!(state(&s, "report"), State::Ready);
}

#[test]
fn replay_reads_until_verdicts_and_never_evaluates_a_guard() {
    let mut f = fx(
        "flow: f\nversion: 1\nnodes:\n  a: {class: work, verify: [x]}\nrepeats:\n  r: {body: [a], max: 3, until: 'true'}\n",
    );
    let fi = f.begin(json!({}));
    let now = Timestamp::now();
    f.complete(&fi, "a", &[("r", 0)], json!({}));
    let s = f.status(&fi, now);
    assert_eq!((s.repeats["r"].state, s.repeats["r"].iteration), (RepeatState::Running, 0), "no verdict yet");
    assert!(s.repeats["r"].awaiting_verdict);
    f.verdict(&fi, "r", 0, false);
    let s = f.status(&fi, now);
    assert_eq!(
        (s.repeats["r"].state, s.repeats["r"].iteration),
        (RepeatState::Running, 1),
        "the recorded verdict wins"
    );
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
    assert_eq!(unmet(&s, "merge"), vec![Reason::RepeatExhausted { repeat: "rounds".into() }]);
    assert_eq!(unmet(&s, "announce"), vec![unreachable("merge")], "it will never run");
    assert_eq!(unmet(&s, "report"), vec![Reason::RepeatExhausted { repeat: "rounds".into() }]);
    assert_eq!(unmet(&s, "waive"), vec![needs("escalate")]);
    assert_eq!(state(&s, "escalate"), State::Waiting, "on_exhausted is the route, here to the owner");
    assert_eq!(
        s.nodes["escalate"].waits,
        vec![Reason::Owner { ask: "The review did not converge. Merge anyway?".into() }]
    );
    // An exhausted repeat takes no more receipts, and a repeat with no until takes no verdict.
    f.complete(&fi, "review", &[("rounds", 2)], json!({}));
    f.verdict(&fi, "rounds", 2, true);
    assert_eq!(rejected(&f.status(&fi, now)), vec![rej("review", "not_current_iteration"), rej("rounds", "no_until")]);
}

#[test]
fn a_branch_not_taken_and_its_dependents_settle_a_repeat() {
    let mut f = fx("flow: f
version: 1
nodes:
  pick: {class: choice, edges: {quick: {guard: 'true'}, deep: {guard: 'true'}}}
  skim: {class: work, verify: [x], switch: {on: pick, case: quick}}
  dig: {class: work, verify: [x], switch: {on: pick, case: deep}}
  after_dig: {class: work, verify: [x], needs: [dig]}
  after: {class: work, verify: [x], needs: [tries]}
repeats:
  tries: {body: [pick, skim, dig, after_dig], max: 2, until: '.nodes.pick.value == \"quick\"'}
");
    let fi = f.begin(json!({}));
    f.choose(&fi, "pick", &[("tries", 0)], "quick");
    f.complete(&fi, "skim", &[("tries", 0)], json!({}));
    f.verdict(&fi, "tries", 0, true);
    let s = f.status(&fi, Timestamp::now());
    assert!(rejected(&s).is_empty(), "{:?}", s.rejected);
    assert_eq!(s.repeats["tries"].state, RepeatState::Done, "dig and after_dig will never run, so iteration 0 settled");
    assert_eq!(state(&s, "after"), State::Ready);
}

#[test]
fn a_repeat_whose_whole_body_never_runs_is_unreachable_not_exhausted() {
    let mut f = fx("flow: f
version: 1
nodes:
  gate: {class: choice, edges: {run: {guard: 'true'}, skip: {guard: 'true'}}}
  a: {class: work, verify: [x], switch: {on: gate, case: run}}
  e: {class: work, verify: [x]}
  after: {class: work, verify: [x], needs: [r]}
repeats:
  r: {body: [a], max: 1000, on_exhausted: e}
");
    let fi = f.begin(json!({}));
    f.choose(&fi, "gate", &[], "skip");
    let s = f.status(&fi, Timestamp::now());
    assert_eq!(s.repeats["r"].state, RepeatState::Unreachable);
    assert_eq!(unmet(&s, "e"), vec![Reason::RepeatUnreachable { repeat: "r".into() }]);
    assert_eq!(unmet(&s, "after"), vec![Reason::RepeatUnreachable { repeat: "r".into() }]);
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
    f.observe(&fi, "surface", &[], 0);
    let t = f.observe(&fi, "pin", &[], 0);
    let s = f.status(&fi, t + mins(10));
    assert_eq!(state(&s, "pin"), State::Done, "at exactly fresh_for it is still fresh");
    assert_eq!(state(&s, "check"), State::Ready);
    let s = f.status(&fi, t + mins(11));
    assert_eq!(state(&s, "pin"), State::Stale);
    assert_eq!(unmet(&s, "pin"), vec![Reason::Expired { age_secs: 660, fresh_for_secs: 600 }]);
    assert_eq!(unmet(&s, "check"), vec![Reason::Stale { node: "pin".into() }]);
    assert_eq!(enabled(&f.next(&fi, t + mins(11))), vec!["pin"], "only the re-run of pin");

    // Further down, past a node that is done: exercise is blocked too.
    f.complete(&fi, "check", &[], json!({}));
    assert_eq!(state(&f.status(&fi, t + mins(9)), "exercise"), State::Ready);
    let s = f.status(&fi, t + mins(11));
    assert_eq!(state(&s, "check"), State::Done);
    assert_eq!(unmet(&s, "exercise"), vec![Reason::Stale { node: "pin".into() }]);

    // A fresh observation with the same result unblocks them and keeps check's receipt.
    let t2 = f.observe(&fi, "pin", &[], 0);
    let s = f.status(&fi, t2 + mins(1));
    assert_eq!(state(&s, "check"), State::Done);
    assert_eq!(state(&s, "exercise"), State::Ready);
}

#[test]
fn a_receipt_whose_inputs_changed_is_not_done() {
    // V2: a check never stands against a head that has since moved.
    let mut f = fx(VERIFY);
    let fi = f.begin(verify_params(true));
    f.observe(&fi, "surface", &[], 0);
    f.observe(&fi, "pin", &[], 0);
    f.complete(&fi, "check", &[], json!({}));
    let t = f.observe(&fi, "pin", &[], 1);
    let s = f.status(&fi, t);
    assert_eq!(
        unmet(&s, "check"),
        vec![Reason::Switch { on: "pin".into(), took: Some("moved".into()), case: "at_head".into() }]
    );
    assert_eq!(unmet(&s, "exercise"), vec![unreachable("check")]);

    // The rebuilder: a new upstream receipt makes a downstream receipt stale until it is redone.
    let mut f = fx(
        "flow: f\nversion: 1\nnodes:\n  probe: {class: observe, run: [x], fresh_for: 1h, exit_map: {0: ok}}\n  build: {class: pure, run: [x], needs: [probe]}\n  deploy: {class: work, verify: [x], needs: [build]}\n",
    );
    let fi = f.begin(json!({}));
    f.receipt(&fi, "probe", &[], json!({ "sha": "a" }), Some(0));
    f.complete(&fi, "build", &[], json!({ "artifact": 1 }));
    let t = f.receipt(&fi, "probe", &[], json!({ "sha": "b" }), Some(0));
    let s = f.status(&fi, t);
    assert_eq!(state(&s, "build"), State::Stale);
    assert_eq!(unmet(&s, "build"), vec![Reason::InputChanged { input: "probe".into() }]);
    assert_eq!(unmet(&s, "deploy"), vec![needs("build")]);
    assert_eq!(enabled(&f.next(&fi, t)), vec!["build"], "a stale receipt is redone");
    f.complete(&fi, "build", &[], json!({ "artifact": 2 }));
    assert_eq!(state(&f.status(&fi, t), "deploy"), State::Ready);
}

#[test]
fn an_observation_with_no_exit_status_or_one_outside_its_exit_map_fails_closed() {
    let mut f = fx(VERIFY);
    let fi = f.begin(verify_params(true));
    let t = f.observe(&fi, "pin", &[], 7);
    let s = f.status(&fi, t);
    assert_eq!(state(&s, "pin"), State::Failed);
    assert_eq!(unmet(&s, "pin"), vec![Reason::ExitUnmapped { exit: Some(7) }]);
    assert_eq!(state(&s, "check"), State::Blocked);

    let fi = f.begin(verify_params(true));
    let t = f.receipt(&fi, "pin", &[], Value::Null, None);
    let s = f.status(&fi, t);
    assert_eq!(unmet(&s, "pin"), vec![Reason::ExitUnmapped { exit: None }]);
    assert_eq!(state(&s, "check"), State::Blocked);
}

#[test]
fn exercise_applies_only_when_a_surface_is_declared() {
    // V4: the surface observation reads the param; with none declared, exercise never runs.
    let mut f = fx(VERIFY);
    let fi = f.begin(verify_params(false));
    f.observe(&fi, "surface", &[], 1);
    let t = f.observe(&fi, "pin", &[], 0);
    f.complete(&fi, "check", &[], json!({}));
    let s = f.status(&fi, t);
    assert_eq!(
        unmet(&s, "exercise"),
        vec![Reason::Switch { on: "surface".into(), took: Some("none".into()), case: "declared".into() }]
    );

    let fi = f.begin(verify_params(true));
    f.observe(&fi, "surface", &[], 0);
    let t = f.observe(&fi, "pin", &[], 0);
    f.complete(&fi, "check", &[], json!({}));
    let n = f.next(&fi, t);
    assert_eq!(enabled(&n), vec!["exercise"]);
    assert_eq!(n.enabled[0].launch.skill.as_deref(), Some("dogfood"));
    assert_eq!(n.enabled[0].launch.performer.as_deref(), Some("agent:verifier"));
}

#[test]
fn a_stale_observation_inside_a_repeat_blocks_what_needs_the_repeat() {
    let mut f = fx("flow: f
version: 1
nodes:
  ci: {class: observe, run: [x], fresh_for: 1m, exit_map: {0: green, 1: red}}
  land: {class: effect, reconcile: [x], needs: [poll]}
repeats:
  poll: {body: [ci], max: 5, until: '.nodes.ci.value == \"green\"'}
");
    let fi = f.begin(json!({}));
    let t = f.observe(&fi, "ci", &[("poll", 0)], 0);
    f.verdict(&fi, "poll", 0, true);
    assert_eq!(state(&f.status(&fi, t + SignedDuration::from_secs(30)), "land"), State::Ready);
    let s = f.status(&fi, t + mins(2));
    assert_eq!(unmet(&s, "land"), vec![Reason::Stale { node: "ci".into() }]);
}

#[test]
fn an_observation_outside_a_repeat_reads_a_body_node_at_its_final_iteration() {
    let mut f = fx("flow: f
version: 1
nodes:
  build: {class: work, verify: [x]}
  ci: {class: observe, run: [x], needs: [build], inputs: [build], exit_map: {0: green}}
  land: {class: work, verify: [x], needs: [ci]}
repeats:
  r: {body: [build], max: 2, until: 'true'}
");
    let fi = f.begin(json!({}));
    f.complete(&fi, "build", &[("r", 0)], json!({ "sha": "a" }));
    f.verdict(&fi, "r", 0, true);
    let t = f.observe(&fi, "ci", &[], 0);
    let s = f.status(&fi, t);
    assert_eq!(state(&s, "ci"), State::Done);
    assert_eq!(state(&s, "land"), State::Ready);
}

#[test]
fn claims_leases_retry_and_cancel_decide_waiting_ready_and_failed() {
    let mut f = fx(
        "flow: f\nversion: 1\nnodes:\n  ci: {class: work, verify: [x], retry: {attempts: 2}, wait: {external: CI checks, heartbeat: 5m}}\n  long: {class: work, verify: [x], cancel: {after: 30m}}\n  gaps: {class: work, verify: [x]}\n",
    );
    let fi = f.begin(json!({}));
    let lease = Timestamp::now() + mins(10);
    let t = f.claim(&fi, "ci", 1, lease);
    let s = f.status(&fi, t);
    assert_eq!(state(&s, "ci"), State::Waiting);
    assert!(matches!(s.nodes["ci"].waits[0], Reason::Claimed { attempt: 1, .. }));
    assert_eq!(s.nodes["ci"].waits[1], Reason::External { waits_for: "CI checks".into(), heartbeat_secs: 300 });
    assert_eq!(state(&f.status(&fi, lease), "ci"), State::Ready, "a lease ends at its instant");

    // The lease passed: the node is released, and the next claim is attempt 2.
    let later = t + mins(20);
    let n = f.next(&fi, later);
    assert_eq!(enabled(&n), vec!["ci", "gaps", "long"]);
    assert_eq!(n.enabled[0].attempt, 2);
    // A claim that does not number above the last counts for nothing.
    f.claim(&fi, "ci", 1, later + mins(30));
    f.claim(&fi, "ci", 2, later + mins(1));
    let s = f.status(&fi, later + mins(5));
    assert_eq!(state(&s, "ci"), State::Failed, "the second lapsed attempt uses the two retry allows");
    assert_eq!(unmet(&s, "ci"), vec![Reason::AttemptsUsed { attempts: 2 }]);

    // cancel.after ends an attempt whatever its heartbeats say, at its instant.
    let t = f.claim(&fi, "long", 1, Timestamp::now() + mins(10));
    f.heartbeat(&fi, "long", 1, t + mins(60));
    f.heartbeat(&fi, "long", 1, t + mins(15));
    f.heartbeat(&fi, "long", 9, t + mins(90));
    assert_eq!(state(&f.status(&fi, t + mins(20)), "long"), State::Waiting, "the heartbeat extended the lease");
    assert_eq!(state(&f.status(&fi, t + mins(30)), "long"), State::Ready, "cancelled at 30m, so released");

    // Attempts number on from the last one; when the numbers run out, the node fails closed.
    let t = f.claim(&fi, "gaps", 1, Timestamp::now());
    f.claim(&fi, "gaps", 3, t);
    assert_eq!(f.next(&fi, t + mins(1)).enabled.iter().find(|e| e.node == "gaps").map(|e| e.attempt), Some(4));
    f.claim(&fi, "gaps", u32::MAX, t);
    let s = f.status(&fi, t + mins(1));
    assert_eq!(state(&s, "gaps"), State::Failed);
    assert_eq!(unmet(&s, "gaps"), vec![Reason::AttemptsExhausted]);
    assert_eq!(
        rejected(&s),
        vec![rej("ci", "attempt_not_after_last"), rej("long", "lease_not_extended"), rej("long", "no_such_attempt")],
        "and a heartbeat never shortens a lease"
    );
}

#[test]
fn replay_is_deterministic() {
    let mut f = fx(ROUNDS);
    let fi = f.begin(json!({}));
    f.complete(&fi, "review", &[("rounds", 0)], json!({ "verdict": "fail", "notes": ["a", "b"] }));
    f.complete(&fi, "fix", &[("rounds", 0)], json!({}));
    f.verdict(&fi, "rounds", 0, false);
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
        assert!(s.rejected.is_empty(), "{:?}", s.rejected);
        assert_eq!(state(&s, "ask"), if now == t { State::Waiting } else { State::Blocked });
        assert_eq!(enabled(&n), if now == t { Vec::<&str>::new() } else { vec!["pin"] });
    }
    assert!(!marker.exists(), "replay ran a command");
    assert_eq!(snapshot(&f.root), before, "a read wrote under the flows root");
}

#[test]
fn an_event_naming_an_undeclared_node_or_repeat_counts_for_nothing_whatever_its_kind() {
    let mut f = fx(CHAIN);
    let fi = f.begin(json!({}));
    let ghost = |kind: &str| {
        let mut body = json!({ "node": "ghost", "iteration": {}, "attempt": 1 });
        match kind {
            "Completed" => {}
            "Chosen" => body["edge"] = json!("x"),
            "Claimed" => {
                body["lease_until"] = json!("2026-11-07T00:00:00Z");
                body["token_sha256"] = json!("sha256:00");
            }
            "Heartbeat" => body["lease_until"] = json!("2026-11-07T00:00:00Z"),
            _ => {
                body["code"] = json!("x");
                body["reason"] = json!("x");
            }
        }
        NewEvent::new(kind, body)
    };
    for kind in ["Completed", "Chosen", "Claimed", "Heartbeat", "Refused"] {
        f.append(&fi, ghost(kind));
    }
    f.verdict(&fi, "ghost", 0, true);
    let refused = Refused { node: "a".into(), iteration: iter(&[]), attempt: 1, code: "x".into(), reason: "x".into() };
    f.append(&fi, refused.to_event().unwrap());
    let s = f.try_status(&fi).unwrap();
    let ghosts: Vec<_> =
        ["Completed", "Chosen", "Claimed", "Heartbeat", "Refused"].map(|_| rej("ghost", "unknown_node")).into();
    assert_eq!(rejected(&s)[..5], ghosts[..]);
    assert_eq!(rejected(&s)[5..], [rej("ghost", "unknown_repeat")]);
    assert_eq!(state(&s, "a"), State::Ready);
}

#[test]
fn an_instance_that_does_not_replay_is_an_error_never_an_empty_flow() {
    let mut f = fx(CHAIN);
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
    let body = json!({ "node": "a", "iteration": {}, "attempt": 1, "code": "x", "reason": "x" });
    f.append(&fi, NewEvent::new("Teleported", body));
    assert_eq!(f.try_status(&fi).unwrap_err(), "malformed_instance");

    // A first event that is not Started, even with a Started body.
    let body = started(&f).to_event().unwrap().body;
    let fi = fresh(&mut f, "fi_not_started", NewEvent::new("Claimed", body));
    assert_eq!(f.try_status(&fi).unwrap_err(), "malformed_instance");

    // A pinned IR that does not match its hash.
    let mut s = started(&f);
    s.ir.nodes.remove("c");
    let fi = fresh(&mut f, "fi_hash", s.to_event().unwrap());
    assert_eq!(f.try_status(&fi).unwrap_err(), "malformed_instance");

    // A pinned IR that no longer validates, however its hash was made.
    let mut s = started(&f);
    s.ir.nodes.get_mut("a").unwrap().needs.push(Need::Node { node: "c".into() });
    s.ir_hash = content_hash(&s.ir).unwrap();
    let fi = fresh(&mut f, "fi_cycle", s.to_event().unwrap());
    assert_eq!(f.try_status(&fi).unwrap_err(), "malformed_instance");

    // A pinned IR of another version, and a Started naming another flow.
    let mut s = started(&f);
    s.ir.ir_version = IR_VERSION + 1;
    s.ir_hash = content_hash(&s.ir).unwrap();
    let fi = fresh(&mut f, "fi_version", s.to_event().unwrap());
    assert_eq!(f.try_status(&fi).unwrap_err(), "malformed_instance");
    let mut s = started(&f);
    s.flow = "other@7".into();
    let fi = fresh(&mut f, "fi_flow", s.to_event().unwrap());
    assert_eq!(f.try_status(&fi).unwrap_err(), "malformed_instance");

    // And the untouched pin replays.
    let first = started(&f).to_event().unwrap();
    let fi = fresh(&mut f, "fi_ok", first);
    assert_eq!(state(&f.try_status(&fi).unwrap(), "a"), State::Ready);
}
