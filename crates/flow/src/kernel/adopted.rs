//! The adoption and retirement tests (BRO-3635; ADR decisions 6 and 7), each guard
//! mutation-checked in `scripts/mutation-check.sh`. They share the human-step fixture
//! (`asks::asked`): the stub writer of record, a workspace whose answers remote is a local bare
//! repository (`asks::seams::REMOTE`), and the test clock, which exist in no other build.

use super::*;
use crate::clock::TestClock;
use crate::hooked::common::{agent, host};
use crate::kernel::asks::asked::{Fx, code, filing, fx, git, machine, wrapper};
use crate::kernel::asks::seams;
use crate::kernel::events::marker_digest;
use crate::kernel::{CompiledFlow, Completion, DEFAULT_LEASE_SECS, Limits, State, claim, compile, complete, heartbeat};
use crate::{NewEvent, Options, Reader};
use jiff::SignedDuration;
use std::path::Path;

/// `one`, then `two`, then `three`: work the owner claims and completes.
fn relay(ws: &Path) -> CompiledFlow {
    compile(&format!(
        "flow: relay\nversion: 1\nworkdir: {}\nnodes:\n  one: {{class: pure, run: ['true']}}\n  two: {{class: pure, run: ['true'], needs: [one]}}\n  three: {{class: pure, run: ['true'], needs: [two]}}\n",
        ws.display()
    ))
    .unwrap()
}

/// The fixture with the relay flow started by its owner (`t3:-/owner`, marker `marker-1`), and
/// the test clock installed on this thread.
fn started() -> (Fx, InstanceId, TestClock) {
    let mut f = fx(None, "");
    f.flow = relay(&f.ws);
    let clock = TestClock::install();
    let fi = f.begin();
    (f, fi, clock)
}

/// What an agent gives for a first proposal: the fixture's ask fields but the kernel's.
fn proposing() -> Filing {
    let mut given = filing();
    given.body.retain(|k, _| !PROPOSAL_KEYS.contains(&k.as_str()));
    given
}

fn other() -> VerifiedSession {
    agent("other", "marker-9")
}

fn adopt(f: &mut Fx, who: &VerifiedSession, fi: &InstanceId, filing: Option<&Filing>) -> Result<Proposed, KernelError> {
    propose_adoption(&mut f.ledger, &f.paths, who, fi, Proposing { filing, again: false })
}

fn retire(
    f: &mut Fx,
    who: &VerifiedSession,
    fi: &InstanceId,
    filing: Option<&Filing>,
) -> Result<Proposed, KernelError> {
    propose_retirement(&mut f.ledger, &f.paths, who, fi, Proposing { filing, again: false })
}

/// Proposes adoption again, with the fixture's filing, in place of a declined or stale proposal.
fn adopt_again(f: &mut Fx, who: &VerifiedSession, fi: &InstanceId) -> Result<Proposed, KernelError> {
    propose_adoption(&mut f.ledger, &f.paths, who, fi, Proposing { filing: Some(&proposing()), again: true })
}

fn retire_again(f: &mut Fx, who: &VerifiedSession, fi: &InstanceId) -> Result<Proposed, KernelError> {
    propose_retirement(&mut f.ledger, &f.paths, who, fi, Proposing { filing: Some(&proposing()), again: true })
}

/// The ask id of a linked proposal: the call said waiting and linked it.
fn linked(p: &Proposed) -> String {
    match p {
        Proposed::Waiting { ask_id, appended: Some(_), .. } => ask_id.clone(),
        other => panic!("not linked now: {other:?}"),
    }
}

fn state(p: &Proposed) -> &'static str {
    match p {
        Proposed::Waiting { .. } => "waiting",
        Proposed::Withdrawn { .. } => "withdrawn",
        Proposed::Declined { .. } => "declined",
        Proposed::Adopted { .. } => "adopted",
        Proposed::Retired { .. } => "retired",
    }
}

/// The owner's answer on row `id` of the working tree: `lines` are the answer's keys.
fn answer(f: &Fx, id: &str, option: Option<&str>) {
    let option = option.map(|o| format!("    answer_option: \"{o}\"\n")).unwrap_or_default();
    f.add_to_row(
        id,
        &format!("    answer: \"Done\"\n{option}    answered_at: \"2026-11-11T00:00:00Z\"\n    answered_by: \"owner:maestro-web\"\n"),
    );
}

fn done(c: Result<Completion, KernelError>) -> &'static str {
    match c {
        Ok(Completion::Completed { .. }) => "completed",
        Ok(Completion::Refused { code, .. }) => code,
        Err(e) => e.code(),
    }
}

fn complete_as(f: &mut Fx, who: &VerifiedSession, token: &str) -> &'static str {
    done(complete(&mut f.ledger, who, token, Value::Null, &Limits::default()))
}

/// What the owner sees of the work: each node's state and the owner, nothing about proposals.
fn work(f: &Fx, fi: &InstanceId) -> (Option<String>, Vec<(String, State)>) {
    let s = f.status(fi);
    (s.owner.map(|o| o.to_string()), s.nodes.into_iter().map(|(n, st)| (n, st.state)).collect())
}

fn proposal_body(f: &Fx, fi: &InstanceId) -> Value {
    let pins: Vec<Value> = f.events(fi).into_iter().filter(|e| e["kind"] == "ProposalPinned").collect();
    pins.last().expect("a proposal pin")["body"]["body"].clone()
}

fn append_raw(f: &mut Fx, fi: &InstanceId, kind: &str, body: Value) {
    let mut guard = f.ledger.lock().unwrap();
    let seq = guard.log(fi).unwrap().seq();
    guard.append(fi, seq, NewEvent::new(kind, body)).unwrap();
}

fn owner_json(who: &VerifiedSession) -> Value {
    serde_json::to_value(super::owner(who).unwrap()).unwrap()
}

fn push(ledger: &mut Ledger, fi: &InstanceId, kind: &str, body: Value) {
    let mut guard = ledger.lock().unwrap();
    let seq = guard.log(fi).unwrap().seq();
    guard.append(fi, seq, NewEvent::new(kind, body)).unwrap();
}

fn approval(id: &str, option: &str) -> Value {
    json!({ "ask_id": id, "ledger": "ledger.yaml", "answer_option": option, "source": "main@x", "row_hash": "x" })
}

