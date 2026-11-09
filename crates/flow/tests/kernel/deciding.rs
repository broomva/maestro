//! BRO-3633 PR 3b2b: `choose`, the `until` verdicts and effects, through the library API. Every
//! guard here runs in the evaluator's child process (this binary again, `guard_process_entry`).
//! The tests that move the clock, or kill a child at a pause point, are in `src/kernel/clocked.rs`.

use crate::claiming::*;
use maestro_flow::InstanceId;
use maestro_flow::kernel::events::{Chosen, Completed, EffectStarted, Guarded, KernelEvent, digest};
use maestro_flow::kernel::{Choosing, Completion, RepeatState, State, Verdict, choose, claim, complete};
use serde_json::{Value, json};

/// review (work) -> pick (choice, guarded on the review's outputs) -> ship or fix.
const DECIDE: &str = r#"
flow: decide
version: 1
params: [dir]
workdir: "{{params.dir}}"
nodes:
  review: {class: work, verify: [sh, -c, 'test -f ok']}
  pick: {class: choice, needs: [review], edges: {ship: {guard: '.nodes.review.outputs.verdict == "pass"'}, fix: {guard: '.nodes.review.outputs.verdict != "pass"'}, broken: {guard: '.nodes.review.outputs.verdict | keys | length > 0'}}}
  ship: {class: pure, run: ['true'], switch: {on: pick, case: ship}}
  fix: {class: pure, run: ['true'], switch: {on: pick, case: fix}}
"#;

/// The refusal code of a choice, or `chosen`.
fn chose(c: &Choosing) -> &str {
    match c {
        Choosing::Chosen { .. } => "chosen",
        Choosing::Refused { code, .. } => code,
    }
}

impl Fx {
    fn choose(&mut self, token: &str, edge: &str) -> Choosing {
        choose(&mut self.ledger, &self.me, token, edge).unwrap_or_else(|e| panic!("{e}"))
    }

    fn params(&self) -> Value {
        json!({ "dir": self.dir.to_str().unwrap() })
    }

    /// Claims and completes `node` with `outputs`, which must pass.
    fn pass(&mut self, fi: &InstanceId, node: &str, outputs: Value) -> Completion {
        let c = self.claim(fi, node);
        let done = self.complete(&c.token, outputs);
        assert_eq!(outcome(&done), "completed", "{node}: {done:?}");
        done
    }
}

#[test]
fn choose_takes_only_a_declared_edge_whose_guard_holds_over_the_recorded_state() {
    // Spec acceptance (BRO-3624 item 10): an undeclared edge refused; a guard over the recorded
    // state; a refusal keeps the token and its attempt; the receipt spends it.
    let mut f = fx(DECIDE);
    let fi = f.begin();
    std::fs::write(f.dir.join("ok"), "").unwrap();
    f.pass(&fi, "review", json!({ "verdict": "pass" }));
    let c = f.claim(&fi, "pick");
    assert_eq!(chose(&f.choose(&c.token, "nope")), "undeclared_edge");
    assert_eq!(chose(&f.choose(&c.token, "fix")), "guard_false");
    assert_eq!(chose(&f.choose(&c.token, "broken")), "guard_error");
    assert_eq!(f.last_body(&fi)["attempt"], 1, "a refusal keeps the attempt");
    // Another session, or one that replaced the owner, chooses nothing.
    assert_eq!(code(choose(&mut f.ledger, &agent("other", "marker-1"), &c.token, "ship")), "not_owner");
    assert_eq!(code(choose(&mut f.ledger, &agent("owner", "marker-2"), &c.token, "ship")), "not_owner");

    let Choosing::Chosen { guard_digest, .. } = f.choose(&c.token, "ship") else { panic!("not chosen") };
    // The guard read the params and the receipt the choice consumes, and the digest says so.
    let read =
        json!({ "params": f.params(), "nodes": { "review": { "outputs": { "verdict": "pass" }, "value": null } } });
    assert_eq!(guard_digest, digest(&read));
    let chosen: Chosen = serde_json::from_value(f.last_body(&fi)).unwrap();
    assert_eq!((chosen.edge.as_str(), chosen.guard_digest.as_deref()), ("ship", Some(guard_digest.as_str())));
    assert_eq!(chosen.inputs, c.inputs);
    assert_eq!(f.kinds(&fi)[3..], ["Claimed", "Refused", "Refused", "Refused", "Chosen"]);

    // The token is spent, and the switch follows the edge.
    assert_eq!(code(choose(&mut f.ledger, &f.me, &c.token, "ship")), "token_spent");
    let s = f.status(&fi, maestro_flow::clock::now());
    assert_eq!((s.nodes["ship"].state, s.nodes["fix"].state), (State::Ready, State::Blocked));
}

