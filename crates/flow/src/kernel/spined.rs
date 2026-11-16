//! The accepting spine over real runs (BRO-3636; spec acceptance item 2): instances driven
//! through the kernel's writers, compared with `spine::compare`. A run whose performer was refused
//! and repaired, lost a lease and claimed again, re-ran a lapsed observation, or ran under another
//! session has the same spine as a clean one; a node skipped or reordered, another edge, or
//! another verify exit is a difference. They live in the crate under `cfg(test)` because a lost
//! lease and a lapsed observation need the test clock (see `clock`).

use crate::Reader;
use crate::clock::{self, TestClock};
use crate::kernel::events::{Answered, AskLinked, AskPinned, Completed, KernelEvent};
use crate::kernel::{Choosing, Difference, Side, Spine, choose, claim, compare, complete, heartbeat, spine, start};
use crate::{InstanceId, Options};
// The claim tests' fixture, loaded once in this binary (`clocked`).
use super::clocked::claiming::*;
use maestro_core::session::VerifiedSession;
use serde_json::{Value, json};

/// probe (observe, its exit status from the file `probe`) -> left (pure) and right (work, passes
/// once `ok` exists) -> pick (choice of ship or hold).
const SHAPE: &str = r#"
flow: shape
version: 1
params: [dir]
workdir: "{{params.dir}}"
nodes:
  probe: {class: observe, run: [sh, -c, 'exit "$(cat probe 2>/dev/null || echo 0)"'], fresh_for: 1h, exit_map: {0: up, 1: down}}
  left: {class: pure, needs: [probe], run: ['true']}
  right: {class: work, needs: [probe], verify: [sh, -c, 'test -f ok']}
  pick: {class: choice, needs: [left, right], edges: {ship: {guard: 'true'}, hold: {guard: 'true'}}}
"#;

struct Run<'a> {
    f: &'a mut Fx,
    who: VerifiedSession,
    fi: InstanceId,
}

