//! The claim, heartbeat and complete tests that set or shift the clock the writers read
//! (BRO-3633 PR 3b2a). They live in the crate under `cfg(test)` because the test clock exists in
//! no other build (see `clock`'s module docs): a test installs it on its own thread, and sets or
//! shifts it instead of sleeping. Where the clock must move while a command runs, a thread shifts
//! it once the command has started, and where it must move while a guard runs, the guard
//! evaluator's test hook does (`guard::tests::WHILE_JUDGING`). The effect tests that kill a child
//! at a pause point (`hooks`) are here too, for the same reason. The fixture is the integration
//! tests' own (`tests/kernel/claiming.rs`).

#[path = "../../tests/kernel/claiming.rs"]
mod claiming;

use crate::clock::{self, TestClock};
// The shared helpers, loaded once in this binary (`hooked`); `claiming` re-exports them.
use crate::hooked::common;
use crate::kernel::events::{Completed, KernelEvent};
use crate::kernel::{Completion, DEFAULT_LEASE_SECS, Next, Reason, State, claim, complete, heartbeat};
use claiming::*;
use jiff::{SignedDuration, Timestamp};
use maestro_core::session::VerifiedSession;
use serde_json::{Value, json};
use std::time::Duration;

/// The fixture, with a test clock installed on this thread: what the writers read here.
fn clocked(yaml: &str) -> (Fx, TestClock) {
    (fx(yaml), TestClock::install())
}

/// Shifts `clock` by `by` once the command `complete` runs has made `mark` in the workdir (an
/// earlier `mark` is removed first), while the command still runs. Join it after.
fn shift_while_running(f: &Fx, clock: &TestClock, mark: &str, by: SignedDuration) -> std::thread::JoinHandle<()> {
    let mark = f.dir.join(mark);
    let _ = std::fs::remove_file(&mark);
    let clock = clock.clone();
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !mark.exists() {
            assert!(std::time::Instant::now() < deadline, "the command never started");
            std::thread::sleep(Duration::from_millis(1));
        }
        clock.shift(by);
    })
}

#[test]
fn a_resumed_session_finishes_its_own_claim_until_the_lease_ends_and_a_replacement_is_refused() {
    // Owner decision (BRO-3633): a claim is bound to its owner (session and marker) and to its
    // lease. The marker survives turns, provider switches and restarts, so a resumed session
    // (here a new process on another provider, with the same SessionRef and marker) heartbeats and
    // completes its own claim; the lease is the bound on a restart. A replacement (same SessionRef,
    // new marker) is refused every write on the instance, during the lease and after it.
    let (mut f, test_clock) = clocked(CHAIN);
    let fi = f.begin();
    let c = f.claim(&fi, "head");
    let resumed = VerifiedSession::new(
        "t3:-/owner".parse().unwrap(),
        "/elsewhere",
        Some(maestro_core::session::Harness::ClaudeCode),
        None,
        vec![],
    )
    .with_owner_marker("marker-1");
    let replacement = agent("owner", "marker-2");
    assert_eq!(code(heartbeat(&mut f.ledger, &replacement, &c.token)), "not_owner");
    assert_eq!(code(complete(&mut f.ledger, &replacement, &c.token, Value::Null, &quick())), "not_owner");
    assert_eq!(code(heartbeat(&mut f.ledger, &resumed, &c.token)), "ok");
    let done = complete(&mut f.ledger, &resumed, &c.token, Value::Null, &quick()).unwrap();
    assert_eq!(outcome(&done), "completed", "the resumed session finished its own claim");
    let receipt: Completed = serde_json::from_value(f.last_body(&fi)).unwrap();
    assert_eq!(receipt.performer.unwrap().harness, "claude-code");

    // The lease bounds the resumed session too: past it the token is void, and the next claim,
    // the resumed session's or the owner's, takes the next attempt. The replacement takes none.
    assert_eq!(f.kinds(&fi), ["Started", "Claimed", "Heartbeat", "Completed"]);
    let fi = f.begin();
    let c = f.claim(&fi, "head");
    test_clock.set(c.lease_until);
    assert_eq!(code(complete(&mut f.ledger, &resumed, &c.token, Value::Null, &quick())), "lease_expired");
    assert_eq!(code(claim(&mut f.ledger, &replacement, &fi, "head")), "not_owner");
    assert_eq!(code(heartbeat(&mut f.ledger, &replacement, &c.token)), "not_owner");
    assert_eq!(claim(&mut f.ledger, &resumed, &fi, "head").unwrap().attempt, 2);
    assert_eq!(f.kinds(&fi), ["Started", "Claimed", "Claimed"]);
}

#[test]
fn claim_takes_only_an_enabled_node_the_kernel_can_verify() {
    // Spec acceptance (BRO-3624 item 10): out-of-order refusal, and write-time freshness.
    let (mut f, test_clock) = clocked(CHAIN);
    let fi = f.begin();
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "build")), "not_enabled");
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "ghost")), "unknown_node");
    // A choice is claimed, then chosen (3b2b); a human node waits for the owner and is never claimed.
    assert_eq!(f.claim(&fi, "pick").attempt, 1);
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "owner")), "not_claimable");
    let at = f.done(&fi, "head");
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "head")), "not_enabled", "done");
    // Fresh, what consumes head is enabled; once head has gone stale by the clock, it is not.
    test_clock.set(at + secs(59));
    assert_eq!(f.claim(&fi, "build").attempt, 1);
    let fi = f.begin();
    let at = f.done(&fi, "head");
    test_clock.set(at + secs(61));
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "build")), "not_enabled");
    assert_eq!(f.kinds(&fi), ["Started", "Claimed", "Completed"], "a refused claim writes nothing");
}

#[test]
fn a_held_node_is_not_claimed_again_and_the_next_claim_after_expiry_takes_the_next_attempt() {
    // Spec acceptance: a refusal during the lease leaves the attempt; the next claim increments it.
    let (mut f, test_clock) = clocked(CHAIN);
    let fi = f.begin();
    let now = clock::now();
    test_clock.set(now);
    let first = f.claim(&fi, "head");
    assert_eq!((first.attempt, first.lease_until), (1, now + secs(DEFAULT_LEASE_SECS as i64)));
    test_clock.set(first.lease_until - secs(1));
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "head")), "already_claimed");
    std::fs::write(f.dir.join("head"), "7").unwrap();
    let refused = f.complete(&first.token, Value::Null);
    assert_eq!(outcome(&refused), "exit_unmapped");
    assert_eq!(f.last_body(&fi)["attempt"], 1, "a refusal keeps the attempt");
    test_clock.set(first.lease_until);
    let second = f.claim(&fi, "head");
    assert_eq!(second.attempt, 2);
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &first.token)), "lease_expired");
}