#[test]
fn a_choice_is_chosen_and_everything_else_completed() {
    let mut f = fx(DECIDE);
    let fi = f.begin();
    let review = f.claim(&fi, "review");
    assert_eq!(code(choose(&mut f.ledger, &f.me, &review.token, "ship")), "wrong_verb");
    std::fs::write(f.dir.join("ok"), "").unwrap();
    f.complete(&review.token, json!({ "verdict": "pass" }));
    let pick = f.claim(&fi, "pick");
    assert_eq!(code(complete(&mut f.ledger, &f.me, &pick.token, Value::Null, &quick())), "wrong_verb");
    assert_eq!(f.kinds(&fi), ["Started", "Claimed", "Completed", "Claimed"], "the wrong verb writes nothing");
}

/// A repeat over `try` whose `until` reads its body; `outside` is done before it and must not be
/// readable from the `until`.
const LOOP: &str = r#"
flow: loop
version: 1
params: [dir]
workdir: "{{params.dir}}"
nodes:
  outside: {class: work, verify: ['true']}
  try: {class: work, verify: ['true']}
  after: {class: pure, run: ['true'], needs: [rounds]}
  pick: {class: choice, needs: [rounds], edges: {go: {guard: '.nodes.try.outputs.ok'}}}
  gave_up: {class: pure, run: ['true']}
repeats:
  rounds: {body: [try], max: 3, until: '.nodes.try.outputs.ok == true and .nodes.outside == null', on_exhausted: gave_up}
"#;

fn recorded(c: &Completion) -> Vec<(String, u32, bool)> {
    let Completion::Completed { verdicts, .. } = c else { panic!("{c:?}") };
    verdicts
        .iter()
        .map(|v| match v {
            Verdict::Recorded { repeat, iteration, holds, .. } => (repeat.clone(), *iteration, *holds),
            Verdict::Unrecorded { code, reason, .. } => panic!("unrecorded: {code} {reason}"),
        })
        .collect()
}

#[test]
fn an_until_verdict_is_written_when_its_iteration_settles_and_reads_only_its_body() {
    let mut f = fx(LOOP);
    let fi = f.begin();
    f.pass(&fi, "outside", json!({ "x": 1 }));
    let no = f.pass(&fi, "try", json!({ "ok": false }));
    assert_eq!(recorded(&no), [("rounds".to_string(), 0, false)]);
    let yes = f.pass(&fi, "try", json!({ "ok": true }));
    assert_eq!(recorded(&yes), [("rounds".to_string(), 1, true)]);
    // `outside` is not in the until's input (else `.nodes.outside == null` would be false), and
    // the digest is of exactly the body's receipt and the params.
    let verdict: Guarded = serde_json::from_value(f.last_body(&fi)).unwrap();
    let read = json!({ "params": f.params(), "nodes": { "try": { "outputs": { "ok": true }, "value": null } } });
    assert_eq!(verdict.digest, Some(digest(&read)));
    let s = f.status(&fi, maestro_flow::clock::now());
    assert_eq!((s.repeats["rounds"].state, s.nodes["after"].state), (RepeatState::Done, State::Ready));
    assert_eq!(f.kinds(&fi)[3..], ["Claimed", "Completed", "Guarded", "Claimed", "Completed", "Guarded"]);
    // A guard on a node that needs the repeat reads the body at the iteration judged.
    let c = f.claim(&fi, "pick");
    assert_eq!(chose(&f.choose(&c.token, "go")), "chosen");
}

#[test]
fn a_verdict_a_crash_left_unwritten_is_written_by_the_next_claim() {
    let mut f = fx(LOOP);
    let fi = f.begin();
    // A receipt whose writer died before its verdict: the repeat awaits it.
    let receipt = Completed {
        node: "try".into(),
        iteration: [("rounds".to_string(), 0)].into(),
        attempt: 1,
        inputs: Default::default(),
        outputs: json!({ "ok": true }),
        verify: None,
        performer: None,
        tool_use_id: None,
    };
    f.append(&fi, receipt.to_event().unwrap());
    assert!(f.status(&fi, maestro_flow::clock::now()).repeats["rounds"].awaiting_verdict);
    // Only the owner writes it: another session's claim writes nothing, the verdict included.
    assert_eq!(code(claim(&mut f.ledger, &agent("other", "marker-1"), &fi, "after")), "not_owner");
    assert_eq!(f.kinds(&fi), ["Started", "Completed"]);
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "after")), "ok", "the claim wrote the verdict first");
    assert_eq!(f.kinds(&fi), ["Started", "Completed", "Guarded", "Claimed"]);
}