/// A proposal of `kind` by `by`, naming `owner` within `epoch`, pinned and linked as the kernel
/// appends them.
fn push_proposal(
    ledger: &mut Ledger,
    fi: &InstanceId,
    kind: &str,
    owner: &VerifiedSession,
    by: &VerifiedSession,
    epoch: u64,
    id: &str,
) {
    let pin = json!({ "proposal": kind, "by": owner_json(by), "owner": owner_json(owner), "epoch": epoch, "ask_id": id, "ledger": "ledger.yaml", "body": {}, "body_hash": "sha256:h" });
    push(ledger, fi, "ProposalPinned", pin);
    push(ledger, fi, "ProposalLinked", json!({ "ask_id": id, "ledger": "ledger.yaml", "body_hash": "sha256:h" }));
}

/// A whole approved adoption from `from` to `to`, as the kernel appends it.
fn push_adoption(
    ledger: &mut Ledger,
    fi: &InstanceId,
    from: &VerifiedSession,
    to: &VerifiedSession,
    epoch: u64,
    id: &str,
) {
    push_proposal(ledger, fi, "adoption", from, to, epoch, id);
    let adopted = json!({ "from": owner_json(from), "to": owner_json(to), "approval": approval(id, "adopt") });
    push(ledger, fi, "Adopted", adopted);
}

/// The seq of the last event of `fi`: the epoch an `Adopted` just appended starts.
fn last_seq(f: &Fx, fi: &InstanceId) -> u64 {
    f.events(fi).last().unwrap()["seq"].as_u64().unwrap()
}

#[test]
fn an_approved_adoption_hands_the_instance_to_the_new_owner() {
    // ADR decision 6: the new session files an ask naming the instance, the old and new owner and
    // the progress; on the owner's approval the kernel appends `Adopted`. Completed steps stand,
    // claims in flight run out on their leases, and the new owner goes on from the next ready step.
    let (mut f, fi, clock) = started();
    let me = f.me.clone();
    let c1 = claim(&mut f.ledger, &me, &fi, "one").unwrap();
    assert_eq!(complete_as(&mut f, &me, &c1.token), "completed");
    let c2 = claim(&mut f.ledger, &me, &fi, "two").unwrap();

    let id = linked(&adopt(&mut f, &other(), &fi, Some(&proposing())).unwrap());
    assert!(id.starts_with("flow-adopt-"), "{id}");
    // The ask names the instance, both owners (session and marker digest, never the marker) and
    // the progress so far; the option set is the kernel's, and keep is the default.
    let body = proposal_body(&f, &fi);
    let ask = body["ask"].as_str().unwrap();
    for part in [
        fi.as_str(),
        "relay@1",
        "t3:-/owner",
        &marker_digest("marker-1"),
        "t3:-/other",
        &marker_digest("marker-9"),
        "1 of 3 nodes done (one)",
        "next ready: two",
    ] {
        assert!(ask.contains(part), "the question does not name {part}: {ask}");
    }
    let options: Vec<&str> = body["options"].as_array().unwrap().iter().map(|o| o["id"].as_str().unwrap()).collect();
    assert_eq!((options, &body["default"]), (vec!["adopt", "keep"], &json!("keep")));
    let log = std::fs::read_to_string(f.flows.join("instances").join(fi.as_str()).join("events.jsonl")).unwrap();
    for marker in ["marker-1", "marker-9"] {
        assert!(!f.ledger_text().contains(marker) && !log.contains(marker), "a marker was written");
    }

    // Approved, and landed on main: `Adopted`, with the answer's evidence.
    answer(&f, &id, Some("adopt"));
    f.land_on_main();
    match adopt(&mut f, &other(), &fi, None).unwrap() {
        Proposed::Adopted { from, to, source, ask_id, .. } => {
            assert_eq!((from, to), (super::owner(&me).unwrap(), super::owner(&other()).unwrap()));
            assert!(source.starts_with("main@"), "{source}");
            assert_eq!(ask_id, id);
        }
        other => panic!("{other:?}"),
    }
    let last = f.events(&fi).pop().unwrap();
    assert_eq!((&last["kind"], &last["body"]["approval"]["answer_option"]), (&json!("Adopted"), &json!("adopt")));
    let s = f.status(&fi);
    assert_eq!(s.owner.as_ref().map(ToString::to_string).as_deref(), Some("t3:-/other"));
    assert_eq!(s.nodes["one"].state, State::Done, "completed steps stand");

    // The former owner is refused every write, its own claim included.
    assert_eq!(code(heartbeat(&mut f.ledger, &me, &c2.token)), "not_owner");
    assert_eq!(complete_as(&mut f, &me, &c2.token), "not_owner");
    assert_eq!(code(claim(&mut f.ledger, &me, &fi, "two")), "not_owner");
    // Its token grants the new owner nothing, and its claim holds the node until the lease ends.
    assert_eq!(code(heartbeat(&mut f.ledger, &other(), &c2.token)), "former_owners_claim");
    assert_eq!(complete_as(&mut f, &other(), &c2.token), "former_owners_claim");
    assert_eq!(code(claim(&mut f.ledger, &other(), &fi, "two")), "already_claimed");
    clock.shift(SignedDuration::from_secs(DEFAULT_LEASE_SECS as i64 + 1));
    let c = claim(&mut f.ledger, &other(), &fi, "two").unwrap();
    assert_eq!(c.attempt, 2, "the next attempt");
    assert_eq!(complete_as(&mut f, &other(), &c.token), "completed");
    let c = claim(&mut f.ledger, &other(), &fi, "three").unwrap();
    assert_eq!(complete_as(&mut f, &other(), &c.token), "completed");
    // The projection is a cache: without it, replay still says who owns the instance.
    let _ = std::fs::remove_file(f.flows.join("projection.sqlite"));
    let s = f.status(&fi);
    assert_eq!(s.owner.as_ref().map(ToString::to_string).as_deref(), Some("t3:-/other"));
    assert!(s.nodes.values().all(|n| n.state == State::Done));
    // The new owner adopts nothing more; the former owner may propose to adopt it back.
    assert_eq!(code(adopt(&mut f, &other(), &fi, Some(&proposing()))), "already_owner");
}