#[test]
fn a_lease_ends_at_its_time_and_a_heartbeat_moves_it() {
    // Spec acceptance: lease expiry makes complete and heartbeat refuse lease_expired.
    let (mut f, test_clock) = clocked(CHAIN);
    let fi = f.begin();
    let c = f.claim(&fi, "head");
    let end = c.lease_until;
    test_clock.set(end - secs(1));
    let beat = heartbeat(&mut f.ledger, &f.me, &c.token).unwrap();
    assert_eq!(beat.lease_until, end - secs(1) + secs(DEFAULT_LEASE_SECS as i64));
    test_clock.set(end);
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "completed", "the heartbeat kept it");
    assert_eq!(f.kinds(&fi), ["Started", "Claimed", "Heartbeat", "Completed"]);
    // Without one, the lease ends at its time.
    let fi = f.begin();
    let c = f.claim(&fi, "head");
    test_clock.set(c.lease_until);
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &c.token)), "lease_expired", "the lease ends at its time");
    assert_eq!(code(complete(&mut f.ledger, &f.me, &c.token, Value::Null, &quick())), "lease_expired");
    assert_eq!(f.kinds(&fi), ["Started", "Claimed"]);
}

#[test]
fn a_heartbeat_lease_is_the_node_s_and_bounded() {
    let (mut f, test_clock) = clocked(
        "flow: w\nversion: 1\nparams: [dir]\nworkdir: \"{{params.dir}}\"\nnodes:\n  short: {class: work, verify: [true], wait: {external: ci, heartbeat: 30s}}\n  long: {class: work, verify: [true], wait: {external: ci, heartbeat: 1000000h}}\n",
    );
    let fi = f.begin();
    let now = clock::now();
    test_clock.set(now);
    assert_eq!(f.claim(&fi, "short").lease_until, now + secs(30));
    assert_eq!(f.claim(&fi, "long").lease_until, now + secs(maestro_flow::kernel::MAX_LEASE_SECS as i64));
    // A clock so late that a lease would end past the last time there is.
    let fresh = f.begin();
    test_clock.set(Timestamp::MAX - secs(10));
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fresh, "short")), "clock_out_of_range");
}

#[test]
fn a_receipt_is_written_only_while_what_it_consumes_is_fresh() {
    // The writer contract: write-time freshness, at the wall clock.
    let (mut f, test_clock) = clocked(CHAIN);
    let fi = f.begin();
    let at = f.done(&fi, "head");
    let c = f.claim(&fi, "build");
    test_clock.set(at + secs(61));
    let refused = f.complete(&c.token, Value::Null);
    assert_eq!(outcome(&refused), "input_stale");
    assert!(!f.dir.join("built").exists(), "nothing ran");
    // head re-run with the same result re-validates build's inputs, and the token was kept.
    let rerun = f.claim(&fi, "head");
    assert_eq!(outcome(&f.complete(&rerun.token, Value::Null)), "completed");
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "completed", "the token was kept");
}

#[test]
fn a_claim_bound_to_inputs_that_changed_holds_nothing() {
    let (mut f, test_clock) = clocked(CHAIN);
    let fi = f.begin();
    let at = f.done(&fi, "head");
    let c = f.claim(&fi, "build");
    // head goes stale and is re-run with another result: build's inputs change.
    test_clock.set(at + secs(61));
    let rerun = f.claim(&fi, "head");
    assert_eq!(outcome(&f.complete(&rerun.token, json!({ "v": 2 }))), "completed");
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &c.token)), "inputs_changed");
    assert_eq!(code(complete(&mut f.ledger, &f.me, &c.token, Value::Null, &quick())), "inputs_changed");
    let again = f.claim(&fi, "build");
    assert_eq!(again.attempt, 2, "the node is released at once");
    assert_ne!(again.inputs, c.inputs);
    // The first claim's lease has most of its time left, but a later claim took the node.
    assert!(clock::now() < c.lease_until);
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &c.token)), "lease_expired");
}

#[test]
fn a_lease_or_a_freshness_that_ends_while_the_command_runs_refuses_the_receipt() {
    // After the run the clock is read again, never the run's start plus a measured duration: the
    // clock jumps while the command runs (as the wall clock does across a sleep, while a monotonic
    // clock stands still), and the receipt is judged at the clock after the jump.
    let (mut f, test_clock) = clocked(CHAIN);
    let fi = f.begin();
    let at = f.done(&fi, "head");
    std::fs::write(f.dir.join("nap"), "0.1").unwrap();
    // head is fresh as the run starts and stale after the jump.
    let c = f.claim(&fi, "build");
    test_clock.set(at + secs(59));
    let jump = shift_while_running(&f, &test_clock, "built", secs(2));
    let refused = f.complete(&c.token, Value::Null);
    jump.join().unwrap();
    assert_eq!(outcome(&refused), "input_stale");
    assert_eq!(f.kinds(&fi)[3..], ["Claimed", "Refused"], "no receipt");
    // The lease ends in the jump (head consumes nothing, so only the lease can lapse).
    let fi = f.begin();
    let c = f.claim(&fi, "head");
    test_clock.set(c.lease_until - secs(1));
    let jump = shift_while_running(&f, &test_clock, "observed", secs(2));
    let lapsed = complete(&mut f.ledger, &f.me, &c.token, Value::Null, &quick());
    jump.join().unwrap();
    assert_eq!(code(lapsed), "lease_expired");
    assert_eq!(f.kinds(&fi), ["Started", "Claimed"], "no receipt");
}

#[test]
#[ignore = "builds a log at the event cap, about 0.8 s in debug; run with --ignored"]
fn claim_and_heartbeat_churn_at_the_event_cap_is_refused_clearly() {
    // Refusals and heartbeats cannot grow a log past the event cap: each writer reports the cap
    // as a refusal, with nothing written.
    use maestro_flow::log::MAX_EVENTS;
    let (mut f, test_clock) = clocked(CHAIN);
    let fi = f.begin();
    let c = f.claim(&fi, "head");
    let path = f.root.join("instances").join(fi.as_str()).join("events.jsonl");
    let mut text = std::fs::read_to_string(&path).unwrap();
    let mut prev = maestro_flow::log::line_hash(text.lines().last().unwrap().as_bytes());
    let body = json!({ "node": "head", "iteration": {}, "attempt": 1, "code": "x", "reason": "x" });
    for seq in 3..=MAX_EVENTS {
        let ev =
            json!({ "seq": seq, "prev_event": prev, "at": "2026-11-08T00:00:00Z", "kind": "Refused", "body": body });
        let line = serde_json::to_string(&ev).unwrap();
        prev = maestro_flow::log::line_hash(line.as_bytes());
        text.push_str(&line);
        text.push('\n');
    }
    std::fs::write(&path, &text).unwrap();
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &c.token)), "too_many_events");
    test_clock.set(c.lease_until);
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "head")), "too_many_events");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "nothing was written");
}

