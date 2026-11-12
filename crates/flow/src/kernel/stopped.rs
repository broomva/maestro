//! The stop gate's kernel half (BRO-3636): the predicate, the open instances, `StopUnblockable`
//! and the loop bound, each guard mutation-checked in `scripts/mutation-check.sh`. They share the
//! human-step fixture (`asks::asked`): a workspace whose origin is a local bare repository, so an
//! answer can be put on the answers branch or landed on main.

use super::asks::asked::{Fx, filing, fx, git};
use super::stop::{self, Assessment, Count, MAX_REFUSALS, Recorded, Refusals, Waiting, Work};
use super::*;
use crate::clock;
use crate::hooked::common::agent;
use crate::kernel::events::{KernelEvent, StopUnblockable, Unblockable};
use crate::{InstanceId, NewEvent, Options, Reader};
use maestro_core::session::VerifiedSession;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// `one`, then `two`: work its owner claims and completes, in `{dir}`.
const RELAY: &str = "flow: relay\nversion: 1\nworkdir: {dir}\nnodes:\n  one: {class: pure, run: ['true']}\n  two: {class: pure, run: ['true'], needs: [one]}\n";

/// A repeat whose `until` is judged when its body settles.
const LOOP: &str = "flow: loop\nversion: 1\nnodes:\n  try: {class: work, verify: ['true']}\nrepeats:\n  rounds: {body: [try], max: 3, until: '.nodes.try.outputs.ok'}\n";

fn with(definition: &str) -> (Fx, InstanceId) {
    let mut f = fx(None, "");
    f.flow = compile(&definition.replace("{dir}", &f.ws.display().to_string())).unwrap();
    let fi = f.begin();
    (f, fi)
}

fn judge(f: &Fx, fi: &InstanceId) -> Assessment {
    let reader = Reader::new(&f.flows, Options::default()).unwrap();
    stop::assess_instance(&reader.lock().unwrap(), fi, clock::now(), &|l, a| seen(&f.paths, l, a)).unwrap()
}

fn open_for(f: &Fx, who: &VerifiedSession) -> stop::Open {
    let reader = Reader::new(&f.flows, Options::default()).unwrap();
    stop::open(&reader.lock().unwrap(), who, clock::now(), &|l, a| seen(&f.paths, l, a)).unwrap()
}

fn push(f: &mut Fx, fi: &InstanceId, kind: &str, body: Value) {
    let mut guard = f.ledger.lock().unwrap();
    let seq = guard.log(fi).unwrap().seq();
    guard.append(fi, seq, NewEvent::new(kind, body)).unwrap();
}

fn done(f: &mut Fx, fi: &InstanceId, node: &str) {
    let me = f.me.clone();
    let c = claim(&mut f.ledger, &me, fi, node).unwrap();
    assert!(matches!(
        complete(&mut f.ledger, &me, &c.token, Value::Null, &Limits::default()),
        Ok(Completion::Completed { .. })
    ));
}

fn kinds_of(work: &[Work]) -> Vec<String> {
    work.iter().map(|w| serde_json::to_value(w).unwrap()["kind"].as_str().unwrap().to_string()).collect()
}

fn record(f: &mut Fx, who: &VerifiedSession, fi: &InstanceId, reason: Unblockable) -> Result<Recorded, KernelError> {
    stop::record(&mut f.ledger, who, fi, "claude", reason, &[fi.to_string()])
}

#[test]
fn an_enabled_node_is_work_and_a_stop_is_refused_over_it() {
    let (f, fi) = with(RELAY);
    let a = judge(&f, &fi);
    assert_eq!(
        a.work,
        [Work::Node { node: "one".into(), class: "pure".into(), iteration: Iteration::default(), held: false }]
    );
    assert!(a.waiting.is_empty() && !a.terminal);
    assert!(a.refuses_stop());
    let steps = a.next_steps().join(" ");
    assert!(steps.contains(&format!("maestro flow claim --instance {fi} --node-id one")), "{steps}");
}