#[test]
fn a_choice_that_settles_an_iteration_writes_its_verdict() {
    let yaml = r#"
flow: ask
version: 1
params: [dir]
nodes:
  pick: {class: choice, edges: {again: {guard: 'true'}, done: {guard: 'true'}}}
repeats:
  rounds: {body: [pick], max: 3, until: '.nodes.pick.value == "done"'}
"#;
    let mut f = fx(yaml);
    let fi = f.begin();
    for (edge, holds) in [("again", false), ("done", true)] {
        let c = f.claim(&fi, "pick");
        let Choosing::Chosen { verdicts, .. } = f.choose(&c.token, edge) else { panic!("{edge}") };
        assert!(matches!(&verdicts[..], [Verdict::Recorded { holds: h, .. }] if *h == holds), "{edge}: {verdicts:?}");
    }
    assert_eq!(f.status(&fi, maestro_flow::clock::now()).repeats["rounds"].state, RepeatState::Done);
}

#[test]
fn a_verdict_that_cannot_be_had_is_not_written_and_the_repeat_keeps_awaiting_it() {
    let failing = LOOP.replace(
        "'.nodes.try.outputs.ok == true and .nodes.outside == null'",
        "'.nodes.try.outputs.ok | keys | length > 0'",
    );
    let mut f = fx(&failing);
    let fi = f.begin();
    let c = f.claim(&fi, "try");
    let Completion::Completed { verdicts, .. } = f.complete(&c.token, json!({ "ok": true })) else { panic!() };
    assert!(
        matches!(&verdicts[..], [Verdict::Unrecorded { repeat: Some(r), code: "guard_error", .. }] if r == "rounds"),
        "{verdicts:?}"
    );
    assert_eq!(f.kinds(&fi), ["Started", "Claimed", "Completed"]);
    let s = f.status(&fi, maestro_flow::clock::now());
    assert!(s.repeats["rounds"].awaiting_verdict && s.repeats["rounds"].iteration == 0);
}

#[test]
fn an_effect_runs_once_its_preconditions_hold_and_records_that_it_started() {
    let mut f = fx(EFFECT);
    let fi = f.begin();
    f.pass(&fi, "build", json!({ "ready": false }));
    let c = f.claim(&fi, "push");
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "precondition_false");
    assert_eq!(runs(&f), 0, "a precondition that does not hold runs nothing");
    assert!(!f.kinds(&fi).contains(&"EffectStarted".to_string()));

    let fi = f.begin();
    f.pass(&fi, "build", json!({ "ready": true }));
    let c = f.claim(&fi, "push");
    let Completion::Completed { verify, .. } = f.complete(&c.token, Value::Null) else { panic!() };
    assert_eq!(runs(&f), 1);
    assert_eq!(verify.argv[2], "echo ran >> effects; exit \"$(cat fail 2>/dev/null || echo 0)\"");
    assert_eq!(f.kinds(&fi)[3..], ["Claimed", "EffectStarted", "Completed"]);
    let started = f.log_text(&fi).lines().nth(4).map(|l| serde_json::from_str::<Value>(l).unwrap()["body"].clone());
    let started: EffectStarted = serde_json::from_value(started.unwrap()).unwrap();
    let read = json!({ "params": f.params(), "nodes": { "build": { "outputs": { "ready": true }, "value": null } } });
    assert_eq!((started.attempt, started.guard_digest), (1, Some(digest(&read))));
}

#[test]
fn an_effect_that_ran_without_a_receipt_is_reconciled_and_never_run_again() {
    // Spec acceptance (BRO-3624 item 10): an effect reconciled instead of re-run. Here the run
    // failed; the crash case is `clocked::an_effect_killed_mid_run_is_reconciled_not_run_again`.
    let mut f = fx(EFFECT);
    let fi = f.begin();
    f.pass(&fi, "build", json!({ "ready": true }));
    std::fs::write(f.dir.join("fail"), "3").unwrap();
    let c = f.claim(&fi, "push");
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "effect_failed");
    assert_eq!(runs(&f), 1);
    // Unconfirmed, the reconcile refuses and nothing runs the effect again.
    std::fs::write(f.dir.join("unconfirmed"), "").unwrap();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "effect_unreconciled");
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "effect_unreconciled");
    std::fs::remove_file(f.dir.join("unconfirmed")).unwrap();
    let Completion::Completed { verify, .. } = f.complete(&c.token, Value::Null) else { panic!() };
    assert_eq!(runs(&f), 1, "the effect ran once");
    assert_eq!(verify.argv[2], "test -s effects && test ! -f unconfirmed", "the receipt is the reconcile's");
    assert_eq!(f.kinds(&fi)[3..], ["Claimed", "EffectStarted", "Refused", "Refused", "Refused", "Completed"]);
}