#[test]
fn a_heartbeat_moves_only_the_lease_of_its_own_attempt() {
    let (mut f, test_clock) = clocked(CHAIN);
    let fi = f.begin();
    let c = f.claim(&fi, "head");
    // A heartbeat for another attempt of the same node and iteration, with a later lease.
    let other = maestro_flow::kernel::events::Heartbeat {
        node: "head".into(),
        iteration: Default::default(),
        attempt: c.attempt + 1,
        lease_until: (c.lease_until + secs(3600)).to_string(),
    };
    f.append(&fi, other.to_event().unwrap());
    test_clock.set(c.lease_until);
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &c.token)), "lease_expired", "the claim's lease did not move");
    assert_eq!(f.claim(&fi, "head").attempt, 2, "the node was released at its lease end");
}

#[test]
fn a_caller_cannot_make_an_observation_younger_than_its_run() {
    // P20 round 2 (Codex, reproduced): with a caller's clock 30 s ahead, a 0.3 s observation was
    // still done and its dependent claimable more than a minute after the run started. The
    // writers now read the wall clock themselves, so `ran_at` is the run's start: a minute after
    // it (though less than a minute after the append) head is stale and build is not enabled.
    let (mut f, test_clock) = clocked(CHAIN);
    let fi = f.begin();
    std::fs::write(f.dir.join("nap"), "0.12").unwrap();
    let c = f.claim(&fi, "head");
    let before = clock::now();
    let Completion::Completed { verify, appended, .. } = f.complete(&c.token, Value::Null) else {
        panic!("head completes")
    };
    let ran: Timestamp = verify.ran_at.parse().unwrap();
    let at: Timestamp = appended.event.at.parse().unwrap();
    assert!(before <= ran && at.duration_since(ran) >= ms(120), "ran_at is the run's start");
    let read = at + secs(60) - ms(50);
    assert!(read.duration_since(ran) > secs(60));
    assert_eq!(f.status(&fi, read).nodes["head"].state, State::Stale);
    test_clock.set(read);
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "build")), "not_enabled");
}

#[test]
fn a_run_stamped_after_its_append_counts_from_the_append() {
    // The wall clock steps back 30 s while head runs (not below the last event, so the receipt is
    // written): its `ran_at` is later than its `at`, and its freshness counts from the append.
    let (mut f, test_clock) = clocked(CHAIN);
    let fi = f.begin();
    let c = f.claim(&fi, "head");
    test_clock.shift(secs(40));
    std::fs::write(f.dir.join("nap"), "0.1").unwrap();
    let back = shift_while_running(&f, &test_clock, "observed", secs(-30));
    let Completion::Completed { verify, appended, .. } = f.complete(&c.token, Value::Null) else {
        panic!("head completes")
    };
    back.join().unwrap();
    let ran: Timestamp = verify.ran_at.parse().unwrap();
    let at: Timestamp = appended.event.at.parse().unwrap();
    assert!(ran > at + secs(29), "the run is stamped after its append");
    assert_eq!(f.status(&fi, at + secs(59)).nodes["head"].state, State::Done);
    assert_eq!(f.status(&fi, at + secs(60) + ms(10)).nodes["head"].state, State::Stale);
}

#[test]
fn a_wall_clock_that_steps_back_past_the_instance_s_last_event_is_refused() {
    // Judging at an earlier clock than the log has reached could pass a lease or an input that
    // has already lapsed, so each verb refuses it with nothing written, before the run and after
    // it; once the clock passes the last event the writes go on, on the same token.
    let (mut f, test_clock) = clocked(CHAIN);
    let fi = f.begin();
    let at = f.done(&fi, "head");
    test_clock.set(at - ms(1));
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "build")), "clock_behind");
    test_clock.set(at);
    let c = f.claim(&fi, "build");
    test_clock.set(at - ms(1));
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &c.token)), "clock_behind");
    assert_eq!(code(complete(&mut f.ledger, &f.me, &c.token, Value::Null, &quick())), "clock_behind");
    assert!(!f.dir.join("built").exists(), "nothing ran");
    // A step back while the command runs is refused after it, with nothing appended.
    test_clock.set(at);
    std::fs::write(f.dir.join("nap"), "0.1").unwrap();
    let back = shift_while_running(&f, &test_clock, "built", ms(-1));
    assert_eq!(code(complete(&mut f.ledger, &f.me, &c.token, Value::Null, &quick())), "clock_behind");
    back.join().unwrap();
    assert_eq!(f.kinds(&fi), ["Started", "Claimed", "Completed", "Claimed"]);
    std::fs::write(f.dir.join("nap"), "0").unwrap();
    test_clock.set(at + ms(1));
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "completed", "the token was kept");
}

// BRO-3633 PR 3b2b: retry and cancel, a choice or an effect whose input goes stale while its
// guards run, an effect killed mid-run or raced by a second completion, and the until verdict
// written only over the input it was evaluated on.

/// A choice and an effect that consume an observation fresh for a minute; a node with two
/// attempts; a node cancelled 30 s into each attempt.
const LIMITS: &str = r#"
flow: limits
version: 1
params: [dir]
workdir: "{{params.dir}}"
nodes:
  head: {class: observe, run: [sh, -c, 'exit "$(cat head 2>/dev/null || echo 0)"'], fresh_for: 1m, exit_map: {0: up, 1: down}, retry: {attempts: 1}}
  pick: {class: choice, needs: [head], edges: {go: {guard: 'true'}}}
  push: {class: effect, needs: [head], preconditions: ['true'], run: [sh, -c, 'echo ran >> effects; exit "$(cat fail 2>/dev/null || echo 0)"'], reconcile: ['true']}
  twice: {class: work, verify: ['true'], retry: {attempts: 2}}
  once: {class: work, verify: ['true'], needs: [head], retry: {attempts: 1}}
  flip: {class: work, verify: ['true'], needs: [head], retry: {attempts: 2}}
  single: {class: work, verify: ['true'], retry: {attempts: 1}}
  pushonce: {class: effect, needs: [head], run: [sh, -c, 'echo ran >> effects; exit "$(cat fail 2>/dev/null || echo 0)"'], reconcile: ['true'], retry: {attempts: 1}}
  brief: {class: work, verify: [sh, -c, 'touch briefed; sleep "$(cat nap 2>/dev/null || echo 0)"'], cancel: {after: 30s}}
  slow: {class: effect, run: [sh, -c, 'sleep 1; echo ran >> effects'], reconcile: ['true'], cancel: {after: 30s}}
  retried: {class: effect, run: [sh, -c, 'echo ran >> effects; exit "$(cat fail 2>/dev/null || echo 0)"'], reconcile: [sh, -c, 'test -s effects'], retry: {attempts: 1}}
  act: {class: effect, run: ['{{params.dir}}/act'], reconcile: [sh, -c, 'test -s effects'], retry: {attempts: 1}}
  recmiss: {class: effect, run: [sh, -c, 'echo ran >> effects; exit "$(cat fail 2>/dev/null || echo 0)"'], reconcile: ['{{params.dir}}/check'], retry: {attempts: 1}}