#[test]
fn without_the_owners_approval_nothing_changes() {
    // ADR decision 6, "Without approval: nothing changes": unanswered, answered off main, unread,
    // declined, answered with another option or with none. The pins and links are the proposal's
    // own; the owner, its claims, the receipts and what is ready are as they were.
    let (mut f, fi, _clock) = started();
    let me = f.me.clone();
    let c1 = claim(&mut f.ledger, &me, &fi, "one").unwrap();
    let before = work(&f, &fi);
    let id = linked(&adopt(&mut f, &other(), &fi, Some(&proposing())).unwrap());
    assert_eq!(work(&f, &fi), before, "a pin and a link change no work");
    // Unanswered on main.
    f.land_on_main();
    assert_eq!(state(&adopt(&mut f, &other(), &fi, None).unwrap()), "waiting");
    // Approved in the working tree only, by anyone who can write it.
    answer(&f, &id, Some("adopt"));
    assert_eq!(state(&adopt(&mut f, &other(), &fi, None).unwrap()), "waiting");
    // Approved on Maestro's answers branch and not on main: waiting to land.
    f.put_on_answers_branch();
    match adopt(&mut f, &other(), &fi, None).unwrap() {
        Proposed::Waiting { landing, .. } => assert_eq!(landing.as_deref(), Some("maestro/answers")),
        other => panic!("{other:?}"),
    }
    // Landed, but the remote cannot be asked: unread is not unanswered.
    git(&f.ws, &["push", "-q", "origin", "main"]);
    let nowhere = f.tmp.path().join("nowhere.git");
    let origin = seams::REMOTE.with(|r| r.replace(Some(nowhere.to_str().unwrap().to_string())));
    assert_eq!(code(adopt(&mut f, &other(), &fi, None)), "answers_unread");
    seams::REMOTE.with(|r| *r.borrow_mut() = origin);
    git(&f.ws, &["reset", "-q", "--hard", "HEAD~1"]);
    git(&f.ws, &["push", "-q", "-f", "origin", "main"]);
    // Declined (keep), another spelling of the option, and an answer with no option.
    let mut id = id;
    for option in [Some("keep"), Some("Adopt"), None] {
        answer(&f, &id, option);
        f.land_on_main();
        match adopt(&mut f, &other(), &fi, None).unwrap() {
            Proposed::Declined { answer_option, .. } => assert_eq!(answer_option.as_deref(), option),
            other => panic!("{option:?}: {other:?}"),
        }
        // The same call again is declined again, filing or not: nothing is pinned or filed.
        let kinds = f.kinds(&fi);
        let asks = f.ledger_text();
        assert_eq!(state(&adopt(&mut f, &other(), &fi, Some(&proposing())).unwrap()), "declined");
        assert_eq!((f.kinds(&fi), f.ledger_text()), (kinds, asks), "the owner is not asked again");
        // Only asking again proposes anew.
        id = linked(&adopt_again(&mut f, &other(), &fi).unwrap());
    }
    assert!(!f.kinds(&fi).iter().any(|k| k == "Adopted"));
    assert_eq!(work(&f, &fi), before);
    // The owner's claim still holds, and it completes it; the proposer writes nothing.
    assert_eq!(code(heartbeat(&mut f.ledger, &other(), &c1.token)), "not_owner");
    assert_eq!(complete_as(&mut f, &me, &c1.token), "completed");
}

#[test]
fn an_approved_retirement_retires_the_instance_and_keeps_its_log() {
    // ADR decision 6: an agent proposes retiring an abandoned instance through an ask, and the
    // kernel retires it (log kept, never reopened) only after the owner's approval, through the
    // same move as `operator::retire`.
    let (mut f, fi, _clock) = started();
    let me = f.me.clone();
    let c1 = claim(&mut f.ledger, &me, &fi, "one").unwrap();
    assert_eq!(complete_as(&mut f, &me, &c1.token), "completed");
    let id = linked(&retire(&mut f, &other(), &fi, Some(&proposing())).unwrap());
    assert!(id.starts_with("flow-retire-"), "{id}");
    let body = proposal_body(&f, &fi);
    let options: Vec<&str> = body["options"].as_array().unwrap().iter().map(|o| o["id"].as_str().unwrap()).collect();
    assert_eq!(options, ["retire", "keep"]);
    assert!(body["ask"].as_str().unwrap().contains("1 of 3 nodes done (one)"));
    // Declined: nothing changes, and the owner goes on writing.
    answer(&f, &id, Some("keep"));
    f.land_on_main();
    assert_eq!(state(&retire(&mut f, &other(), &fi, None).unwrap()), "declined");
    assert!(f.flows.join("instances").join(fi.as_str()).is_dir());
    let c2 = claim(&mut f.ledger, &me, &fi, "two").unwrap();
    // Proposed again and approved: retired.
    let id = linked(&retire_again(&mut f, &other(), &fi).unwrap());
    answer(&f, &id, Some("retire"));
    f.land_on_main();
    let kept = std::fs::read_to_string(f.flows.join("instances").join(fi.as_str()).join("events.jsonl")).unwrap();
    match retire(&mut f, &other(), &fi, None).unwrap() {
        Proposed::Retired { ask_id, appended: Some(_), source: Some(_), .. } => assert_eq!(ask_id, id),
        other => panic!("{other:?}"),
    }
    assert!(!f.flows.join("instances").join(fi.as_str()).exists());
    let log = std::fs::read_to_string(f.flows.join("retired").join(fi.as_str()).join("events.jsonl")).unwrap();
    assert!(log.starts_with(&kept), "the log is kept as it was");
    let last: Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    assert_eq!((&last["kind"], &last["body"]["approval"]["answer_option"]), (&json!("Retired"), &json!("retire")));
    // Never reopened: every later write or read naming it is refused.
    assert_eq!(complete_as(&mut f, &me, &c2.token), "retired");
    assert_eq!(code(claim(&mut f.ledger, &me, &fi, "two")), "retired");
    assert_eq!(code(adopt(&mut f, &other(), &fi, Some(&proposing()))), "retired");
    assert_eq!(code(retire(&mut f, &other(), &fi, None)), "retired");
    let reader = Reader::new(&f.flows, Options::default()).unwrap();
    assert_eq!(code(crate::kernel::status(&reader.lock().unwrap(), &fi, clock::now())), "retired");
    assert_eq!(crate::operator::retire(&mut f.ledger, &fi).unwrap_err().code(), "retired");
}