/// Starts `ir`, built by hand, as `name`, owned by the fixture's session, with a hash that
/// matches, so replay checks it as it checks any pin.
fn pin(f: &mut Fx, name: &str, ir: maestro_flow::kernel::Ir) -> InstanceId {
    use maestro_flow::kernel::events::{Owner, Started, marker_digest};
    let started = Started {
        flow: format!("{}@{}", ir.flow, ir.version),
        ir_hash: maestro_flow::kernel::ir::content_hash(&ir).unwrap(),
        ir,
        params: f.params(),
        owner: Some(Owner { session: "t3:-/owner".parse().unwrap(), marker_sha256: marker_digest("marker-1") }),
    };
    let fi = id(name);
    f.ledger.lock().unwrap().append(&fi, 0, started.to_event().unwrap()).unwrap();
    fi
}

#[test]
fn an_effect_s_command_is_pinned_in_ir_version_3_and_an_earlier_effect_never_runs() {
    use maestro_flow::kernel::ir::Class;
    let mut f = fx(EFFECT);
    let set_run = |ir: &mut maestro_flow::kernel::Ir, run: Vec<String>| {
        let Class::Effect { run: r, .. } = &mut ir.nodes.get_mut("push").unwrap().class else { panic!() };
        *r = run;
    };
    let tried = |f: &mut Fx, name: &str, version: u32, run: Vec<String>| {
        let mut ir = f.flow.ir().clone();
        ir.ir_version = version;
        set_run(&mut ir, run);
        let fi = pin(f, name, ir);
        let reader = maestro_flow::Reader::new(&f.root, maestro_flow::Options::default()).unwrap();
        let s = maestro_flow::kernel::status(&reader.lock().unwrap(), &fi, maestro_flow::clock::now());
        (fi, s.map(|_| ()).map_err(|e| e.code()))
    };
    // An effect with a command is version 3; version 3 needs one on every effect.
    let x = vec!["x".to_string()];
    assert_eq!(tried(&mut f, "fi_v2_run", 2, x.clone()).1, Err("malformed_instance"));
    assert_eq!(tried(&mut f, "fi_v3_none", 3, vec![]).1, Err("malformed_instance"));
    assert_eq!(tried(&mut f, "fi_v3_run", 3, x).1, Ok(()));
    // Version 3 by one effect's command, with a second effect that has none: refused by the
    // version 3 rule itself, not by the version.
    let mut ir = f.flow.ir().clone();
    let mut bare = ir.nodes["push"].clone();
    let Class::Effect { run, .. } = &mut bare.class else { panic!() };
    run.clear();
    ir.nodes.insert("again".into(), bare);
    assert_eq!(maestro_flow::kernel::ir::version_for(&ir), 3);
    let fi = pin(&mut f, "fi_v3_mixed", ir);
    let reader = maestro_flow::Reader::new(&f.root, maestro_flow::Options::default()).unwrap();
    let s = maestro_flow::kernel::status(&reader.lock().unwrap(), &fi, maestro_flow::clock::now());
    assert_eq!(s.map(|_| ()).map_err(|e| e.code()), Err("malformed_instance"));
    // An effect pinned before version 3 has no command, so the kernel never runs one for it.
    let (fi, replays) = tried(&mut f, "fi_v2_none", 2, vec![]);
    assert_eq!(replays, Ok(()));
    f.pass(&fi, "build", json!({ "ready": true }));
    let c = f.claim(&fi, "push");
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "not_claimable");
    assert_eq!(runs(&f), 0);
}

/// A flow whose pinned IR carries guards the compiler would refuse: built by hand, with a hash
/// that matches, so replay accepts it and only the evaluator's own check stands in the way.
#[test]
fn a_hand_pinned_guard_the_compiler_would_refuse_is_refused_before_it_runs() {
    let mut f = fx(DECIDE);
    let mut ir = f.flow.ir().clone();
    let maestro_flow::kernel::ir::Class::Choice { edges, .. } = &mut ir.nodes.get_mut("pick").unwrap().class else {
        panic!()
    };
    edges.insert("ship".into(), "def f: f; f".into());
    let fi = pin(&mut f, "fi_pinned", ir);
    std::fs::write(f.dir.join("ok"), "").unwrap();
    f.pass(&fi, "review", json!({ "verdict": "pass" }));
    let c = f.claim(&fi, "pick");
    assert_eq!(chose(&f.choose(&c.token, "ship")), "guard_invalid");
}