"#;

use crate::kernel::guard::tests::WHILE_JUDGING;
use crate::kernel::{Choosing, choose};

/// Runs `then` while the guards this thread evaluates shift `clock` by `by` as each starts.
fn while_judging<T>(clock: &TestClock, by: SignedDuration, then: impl FnOnce() -> T) -> T {
    let shifter = clock.clone();
    WHILE_JUDGING.with(|hook| *hook.borrow_mut() = Some(Box::new(move || shifter.shift(by))));
    let out = then();
    WHILE_JUDGING.with(|hook| *hook.borrow_mut() = None);
    out
}

/// Completes `build` with `{"ready": true}`, so an effect's precondition holds.
fn ready(f: &mut Fx, fi: &crate::InstanceId) {
    let c = f.claim(fi, "build");
    assert_eq!(outcome(&f.complete(&c.token, json!({ "ready": true }))), "completed");
}

#[test]
fn a_node_takes_no_claim_once_its_attempts_have_ended_without_a_receipt() {
    let (mut f, test_clock) = clocked(LIMITS);
    let fi = f.begin();
    let first = f.claim(&fi, "twice");
    test_clock.set(first.lease_until);
    let second = f.claim(&fi, "twice");
    assert_eq!(second.attempt, 2);
    test_clock.set(second.lease_until);
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "twice")), "attempts_exhausted");
    // Replay projects it: `status` says failed, and `next` does not enable it.
    let s = f.status(&fi, clock::now());
    assert_eq!(
        (s.nodes["twice"].state, &s.nodes["twice"].unmet[..]),
        (State::Failed, &[Reason::AttemptsExhausted { attempts: 2 }][..])
    );
    let n = next_now(&f, &fi);
    assert!(n.enabled.iter().all(|e| e.node != "twice") && n.pending["twice"].state == State::Failed);
    // An attempt whose inputs changed has ended too, lease or not: a node with one attempt takes
    // no claim after its input is re-run with another value.
    let fi = f.begin();
    let at = f.done(&fi, "head");
    f.claim(&fi, "once");
    test_clock.set(at + secs(61));
    std::fs::write(f.dir.join("head"), "1").unwrap();
    f.done(&fi, "head");
    std::fs::remove_file(f.dir.join("head")).unwrap();
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "once")), "attempts_exhausted");
    // A claim a later one took over has ended, even once its inputs are the node's again.
    let fi = f.begin();
    let at = clock::now();
    test_clock.set(at);
    f.done(&fi, "head");
    f.claim(&fi, "flip");
    test_clock.set(at + secs(61));
    std::fs::write(f.dir.join("head"), "1").unwrap();
    f.done(&fi, "head");
    assert_eq!(f.claim(&fi, "flip").attempt, 2);
    test_clock.set(at + secs(122));
    std::fs::remove_file(f.dir.join("head")).unwrap();
    f.done(&fi, "head");
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "flip")), "attempts_exhausted");
    // A heartbeat moves the end of the attempt it extends.
    let fi = f.begin();
    let c = f.claim(&fi, "single");
    test_clock.set(c.lease_until - secs(300));
    heartbeat(&mut f.ledger, &f.me, &c.token).unwrap();
    test_clock.set(c.lease_until);
    assert_eq!(f.status(&fi, clock::now()).nodes["single"].state, State::Ready);
    // Within its attempts a node completes as any other.
    let fi = f.begin();
    let first = f.claim(&fi, "twice");
    test_clock.set(first.lease_until);
    let second = f.claim(&fi, "twice");
    assert_eq!(outcome(&f.complete(&second.token, Value::Null)), "completed");
    // Only attempts that ended without a receipt count: an observation with one attempt is
    // claimed again once its receipt has gone stale.
    let at = f.done(&fi, "head");
    test_clock.set(at + secs(61));
    assert_eq!(f.claim(&fi, "head").attempt, 2);
}

#[test]
fn an_effect_whose_inputs_changed_after_its_receipt_runs_again() {
    // An effect runs once per iteration and set of inputs: a receipt clears its start, so a new
    // input (here the observation it consumes re-run with another value) is a new effect.
    let (mut f, test_clock) = clocked(LIMITS);
    let fi = f.begin();
    let at = f.done(&fi, "head");
    let c = f.claim(&fi, "push");
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "completed");
    test_clock.set(at + secs(61));
    std::fs::write(f.dir.join("head"), "1").unwrap();
    f.done(&fi, "head");
    let c = f.claim(&fi, "push");
    let Completion::Completed { verify, .. } = f.complete(&c.token, Value::Null) else { panic!() };
    assert!(verify.argv[2].starts_with("echo ran"), "the effect ran, not its reconcile: {:?}", verify.argv);
    assert_eq!(runs(&f), 2);
}

/// A repeat over an observation, `ci`, whose `until` holds when it is green.
const POLL: &str = r#"
flow: poll
version: 1
params: [dir]
workdir: "{{params.dir}}"
nodes:
  ci: {class: observe, run: [sh, -c, 'exit "$(cat ci 2>/dev/null || echo 0)"'], fresh_for: 1h, exit_map: {0: green, 1: red}}
  after: {class: pure, run: ['true'], needs: [poll]}
repeats:
  poll: {body: [ci], max: 3, until: '.nodes.ci.value == "green"'}
"#;