#[test]
fn a_retirement_a_crash_left_unmoved_takes_no_write_and_the_next_call_moves_it() {
    // `Retired` is appended, then the instance moves, under one lock. A crash between the two
    // leaves a log ending in `Retired`: it takes no write, and the next call finishes the move.
    let (mut f, fi, _clock) = started();
    let me = f.me.clone();
    let id = linked(&retire(&mut f, &other(), &fi, Some(&proposing())).unwrap());
    let approval = json!({ "ask_id": id, "ledger": "ledger.yaml", "answer_option": "retire", "source": "main@x", "row_hash": "x" });
    append_raw(&mut f, &fi, "Retired", json!({ "by": owner_json(&other()), "approval": approval }));
    assert_eq!(f.status(&fi).retired.as_deref(), Some(id.as_str()));
    // Nothing counts after `Retired`: not even a whole adoption appended as it would be.
    let third = agent("third", "marker-3");
    push_adoption(&mut f.ledger, &fi, &me, &third, 0, "flow-adopt-late");
    let s = f.status(&fi);
    assert_eq!(s.owner.as_ref().map(ToString::to_string).as_deref(), Some("t3:-/owner"));
    let late: Vec<&str> =
        s.rejected.iter().filter(|r| r.subject == "flow-adopt-late").map(|r| r.reason.as_str()).collect();
    assert_eq!(late, ["retired", "retired", "retired"]);
    assert_eq!(code(claim(&mut f.ledger, &me, &fi, "one")), "retired");
    assert_eq!(code(adopt(&mut f, &other(), &fi, Some(&proposing()))), "retired");
    match retire(&mut f, &agent("third", "marker-3"), &fi, None).unwrap() {
        Proposed::Retired { ask_id, appended: None, .. } => assert_eq!(ask_id, id),
        other => panic!("{other:?}"),
    }
    assert!(f.flows.join("retired").join(fi.as_str()).join("events.jsonl").is_file());
    assert!(!f.flows.join("instances").join(fi.as_str()).exists());
}

#[test]
fn an_approval_for_an_owner_since_replaced_is_proposal_stale() {
    // The owner's answer approves the change the ask named, from the owner it named. Two sessions
    // propose; the first approved adopts; the second's approval names an owner that is gone.
    let (mut f, fi, _clock) = started();
    let (b, c) = (other(), agent("third", "marker-3"));
    let ib = linked(&adopt(&mut f, &b, &fi, Some(&proposing())).unwrap());
    let ic = linked(&adopt(&mut f, &c, &fi, Some(&proposing())).unwrap());
    let ir = linked(&retire(&mut f, &b, &fi, Some(&proposing())).unwrap());
    answer(&f, &ic, Some("adopt"));
    answer(&f, &ib, Some("adopt"));
    answer(&f, &ir, Some("retire"));
    f.land_on_main();
    assert_eq!(state(&adopt(&mut f, &c, &fi, None).unwrap()), "adopted");
    let kinds = f.kinds(&fi);
    assert_eq!(code(adopt(&mut f, &b, &fi, None)), "proposal_stale");
    assert_eq!(code(retire(&mut f, &b, &fi, None)), "proposal_stale");
    assert_eq!(f.kinds(&fi), kinds, "nothing recorded");
    assert_eq!(f.status(&fi).owner.as_ref().map(ToString::to_string).as_deref(), Some("t3:-/third"));
    assert!(f.flows.join("instances").join(fi.as_str()).is_dir());
    // Replay agrees: the same approvals, appended as they are, change nothing either.
    let me = f.me.clone();
    let approval = |id: &str, option: &str| json!({ "ask_id": id, "ledger": "ledger.yaml", "answer_option": option, "source": "main@x", "row_hash": "x" });
    append_raw(
        &mut f,
        &fi,
        "Adopted",
        json!({ "from": owner_json(&me), "to": owner_json(&b), "approval": approval(&ib, "adopt") }),
    );
    append_raw(&mut f, &fi, "Retired", json!({ "by": owner_json(&b), "approval": approval(&ir, "retire") }));
    let s = f.status(&fi);
    assert_eq!(s.owner.as_ref().map(ToString::to_string).as_deref(), Some("t3:-/third"));
    assert_eq!(s.retired, None);
    let why: Vec<&str> = s.rejected.iter().map(|r| r.reason.as_str()).collect();
    assert_eq!(why, ["owner_changed", "owner_changed"]);
    // The reads say the proposals are stale; a filing alone does not propose anew, asking again
    // does, naming the owner as it is now.
    let states: Vec<&str> = s.proposals.iter().map(|p| p.state).collect();
    assert_eq!(states, ["stale", "stale"]);
    assert_eq!(code(adopt(&mut f, &b, &fi, Some(&proposing()))), "proposal_stale");
    let again = linked(&adopt_again(&mut f, &b, &fi).unwrap());
    assert_ne!(again, ib);
    assert!(proposal_body(&f, &fi)["ask"].as_str().unwrap().contains("is owned by session t3:-/third"));
    let states: Vec<&str> = f.status(&fi).proposals.iter().map(|p| p.state).collect();
    assert_eq!(states, ["stale", "owner"]);
}

#[test]
fn replay_takes_ownership_only_from_an_approved_linked_proposal_of_the_owner_it_named() {
    // Nothing an agent appends stands in for the owner's approval: an `Adopted` or `Retired` with
    // no proposal, an unlinked one, another proposer's, another option, or another owner is set
    // aside, and the owner goes on writing.
    let (mut f, fi, _clock) = started();
    let me = f.me.clone();
    let (b, c) = (other(), agent("third", "marker-3"));
    let approval = |id: &str, option: &str| json!({ "ask_id": id, "ledger": "ledger.yaml", "answer_option": option, "source": "main@x", "row_hash": "x" });
    let adopted = |from: &VerifiedSession, to: &VerifiedSession, id: &str, option: &str| json!({ "from": owner_json(from), "to": owner_json(to), "approval": approval(id, option) });
    append_raw(&mut f, &fi, "Adopted", adopted(&me, &b, "flow-adopt-none", "adopt"));
    append_raw(
        &mut f,
        &fi,
        "Retired",
        json!({ "by": owner_json(&b), "approval": approval("flow-retire-none", "retire") }),
    );
    // Pinned and not linked.
    let pin = |id: &str, hash: &str| json!({ "proposal": "adoption", "by": owner_json(&b), "owner": owner_json(&me), "ask_id": id, "ledger": "ledger.yaml", "body": {}, "body_hash": hash });
    append_raw(&mut f, &fi, "ProposalPinned", pin("flow-adopt-raw", "sha256:pinned"));
    append_raw(&mut f, &fi, "Adopted", adopted(&me, &b, "flow-adopt-raw", "adopt"));
    // A link with another hash does not link it.
    append_raw(
        &mut f,
        &fi,
        "ProposalLinked",
        json!({ "ask_id": "flow-adopt-raw", "ledger": "ledger.yaml", "body_hash": "sha256:other" }),
    );
    append_raw(&mut f, &fi, "Adopted", adopted(&me, &b, "flow-adopt-raw", "adopt"));
    append_raw(
        &mut f,
        &fi,
        "ProposalLinked",
        json!({ "ask_id": "flow-adopt-raw", "ledger": "ledger.yaml", "body_hash": "sha256:pinned" }),
    );
    // Linked: still not another option, another proposer, or another owner.
    append_raw(&mut f, &fi, "Adopted", adopted(&me, &b, "flow-adopt-raw", "keep"));
    append_raw(&mut f, &fi, "Adopted", adopted(&me, &c, "flow-adopt-raw", "adopt"));
    append_raw(&mut f, &fi, "Adopted", adopted(&c, &b, "flow-adopt-raw", "adopt"));
    let s = f.status(&fi);
    assert_eq!(s.owner.as_ref().map(ToString::to_string).as_deref(), Some("t3:-/owner"));
    assert_eq!(s.retired, None);
    let why: Vec<&str> = s.rejected.iter().map(|r| r.reason.as_str()).collect();
    assert_eq!(
        why,
        [
            "not_proposed",
            "not_proposed",
            "not_linked",
            "not_pinned",
            "not_linked",
            "not_approved",
            "not_the_proposer",
            "not_the_owner_named"
        ]
    );
    let c1 = claim(&mut f.ledger, &me, &fi, "one").unwrap();
    assert_eq!(complete_as(&mut f, &me, &c1.token), "completed");
    // The approved, linked proposal of the owner it named does count.
    append_raw(&mut f, &fi, "Adopted", adopted(&me, &b, "flow-adopt-raw", "adopt"));
    assert_eq!(f.status(&fi).owner.as_ref().map(ToString::to_string).as_deref(), Some("t3:-/other"));
    assert_eq!(code(claim(&mut f.ledger, &me, &fi, "two")), "not_owner");
}