#[test]
fn a_claim_the_owner_holds_is_work_it_finishes() {
    let (mut f, fi) = with(RELAY);
    let me = f.me.clone();
    claim(&mut f.ledger, &me, &fi, "one").unwrap();
    let a = judge(&f, &fi);
    assert_eq!(
        a.work,
        [Work::Node { node: "one".into(), class: "pure".into(), iteration: Iteration::default(), held: true }]
    );
    assert!(a.next_steps()[0].contains("you hold a live claim on node one"), "{:?}", a.next_steps());
}

#[test]
fn a_finished_instance_has_no_work_and_is_not_open() {
    let (mut f, fi) = with(RELAY);
    done(&mut f, &fi, "one");
    done(&mut f, &fi, "two");
    let a = judge(&f, &fi);
    assert!(a.terminal && a.work.is_empty() && !a.refuses_stop(), "{a:?}");
    assert!(open_for(&f, &f.me.clone()).instances.is_empty());
}

#[test]
fn a_pending_verdict_is_work_a_claim_retries() {
    let (mut f, fi) = with(LOOP);
    // The body settled, and no verdict was written: the repeat awaits it.
    push(
        &mut f,
        &fi,
        "Completed",
        json!({ "node": "try", "iteration": { "rounds": 0 }, "attempt": 1, "outputs": { "ok": true } }),
    );
    let a = judge(&f, &fi);
    assert_eq!(a.work, [Work::Verdict { repeat: "rounds".into(), iteration: 0, node: "try".into() }], "{a:?}");
    assert!(a.next_steps()[0].contains(&format!("maestro flow claim --instance {fi} --node-id try")));
}

#[test]
fn an_unfiled_ask_is_work_a_filed_one_waits_on_the_owner() {
    let mut f = fx(None, "");
    let fi = f.begin();
    assert_eq!(kinds_of(&judge(&f, &fi).work), ["ask"]);
    f.ask(&fi, Some(&filing())).unwrap();
    let a = judge(&f, &fi);
    let ask_id = f.pinned(&fi)["id"].as_str().unwrap().to_string();
    assert!(a.work.is_empty(), "{a:?}");
    assert!(!a.refuses_stop(), "waiting only on the owner is no ground to refuse a stop");
    let owner = Waiting::Owner {
        node: "owner".into(),
        iteration: Iteration::default(),
        ask_id: ask_id.clone(),
        ledger: "ledger.yaml".into(),
        landing: None,
    };
    assert_eq!(a.waiting, [owner]);
    // On the answers branch only: it has not landed, so it still waits on the owner.
    f.answer_in_worktree(&ask_id);
    f.put_on_answers_branch();
    let a = judge(&f, &fi);
    assert!(a.work.is_empty() && matches!(&a.waiting[..], [Waiting::Owner { landing: Some(_), .. }]), "{a:?}");
    // Landed on main: taking it is the session's to do.
    git(&f.ws, &["push", "-q", "origin", "main"]);
    git(&f.ws, &["fetch", "-q", "origin"]);
    let a = judge(&f, &fi);
    assert_eq!(kinds_of(&a.work), ["answer"], "{a:?}");
    assert!(a.waiting.is_empty() && a.refuses_stop());
    assert!(a.next_steps()[0].contains(&format!("take it with `maestro flow ask --instance {fi} --node-id owner`")));
}

#[test]
fn open_lists_only_the_sessions_own_open_instances() {
    let (mut f, mine) = with(RELAY);
    let finished = f.begin();
    done(&mut f, &finished, "one");
    done(&mut f, &finished, "two");
    let other = agent("other", "marker-9");
    let theirs = start(&mut f.ledger, &other, &f.flow, json!({})).unwrap().instance;
    let ids = |o: &stop::Open| o.instances.iter().map(|a| a.instance.clone()).collect::<Vec<_>>();
    let me = f.me.clone();
    assert_eq!(ids(&open_for(&f, &me)), [mine.to_string()]);
    assert_eq!(ids(&open_for(&f, &other)), [theirs.to_string()]);
    // A replacement of the owner, the same session with a new marker, owns none of them.
    assert!(open_for(&f, &agent("owner", "marker-2")).instances.is_empty());
    // A session with no marker owns nothing and is refused as a write is.
    let reader = Reader::new(&f.flows, Options::default()).unwrap();
    let bare = VerifiedSession::new("t3:-/owner".parse().unwrap(), "/tmp", None, None, vec![]);
    let refused = stop::open(&reader.lock().unwrap(), &bare, clock::now(), &|_, _| AnswerSeen::Open);
    assert_eq!(refused.unwrap_err().code(), "no_owner_marker");
}