/// A receipt for `ci` at iteration 0 that maps to `red`, as a re-run would write it.
fn red() -> crate::NewEvent {
    let verify = crate::kernel::events::VerifyRecord {
        argv: vec!["x".into()],
        exit: 1,
        stdout_sha256: String::new(),
        ran_at: clock::now().to_string(),
    };
    let receipt = Completed {
        node: "ci".into(),
        iteration: [("poll".to_string(), 0)].into(),
        attempt: 1,
        inputs: Default::default(),
        outputs: Value::Null,
        verify: Some(verify),
        performer: None,
        tool_use_id: None,
    };
    receipt.to_event().unwrap()
}

#[test]
fn a_verdict_whose_body_changed_while_its_until_ran_is_not_written() {
    // The until runs with the lock released; another writer re-runs the body meanwhile (here a
    // second handle on the ledger, from the evaluator's test hook). The verdict judged the old body,
    // so it is not written, and the repeat awaits a verdict on the new one.
    let (mut f, _clock) = clocked(POLL);
    let fi = f.begin();
    let c = f.claim(&fi, "ci");
    let (root, instance) = (f.root.clone(), fi.clone());
    WHILE_JUDGING.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let mut other = crate::Ledger::open(&root, &common::host("mac"), crate::Options::default()).unwrap();
            let mut guard = other.lock().unwrap();
            let seq = guard.log(&instance).unwrap().seq();
            guard.append(&instance, seq, red()).unwrap();
        }))
    });
    let done = f.complete(&c.token, Value::Null);
    WHILE_JUDGING.with(|hook| *hook.borrow_mut() = None);
    let Completion::Completed { verdicts, .. } = done else { panic!() };
    assert!(
        matches!(&verdicts[..], [crate::kernel::Verdict::Unrecorded { code: "verdict_stale", .. }]),
        "{verdicts:?}"
    );
    assert_eq!(f.kinds(&fi), ["Started", "Claimed", "Completed", "Completed"]);
    let s = f.status(&fi, clock::now());
    assert!(s.repeats["poll"].awaiting_verdict);
}

#[test]
fn a_verdict_is_not_written_at_a_clock_behind_the_log() {
    // `claim` writes a pending verdict first, judged at the wall clock read under the lock after
    // the until ran: a clock behind the log then, or already before it ran, writes nothing.
    let (mut f, test_clock) = clocked(POLL);
    for during in [false, true] {
        let fi = f.begin();
        f.append(&fi, red());
        let last = f.log_text(&fi).lines().last().map(|l| serde_json::from_str::<Value>(l).unwrap()["at"].clone());
        let at: Timestamp = last.and_then(|at| at.as_str().map(str::to_string)).unwrap().parse().unwrap();
        if during {
            let back = test_clock.clone();
            let refused = while_judging(&back, secs(-3600), || claim(&mut f.ledger, &f.me, &fi, "after").map(|_| ()));
            assert_eq!(code(refused), "clock_behind", "during");
            test_clock.set(at + secs(1));
        } else {
            test_clock.set(at - ms(1));
            assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "after")), "clock_behind", "before");
            test_clock.set(at);
        }
        assert_eq!(f.kinds(&fi), ["Started", "Completed"], "during {during}");
    }
}

#[test]
fn a_cancelled_attempt_ends_at_its_deadline_heartbeats_or_not() {
    let (mut f, test_clock) = clocked(LIMITS);
    let fi = f.begin();
    let now = clock::now();
    test_clock.set(now);
    let c = f.claim(&fi, "brief");
    assert_eq!(c.lease_until, now + secs(30), "the lease stops at the deadline");
    assert_eq!(f.last_body(&fi)["cancel_at"], (now + secs(30)).to_string());
    test_clock.set(now + secs(20));
    let beat = heartbeat(&mut f.ledger, &f.me, &c.token).unwrap();
    assert_eq!(beat.lease_until, now + secs(30), "a heartbeat does not move it past the deadline");
    test_clock.set(now + secs(30));
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &c.token)), "cancelled");
    assert_eq!(code(complete(&mut f.ledger, &f.me, &c.token, Value::Null, &quick())), "cancelled");
    assert!(!f.dir.join("briefed").exists(), "nothing ran");
    // The node takes a new claim, the next attempt; a run that crosses its deadline is refused.
    let next = f.claim(&fi, "brief");
    assert_eq!(next.attempt, 2);
    // The first claim, taken over by the next, has lost its lease: it is no longer cancelled.
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &c.token)), "lease_expired");
    std::fs::write(f.dir.join("nap"), "0.1").unwrap();
    let late = shift_while_running(&f, &test_clock, "briefed", secs(31));
    assert_eq!(code(complete(&mut f.ledger, &f.me, &next.token, Value::Null, &quick())), "cancelled");
    late.join().unwrap();
    assert_eq!(f.kinds(&fi), ["Started", "Claimed", "Heartbeat", "Claimed"]);
}

#[test]
fn choose_refuses_a_claim_whose_lease_ended() {
    // Spec acceptance (BRO-3624 item 10): lease expiry makes choose refuse with lease_expired.
    let (mut f, test_clock) = clocked(LIMITS);
    let fi = f.begin();
    f.done(&fi, "head");
    let c = f.claim(&fi, "pick");
    test_clock.set(c.lease_until);
    assert_eq!(code(choose(&mut f.ledger, &f.me, &c.token, "go")), "lease_expired");
    assert_eq!(f.kinds(&fi), ["Started", "Claimed", "Completed", "Claimed"]);
}

#[test]
fn a_choice_or_an_effect_whose_input_goes_stale_while_its_guards_run_is_refused() {
    // The guards run with the lock released; the write after them checks freshness again, at the
    // clock read again.
    let (mut f, test_clock) = clocked(LIMITS);
    let fi = f.begin();
    f.done(&fi, "head");
    let c = f.claim(&fi, "pick");
    let refused = while_judging(&test_clock, secs(61), || choose(&mut f.ledger, &f.me, &c.token, "go"));
    assert!(matches!(refused, Ok(Choosing::Refused { code: "input_stale", .. })), "{refused:?}");

    let fi = f.begin();
    f.done(&fi, "head");
    let c = f.claim(&fi, "push");
    let refused = while_judging(&test_clock, secs(61), || f.complete(&c.token, Value::Null));
    assert_eq!(outcome(&refused), "input_stale");
    assert_eq!(runs(&f), 0, "the effect did not run");
    assert_eq!(f.kinds(&fi), ["Started", "Claimed", "Completed", "Claimed", "Refused"]);
}