impl<'a> Run<'a> {
    fn new(f: &'a mut Fx, who: VerifiedSession) -> Run<'a> {
        let params = json!({ "dir": f.dir.to_str().unwrap() });
        let fi = start(&mut f.ledger, &who, &f.flow, params).unwrap().instance;
        Run { f, who, fi }
    }

    /// Claims and completes `node`, which must pass.
    fn done(&mut self, node: &str) {
        let c = claim(&mut self.f.ledger, &self.who, &self.fi, node).unwrap_or_else(|e| panic!("{node}: {e}"));
        let done = complete(&mut self.f.ledger, &self.who, &c.token, Value::Null, &quick()).unwrap();
        assert_eq!(outcome(&done), "completed", "{node}");
    }

    fn pick(&mut self, edge: &str) {
        let c = claim(&mut self.f.ledger, &self.who, &self.fi, "pick").unwrap();
        let chosen = choose(&mut self.f.ledger, &self.who, &c.token, edge).unwrap();
        assert!(matches!(chosen, Choosing::Chosen { .. }), "{chosen:?}");
    }

    /// probe, left, right, pick `edge`.
    fn clean(&mut self, edge: &str) -> InstanceId {
        std::fs::write(self.f.dir.join("ok"), "").unwrap();
        for node in ["probe", "left", "right"] {
            self.done(node);
        }
        self.pick(edge);
        self.fi.clone()
    }
}

fn spine_of(f: &Fx, fi: &InstanceId) -> Spine {
    let reader = Reader::new(&f.root, Options::default()).unwrap();
    spine(&reader.lock().unwrap(), fi).unwrap()
}

fn differ(f: &Fx, a: &InstanceId, b: &InstanceId) -> Option<Difference> {
    compare(&spine_of(f, a), &spine_of(f, b))
}

#[test]
fn a_spine_is_one_record_per_accepted_node_with_its_edge_and_exit() {
    let (mut f, _clock) = (fx(SHAPE), TestClock::install());
    let me = f.me.clone();
    let fi = Run::new(&mut f, me).clean("ship");
    let s = spine_of(&f, &fi);
    assert_eq!((s.instance.as_str(), s.flow.as_str()), (fi.as_str(), "shape@1"));
    let records: Vec<_> = s.records.iter().map(|r| (r.node.as_str(), r.edge.as_deref(), r.exit)).collect();
    assert_eq!(
        records,
        [("probe", None, Some(0)), ("left", None, Some(0)), ("right", None, Some(0)), ("pick", Some("ship"), None)]
    );
    // Nothing but those fields: no attempt, performer, refusal, timestamp or chain.
    let json = serde_json::to_value(&s.records[0]).unwrap();
    assert_eq!(json, json!({ "node": "probe", "iteration": {}, "edge": null, "exit": 0 }));
}

#[test]
fn refusals_repairs_lost_leases_re_runs_and_performers_leave_the_spine_as_it_is() {
    let (mut f, test_clock) = (fx(SHAPE), TestClock::install());
    let me = f.me.clone();
    let clean = Run::new(&mut f, me).clean("ship");

    // Another session; right is refused, its lease lost, claimed again, refused, then repaired;
    // a heartbeat on the way; and probe lapses and is run again before pick.
    let mut run = Run::new(&mut f, agent("other", "marker-9"));
    std::fs::remove_file(run.f.dir.join("ok")).unwrap();
    run.done("probe");
    run.done("left");
    let first = claim(&mut run.f.ledger, &run.who, &run.fi, "right").unwrap();
    let refused = complete(&mut run.f.ledger, &run.who, &first.token, json!({ "try": 1 }), &quick()).unwrap();
    assert_eq!(outcome(&refused), "verify_failed");
    test_clock.set(first.lease_until);
    let second = claim(&mut run.f.ledger, &run.who, &run.fi, "right").unwrap();
    assert_eq!(second.attempt, 2);
    heartbeat(&mut run.f.ledger, &run.who, &second.token).unwrap();
    assert_eq!(
        outcome(&complete(&mut run.f.ledger, &run.who, &second.token, Value::Null, &quick()).unwrap()),
        "verify_failed"
    );
    std::fs::write(run.f.dir.join("ok"), "").unwrap();
    assert_eq!(
        outcome(&complete(&mut run.f.ledger, &run.who, &second.token, Value::Null, &quick()).unwrap()),
        "completed"
    );
    test_clock.set(clock::now() + secs(7200));
    run.done("probe");
    run.pick("ship");
    let noisy = run.fi.clone();

    let kinds = f.kinds(&noisy);
    for kind in ["Refused", "Heartbeat"] {
        assert!(kinds.iter().any(|k| k == kind), "{kinds:?}");
    }
    assert_eq!(kinds.iter().filter(|k| *k == "Completed").count(), 4, "probe twice: {kinds:?}");
    assert_eq!(differ(&f, &clean, &noisy), None);
    assert_eq!(spine_of(&f, &clean).records, spine_of(&f, &noisy).records);
}

#[test]
fn a_skipped_or_reordered_node_another_edge_or_another_exit_is_a_difference() {
    let (mut f, _clock) = (fx(SHAPE), TestClock::install());
    let me = f.me.clone();
    let clean = Run::new(&mut f, me.clone()).clean("ship");
    let none = crate::kernel::ir::Iteration::new();

    // Skipped: pick never taken.
    let mut run = Run::new(&mut f, me.clone());
    for node in ["probe", "left", "right"] {
        run.done(node);
    }
    let short = run.fi.clone();
    assert_eq!(
        differ(&f, &clean, &short),
        Some(Difference::Skipped {
            node: "pick".into(),
            iteration: none.clone(),
            index: 3,
            missing_from: Side::Against
        })
    );
    assert_eq!(
        differ(&f, &short, &clean),
        Some(Difference::Skipped {
            node: "pick".into(),
            iteration: none.clone(),
            index: 3,
            missing_from: Side::Instance
        })
    );

    // Reordered: right before left.
    let mut run = Run::new(&mut f, me.clone());
    for node in ["probe", "right", "left"] {
        run.done(node);
    }
    run.pick("ship");
    let reordered = run.fi.clone();
    assert_eq!(
        differ(&f, &clean, &reordered),
        Some(Difference::Reordered { node: "left".into(), iteration: none.clone(), index: 1, against_index: 2 })
    );

    // Another edge.
    let hold = Run::new(&mut f, me.clone()).clean("hold");
    assert_eq!(
        differ(&f, &clean, &hold),
        Some(Difference::Edge {
            node: "pick".into(),
            iteration: none.clone(),
            index: 3,
            edge: Some("ship".into()),
            against: Some("hold".into())
        })
    );

    // Another exit: probe observed down.
    std::fs::write(f.dir.join("probe"), "1").unwrap();
    let down = Run::new(&mut f, me.clone()).clean("ship");
    std::fs::remove_file(f.dir.join("probe")).unwrap();
    assert_eq!(
        differ(&f, &clean, &down),
        Some(Difference::Exit {
            node: "probe".into(),
            iteration: none.clone(),
            index: 0,
            exit: Some(0),
            against: Some(1)
        })
    );
}

#[test]
fn a_re_run_keeps_the_first_place_and_the_latest_result() {
    // The policy (spine's module docs): a node recorded again in its iteration is one record, at
    // the place of its first receipt, holding its latest receipt's result. Lease and freshness
    // re-runs are incidental, so a re-run that ends where the first run did is not a difference;
    // one that ends elsewhere is, at the first place.
    let (mut f, test_clock) = (fx(SHAPE), TestClock::install());
    let me = f.me.clone();
    let clean = Run::new(&mut f, me.clone()).clean("ship");

    // probe, left, right, pick, then probe again once it lapsed, with the same result: the A, B,
    // A collapse (the known limit) makes it equal to the clean run.
    let mut run = Run::new(&mut f, me.clone());
    let same = run.clean("ship");
    test_clock.set(clock::now() + secs(7200));
    Run { f: &mut f, who: me.clone(), fi: same.clone() }.done("probe");
    let kinds = f.kinds(&same);
    assert_eq!(kinds.iter().filter(|k| *k == "Completed").count(), 4, "probe ran twice: {kinds:?}");
    let records = spine_of(&f, &same).records;
    assert_eq!(records.iter().map(|r| r.node.as_str()).collect::<Vec<_>>(), ["probe", "left", "right", "pick"]);
    assert_eq!(differ(&f, &clean, &same), None);

    // The same re-run ending down: the latest result, at the first place, is a difference there.
    let mut run = Run::new(&mut f, me.clone());
    let down = run.clean("ship");
    test_clock.set(clock::now() + secs(7200));
    std::fs::write(f.dir.join("probe"), "1").unwrap();
    Run { f: &mut f, who: me.clone(), fi: down.clone() }.done("probe");
    std::fs::remove_file(f.dir.join("probe")).unwrap();
    let records = spine_of(&f, &down).records;
    assert_eq!(records.iter().map(|r| r.node.as_str()).collect::<Vec<_>>(), ["probe", "left", "right", "pick"]);
    assert_eq!(records[0].exit, Some(1));
    assert_eq!(
        differ(&f, &clean, &down),
        Some(Difference::Exit {
            node: "probe".into(),
            iteration: crate::kernel::ir::Iteration::new(),
            index: 0,
            exit: Some(0),
            against: Some(1)
        })
    );
}

#[test]
fn a_human_nodes_answer_is_on_the_spine_with_no_edge_or_exit() {
    let (mut f, _clock) = (
        fx(
            "flow: h\nversion: 1\nparams: [dir]\nworkdir: \"{{params.dir}}\"\nnodes:\n  owner: {class: human, ask: Ship it?}\n  after: {class: pure, needs: [owner], run: ['true']}\n",
        ),
        TestClock::install(),
    );
    let fi = f.begin();
    // The ask pinned, linked and answered, as `asks` records them once the answer has landed.
    let (node, iteration, ask_id, ledger) =
        ("owner".to_string(), Default::default(), "a-1".to_string(), "l.yaml".to_string());
    let pinned = AskPinned {
        node: node.clone(),
        iteration: Default::default(),
        ask_id: ask_id.clone(),
        ledger: ledger.clone(),
        body: json!({}),
        body_hash: "h".into(),
    };
    let linked = AskLinked {
        node: node.clone(),
        iteration: Default::default(),
        ask_id: ask_id.clone(),
        ledger: ledger.clone(),
        body_hash: "h".into(),
    };
    let answered = Answered {
        node,
        iteration,
        ask_id,
        ledger,
        inputs: Default::default(),
        outputs: json!({ "answer_option": "ship" }),
        source: "main@0".into(),
        row_hash: "r".into(),
    };
    f.append(&fi, pinned.to_event().unwrap());
    f.append(&fi, linked.to_event().unwrap());
    f.append(&fi, answered.to_event().unwrap());
    f.done(&fi, "after");
    let records: Vec<_> = spine_of(&f, &fi).records.into_iter().map(|r| (r.node, r.edge, r.exit)).collect();
    assert_eq!(records, [("owner".to_string(), None, None), ("after".to_string(), None, Some(0))]);
}

#[test]
fn an_event_replay_set_aside_is_not_on_the_spine() {
    let (mut f, _clock) = (fx(SHAPE), TestClock::install());
    let me = f.me.clone();
    let clean = Run::new(&mut f, me.clone()).clean("ship");
    // A `Completed` for pick before anything it needs: replay sets it aside, and so does the spine.
    let mut run = Run::new(&mut f, me);
    let early = Completed {
        node: "pick".into(),
        iteration: Default::default(),
        attempt: 1,
        inputs: Default::default(),
        outputs: Value::Null,
        verify: None,
        performer: None,
        tool_use_id: None,
    };
    let fi = run.fi.clone();
    run.f.append(&fi, early.to_event().unwrap());
    run.clean("ship");
    assert_eq!(f.status(&fi, clock::now()).rejected.len(), 1);
    assert_eq!(differ(&f, &clean, &fi), None);
}