#[test]
fn an_instance_that_does_not_read_is_listed_unread_not_left_out() {
    let (mut f, fi) = with(RELAY);
    // A kind no replay knows: the log verifies, and its events do not replay.
    push(&mut f, &fi, "Bogus", json!({}));
    let open = open_for(&f, &f.me.clone());
    assert!(open.instances.is_empty());
    assert_eq!(open.unread.len(), 1, "{open:?}");
    assert_eq!((open.unread[0].instance.as_str(), open.unread[0].code.as_str()), (fi.as_str(), "malformed_instance"));
}

#[test]
fn stop_unblockable_is_listed_and_changes_no_node() {
    let (mut f, fi) = with(RELAY);
    let me = f.me.clone();
    claim(&mut f.ledger, &me, &fi, "one").unwrap();
    let before = f.status(&fi);
    let Recorded::Appended(appended) = record(&mut f, &me, &fi, Unblockable::NoProgress).unwrap() else {
        panic!("not appended")
    };
    let after = f.status(&fi);
    assert_eq!(after.nodes, before.nodes);
    assert_eq!((after.repeats, after.terminal, after.owner), (before.repeats, before.terminal, before.owner));
    assert!(after.rejected.is_empty());
    let listed = StopRecord {
        seq: appended.event.seq,
        harness: "claude".into(),
        session: "t3:-/owner".into(),
        reason: Unblockable::NoProgress,
    };
    assert_eq!(after.stop_unblockable, [listed]);
    let body = f.events(&fi).last().unwrap()["body"].clone();
    assert_eq!(
        body,
        json!({ "harness": "claude", "session": { "surface": "t3", "id": "owner" }, "open_instances": [fi.as_str()], "reason": "no_progress" })
    );
    // Its claim still holds: completing it after the record is accepted.
    let a = judge(&f, &fi);
    assert_eq!(
        a.work,
        [Work::Node { node: "one".into(), class: "pure".into(), iteration: Iteration::default(), held: true }]
    );
}

#[test]
fn a_record_is_written_once_until_the_instance_moves() {
    let (mut f, fi) = with(RELAY);
    let me = f.me.clone();
    assert!(matches!(record(&mut f, &me, &fi, Unblockable::NoProgress), Ok(Recorded::Appended(_))));
    let seq = f.events(&fi).len() as u64;
    assert_eq!(record(&mut f, &me, &fi, Unblockable::NoProgress).unwrap(), Recorded::Already { seq });
    // Another reason is another record.
    assert!(matches!(record(&mut f, &me, &fi, Unblockable::HarnessCannotBlock), Ok(Recorded::Appended(_))));
    done(&mut f, &fi, "one");
    assert!(matches!(record(&mut f, &me, &fi, Unblockable::HarnessCannotBlock), Ok(Recorded::Appended(_))));
}

#[test]
fn only_the_owner_records_a_stop() {
    let (mut f, fi) = with(RELAY);
    let n = f.events(&fi).len();
    assert_eq!(
        record(&mut f, &agent("other", "marker-9"), &fi, Unblockable::NoProgress).unwrap_err().code(),
        "not_owner"
    );
    assert_eq!(
        record(&mut f, &agent("owner", "marker-2"), &fi, Unblockable::NoProgress).unwrap_err().code(),
        "not_owner"
    );
    assert_eq!(f.events(&fi).len(), n);
}