#[test]
fn an_effect_killed_mid_run_is_reconciled_not_run_again() {
    // Spec acceptance (BRO-3624 item 10): an effect reconciled instead of re-run after a crash.
    // The child process running `complete` is killed after the effect ran, before its receipt;
    // then the same token, or a new claim once the lease ended, reconciles.
    let (mut f, test_clock) = clocked(EFFECT);
    for expire in [false, true] {
        let fi = f.begin();
        ready(&mut f, &fi);
        let c = f.claim(&fi, "push");
        let before = runs(&f);
        let pause = common::Pause::at("command_ran");
        let mut child =
            common::spawn_child_pausing(&["complete", f.root.to_str().unwrap(), &c.token, "host"], Some(&pause));
        pause.reached();
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(runs(&f), before + 1, "the effect ran");
        assert_eq!(f.kinds(&fi).last().map(String::as_str), Some("EffectStarted"));
        let token = if expire {
            test_clock.set(c.lease_until);
            assert_eq!(code(complete(&mut f.ledger, &f.me, &c.token, Value::Null, &quick())), "lease_expired");
            let again = f.claim(&fi, "push");
            assert_eq!(again.attempt, 2);
            again.token
        } else {
            c.token.clone()
        };
        let Completion::Completed { verify, .. } = f.complete(&token, Value::Null) else { panic!("reconciled") };
        assert_eq!(verify.argv[2], "test -s effects && test ! -f unconfirmed", "expire {expire}");
        assert_eq!(runs(&f), before + 1, "expire {expire}: the effect ran once");
    }
}

#[test]
fn two_completions_racing_on_one_claim_run_the_effect_once() {
    // A completion that judged the preconditions while another started the effect (here it then
    // failed, so the claim still holds) refuses, and never runs the effect again.
    let (mut f, _clock) = clocked(EFFECT);
    let fi = f.begin();
    ready(&mut f, &fi);
    let c = f.claim(&fi, "push");
    let pause = common::Pause::at("preconditions_judged");
    let child = common::spawn_child_pausing(&["complete", f.root.to_str().unwrap(), &c.token, "host"], Some(&pause));
    pause.reached();
    std::fs::write(f.dir.join("fail"), "1").unwrap();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "effect_failed");
    pause.go();
    assert_eq!(common::child_result(child), "effect_started");
    assert_eq!(runs(&f), 1);
    assert_eq!(f.kinds(&fi)[3..], ["Claimed", "EffectStarted", "Refused", "Refused"]);
}

#[test]
fn a_verdict_is_written_only_over_the_input_it_was_evaluated_on() {
    // `settle` evaluates an until with the lock released, so the body can change before the
    // append: `record` writes the verdict only if the repeat still awaits it, at that iteration,
    // over an input with the digest it judged.
    use crate::kernel::events::digest;
    use crate::kernel::guard::Judged;
    use crate::kernel::replay::{awaiting, pinned};
    use crate::kernel::write::record;
    let (mut f, _clock) = clocked(
        "flow: loop\nversion: 1\nparams: [dir]\nnodes:\n  try: {class: work, verify: ['true']}\nrepeats:\n  rounds: {body: [try], max: 3, until: '.nodes.try.outputs.ok'}\n",
    );
    let fi = f.begin();
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
    let mut guard = f.ledger.lock().unwrap();
    let log = guard.log(&fi).unwrap();
    let started = pinned(&log).unwrap();
    let awaited = awaiting(&log, &started).unwrap().remove(0);
    let judged = Judged { holds: true, digest: digest(&awaited.input) };
    let stale = Judged { holds: true, digest: "sha256:another-input".into() };
    assert_eq!(record(&mut guard, &log, &started, &awaited, &stale).unwrap(), None);
    let moved = crate::kernel::replay::Awaiting { iteration: 1, ..awaited.clone() };
    assert_eq!(record(&mut guard, &log, &started, &moved, &judged).unwrap(), None);
    let other = crate::kernel::replay::Awaiting { repeat: "other".into(), ..awaited.clone() };
    assert_eq!(record(&mut guard, &log, &started, &other, &judged).unwrap(), None);
    assert!(record(&mut guard, &log, &started, &awaited, &judged).unwrap().is_some());
    drop(guard);
    assert_eq!(f.kinds(&fi), ["Started", "Completed", "Guarded"]);
}

#[test]
fn a_process_that_never_registered_as_a_guard_host_evaluates_no_guard() {
    // The writers in a child process that never called `guard_entry`: a choice, an effect's
    // precondition and an until verdict are each refused `guard_host_missing`, before any
    // evaluator starts, and the effect does not run.
    let (mut f, _clock) = clocked(LIMITS);
    let root = f.root.to_str().unwrap().to_string();
    let bare = |spec: &[&str]| common::child_result(common::spawn_child(spec));
    let fi = f.begin();
    f.done(&fi, "head");
    let pick = f.claim(&fi, "pick");
    assert_eq!(bare(&["choose", &root, &pick.token, "go", "bare"]), "guard_host_missing");
    let push = f.claim(&fi, "push");
    assert_eq!(bare(&["complete", &root, &push.token, "bare"]), "guard_host_missing");
    assert_eq!(runs(&f), 0);
    assert!(!f.kinds(&fi).contains(&"EffectStarted".to_string()));

    let mut g = fx(POLL);
    let fi = g.begin();
    let ci = g.claim(&fi, "ci");
    let root = g.root.to_str().unwrap().to_string();
    assert_eq!(bare(&["complete", &root, &ci.token, "bare"]), "completed:guard_host_missing");
    assert_eq!(g.kinds(&fi), ["Started", "Claimed", "Completed"], "no verdict written");
}

#[test]
fn a_program_that_is_not_the_evaluator_is_refused_through_the_writer() {
    // A binary without the entry, which rejects the evaluator's arguments and exits non-zero.
    use crate::kernel::guard::tests::PROGRAM;
    let (mut f, _clock) = clocked(LIMITS);
    let fi = f.begin();
    f.done(&fi, "head");
    let c = f.claim(&fi, "pick");
    PROGRAM.with(|p| *p.borrow_mut() = Some("/bin/ls".into()));
    let refused = choose(&mut f.ledger, &f.me, &c.token, "go");
    PROGRAM.with(|p| *p.borrow_mut() = None);
    let Ok(Choosing::Refused { code, reason, .. }) = refused else { panic!("{refused:?}") };
    assert_eq!(code, "guard_error");
    assert!(reason.starts_with("the evaluator exited"), "{reason}");
}

/// What `next` says for `fi` at the clock this thread reads.
fn next_now(f: &Fx, fi: &crate::InstanceId) -> Next {
    let reader = crate::Reader::new(&f.root, crate::Options::default()).unwrap();
    crate::kernel::next(&reader.lock().unwrap(), fi, clock::now()).unwrap()
}