#[test]
fn who_may_propose() {
    // A session with an owner marker; for adoption, one that does not own the instance; for
    // retirement, the owner too. A proposal body holding a key the kernel writes is refused before
    // anything is pinned, and so is a first proposal with no filing.
    let (mut f, fi, _clock) = started();
    let me = f.me.clone();
    assert_eq!(code(adopt(&mut f, &me, &fi, Some(&proposing()))), "already_owner");
    let unmarked = VerifiedSession::new("t3:-/x".parse().unwrap(), "/tmp", None, None, vec![]);
    assert_eq!(code(adopt(&mut f, &unmarked, &fi, Some(&proposing()))), "no_owner_marker");
    assert_eq!(code(retire(&mut f, &unmarked, &fi, Some(&proposing()))), "no_owner_marker");
    assert_eq!(code(adopt(&mut f, &other(), &fi, None)), "ask_body_invalid");
    for key in PROPOSAL_KEYS {
        let mut given = proposing();
        given.body.insert(key.into(), json!("adopt"));
        assert_eq!(code(adopt(&mut f, &other(), &fi, Some(&given))), "ask_body_invalid", "{key}");
    }
    let mut smuggled = proposing();
    smuggled.body.insert("answer_option".into(), json!("adopt"));
    assert_eq!(code(adopt(&mut f, &other(), &fi, Some(&smuggled))), "ask_body_invalid");
    assert_eq!(f.kinds(&fi), ["Started"]);
    assert_eq!(state(&retire(&mut f, &me, &fi, Some(&proposing())).unwrap()), "waiting");
}

#[test]
fn a_writer_refusal_withdraws_the_proposal_and_the_next_call_proposes_anew() {
    let (mut f, fi, _clock) = started();
    let mut bad = proposing();
    bad.body.insert("stakes".into(), json!("STUB-INVALID"));
    match adopt(&mut f, &other(), &fi, Some(&bad)).unwrap() {
        Proposed::Withdrawn { code, .. } => assert_eq!(code, PROPOSAL_NOT_FILED),
        other => panic!("{other:?}"),
    }
    assert_eq!(f.kinds(&fi), ["Started", "ProposalPinned", "ProposalWithdrawn"]);
    assert!(f.status(&fi).proposals.is_empty());
    linked(&adopt(&mut f, &other(), &fi, Some(&proposing())).unwrap());
    assert_eq!(f.kinds(&fi)[3..], ["ProposalPinned", "ProposalLinked"]);
}

#[test]
fn the_flows_lock_is_free_while_a_proposal_is_filed() {
    // BRO-3624 item 1: the writer of record checks, while it runs, that it can take both OS locks
    // a flows writer holds.
    let (mut f, fi, _clock) = started();
    let lock = f.flows.parent().unwrap().join("flows.lock");
    let check = format!(
        "for p in [{lock:?}, {root:?}]:\n    fd = os.open(p, os.O_RDONLY)\n    try:\n        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)\n        fcntl.flock(fd, fcntl.LOCK_UN)\n    except BlockingIOError:\n        sys.stdin.read()\n        print('error: a flows lock is held while filing', file=sys.stderr)\n        sys.exit(2)\n    finally:\n        os.close(fd)\nopen(os.path.join(HERE, 'checked'), 'w').write('ok')",
        root = f.flows,
    );
    std::fs::write(f.ws.join("scripts/ask_ledger.py"), wrapper(&check)).unwrap();
    linked(&adopt(&mut f, &other(), &fi, Some(&proposing())).unwrap());
    assert!(f.ws.join("scripts/checked").exists(), "the writer checked the locks");
    std::fs::remove_file(f.ws.join("scripts/checked")).unwrap();
    linked(&retire(&mut f, &other(), &fi, Some(&proposing())).unwrap());
    assert!(f.ws.join("scripts/checked").exists(), "the writer checked the locks");
}

#[test]
fn a_proposal_is_found_by_its_pinned_id_never_by_searching() {
    // A decoy ask approving an adoption of this instance, answered on main, is never taken for the
    // proposal: the kernel reads the row its pin names.
    let (mut f, fi, _clock) = started();
    let decoy = format!(
        "\n  - id: decoy-1\n    ask: \"Adopt {fi}?\"\n    headline: \"Hand flow instance {fi} to t3:-/other?\"\n    answer: \"Done\"\n    answer_option: \"adopt\"\n    answered_at: \"2026-11-09T00:00:00Z\"\n"
    );
    f.add_entry(&decoy);
    f.land_on_main();
    linked(&adopt(&mut f, &other(), &fi, Some(&proposing())).unwrap());
    f.land_on_main();
    assert_eq!(state(&adopt(&mut f, &other(), &fi, None).unwrap()), "waiting");
    assert_eq!(f.kinds(&fi), ["Started", "ProposalPinned", "ProposalLinked"]);
    let src = include_str!("adoption.rs");
    assert!(!src.contains(".asks()") && !src.contains("asks_list") && !src.contains("ask_ledger("));
}