#[test]
fn a_stop_unblockable_that_does_not_parse_fails_the_replay() {
    for body in [
        json!({ "harness": "claude", "session": { "surface": "t3", "id": "owner" }, "open_instances": [], "reason": "bored" }),
        json!({ "harness": "claude", "open_instances": [], "reason": "no_progress" }),
    ] {
        let (mut f, fi) = with(RELAY);
        push(&mut f, &fi, StopUnblockable::KIND, body);
        let reader = Reader::new(&f.flows, Options::default()).unwrap();
        let e = status(&reader.lock().unwrap(), &fi, clock::now()).unwrap_err();
        assert_eq!(e.code(), "malformed_instance");
    }
}

fn at(instance: &str, seq: u64) -> Assessment {
    Assessment {
        instance: instance.into(),
        flow: "relay@1".into(),
        ir_hash: "sha256:x".into(),
        seq,
        terminal: false,
        work: vec![Work::Node {
            node: "one".into(),
            class: "pure".into(),
            iteration: Iteration::default(),
            held: false,
        }],
        waiting: vec![],
    }
}

#[test]
fn the_gate_refuses_one_position_three_times_then_lets_the_stop_through() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(".broomva/flows");
    let me = agent("owner", "marker-1");
    let a = at("fi_a", 4);
    for n in 1..=MAX_REFUSALS {
        let mut r = Refusals::load(&root, &me, stop::REFUSALS_WAIT).unwrap();
        let t = r.tally(&[&a]);
        assert_eq!((t.refused.as_slice(), t.spent.len()), ([String::from("fi_a")].as_slice(), 0), "refusal {n}");
        r.save(&me).unwrap();
    }
    let mut r = Refusals::load(&root, &me, stop::REFUSALS_WAIT).unwrap();
    assert_eq!(r.counts["fi_a"], Count { seq: 4, refusals: MAX_REFUSALS });
    let t = r.tally(&[&a]);
    assert_eq!((t.refused.len(), t.spent.as_slice()), (0, [String::from("fi_a")].as_slice()));
    // Durable: one file per session, beside the flows root.
    let path = Refusals::path(&root, &me).unwrap();
    assert!(path.starts_with(tmp.path().join(".broomva/flows.stop-gate")), "{}", path.display());
    assert_ne!(path, Refusals::path(&root, &agent("owner", "marker-2")).unwrap());
}

#[test]
fn the_count_starts_again_when_the_instance_moves() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(".broomva/flows");
    let me = agent("owner", "marker-1");
    let mut r = Refusals::load(&root, &me, stop::REFUSALS_WAIT).unwrap();
    r.counts = BTreeMap::from([("fi_a".to_string(), Count { seq: 4, refusals: MAX_REFUSALS })]);
    let t = r.tally(&[&at("fi_a", 5)]);
    assert_eq!(t.refused, ["fi_a"]);
    assert_eq!(r.counts["fi_a"], Count { seq: 5, refusals: 1 });
    // An instance the gate no longer refuses over drops out: a later refusal is a first one.
    r.counts.insert("fi_b".into(), Count { seq: 2, refusals: MAX_REFUSALS });
    r.tally(&[&at("fi_a", 5)]);
    assert!(!r.counts.contains_key("fi_b"));
}

#[test]
fn the_driver_skill_stops_on_the_kernels_work_list() {
    let flow = compile(include_str!("../../flows/verify.yaml")).unwrap();
    let skill = crate::driver::generate(&flow).unwrap();
    let step4 = skill.split("4. Then, in this order:").nth(1).unwrap();
    let rule = step4.lines().find(|l| l.contains("nothing is yours to do now")).unwrap();
    assert!(rule.contains(&format!("If `{}` (beside `next` in step 2's result) was empty", stop::WORK)), "{rule}");
    assert!(rule.contains(&format!("as `{}` names", stop::WAITING)), "{rule}");
}

/// A choice node takes an edge: its next step is `choose`, never `complete`, which refuses it.
const PICK: &str = "flow: pick\nversion: 1\nworkdir: {dir}\nnodes:\n  pick: {class: choice, edges: {ship: {guard: 'true'}, hold: {guard: 'true'}}}\n";