#[test]
fn attempts_are_counted_in_each_iteration_of_a_repeat() {
    let (mut f, test_clock) = clocked(
        "flow: loop\nversion: 1\nparams: [dir]\nworkdir: \"{{params.dir}}\"\nnodes:\n  step: {class: work, verify: ['true'], retry: {attempts: 2}}\nrepeats:\n  rounds: {body: [step], max: 3, until: '.nodes.step.outputs.ok'}\n",
    );
    let fi = f.begin();
    for ok in [false, true] {
        // One attempt ends without a receipt in each iteration, then the second completes.
        let first = f.claim(&fi, "step");
        test_clock.set(first.lease_until);
        let second = f.claim(&fi, "step");
        assert_eq!(second.attempt, 2, "ok {ok}");
        assert_eq!(outcome(&f.complete(&second.token, json!({ "ok": ok }))), "completed");
    }
    assert_eq!(f.status(&fi, clock::now()).repeats["rounds"].state, crate::kernel::RepeatState::Done);
}

#[test]
fn an_effect_that_started_is_reconciled_whatever_its_attempts() {
    // A reconcile takes no attempt: an effect with one attempt whose run failed (it started, so
    // it is never run again) is reconciled by the next claim once that attempt has ended.
    let (mut f, test_clock) = clocked(LIMITS);
    let fi = f.begin();
    let c = f.claim(&fi, "retried");
    std::fs::write(f.dir.join("fail"), "1").unwrap();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "effect_failed");
    std::fs::remove_file(f.dir.join("fail")).unwrap();
    test_clock.set(c.lease_until);
    assert_eq!(f.status(&fi, clock::now()).nodes["retried"].state, State::Ready);
    let again = f.claim(&fi, "retried");
    let Completion::Completed { verify, .. } = f.complete(&again.token, Value::Null) else { panic!() };
    assert_eq!((verify.argv[2].as_str(), runs(&f)), ("test -s effects", 1));
    // Only over the inputs it started over: once they change, the attempt has ended and the
    // node has none left.
    let fi = f.begin();
    let at = f.done(&fi, "head");
    let c = f.claim(&fi, "pushonce");
    std::fs::write(f.dir.join("fail"), "1").unwrap();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "effect_failed");
    std::fs::remove_file(f.dir.join("fail")).unwrap();
    test_clock.set(at + secs(61));
    std::fs::write(f.dir.join("head"), "1").unwrap();
    f.done(&fi, "head");
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "pushonce")), "attempts_exhausted");
}

#[test]
fn an_effect_that_never_started_is_not_kept_from_running() {
    // The program is missing, so the run fails before anything started (`spawn_failed`): the
    // start is withdrawn, and once the program exists the same claim runs the effect.
    let (mut f, test_clock) = clocked(LIMITS);
    let fi = f.begin();
    let c = f.claim(&fi, "act");
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "spawn_failed");
    let act = f.dir.join("act");
    std::fs::write(&act, "#!/bin/sh\necho ran >> effects\n").unwrap();
    std::fs::set_permissions(&act, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let Completion::Completed { verify, .. } = f.complete(&c.token, Value::Null) else { panic!() };
    assert_eq!((verify.argv[0].ends_with("/act"), runs(&f)), (true, 1), "the effect ran, not its reconcile");
    // A withdrawn start does not spare the attempt: with the program missing again, the one
    // attempt ends, and the node fails.
    std::fs::remove_file(&act).unwrap();
    let fi = f.begin();
    let c = f.claim(&fi, "act");
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "spawn_failed");
    test_clock.set(c.lease_until);
    assert_eq!(f.status(&fi, clock::now()).nodes["act"].state, State::Failed);
}

#[test]
fn an_effect_whose_inputs_changed_after_it_started_runs_over_the_new_ones() {
    // A start stands for its inputs only: over new inputs the effect is a new one, and runs.
    let (mut f, test_clock) = clocked(LIMITS);
    let fi = f.begin();
    let at = f.done(&fi, "head");
    let c = f.claim(&fi, "push");
    std::fs::write(f.dir.join("fail"), "1").unwrap();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "effect_failed");
    std::fs::remove_file(f.dir.join("fail")).unwrap();
    test_clock.set(at + secs(61));
    std::fs::write(f.dir.join("head"), "1").unwrap();
    f.done(&fi, "head");
    let c = f.claim(&fi, "push");
    let Completion::Completed { verify, .. } = f.complete(&c.token, Value::Null) else { panic!() };
    assert!(verify.argv[2].starts_with("echo ran"), "the effect ran, not its reconcile: {:?}", verify.argv);
    assert_eq!(runs(&f), 2);
}

#[test]
fn a_cancel_deadline_ends_a_running_command() {
    // The run is no longer than what is left of the claim: an effect that would act after its
    // deadline is killed first.
    let (mut f, test_clock) = clocked(LIMITS);
    let fi = f.begin();
    let now = clock::now();
    test_clock.set(now);
    let c = f.claim(&fi, "slow");
    test_clock.set(now + secs(30) - ms(150));
    let started = std::time::Instant::now();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "verify_timeout");
    assert!(started.elapsed() < Duration::from_millis(800), "{:?}", started.elapsed());
    assert_eq!(runs(&f), 0, "the effect never acted");
}

// P20 round 2 (maestro#28).

/// Makes `name` in the workdir an executable script.
fn script(f: &Fx, name: &str, body: &str) {
    let path = f.dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

#[test]
fn a_reconcile_that_cannot_start_never_runs_the_effect_again() {
    // The effect ran and failed; its reconcile's program is missing. The reconcile is refused
    // under its own code, which withdraws nothing, and is tried again until it runs.
    let (mut f, _clock) = clocked(LIMITS);
    let fi = f.begin();
    let c = f.claim(&fi, "recmiss");
    std::fs::write(f.dir.join("fail"), "1").unwrap();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "effect_failed");
    std::fs::remove_file(f.dir.join("fail")).unwrap();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "reconcile_spawn_failed");
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "reconcile_spawn_failed");
    assert_eq!(runs(&f), 1, "the effect ran once");
    script(&f, "check", "exit 0");
    let Completion::Completed { verify, .. } = f.complete(&c.token, Value::Null) else { panic!() };
    assert!(verify.argv[0].ends_with("/check"), "the receipt is the reconcile's: {:?}", verify.argv);
    assert_eq!(runs(&f), 1, "the effect ran once");
}