#[test]
fn the_reads_report_proposals_as_data_and_write_nothing() {
    let (mut f, fi, _clock) = started();
    seams::CRASH_AFTER_FILE.with(|c| c.set(true));
    assert_eq!(code(adopt(&mut f, &other(), &fi, Some(&proposing()))), "encode", "the simulated crash");
    let p = &f.status(&fi).proposals;
    assert_eq!((p.len(), p[0].state, p[0].by.to_string()), (1, "unfiled", "t3:-/other".to_string()));
    let id = linked(&adopt(&mut f, &other(), &fi, None).unwrap());
    answer(&f, &id, Some("adopt"));
    f.put_on_answers_branch();
    let before = machine(&f);
    let reader = Reader::new(&f.flows, Options::default()).unwrap();
    let guard = reader.lock().unwrap();
    let mut next = crate::kernel::next(&guard, &fi, clock::now()).unwrap();
    drop(guard);
    assert_eq!((next.proposals[0].ask_id.as_str(), next.proposals[0].state), (id.as_str(), "owner"));
    landing(&f.paths, &mut next.proposals);
    assert_eq!(next.proposals[0].state, "answer_waiting_to_land");
    let text = serde_json::to_string(&next).unwrap();
    assert!(!text.contains("\"answer\"") && !text.contains("answer_option") && !text.contains("Done"), "{text}");
    assert!(machine(&f) == before, "a read wrote");
}

