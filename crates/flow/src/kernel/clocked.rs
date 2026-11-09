//! The claim, heartbeat and complete tests that set or shift the clock the writers read
//! (BRO-3633 PR 3b2a). They live in the crate under `cfg(test)` because the test clock exists in
//! no other build (see `clock`'s module docs): a test installs it on its own thread, and sets or
//! shifts it instead of sleeping. Where the clock must move while a command runs, a thread shifts
//! it once the command has started. The fixture is the integration tests' own
//! (`tests/kernel/claiming.rs`).

#[path = "../../tests/kernel/claiming.rs"]
mod claiming;

use crate::clock::{self, TestClock};
// The shared helpers, loaded once in this binary (`hooked`); `claiming` re-exports them.
use crate::hooked::common;
use crate::kernel::events::{Completed, KernelEvent};
use crate::kernel::{Completion, DEFAULT_LEASE_SECS, State, claim, complete, heartbeat};
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
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "pick")), "not_claimable");
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
