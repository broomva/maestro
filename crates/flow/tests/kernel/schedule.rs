//! BRO-3633 PR 3b1: `start`, and the read-only scheduler. Fixture `Completed`, `Chosen`,
//! `Claimed`, `Heartbeat` and `Guarded` events go through the storage API, as their writers
//! will; a fixture receipt records the input digests `next` names for it, as `complete` will. The
//! clock is passed in: no test sleeps.

use crate::common::*;
use jiff::{SignedDuration, Timestamp};
use maestro_flow::agents::LaunchSpec;
use maestro_flow::kernel::events::{Chosen, Completed, Guarded, KernelEvent, Refused, Started, VerifyRecord};
use maestro_flow::kernel::ir::{IR_VERSION, MAX_DEPTH, Need, content_hash};
use maestro_flow::kernel::{
    CompiledFlow, Iteration, Next, Reason, RepeatState, State, Status, compile, next, start, status,
};
use maestro_flow::{InstanceId, Ledger, NewEvent, Options, Reader};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const VERIFY: &str = include_str!("../../flows/verify.yaml");

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
        start(&mut self.ledger, &agent("a", "m"), &self.flow, params).unwrap().instance
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
        // As `complete` writes it: an observation's run starts just before its append, and its
        // freshness counts from that start.
        let ran = Timestamp::now();
        let verify = exit.map(|exit| VerifyRecord {
            argv: vec!["x".into()],
            exit,
            stdout_sha256: "sha256:00".into(),
            ran_at: ran.to_string(),
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
        let at = self.append(fi, c.to_event().unwrap());
        if exit.is_some() { ran.min(at) } else { at }
    }

    fn choose(&mut self, fi: &InstanceId, node: &str, at: &[(&str, u32)], edge: &str) {
        let inputs = self.inputs(fi, node);
        let (iteration, edge) = (iter(at), edge.into());
        let c =
            Chosen { node: node.into(), iteration, attempt: 1, edge, inputs, tool_use_id: None, guard_digest: None };
        self.append(fi, c.to_event().unwrap());
    }

    fn verdict(&mut self, fi: &InstanceId, repeat: &str, iteration: u32, holds: bool) {
        self.append(fi, Guarded { repeat: repeat.into(), iteration, holds, digest: None }.to_event().unwrap());
    }

    /// Rewrites the `at` of the events numbered in `edits`, keeping the chain whole.
    fn rewrite_at(&self, fi: &InstanceId, edits: &[(u64, &str)]) {
        let path = self.root.join("instances").join(fi.as_str()).join("events.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        let mut prev: Option<String> = None;
        let mut out = String::new();
        for line in text.lines() {
            let mut ev: maestro_flow::Event = serde_json::from_str(line).unwrap();
            if let Some((_, at)) = edits.iter().find(|(seq, _)| *seq == ev.seq) {
                ev.at = at.to_string();
            }
            ev.prev_event = prev.clone();
            let l = serde_json::to_string(&ev).unwrap();
            prev = Some(maestro_flow::log::line_hash(l.as_bytes()));
            out.push_str(&l);
            out.push('\n');
        }
        std::fs::write(&path, out).unwrap();
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
        let err = start(&mut f.ledger, &agent("a", "m"), &f.flow, params).unwrap_err();
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
  depth: {class: choice, edges: {quick: {guard: 'true'}, deep: {guard: '.params != null'}}, performer: agent:verifier}
  skim: {class: work, verify: [x], switch: {on: depth, case: quick}}
  dig: {class: work, verify: [x], switch: {on: depth, case: deep}, performer: agent:verifier, skill: deep-research, routing: {provider: codex}}
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
    assert_eq!(launch.performer.as_deref(), Some("agent:verifier"));
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
  merge: {class: effect, run: [x], needs: [rounds], reconcile: [x]}
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
    assert!(!s.repeats["rounds"].awaiting_verdict, "iteration 1 has not settled");
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
        s.nodes["escalate"].unmet,
        vec![Reason::Unfiled { ask: "The review did not converge. Merge anyway?".into(), ask_id: None, ledger: None }]
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

    // Receipts downstream are not done while pin is stale, however far down.
    f.complete(&fi, "check", &[], json!({}));
    f.complete(&fi, "exercise", &[], json!({}));
    let s = f.status(&fi, t + mins(9));
    assert_eq!((state(&s, "check"), state(&s, "exercise")), (State::Done, State::Done));
    let s = f.status(&fi, t + mins(11));
    assert_eq!((state(&s, "check"), state(&s, "exercise")), (State::Stale, State::Stale));
    assert_eq!(unmet(&s, "check"), vec![Reason::Stale { node: "pin".into() }]);
    assert_eq!(unmet(&s, "exercise"), vec![Reason::Stale { node: "pin".into() }]);
    assert_eq!(enabled(&f.next(&fi, t + mins(11))), vec!["pin"], "pin is re-run; its dependents wait for it");

    // A fresh observation with the same result re-validates them without a re-run.
    let t2 = f.observe(&fi, "pin", &[], 0);
    let s = f.status(&fi, t2 + mins(1));
    assert_eq!((state(&s, "check"), state(&s, "exercise")), (State::Done, State::Done));
    // One stamped after the read's clock is stale too.
    assert_eq!(state(&f.status(&fi, t2 - mins(1)), "pin"), State::Stale);
}

#[test]
fn only_the_stale_observation_furthest_upstream_is_re_run() {
    let mut f = fx(
        "flow: f\nversion: 1\nnodes:\n  o1: {class: observe, run: [x], fresh_for: 1m, exit_map: {0: ok}}\n  o2: {class: observe, run: [x], fresh_for: 1m, needs: [o1], exit_map: {0: ok}}\n",
    );
    let fi = f.begin(json!({}));
    f.observe(&fi, "o1", &[], 0);
    let t = f.observe(&fi, "o2", &[], 0);
    assert_eq!(enabled(&f.next(&fi, t + mins(5))), vec!["o1"], "o2 waits for o1's re-run");
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
    // The same output as before: only its inputs are new, and that is enough.
    f.complete(&fi, "build", &[], json!({ "artifact": 1 }));
    assert_eq!(state(&f.status(&fi, t), "deploy"), State::Ready);

    // An observation's value is part of its digest, even with no outputs; flipping back to the
    // value a receipt consumed re-validates it.
    let mut f = fx(
        "flow: f\nversion: 1\nnodes:\n  probe: {class: observe, run: [x], fresh_for: 1h, exit_map: {0: ok, 1: bad}}\n  build: {class: pure, run: [x], needs: [probe]}\n",
    );
    let fi = f.begin(json!({}));
    f.observe(&fi, "probe", &[], 0);
    f.complete(&fi, "build", &[], json!({}));
    let t = f.observe(&fi, "probe", &[], 1);
    assert_eq!(unmet(&f.status(&fi, t), "build"), vec![Reason::InputChanged { input: "probe".into() }]);
    let t = f.observe(&fi, "probe", &[], 0);
    assert_eq!(state(&f.status(&fi, t), "build"), State::Done);
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
    // The launch spec: the verifier agent, as each harness launches it, for the node's skill.
    let verifier = maestro_flow::agents::find("verifier").unwrap();
    assert_eq!(n.enabled[0].launch_spec, Some(LaunchSpec::Agent(Box::new(verifier.launch(Some("dogfood"))))));
    let json = serde_json::to_value(&n.enabled[0]).unwrap();
    assert_eq!(json["launch_spec"]["kind"], "agent");
    assert_eq!(
        json["launch_spec"]["claude"],
        json!({ "file": ".claude/agents/verifier.md", "tool": "Agent", "subagent_type": "verifier" })
    );
    assert_eq!(json["launch_spec"]["codex"]["tool"], "spawn_agent");
    assert_eq!(json["launch_spec"]["skill"], "dogfood");
}

#[test]
fn a_node_with_no_agent_performer_has_no_launch_spec() {
    let mut f = fx(VERIFY);
    let fi = f.begin(verify_params(true));
    let n = f.next(&fi, Timestamp::now());
    assert_eq!(enabled(&n), vec!["pin", "surface"]);
    assert!(n.enabled.iter().all(|e| e.launch_spec.is_none()));
    let json = serde_json::to_value(&n).unwrap();
    assert!(json["enabled"].as_array().unwrap().iter().all(|e| e.get("launch_spec").is_none()), "{json}");
}

#[test]
fn an_agent_this_build_lacks_is_an_unknown_agent_launch_spec_never_none() {
    // An instance a build with the agent started: its pinned IR names `agent:ghost`.
    let mut f = fx(SWITCH);
    let mut ir = f.flow.ir().clone();
    for node in ir.nodes.values_mut() {
        if let maestro_flow::kernel::ir::Class::Choice { launch, .. } = &mut node.class {
            launch.performer = Some("agent:ghost".into());
        }
    }
    let started =
        Started { flow: f.flow.name(), ir_hash: content_hash(&ir).unwrap(), ir, params: json!({}), owner: None };
    let fi = id("fi_ghost");
    f.ledger.lock().unwrap().append(&fi, 0, started.to_event().unwrap()).unwrap();
    let n = f.next(&fi, Timestamp::now());
    assert_eq!(enabled(&n), vec!["depth"]);
    assert_eq!(
        n.enabled[0].launch_spec,
        Some(LaunchSpec::UnknownAgent {
            agent: "ghost".into(),
            reason: "this maestro has no agent source agents/ghost.agent.yaml".into()
        })
    );
}

#[test]
fn a_stale_observation_inside_a_repeat_blocks_what_needs_the_repeat() {
    let mut f = fx("flow: f
version: 1
nodes:
  ci: {class: observe, run: [x], fresh_for: 1m, exit_map: {0: green, 1: red}}
  land: {class: effect, run: [x], reconcile: [x], needs: [poll]}
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

const POLL: &str = "flow: f
version: 1
nodes:
  ci: {class: observe, run: [x], fresh_for: 1h, exit_map: {0: green, 1: red}}
  land: {class: effect, run: [x], reconcile: [x], needs: [poll]}
  notify: {class: work, verify: [x], needs: [poll]}
  ack: {class: work, verify: [x], needs: [notify]}
repeats:
  poll: {body: [ci], max: 5, until: '.nodes.ci.value == \"green\"'}
";

#[test]
fn a_re_run_that_changes_a_finished_repeat_s_body_takes_its_verdict_back() {
    let mut f = fx(POLL);
    let fi = f.begin(json!({}));
    f.observe(&fi, "ci", &[("poll", 0)], 0);
    f.verdict(&fi, "poll", 0, true);
    f.complete(&fi, "notify", &[], json!({}));
    let s = f.status(&fi, Timestamp::now());
    assert_eq!(
        (s.repeats["poll"].state, state(&s, "land"), state(&s, "notify")),
        (RepeatState::Done, State::Ready, State::Done)
    );

    // The same result again keeps the verdict and what consumed it.
    f.observe(&fi, "ci", &[("poll", 0)], 0);
    let s = f.status(&fi, Timestamp::now());
    assert_eq!((s.repeats["poll"].state, state(&s, "notify")), (RepeatState::Done, State::Done));

    // Red: the verdict judged another body, so the repeat waits for a new one and nothing that
    // needs it, done or not, stands (ack's receipt evaluated notify first, so it was kept).
    f.complete(&fi, "ack", &[], json!({}));
    f.observe(&fi, "ci", &[("poll", 0)], 1);
    let s = f.status(&fi, Timestamp::now());
    assert_eq!(s.repeats["poll"].state, RepeatState::Running);
    assert!(s.repeats["poll"].awaiting_verdict);
    assert_eq!(unmet(&s, "land"), vec![Reason::RepeatRunning { repeat: "poll".into() }]);
    assert_eq!(unmet(&s, "notify"), vec![Reason::RepeatRunning { repeat: "poll".into() }]);
    f.verdict(&fi, "poll", 0, false);
    assert_eq!(f.status(&fi, Timestamp::now()).repeats["poll"].iteration, 1);
}

#[test]
fn a_body_node_made_stale_by_its_input_takes_the_verdict_back() {
    let mut f = fx("flow: f
version: 1
nodes:
  head: {class: observe, run: [x], fresh_for: 1h, exit_map: {0: a, 1: b}}
  ci: {class: work, verify: [x], needs: [head]}
  land: {class: work, verify: [x], needs: [poll]}
repeats:
  poll: {body: [ci], max: 5, until: 'true'}
");
    let fi = f.begin(json!({}));
    f.observe(&fi, "head", &[], 0);
    f.complete(&fi, "ci", &[("poll", 0)], json!({}));
    f.verdict(&fi, "poll", 0, true);
    assert_eq!(state(&f.status(&fi, Timestamp::now()), "land"), State::Ready);
    let t = f.observe(&fi, "head", &[], 1);
    let s = f.status(&fi, t);
    assert_eq!(unmet(&s, "ci"), vec![Reason::InputChanged { input: "head".into() }]);
    assert_eq!(s.repeats["poll"].state, RepeatState::Running, "the verdict judged a body that no longer stands");
    assert_eq!(state(&s, "land"), State::Blocked);
}

#[test]
fn a_receipt_that_needs_a_repeat_consumes_its_body() {
    let mut f = fx(POLL);
    let fi = f.begin(json!({}));
    f.observe(&fi, "ci", &[("poll", 0)], 0);
    f.verdict(&fi, "poll", 0, true);
    f.complete(&fi, "notify", &[], json!({}));
    // A body that changed and was judged again: notify's receipt is out of date.
    f.receipt(&fi, "ci", &[("poll", 0)], json!({ "run": 2 }), Some(0));
    f.verdict(&fi, "poll", 0, true);
    let s = f.status(&fi, Timestamp::now());
    assert_eq!(s.repeats["poll"].state, RepeatState::Done);
    assert_eq!(unmet(&s, "notify"), vec![Reason::InputChanged { input: "poll".into() }]);
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
  push: {{class: effect, run: [touch, '{m}'], reconcile: [touch, '{m}'], preconditions: ['true'], needs: [build]}}
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
        owner: None,
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
    // The version is the IR's shape: version 1 carries no workdir and version 2 carries one.
    for (name, version, workdir) in
        [("fi_v1_dir", 1, Some("/tmp")), ("fi_v2_none", 2, None), ("fi_v2_dir", 2, Some("/tmp"))]
    {
        let mut s = started(&f);
        (s.ir.ir_version, s.ir.workdir) = (version, workdir.map(String::from));
        s.ir_hash = content_hash(&s.ir).unwrap();
        let fi = fresh(&mut f, name, s.to_event().unwrap());
        let want = if name == "fi_v2_dir" { Ok(State::Ready) } else { Err("malformed_instance".to_string()) };
        assert_eq!(f.try_status(&fi).map(|s| state(&s, "a")), want, "{name}");
    }
    let mut s = started(&f);
    s.flow = "other@7".into();
    let fi = fresh(&mut f, "fi_flow", s.to_event().unwrap());
    assert_eq!(f.try_status(&fi).unwrap_err(), "malformed_instance");

    // And the untouched pin replays.
    let first = started(&f).to_event().unwrap();
    let fi = fresh(&mut f, "fi_ok", first);
    assert_eq!(state(&f.try_status(&fi).unwrap(), "a"), State::Ready);
}

/// A chain `depth` deep, counted in nodes and repeats, within the node cap: each node is the body
/// of its own repeat and needs the repeat before it (n0, r0, n1, r1, ...); an odd depth ends with
/// a node outside any repeat.
fn deep(depth: usize) -> (String, String) {
    let mut y = String::from("flow: f\nversion: 1\nnodes:\n");
    let mut repeats = String::from("repeats:\n");
    let nodes = depth.div_ceil(2);
    for k in 0..nodes {
        let needs = if k == 0 { String::new() } else { format!(", needs: [r{}]", k - 1) };
        y.push_str(&format!("  n{k}: {{class: pure, run: [x]{needs}}}\n"));
        if 2 * k + 2 <= depth {
            repeats.push_str(&format!("  r{k}: {{body: [n{k}], max: 2}}\n"));
        }
    }
    (format!("{y}{repeats}"), format!("n{}", nodes - 1))
}

#[test]
fn the_deepest_chain_a_definition_may_have_replays_on_a_small_stack() {
    assert_eq!(compile(&deep(MAX_DEPTH + 1).0).map(|_| ()).map_err(|e| e.code()), Err("definition_invalid"));
    let (yaml, last) = deep(MAX_DEPTH);
    let mut f = fx(&yaml);
    let fi = f.begin(json!({}));
    let root = f.root.clone();
    let replayed = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || {
            let reader = Reader::new(&root, Options::default()).unwrap();
            status(&reader.lock().unwrap(), &fi, Timestamp::now()).unwrap().nodes[&last].state
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(replayed, State::Blocked);
}

#[test]
fn a_stale_observation_anywhere_in_a_finished_body_blocks_what_needs_the_repeat() {
    let mut f = fx("flow: f
version: 1
nodes:
  w: {class: work, verify: [x]}
  ci: {class: observe, run: [x], fresh_for: 1m, exit_map: {0: green}}
  land: {class: work, verify: [x], needs: [poll]}
  after: {class: work, verify: [x], needs: [land]}
repeats:
  poll: {body: [w, ci], max: 5, until: 'true'}
");
    let fi = f.begin(json!({}));
    f.complete(&fi, "w", &[("poll", 0)], json!({}));
    let t = f.observe(&fi, "ci", &[("poll", 0)], 0);
    f.verdict(&fi, "poll", 0, true);
    f.complete(&fi, "land", &[], json!({}));
    // Taking after's receipt evaluates land at that event's clock; the read later must not reuse it.
    f.complete(&fi, "after", &[], json!({}));
    assert_eq!(state(&f.status(&fi, t), "land"), State::Done);
    let s = f.status(&fi, t + mins(5));
    assert_eq!(unmet(&s, "land"), vec![Reason::Stale { node: "ci".into() }]);
}

#[test]
fn a_second_verdict_on_a_body_already_judged_is_set_aside() {
    let mut f = fx(POLL);
    let fi = f.begin(json!({}));
    f.observe(&fi, "ci", &[("poll", 0)], 0);
    f.verdict(&fi, "poll", 0, true);
    f.complete(&fi, "land", &[], json!({}));
    f.verdict(&fi, "poll", 0, false);
    let s = f.status(&fi, Timestamp::now());
    assert_eq!(rejected(&s), vec![rej("poll", "already_judged")]);
    assert_eq!((s.repeats["poll"].state, state(&s, "land")), (RepeatState::Done, State::Done));
}

#[test]
fn a_done_node_takes_a_new_receipt_only_if_it_was_stale_at_that_receipt_s_stamp() {
    let flow = "flow: f\nversion: 1\nnodes:\n  o: {class: observe, run: [x], fresh_for: 1h, exit_map: {0: ok}}\n  a: {class: pure, run: [x], needs: [o]}\n  b: {class: pure, run: [x], needs: [a]}\n";
    // Fresh upstream: the second receipt is set aside.
    let mut f = fx(flow);
    let fi = f.begin(json!({}));
    f.observe(&fi, "o", &[], 0);
    f.complete(&fi, "a", &[], json!({ "v": 1 }));
    f.complete(&fi, "a", &[], json!({ "v": 2 }));
    assert_eq!(rejected(&f.status(&fi, Timestamp::now())), vec![rej("a", "already_done")]);

    // The observation was two hours old when the second receipt was stamped: a was stale then,
    // so the new receipt replaces the old one and b, recorded against the old one, must be redone.
    let mut f = fx(flow);
    let fi = f.begin(json!({}));
    f.observe(&fi, "o", &[], 0);
    f.complete(&fi, "a", &[], json!({ "v": 1 }));
    f.complete(&fi, "b", &[], json!({}));
    let old = (Timestamp::now() - SignedDuration::from_hours(2)).to_string();
    f.rewrite_at(&fi, &[(2, &old), (3, &old), (4, &old)]);
    f.complete(&fi, "a", &[], json!({ "v": 2 }));
    let s = f.status(&fi, Timestamp::now());
    assert!(rejected(&s).is_empty(), "{:?}", s.rejected);
    assert_eq!(unmet(&s, "b"), vec![Reason::InputChanged { input: "a".into() }, Reason::Stale { node: "o".into() }]);
}

#[test]
fn freshness_is_measured_to_the_second() {
    let mut f = fx("flow: f\nversion: 1\nnodes:\n  o: {class: observe, run: [x], fresh_for: 90s, exit_map: {0: ok}}\n");
    let fi = f.begin(json!({}));
    let t = f.observe(&fi, "o", &[], 0);
    assert_eq!(state(&f.status(&fi, t + SignedDuration::from_secs(90)), "o"), State::Done);
    assert_eq!(state(&f.status(&fi, t + SignedDuration::from_secs(91)), "o"), State::Stale);
}

#[test]
fn an_event_stamped_with_something_that_is_not_a_time_is_malformed() {
    let mut f = fx(CHAIN);
    let fi = f.begin(json!({}));
    f.complete(&fi, "a", &[], json!({}));
    f.rewrite_at(&fi, &[(2, "yesterday")]);
    assert_eq!(f.try_status(&fi).unwrap_err(), "malformed_instance");
}

#[test]
#[ignore = "a measurement at the caps; run with --release --ignored"]
fn a_log_at_the_event_cap_replays_inert_events_without_evaluating_them() {
    use maestro_flow::log::MAX_EVENTS;
    // A's reproduction, at the caps: the largest repeat a definition may hold, and heartbeats up
    // to the event cap, written as the ledger writes them (one by one would take minutes).
    let mut yaml = String::from("flow: f\nversion: 1\nnodes:\n  outside: {class: pure, run: [x]}\n");
    for i in 0..63 {
        yaml.push_str(&format!("  n{i}: {{class: pure, run: [x]}}\n"));
    }
    let body: Vec<String> = (0..63).map(|i| format!("n{i}")).collect();
    yaml.push_str(&format!("repeats:\n  r: {{max: 2, body: [{}]}}\n", body.join(", ")));
    let mut f = fx(&yaml);
    let fi = f.begin(json!({}));
    let path = f.root.join("instances").join(fi.as_str()).join("events.jsonl");
    let mut text = std::fs::read_to_string(&path).unwrap();
    let mut prev = maestro_flow::log::line_hash(text.trim_end().as_bytes());
    for seq in 2..=MAX_EVENTS {
        let body = json!({ "node": "outside", "iteration": {}, "attempt": 1, "lease_until": "2026-11-08T00:00:00Z" });
        let ev =
            json!({ "seq": seq, "prev_event": prev, "at": "2026-11-08T00:00:00Z", "kind": "Heartbeat", "body": body });
        let line = serde_json::to_string(&ev).unwrap();
        prev = maestro_flow::log::line_hash(line.as_bytes());
        text.push_str(&line);
        text.push('\n');
    }
    std::fs::write(&path, &text).unwrap();
    let s = f.status(&fi, Timestamp::now());
    assert_eq!(s.seq, MAX_EVENTS);
    assert!(s.rejected.is_empty());
    assert_eq!(state(&s, "outside"), State::Ready);
    let more = maestro_flow::NewEvent::new("Heartbeat", json!({}));
    assert_eq!(f.ledger.lock().unwrap().append(&fi, MAX_EVENTS, more).unwrap_err().code(), "too_many_events");
}

#[test]
fn a_node_needing_a_body_node_from_outside_sees_its_repeat_reopen() {
    let mut f = fx("flow: f
version: 1
nodes:
  ci: {class: observe, run: [x], fresh_for: 1h, exit_map: {0: green, 1: red}}
  w: {class: work, verify: [x]}
  peek: {class: work, verify: [x], needs: [w]}
  ack: {class: work, verify: [x], needs: [peek]}
repeats:
  poll: {body: [ci, w], max: 5, until: 'true'}
");
    let fi = f.begin(json!({}));
    f.observe(&fi, "ci", &[("poll", 0)], 0);
    f.complete(&fi, "w", &[("poll", 0)], json!({}));
    f.verdict(&fi, "poll", 0, true);
    f.complete(&fi, "peek", &[], json!({}));
    f.complete(&fi, "ack", &[], json!({}));
    assert_eq!(state(&f.status(&fi, Timestamp::now()), "peek"), State::Done);
    // Only ci, w's sibling, changes; peek reads w through the repeat, which now waits on a verdict.
    f.observe(&fi, "ci", &[("poll", 0)], 1);
    assert_eq!(unmet(&f.status(&fi, Timestamp::now()), "peek"), vec![Reason::RepeatRunning { repeat: "poll".into() }]);
}

#[test]
fn a_verdict_reaches_what_was_evaluated_before_it() {
    let mut f = fx(POLL);
    let fi = f.begin(json!({}));
    f.observe(&fi, "ci", &[("poll", 0)], 0);
    // Set aside, but evaluated: notify waits on the repeat.
    f.complete(&fi, "notify", &[], json!({}));
    f.verdict(&fi, "poll", 0, true);
    let s = f.status(&fi, Timestamp::now());
    assert_eq!(rejected(&s), vec![rej("notify", "not_enabled")]);
    assert_eq!(state(&s, "notify"), State::Ready);
}

#[test]
fn a_body_that_changes_reaches_what_needs_its_repeat_with_no_clock_involved() {
    let mut f = fx("flow: f
version: 1
params: [p?]
nodes:
  src: {class: observe, run: [x], inputs: [params.p], exit_map: {0: a, 1: b}}
  w: {class: work, verify: [x], needs: [src]}
  notify: {class: work, verify: [x], needs: [poll]}
  ack: {class: work, verify: [x], needs: [notify]}
repeats:
  poll: {body: [w], max: 3, until: 'true'}
");
    let fi = f.begin(json!({}));
    f.observe(&fi, "src", &[], 0);
    f.complete(&fi, "w", &[("poll", 0)], json!({}));
    f.verdict(&fi, "poll", 0, true);
    f.complete(&fi, "notify", &[], json!({}));
    f.complete(&fi, "ack", &[], json!({}));
    f.observe(&fi, "src", &[], 1);
    let s = f.status(&fi, Timestamp::now());
    assert_eq!(unmet(&s, "w"), vec![Reason::InputChanged { input: "src".into() }]);
    assert_eq!(unmet(&s, "notify"), vec![Reason::RepeatRunning { repeat: "poll".into() }]);
}

#[test]
fn a_flow_compiled_at_its_byte_bounds_starts_and_params_have_one_too() {
    use maestro_flow::kernel::ir::{MAX_IR, MAX_PARAMS};
    let one = |arg: usize| {
        format!("flow: f\nversion: 1\nparams: [p?]\nnodes:\n  a: {{class: pure, run: [{}]}}\n", "x".repeat(arg))
    };
    let ir_len = |yaml: &str| serde_json::to_vec(compile(yaml).unwrap().ir()).unwrap().len();
    let base = ir_len(&one(1)) - 1;
    let mut f = fx(&one(MAX_IR - base));
    assert_eq!(serde_json::to_vec(f.flow.ir()).unwrap().len(), MAX_IR);
    // `{"p":"..."}` is 8 bytes around its string.
    let params = |len: usize| json!({ "p": "x".repeat(len - 8) });
    assert_eq!(serde_json::to_vec(&params(MAX_PARAMS)).unwrap().len(), MAX_PARAMS);
    let begun = start(&mut f.ledger, &agent("a", "m"), &f.flow, params(MAX_PARAMS)).unwrap();
    assert!(f.try_status(&begun.instance).is_ok(), "the largest IR with the largest params starts and reads");
    let err = start(&mut f.ledger, &agent("a", "m"), &f.flow, params(MAX_PARAMS + 1)).unwrap_err();
    assert_eq!(err.code(), "invalid_params");
}

/// Fills instance `fi`'s log, after its `Started`, with `events` up to the event cap, written as
/// the ledger writes them (appending them one by one re-verifies the log each time).
fn fill(f: &Fx, fi: &InstanceId, mut events: impl FnMut(u64) -> (String, Value)) {
    let path = f.root.join("instances").join(fi.as_str()).join("events.jsonl");
    let mut text = std::fs::read_to_string(&path).unwrap();
    let mut prev = maestro_flow::log::line_hash(text.trim_end().as_bytes());
    for seq in 2..=maestro_flow::log::MAX_EVENTS {
        let (kind, body) = events(seq);
        let ev = json!({ "seq": seq, "prev_event": prev, "at": "2026-11-08T00:00:00Z", "kind": kind, "body": body });
        let line = serde_json::to_string(&ev).unwrap();
        prev = maestro_flow::log::line_hash(line.as_bytes());
        text.push_str(&line);
        text.push('\n');
    }
    std::fs::write(&path, &text).unwrap();
}

fn observed(node: &str, iteration: Value, exit: i32) -> (String, Value) {
    let verify = json!({ "argv": ["x"], "exit": exit, "stdout_sha256": "s", "ran_at": "x" });
    ("Completed".into(), json!({ "node": node, "iteration": iteration, "attempt": 1, "verify": verify }))
}

/// Dense layers under one clocked observation, at the caps: `head` (with `fresh_for`), then four
/// layers of 15 nodes, each node of a layer needing every node above it (61 nodes, fan-in 15).
fn layers(inside_repeat: bool) -> String {
    let mut y = String::from(
        "flow: f\nversion: 1\nnodes:\n  head: {class: observe, run: [x], fresh_for: 10m, exit_map: {0: a, 1: b}}\n",
    );
    let mut above = vec!["head".to_string()];
    let mut all = vec!["head".to_string()];
    for l in 0..4 {
        let layer: Vec<String> = (0..15).map(|i| format!("l{l}n{i}")).collect();
        for n in &layer {
            y.push_str(&format!("  {n}: {{class: pure, run: [x], needs: [{}]}}\n", above.join(", ")));
        }
        all.extend(layer.iter().cloned());
        above = layer;
    }
    if inside_repeat {
        y.push_str(&format!("repeats:\n  r: {{max: 1000, body: [{}]}}\n", all.join(", ")));
    }
    y
}

fn measure(label: &str, f: &Fx, fi: &InstanceId) {
    let reader = Reader::new(&f.root, Options::default()).unwrap();
    let started = std::time::Instant::now();
    let s = status(&reader.lock().unwrap(), fi, Timestamp::now()).unwrap();
    eprintln!("WORST {label}: {} events, {} set aside, status in {:?}", s.seq, s.rejected.len(), started.elapsed());
    assert_eq!(s.seq, maestro_flow::log::MAX_EVENTS);
}

#[test]
#[ignore = "a measurement at the caps; run with --release --ignored"]
fn worst_case_dense_layers_under_a_clocked_observation() {
    // Every other event flips head's value (every core below must be redone), and the one between
    // asks for a node of the last layer (whose evaluation reads everything above it).
    let mut f = fx(&layers(false));
    let fi = f.begin(json!({}));
    fill(&f, &fi, |seq| match seq % 2 {
        0 => observed("head", json!({}), (seq / 2 % 2) as i32),
        _ => ("Completed".into(), json!({ "node": format!("l3n{}", seq % 15), "iteration": {}, "attempt": 1 })),
    });
    measure("dense layers, head flipping", &f, &fi);
    // B's shape: only the bottom observation re-run, over and over.
    let bottom = "l3n0: {class: observe, run: [x], fresh_for: 1m, exit_map: {0: ok},";
    let mut f = fx(&layers(false).replace("l3n0: {class: pure, run: [x],", bottom));
    let fi = f.begin(json!({}));
    fill(&f, &fi, |seq| if seq == 2 { observed("head", json!({}), 0) } else { observed("l3n0", json!({}), 0) });
    measure("dense layers, bottom re-run", &f, &fi);
}

#[test]
#[ignore = "a measurement at the caps; run with --release --ignored"]
fn worst_case_largest_repeat_body_re_observed() {
    // The same 61 nodes as one repeat body with no until. Re-observing head with a new value
    // redoes every core of the body, and each event checks whether the iteration settled.
    let mut f = fx(&layers(true));
    let fi = f.begin(json!({}));
    fill(&f, &fi, |seq| match seq % 2 {
        0 => observed("head", json!({ "r": 0 }), (seq / 2 % 2) as i32),
        _ => ("Completed".into(), json!({ "node": format!("l3n{}", seq % 15), "iteration": { "r": 0 }, "attempt": 1 })),
    });
    measure("repeat body, head flipping", &f, &fi);
}

fn nested(levels: usize) -> Value {
    (0..levels).fold(json!(1), |v, _| json!([v]))
}

#[test]
fn params_and_outputs_nested_too_deep_to_read_back_are_refused_before_anything_is_written() {
    use maestro_flow::log::MAX_NESTING;
    let mut f = fx("flow: f\nversion: 1\nparams: [p?]\nnodes:\n  a: {class: pure, run: [x]}\n");
    // Codex's reproduction: 140 levels, under the byte bound, once made the instance unreadable.
    // `{"p": ...}` adds a level, and `Started` one more around the params.
    for levels in [140, MAX_NESTING - 1] {
        let err = start(&mut f.ledger, &agent("a", "m"), &f.flow, json!({ "p": nested(levels) })).unwrap_err();
        assert_eq!(err.code(), "invalid_params", "{levels} levels");
    }
    assert!(f.ledger.lock().unwrap().instances().unwrap().is_empty(), "nothing was appended");
    // Params `MAX_NESTING - 1` deep put `Started`'s body exactly at the bound, and it reads back.
    let fi = f.begin(json!({ "p": nested(MAX_NESTING - 2) }));
    assert!(f.try_status(&fi).is_ok());
    // A receipt's outputs are bounded by the same rule at append.
    let c = Completed {
        node: "a".into(),
        iteration: iter(&[]),
        attempt: 1,
        inputs: Default::default(),
        outputs: nested(MAX_NESTING),
        verify: None,
        performer: None,
        tool_use_id: None,
    };
    let mut guard = f.ledger.lock().unwrap();
    let seq = guard.log(&fi).unwrap().seq();
    assert_eq!(guard.append(&fi, seq, c.to_event().unwrap()).unwrap_err().code(), "too_deep");
    drop(guard);
    assert_eq!(f.try_status(&fi).unwrap().seq, 1, "nothing was appended");
}

#[test]
fn terminal_is_the_kernels_no_node_ready_stale_or_waiting() {
    // verify@1 without a surface: terminal only once pin, surface and check are done, exercise
    // blocked by the branch not taken.
    let mut f = fx(VERIFY);
    let fi = f.begin(verify_params(false));
    let s = f.status(&fi, Timestamp::now());
    assert!(!s.terminal, "every root is ready");
    f.observe(&fi, "surface", &[], 1);
    let t = f.observe(&fi, "pin", &[], 0);
    assert!(!f.status(&fi, t).terminal, "check is ready");
    f.complete(&fi, "check", &[], json!({}));
    let s = f.status(&fi, t);
    assert_eq!(state(&s, "exercise"), State::Blocked);
    assert!(s.terminal, "{s:?}");
    assert!(f.next(&fi, t).terminal, "next says what status says");
    // The field the driver skill reads is `terminal`, in both.
    assert_eq!(serde_json::to_value(&s).unwrap()["terminal"], true);
    assert_eq!(serde_json::to_value(f.next(&fi, t)).unwrap()["terminal"], true);
    // Once pin is stale it can be run again: no longer terminal.
    let later = t.checked_add(SignedDuration::from_mins(11)).unwrap();
    assert_eq!(state(&f.status(&fi, later), "pin"), State::Stale);
    assert!(!f.status(&fi, later).terminal);
    assert!(!f.next(&fi, later).terminal);

    // A human node waiting on the owner is not terminal.
    let mut f = fx("flow: h\nversion: 1\nnodes:\n  q: {class: human, ask: Go?}\n");
    let fi = f.begin(json!({}));
    let s = f.status(&fi, Timestamp::now());
    assert_eq!(state(&s, "q"), State::Waiting);
    assert!(!s.terminal);
    // A repeat that runs out routes on: its on_exhausted node is ready, so not terminal.
    let mut f = fx("flow: r
version: 1
nodes:
  ci: {class: observe, run: [x], fresh_for: 1h, exit_map: {0: green, 1: red}}
  give_up: {class: work, verify: [x]}
repeats:
  poll: {body: [ci], max: 1, until: '.nodes.ci.value == \"green\"', on_exhausted: give_up}
");
    let fi = f.begin(json!({}));
    let t = f.observe(&fi, "ci", &[("poll", 0)], 1);
    f.verdict(&fi, "poll", 0, false);
    let s = f.status(&fi, t);
    assert_eq!(state(&s, "give_up"), State::Ready, "{s:?}");
    assert!(!s.terminal);
    f.complete(&fi, "give_up", &[], json!({}));
    assert!(f.status(&fi, t).terminal);
}

#[test]
fn a_launch_spec_carries_the_digest_of_the_current_agent_source() {
    let mut f = fx(VERIFY);
    let fi = f.begin(verify_params(true));
    f.observe(&fi, "surface", &[], 0);
    let t = f.observe(&fi, "pin", &[], 0);
    f.complete(&fi, "check", &[], json!({}));
    let n = f.next(&fi, t);
    let Some(LaunchSpec::Agent(spec)) = &n.enabled[0].launch_spec else { panic!("{n:?}") };
    let source = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../agents/verifier.agent.yaml")).unwrap();
    assert_eq!(spec.source_sha256, maestro_flow::dogfood::sha256(&source));
    assert_eq!(spec.source, "agents/verifier.agent.yaml");
}