#[test]
fn a_proposal_that_changed_while_it_was_filed_records_nothing() {
    // Between filing (flows lock released) and linking, the proposal was withdrawn, or replaced by
    // another of the same session's: nothing is linked to a proposal that no longer stands.
    for replaced in [false, true] {
        let hold = "open(os.path.join(HERE, 'reached'), 'w').write('1')\nimport time\nwhile not os.path.exists(os.path.join(HERE, 'go')):\n    time.sleep(0.01)";
        let (mut f, fi, _clock) = started();
        std::fs::write(f.ws.join("scripts/ask_ledger.py"), wrapper(hold)).unwrap();
        let (paths, flows, fi2) = (f.paths.clone(), f.flows.clone(), fi.clone());
        let filer = std::thread::spawn(move || {
            let mut ledger = Ledger::open(&flows, &host("mac"), Options::default()).unwrap();
            let asking = Proposing { filing: Some(&proposing()), again: false };
            code(propose_adoption(&mut ledger, &paths, &other(), &fi2, asking))
        });
        let reached = f.ws.join("scripts/reached");
        let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !reached.exists() {
            assert!(std::time::Instant::now() < until, "the writer never ran");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let pin: Value = f.events(&fi).into_iter().find(|e| e["kind"] == "ProposalPinned").unwrap();
        let id = pin["body"]["ask_id"].as_str().unwrap().to_string();
        append_raw(&mut f, &fi, "ProposalWithdrawn", json!({ "ask_id": id, "code": "x", "reason": "x" }));
        if replaced {
            let mut again = pin["body"].clone();
            again["ask_id"] = json!("flow-adopt-replaced");
            append_raw(&mut f, &fi, "ProposalPinned", again);
        }
        std::fs::write(f.ws.join("scripts/go"), "").unwrap();
        assert_eq!(filer.join().unwrap(), "proposal_changed", "replaced: {replaced}");
        assert!(!f.kinds(&fi).iter().any(|k| k == "ProposalLinked"), "nothing linked");
    }
}

// P20 round 1 (BRO-3635): ownership epochs, the owner's checks around a verdict, bounded proposals,
// and the guards no test killed.

#[test]
fn an_approval_given_while_stale_never_applies_even_when_the_owner_returns() {
    // A owns; B proposes adopting it. C's adoption is approved, so B's proposal is stale; the
    // owner approves it anyway; A takes the instance back. B's approval still never applies: a
    // proposal belongs to the ownership epoch it was pinned under.
    let (mut f, fi, _clock) = started();
    let (me, b, c) = (f.me.clone(), other(), agent("third", "marker-3"));
    let ib = linked(&adopt(&mut f, &b, &fi, Some(&proposing())).unwrap());
    let rb = linked(&retire(&mut f, &b, &fi, Some(&proposing())).unwrap());
    let ic = linked(&adopt(&mut f, &c, &fi, Some(&proposing())).unwrap());
    answer(&f, &ic, Some("adopt"));
    f.land_on_main();
    assert_eq!(state(&adopt(&mut f, &c, &fi, None).unwrap()), "adopted");
    // Stale and unanswered: refused at once, before the answers remote is asked.
    let nowhere = f.tmp.path().join("nowhere.git");
    let origin = seams::REMOTE.with(|r| r.replace(Some(nowhere.to_str().unwrap().to_string())));
    assert_eq!(code(adopt(&mut f, &b, &fi, None)), "proposal_stale");
    seams::REMOTE.with(|r| *r.borrow_mut() = origin);
    answer(&f, &ib, Some("adopt"));
    f.land_on_main();
    assert_eq!(code(adopt(&mut f, &b, &fi, None)), "proposal_stale");
    // A proposes taking it back, and the owner approves.
    let ia = linked(&adopt(&mut f, &me, &fi, Some(&proposing())).unwrap());
    answer(&f, &ia, Some("adopt"));
    f.land_on_main();
    assert_eq!(state(&adopt(&mut f, &me, &fi, None).unwrap()), "adopted");
    let back = last_seq(&f, &fi);
    assert_eq!(f.status(&fi).owner.as_ref().map(ToString::to_string).as_deref(), Some("t3:-/owner"));
    // B's approval, given while it was stale, does not apply now that A owns it again.
    let kinds = f.kinds(&fi);
    assert_eq!(code(adopt(&mut f, &b, &fi, None)), "proposal_stale");
    assert_eq!(f.kinds(&fi), kinds, "nothing recorded");
    let s = f.status(&fi);
    assert_eq!(s.owner.as_ref().map(ToString::to_string).as_deref(), Some("t3:-/owner"));
    let states: Vec<&str> = s.proposals.iter().filter(|p| p.ask_id == ib || p.ask_id == rb).map(|p| p.state).collect();
    assert_eq!(states, ["stale", "stale"]);
    // B's unanswered retirement, pinned while A owned it, is stale too: refused at once.
    let origin = seams::REMOTE.with(|r| r.replace(Some(nowhere.to_str().unwrap().to_string())));
    assert_eq!(code(retire(&mut f, &b, &fi, None)), "proposal_stale");
    seams::REMOTE.with(|r| *r.borrow_mut() = origin);
    // Replay agrees: the same approval, appended as it would be, is set aside.
    let adopted = json!({ "from": owner_json(&me), "to": owner_json(&b), "approval": approval(&ib, "adopt") });
    append_raw(&mut f, &fi, "Adopted", adopted);
    let s = f.status(&fi);
    assert_eq!(s.owner.as_ref().map(ToString::to_string).as_deref(), Some("t3:-/owner"));
    assert_eq!(s.rejected.last().map(|r| r.reason.as_str()), Some("owner_changed"));
    // A pin within the current epoch does apply: the epoch is the one A took it back in.
    push_adoption(&mut f.ledger, &fi, &me, &b, back, "flow-adopt-now");
    assert_eq!(f.status(&fi).owner.as_ref().map(ToString::to_string).as_deref(), Some("t3:-/other"));
}

#[test]
fn a_retirement_approved_while_stale_never_retires_even_when_the_owner_returns() {
    // C proposes retiring A's instance; B adopts it, then A takes it back; the owner approves C's
    // retirement. It never applies: the instance changed owner since it was pinned.
    let (mut f, fi, _clock) = started();
    let (me, b, c) = (f.me.clone(), other(), agent("third", "marker-3"));
    let ir = linked(&retire(&mut f, &c, &fi, Some(&proposing())).unwrap());
    let ib = linked(&adopt(&mut f, &b, &fi, Some(&proposing())).unwrap());
    answer(&f, &ib, Some("adopt"));
    f.land_on_main();
    assert_eq!(state(&adopt(&mut f, &b, &fi, None).unwrap()), "adopted");
    let ia = linked(&adopt(&mut f, &me, &fi, Some(&proposing())).unwrap());
    answer(&f, &ia, Some("adopt"));
    f.land_on_main();
    assert_eq!(state(&adopt(&mut f, &me, &fi, None).unwrap()), "adopted");
    answer(&f, &ir, Some("retire"));
    f.land_on_main();
    assert_eq!(code(retire(&mut f, &c, &fi, None)), "proposal_stale");
    assert!(f.flows.join("instances").join(fi.as_str()).is_dir(), "not retired");
    let retired = json!({ "by": owner_json(&c), "approval": approval(&ir, "retire") });
    append_raw(&mut f, &fi, "Retired", retired);
    let s = f.status(&fi);
    assert_eq!(s.retired, None);
    assert_eq!(s.rejected.last().map(|r| r.reason.as_str()), Some("owner_changed"));
    assert_eq!(s.proposals.iter().find(|p| p.ask_id == ir).unwrap().state, "stale");
    // The owner goes on writing.
    let c1 = claim(&mut f.ledger, &me, &fi, "one").unwrap();
    assert_eq!(complete_as(&mut f, &me, &c1.token), "completed");
}

#[test]
fn an_owner_change_between_reading_an_approval_and_applying_it_is_proposal_stale() {
    // The approval is read with the flows lock released; another adoption lands before it is
    // applied. Under the lock, the kernel finds the epoch moved: nothing is appended, and a
    // retirement moves nothing.
    for kind in ["adoption", "retirement"] {
        let (mut f, fi, _clock) = started();
        let (me, b) = (f.me.clone(), other());
        let id = if kind == "adoption" {
            linked(&adopt(&mut f, &b, &fi, Some(&proposing())).unwrap())
        } else {
            linked(&retire(&mut f, &b, &fi, Some(&proposing())).unwrap())
        };
        answer(&f, &id, Some(if kind == "adoption" { "adopt" } else { "retire" }));
        f.land_on_main();
        let (flows, fi2, me2) = (f.flows.clone(), fi.clone(), me.clone());
        super::seams::BEFORE_APPLY.with(|c| {
            *c.borrow_mut() = Some(Box::new(move || {
                let mut ledger = Ledger::open(&flows, &host("mac"), Options::default()).unwrap();
                push_adoption(&mut ledger, &fi2, &me2, &agent("third", "marker-3"), 0, "flow-adopt-meanwhile");
            }));
        });
        let result = if kind == "adoption" { adopt(&mut f, &b, &fi, None) } else { retire(&mut f, &b, &fi, None) };
        assert_eq!(code(result), "proposal_stale", "{kind}");
        assert_eq!(f.kinds(&fi).last().map(String::as_str), Some("Adopted"), "{kind}: nothing after the meanwhile");
        assert_eq!(f.status(&fi).owner.as_ref().map(ToString::to_string).as_deref(), Some("t3:-/third"));
        assert!(f.flows.join("instances").join(fi.as_str()).is_dir(), "{kind}: not retired");
    }
}

#[test]
fn a_verdict_is_recorded_only_while_its_writer_still_owns_the_instance() {
    // `complete` settles a repeat's iteration and evaluates its `until` with the flows lock
    // released; an adoption lands meanwhile. The former owner records no verdict.
    let mut f = fx(None, "");
    f.flow = compile(&format!(
        "flow: loop\nversion: 1\nworkdir: {}\nnodes:\n  step: {{class: pure, run: ['true']}}\nrepeats:\n  rounds: {{body: [step], max: 3, until: '.nodes.step.outputs == null'}}\n",
        f.ws.display()
    ))
    .unwrap();
    let fi = f.begin();
    let me = f.me.clone();
    let c = claim(&mut f.ledger, &me, &fi, "step").unwrap();
    let (flows, fi2, me2) = (f.flows.clone(), fi.clone(), me.clone());
    let once = std::cell::Cell::new(false);
    crate::kernel::guard::tests::WHILE_JUDGING.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            if !once.replace(true) {
                let mut ledger = Ledger::open(&flows, &host("mac"), Options::default()).unwrap();
                push_adoption(&mut ledger, &fi2, &me2, &other(), 0, "flow-adopt-meanwhile");
            }
        }));
    });
    match complete(&mut f.ledger, &me, &c.token, Value::Null, &Limits::default()).unwrap() {
        Completion::Completed { verdicts, .. } => match verdicts.as_slice() {
            [crate::kernel::Verdict::Unrecorded { code, .. }] => assert_eq!(*code, "not_owner"),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
    assert!(!f.kinds(&fi).iter().any(|k| k == "Guarded"), "{:?}", f.kinds(&fi));
    // The repeat still awaits its verdict. The former owner's claim is refused before any guard
    // runs; the new owner's claim evaluates it first and records it.
    let judged = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = judged.clone();
    crate::kernel::guard::tests::WHILE_JUDGING
        .with(|hook| *hook.borrow_mut() = Some(Box::new(move || count.set(count.get() + 1))));
    assert_eq!(code(claim(&mut f.ledger, &me, &fi, "step")), "not_owner");
    assert_eq!(judged.get(), 0, "a guard ran for a session that does not own the instance");
    // (The verdict holds, so the repeat is done and `step` takes no claim.)
    assert_eq!(code(claim(&mut f.ledger, &other(), &fi, "step")), "not_enabled");
    assert_eq!(judged.get(), 1);
    crate::kernel::guard::tests::WHILE_JUDGING.with(|hook| *hook.borrow_mut() = None);
    assert!(f.kinds(&fi).iter().any(|k| k == "Guarded"), "{:?}", f.kinds(&fi));
}

#[test]
fn a_session_pins_a_bounded_number_of_proposals() {
    // A stranger whose filings the writer keeps refusing (pinned, then withdrawn) stops at
    // `MAX_PROPOSALS`: the owner's log does not grow without the owner's part.
    let (mut f, fi, _clock) = started();
    let mut bad = proposing();
    bad.body.insert("stakes".into(), json!("STUB-INVALID"));
    for _ in 0..MAX_PROPOSALS {
        assert_eq!(state(&adopt(&mut f, &other(), &fi, Some(&bad)).unwrap()), "withdrawn");
    }
    let before = machine(&f);
    assert_eq!(code(adopt(&mut f, &other(), &fi, Some(&bad))), "proposals_exhausted");
    assert_eq!(code(retire(&mut f, &other(), &fi, Some(&proposing()))), "proposals_exhausted");
    assert!(machine(&f) == before, "a refused proposal wrote");
    assert_eq!(f.kinds(&fi).len(), 1 + 2 * MAX_PROPOSALS);
    // Another session, and the owner, are not held to the stranger's count.
    let me = f.me.clone();
    assert_eq!(state(&retire(&mut f, &me, &fi, Some(&proposing())).unwrap()), "waiting");
    assert_eq!(state(&adopt(&mut f, &agent("third", "marker-3"), &fi, Some(&proposing())).unwrap()), "waiting");
}

#[test]
fn a_strangers_proposal_never_fails_the_owners_ask_but_an_adoption_does() {
    // Between the owner's mint and its pin, a stranger pins and withdraws a proposal: the owner's
    // ask is pinned and filed all the same. An adoption landing there instead stops it.
    for adoption in [false, true] {
        let mut f = fx(None, "");
        let fi = f.begin();
        let (flows, fi2, me) = (f.flows.clone(), fi.clone(), f.me.clone());
        crate::kernel::asks::seams::BEFORE_PIN.with(|c| {
            *c.borrow_mut() = Some(Box::new(move || {
                let mut ledger = Ledger::open(&flows, &host("mac"), Options::default()).unwrap();
                if adoption {
                    push_adoption(&mut ledger, &fi2, &me, &other(), 0, "flow-adopt-meanwhile");
                } else {
                    push_proposal(&mut ledger, &fi2, "adoption", &me, &other(), 0, "flow-adopt-x");
                    push(
                        &mut ledger,
                        &fi2,
                        "ProposalWithdrawn",
                        json!({ "ask_id": "flow-adopt-y", "code": "x", "reason": "x" }),
                    );
                }
            }));
        });
        let result = f.ask(&fi, Some(&filing()));
        if adoption {
            assert_eq!(code(result), "stale_seq");
            assert!(!f.kinds(&fi).iter().any(|k| k == "AskPinned"), "a former owner pinned");
        } else {
            assert!(matches!(result.unwrap(), crate::kernel::Asked::Waiting { appended: Some(_), .. }));
            assert_eq!(f.kinds(&fi)[4..], ["AskPinned", "AskLinked"]);
        }
    }
}

#[test]
fn replay_keeps_a_proposal_to_the_owner_it_names_and_the_kind_it_is() {
    // A pin naming someone other than the owner is set aside, so no approval of it applies; and an
    // approval applies only to a proposal of its own kind.
    let (f, fi, _clock) = started();
    let (me, b, c) = (f.me.clone(), other(), agent("third", "marker-3"));
    let mut ledger = Ledger::open(&f.flows, &host("mac"), Options::default()).unwrap();
    push_proposal(&mut ledger, &fi, "adoption", &c, &b, 0, "flow-adopt-misnamed");
    let adopted =
        json!({ "from": owner_json(&c), "to": owner_json(&b), "approval": approval("flow-adopt-misnamed", "adopt") });
    push(&mut ledger, &fi, "Adopted", adopted);
    push_proposal(&mut ledger, &fi, "retirement", &me, &b, 0, "flow-retire-kind");
    let adopted =
        json!({ "from": owner_json(&me), "to": owner_json(&b), "approval": approval("flow-retire-kind", "adopt") });
    push(&mut ledger, &fi, "Adopted", adopted);
    let s = f.status(&fi);
    assert_eq!(s.owner.as_ref().map(ToString::to_string).as_deref(), Some("t3:-/owner"));
    let why: Vec<(&str, &str)> = s.rejected.iter().map(|r| (r.kind.as_str(), r.reason.as_str())).collect();
    assert_eq!(
        why,
        [
            ("ProposalPinned", "not_the_owner_named"),
            ("ProposalLinked", "not_pinned"),
            ("Adopted", "not_proposed"),
            ("Adopted", "not_proposed")
        ]
    );
}

#[test]
fn an_instance_with_no_recorded_owner_takes_no_proposal() {
    let (mut f, _fi, _clock) = started();
    let fi = InstanceId::new("fi_legacy").unwrap();
    let started = crate::kernel::events::Started {
        flow: f.flow.name(),
        ir_hash: f.flow.hash().into(),
        ir: f.flow.ir().clone(),
        params: json!({}),
        owner: None,
    };
    f.ledger.lock().unwrap().append(&fi, 0, started.to_event().unwrap()).unwrap();
    assert_eq!(code(adopt(&mut f, &other(), &fi, Some(&proposing()))), "no_owner");
    assert_eq!(code(retire(&mut f, &other(), &fi, Some(&proposing()))), "no_owner");
    assert_eq!(f.kinds(&fi), ["Started"]);
}