#[test]
fn an_effect_that_acted_is_not_exhausted_when_its_reconcile_cannot_start() {
    // With one attempt: the effect acted, its reconcile could not start, the attempt ended. The
    // node is not failed; the next claim reconciles it.
    let (mut f, test_clock) = clocked(LIMITS);
    let fi = f.begin();
    let c = f.claim(&fi, "recmiss");
    std::fs::write(f.dir.join("fail"), "1").unwrap();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "effect_failed");
    std::fs::remove_file(f.dir.join("fail")).unwrap();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "reconcile_spawn_failed");
    test_clock.set(c.lease_until);
    assert_eq!(f.status(&fi, clock::now()).nodes["recmiss"].state, State::Ready);
    script(&f, "check", "exit 0");
    let again = f.claim(&fi, "recmiss");
    assert_eq!(outcome(&f.complete(&again.token, Value::Null)), "completed");
    assert_eq!(runs(&f), 1, "the effect ran once");
}

#[test]
fn a_command_that_never_started_withdraws_only_its_own_attempt_s_start() {
    // A `spawn_failed` for another attempt, or for another iteration, leaves the start standing:
    // the effect is reconciled, never run again, and its one attempt is not exhausted.
    let (mut f, test_clock) = clocked(LIMITS);
    let fi = f.begin();
    let c = f.claim(&fi, "recmiss");
    std::fs::write(f.dir.join("fail"), "1").unwrap();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "effect_failed");
    std::fs::remove_file(f.dir.join("fail")).unwrap();
    let refused = |attempt: u32, iteration: crate::kernel::Iteration| crate::kernel::events::Refused {
        node: "recmiss".into(),
        iteration,
        attempt,
        code: "spawn_failed".into(),
        reason: "fixture".into(),
    };
    f.append(&fi, refused(2, Default::default()).to_event().unwrap());
    f.append(&fi, refused(1, [("elsewhere".to_string(), 0)].into()).to_event().unwrap());
    script(&f, "check", "exit 0");
    let Completion::Completed { verify, .. } = f.complete(&c.token, Value::Null) else { panic!() };
    assert!(verify.argv[0].ends_with("/check"), "reconciled, not run again: {:?}", verify.argv);
    assert_eq!(runs(&f), 1);
    // The same in replay's retry projection, with the claim's lease over and no receipt.
    let fi = f.begin();
    let c = f.claim(&fi, "recmiss");
    std::fs::write(f.dir.join("fail"), "1").unwrap();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "effect_failed");
    std::fs::remove_file(f.dir.join("fail")).unwrap();
    f.append(&fi, refused(2, Default::default()).to_event().unwrap());
    test_clock.set(c.lease_until);
    assert_eq!(f.status(&fi, clock::now()).nodes["recmiss"].state, State::Ready);
}

#[test]
fn attempts_are_exhausted_in_a_later_iteration_of_a_repeat() {
    let (mut f, test_clock) = clocked(
        "flow: loop\nversion: 1\nparams: [dir]\nworkdir: \"{{params.dir}}\"\nnodes:\n  step: {class: work, verify: ['true'], retry: {attempts: 2}}\nrepeats:\n  rounds: {body: [step], max: 3, until: '.nodes.step.outputs.ok'}\n",
    );
    let fi = f.begin();
    let c = f.claim(&fi, "step");
    assert_eq!(outcome(&f.complete(&c.token, json!({ "ok": false }))), "completed");
    // Iteration 1: two attempts end without a receipt, and a third claim is refused.
    let a = f.claim(&fi, "step");
    test_clock.set(a.lease_until);
    let b = f.claim(&fi, "step");
    test_clock.set(b.lease_until);
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "step")), "attempts_exhausted");
    assert_eq!(f.status(&fi, clock::now()).nodes["step"].state, State::Failed);
}

#[test]
fn a_heartbeat_on_a_later_attempt_keeps_that_attempt_going() {
    let (mut f, test_clock) = clocked(LIMITS);
    let fi = f.begin();
    let a = f.claim(&fi, "twice");
    test_clock.set(a.lease_until);
    let b = f.claim(&fi, "twice");
    test_clock.set(b.lease_until - secs(300));
    heartbeat(&mut f.ledger, &f.me, &b.token).unwrap();
    test_clock.set(b.lease_until);
    assert_eq!(f.status(&fi, clock::now()).nodes["twice"].state, State::Ready);
    assert_eq!(outcome(&f.complete(&b.token, Value::Null)), "completed");
}

#[test]
fn an_evaluator_a_choose_started_ends_itself_when_its_kernel_is_killed() {
    // Through the writers' own path: a kernel process runs `choose` with a guard that never ends,
    // and is killed while the guard runs. The orphaned evaluator ends itself at the deadline the
    // writers give it (`GUARD_TIMEOUT` plus `CHILD_GRACE`, 1.5 s from its start).
    use crate::kernel::guard::EVALUATOR_ARG;
    let (mut f, _clock) = clocked(
        "flow: g\nversion: 1\nparams: [dir]\nworkdir: \"{{params.dir}}\"\nnodes:\n  review: {class: work, verify: ['true']}\n  pick: {class: choice, needs: [review], edges: {go: {guard: 'reduce (.nodes.review.outputs.xs[] as $a | .nodes.review.outputs.xs[] as $b | .nodes.review.outputs.xs[]) as $x (0; . + $x) > 0'}}}\n",
    );
    let fi = f.begin();
    let review = f.claim(&fi, "review");
    let xs: Vec<u32> = (0..2000).collect();
    assert_eq!(outcome(&f.complete(&review.token, json!({ "xs": xs }))), "completed");
    let c = f.claim(&fi, "pick");
    let mut kernel = common::spawn_child(&["choose", f.root.to_str().unwrap(), &c.token, "go", "host"]);
    let kernel_pid = kernel.id().to_string();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let evaluator = loop {
        let found =
            std::process::Command::new("pgrep").args(["-P", &kernel_pid, "-f", EVALUATOR_ARG]).output().unwrap();
        let pid = String::from_utf8_lossy(&found.stdout).lines().next().map(str::to_string);
        if let Some(pid) = pid {
            break pid;
        }
        assert!(std::time::Instant::now() < deadline, "the evaluator never started");
        std::thread::sleep(Duration::from_millis(5));
    };
    let killed = std::time::Instant::now();
    kernel.kill().unwrap();
    kernel.wait().unwrap();
    let alive = |pid: &str| {
        std::process::Command::new("kill")
            .args(["-0", pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    while alive(&evaluator) && killed.elapsed() < Duration::from_millis(2500) {
        std::thread::sleep(Duration::from_millis(10));
    }
    if alive(&evaluator) {
        let _ = std::process::Command::new("kill").args(["-KILL", &evaluator]).status();
        panic!("the orphaned evaluator outlived its deadline");
    }
}
