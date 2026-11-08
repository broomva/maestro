//! Cache invalidation, each case a path where a kept core must be dropped (from P20 round 4,
//! stratum B).
use super::*;
use crate::kernel::compile;
use serde_json::json;

struct L {
    started: Started,
    events: Vec<Event>,
}
impl L {
    fn new(yaml: &str) -> L {
        let flow = compile(yaml).unwrap_or_else(|e| panic!("{e}"));
        let started = Started {
            flow: flow.name(),
            ir_hash: flow.hash().into(),
            ir: flow.ir().clone(),
            params: json!({}),
            owner: None,
        };
        let ev = Event {
            seq: 1,
            prev_event: None,
            at: "2026-11-08T00:00:00Z".into(),
            kind: "Started".into(),
            body: serde_json::to_value(&started).unwrap(),
        };
        L { started, events: vec![ev] }
    }
    fn log(&self) -> Log {
        Log { instance: InstanceId::new("fi_x").unwrap(), events: self.events.clone(), last_hash: None }
    }
    fn now() -> Timestamp {
        "2026-11-08T00:00:00Z".parse().unwrap()
    }
    fn status(&self) -> Status {
        let log = self.log();
        let f = Fold::replay(&log, &self.started).unwrap();
        f.read(&log, &self.started, L::now()).0
    }
    fn push(&mut self, kind: &str, body: Value) {
        let seq = self.events.len() as u64 + 1;
        self.events.push(Event { seq, prev_event: None, at: "2026-11-08T00:00:00Z".into(), kind: kind.into(), body });
    }
    fn done(&mut self, node: &str, it: Value) {
        let iter: Iteration = serde_json::from_value(it.clone()).unwrap();
        let inputs =
            self.status().nodes.get(node).filter(|n| n.iteration == iter).map(|n| n.inputs.clone()).unwrap_or_default();
        self.push("Completed", json!({"node": node, "iteration": it, "attempt": 1, "inputs": inputs}));
    }
    fn obs(&mut self, node: &str, it: Value, exit: i32) {
        self.push("Completed", json!({"node": node, "iteration": it, "attempt": 1, "verify": {"argv": ["x"], "exit": exit, "stdout_sha256": "s", "ran_at": "x"}}));
    }
    fn verdict(&mut self, r: &str, i: u32, holds: bool) {
        self.push("Guarded", json!({"repeat": r, "iteration": i, "holds": holds}));
    }
}

// G1 (forget in advance): a repeat that moves on reaches a body node of another repeat that was
// evaluated, in the same advance, while it was still running.
#[test]
fn g1_a_repeat_that_runs_out_in_advance_reaches_another_repeats_body() {
    let mut l = L::new(
        "flow: f
version: 1
nodes:
  x1: {class: work, verify: [x]}
  y1: {class: work, verify: [x], needs: [x1]}
  e1: {class: work, verify: [x]}
repeats:
  r1: {body: [x1], max: 2, on_exhausted: e1}
  r2: {body: [y1], max: 2}
",
    );
    l.done("x1", json!({"r1": 0}));
    l.done("x1", json!({"r1": 1}));
    let s = l.status();
    assert_eq!(s.repeats["r1"].state, RepeatState::Exhausted, "{s:?}");
    assert_eq!(s.nodes["y1"].unmet, vec![Reason::RepeatExhausted { repeat: "r1".into() }], "{:?}", s.nodes["y1"]);
    assert_eq!(s.repeats["r2"].state, RepeatState::Unreachable);
}

// G2 (forget on a false verdict): an on_exhausted route evaluated while its repeat ran becomes
// ready when a false verdict exhausts the repeat.
#[test]
fn g2_a_false_verdict_that_exhausts_a_repeat_reaches_what_was_evaluated_before_it() {
    let mut l = L::new(
        "flow: f
version: 1
params: [p?]
nodes:
  ci: {class: observe, run: [x], inputs: [params.p], exit_map: {0: green, 1: red}}
  give_up: {class: work, verify: [x]}
repeats:
  poll: {body: [ci], max: 1, until: '.nodes.ci.value == \"green\"', on_exhausted: give_up}
",
    );
    l.obs("ci", json!({"poll": 0}), 1);
    l.done("give_up", json!({})); // set aside: the repeat still runs
    l.verdict("poll", 0, false);
    let s = l.status();
    assert_eq!(s.repeats["poll"].state, RepeatState::Exhausted);
    assert_eq!(s.nodes["give_up"].state, State::Ready, "{:?}", s.nodes["give_up"]);
}

// G3 (a switch is a read): a node that only switches on an observation sees its re-run.
#[test]
fn g3_a_node_that_only_switches_on_a_node_sees_its_new_value() {
    let mut l = L::new(
        "flow: f
version: 1
params: [p?]
nodes:
  o: {class: observe, run: [x], inputs: [params.p], exit_map: {0: a, 1: b}}
  s: {class: pure, run: [x], switch: {on: o, case: a}}
  d: {class: pure, run: [x], needs: [s]}
",
    );
    l.obs("o", json!({}), 0);
    l.done("s", json!({}));
    l.done("d", json!({}));
    l.obs("o", json!({}), 1);
    let s = l.status();
    assert_ne!(s.nodes["s"].state, State::Done, "{:?}", s.nodes["s"]);
    assert_ne!(s.nodes["d"].state, State::Done, "{:?}", s.nodes["d"]);
}

// G4 (on_exhausted reads the body): the route sees its repeat reopen after a body change.
#[test]
fn g4_an_on_exhausted_route_sees_its_repeat_reopen() {
    let mut l = L::new(
        "flow: f
version: 1
params: [p?]
nodes:
  ci: {class: observe, run: [x], inputs: [params.p], exit_map: {0: green, 1: red}}
  give_up: {class: work, verify: [x]}
  after: {class: pure, run: [x], needs: [give_up]}
repeats:
  poll: {body: [ci], max: 2, until: '.nodes.ci.value == \"green\"', on_exhausted: give_up}
",
    );
    l.obs("ci", json!({"poll": 0}), 0);
    l.verdict("poll", 0, true);
    l.done("after", json!({})); // set aside: give_up will never run; caches give_up
    l.obs("ci", json!({"poll": 0}), 1);
    let s = l.status();
    assert_eq!(s.repeats["poll"].state, RepeatState::Running);
    assert_eq!(
        s.nodes["give_up"].unmet,
        vec![Reason::RepeatRunning { repeat: "poll".into() }],
        "{:?}",
        s.nodes["give_up"]
    );
}

// G5 (a verdict can settle a repeat with no until): a false verdict that exhausts r1 kills r2's
// last open body node, so r2 settles and must move on with no further event.
#[test]
fn g5_a_verdict_that_settles_another_repeat_moves_it_on() {
    let mut l = L::new(
        "flow: f
version: 1
params: [p?]
nodes:
  a: {class: observe, run: [x], inputs: [params.p], exit_map: {0: g, 1: r}}
  y1: {class: work, verify: [x]}
  y2: {class: work, verify: [x], needs: [a]}
repeats:
  r1: {body: [a], max: 1, until: '.nodes.a.value == \"g\"'}
  r2: {body: [y1, y2], max: 3}
",
    );
    l.obs("a", json!({"r1": 0}), 1);
    l.done("y1", json!({"r2": 0}));
    l.verdict("r1", 0, false);
    let s = l.status();
    assert_eq!(s.repeats["r1"].state, RepeatState::Exhausted);
    assert_eq!(s.repeats["r2"].iteration, 1, "{:?}", s.repeats["r2"]);
}