#[test]
fn a_choice_node_is_finished_with_choose() {
    let (mut f, fi) = with(PICK);
    let a = judge(&f, &fi);
    assert_eq!(
        a.work,
        [Work::Node { node: "pick".into(), class: "choice".into(), iteration: Iteration::default(), held: false }]
    );
    let step = &a.next_steps()[0];
    assert!(step.contains("choose one of its edges with `maestro flow choose --claim-token"), "{step}");
    assert!(!step.contains("maestro flow complete"), "{step}");
    let me = f.me.clone();
    claim(&mut f.ledger, &me, &fi, "pick").unwrap();
    let step = &judge(&f, &fi).next_steps()[0];
    assert!(step.contains("you hold a live claim on node pick: choose one of its edges"), "{step}");
    assert!(!step.contains("maestro flow complete"), "{step}");
    // Any other class is completed.
    let (f, fi) = with(RELAY);
    assert!(judge(&f, &fi).next_steps()[0].contains("complete it with `maestro flow complete --claim-token"));
}

/// Two stops of one session at once: the count lock is held from load to save, so the second
/// waits for the first and both are counted.
#[test]
fn two_stops_at_once_are_both_counted() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(".broomva/flows");
    let me = agent("owner", "marker-1");
    let a = at("fi_a", 4);
    let mut first = Refusals::load(&root, &me, stop::REFUSALS_WAIT).unwrap();
    let second = {
        let (root, me, a) = (root.clone(), me.clone(), a.clone());
        std::thread::spawn(move || {
            let mut r = Refusals::load(&root, &me, stop::REFUSALS_WAIT).unwrap();
            r.tally(&[&a]);
            r.save(&me).unwrap();
        })
    };
    // The second stop has had time to read; under the lock it is still waiting.
    std::thread::sleep(std::time::Duration::from_millis(300));
    first.tally(&[&a]);
    first.save(&me).unwrap();
    drop(first);
    second.join().unwrap();
    let counts = Refusals::load(&root, &me, stop::REFUSALS_WAIT).unwrap().counts;
    assert_eq!(counts["fi_a"], Count { seq: 4, refusals: 2 });
    // A lock held past the wait is refused busy, never waited out.
    let held = Refusals::load(&root, &me, stop::REFUSALS_WAIT).unwrap();
    let busy = std::thread::spawn(move || {
        Refusals::load(&root, &me, std::time::Duration::from_millis(50)).map(|_| ()).unwrap_err().code()
    });
    assert_eq!(busy.join().unwrap(), "busy");
    drop(held);
}

/// A live claim holds the node only for the inputs it was taken over: one over inputs the node no
/// longer has is not the owner's work in hand, so the node is work to claim anew.
#[test]
fn a_claim_over_other_inputs_does_not_hold_the_node() {
    let (mut f, fi) = with(RELAY);
    done(&mut f, &fi, "one");
    let lease_until = (clock::now() + jiff::SignedDuration::from_secs(600)).to_string();
    let stale = json!({ "node": "two", "iteration": {}, "attempt": 1, "lease_until": lease_until, "token_sha256": "sha256:x", "inputs": { "one": "sha256:other" } });
    push(&mut f, &fi, "Claimed", stale);
    let a = judge(&f, &fi);
    assert_eq!(
        a.work,
        [Work::Node { node: "two".into(), class: "pure".into(), iteration: Iteration::default(), held: false }]
    );
    // Over the node's inputs as they are, the same claim holds it.
    let inputs = serde_json::to_value(&judge_next(&f, &fi).enabled[0].inputs).unwrap();
    let current = json!({ "node": "two", "iteration": {}, "attempt": 2, "lease_until": lease_until, "token_sha256": "sha256:y", "inputs": inputs });
    push(&mut f, &fi, "Claimed", current);
    assert!(matches!(&judge(&f, &fi).work[..], [Work::Node { held: true, .. }]));
}

fn judge_next(f: &Fx, fi: &InstanceId) -> Next {
    let reader = Reader::new(&f.flows, Options::default()).unwrap();
    next(&reader.lock().unwrap(), fi, clock::now()).unwrap()
}
