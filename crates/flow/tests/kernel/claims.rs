//! BRO-3633 PR 3b2a: `claim`, `heartbeat` and `complete`, with the commands the kernel runs.
//! Each test names the BRO-3624 items it covers. The clock is passed in; a lease or a freshness
//! window is crossed by passing a later `now`, never by sleeping. Commands are short `sh` lines.

use crate::common::*;
use jiff::{SignedDuration, Timestamp};
use maestro_core::session::VerifiedSession;
use maestro_flow::kernel::events::{Claimed, Completed, KernelEvent, Started};
use maestro_flow::kernel::{
    Claim, CompiledFlow, Completion, DEFAULT_LEASE_SECS, KernelError, Limits, MAX_ARGV, State, Status, claim, compile,
    complete, heartbeat, next, start, status,
};
use maestro_flow::{InstanceId, Ledger, NewEvent, Options, Reader};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;

/// head (observe) -> build (pure) -> review (work); a choice and a human node beside them.
const CHAIN: &str = r#"
flow: chain
version: 1
params: [dir, big?]
workdir: "{{params.dir}}"
nodes:
  head: {class: observe, run: [sh, -c, 'sleep "$(cat nap 2>/dev/null || echo 0)"; exit "$(cat head 2>/dev/null || echo 0)"'], fresh_for: 1m, exit_map: {0: up, 1: down}}
  build: {class: pure, needs: [head], run: [sh, -c, 'touch built; sleep "$(cat nap 2>/dev/null || echo 0)"']}
  review: {class: work, needs: [build], verify: [sh, -c, 'test -f ok']}
  pick: {class: choice, edges: {a: {guard: "true"}}}
  owner: {class: human, ask: budget}
"#;

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    /// The flow's workdir.
    dir: PathBuf,
    ledger: Ledger,
    flow: CompiledFlow,
    me: VerifiedSession,
}

fn fx(yaml: &str) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let dir = tmp.path().join("work");
    std::fs::create_dir(&dir).unwrap();
    let ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    let flow = compile(yaml).unwrap_or_else(|e| panic!("{e}"));
    Fx { _tmp: tmp, root, dir, ledger, flow, me: agent("owner", "marker-1") }
}

fn code<T>(r: Result<T, KernelError>) -> String {
    r.map_or_else(|e| e.code().to_string(), |_| "ok".to_string())
}

fn secs(s: i64) -> SignedDuration {
    SignedDuration::from_secs(s)
}

fn quick() -> Limits {
    Limits { timeout: Duration::from_secs(5), max_output: 64 << 10 }
}

/// The refusal code of a completion, or `completed`.
fn outcome(c: &Completion) -> &str {
    match c {
        Completion::Completed { .. } => "completed",
        Completion::Refused { code, .. } => code,
    }
}

impl Fx {
    fn begin(&mut self) -> InstanceId {
        let params = json!({ "dir": self.dir.to_str().unwrap() });
        start(&mut self.ledger, &self.me, &self.flow, params).unwrap().instance
    }

    fn claim(&mut self, fi: &InstanceId, node: &str, now: Timestamp) -> Claim {
        claim(&mut self.ledger, &self.me, fi, node, now).unwrap_or_else(|e| panic!("claim {node}: {e}"))
    }

    fn complete(&mut self, token: &str, outputs: Value, now: Timestamp) -> Completion {
        complete(&mut self.ledger, &self.me, token, outputs, now, &quick()).unwrap_or_else(|e| panic!("{e}"))
    }

    /// Claims and completes `node` now; the receipt's stamp.
    fn done(&mut self, fi: &InstanceId, node: &str) -> Timestamp {
        let c = self.claim(fi, node, Timestamp::now());
        match self.complete(&c.token, Value::Null, Timestamp::now()) {
            Completion::Completed { appended, .. } => appended.event.at.parse().unwrap(),
            other => panic!("{node}: {other:?}"),
        }
    }

    fn status(&self, fi: &InstanceId, now: Timestamp) -> Status {
        let reader = Reader::new(&self.root, Options::default()).unwrap();
        status(&reader.lock().unwrap(), fi, now).unwrap()
    }

    fn log_text(&self, fi: &InstanceId) -> String {
        std::fs::read_to_string(self.root.join("instances").join(fi.as_str()).join("events.jsonl")).unwrap()
    }

    fn kinds(&self, fi: &InstanceId) -> Vec<String> {
        self.log_text(fi)
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap()["kind"].as_str().unwrap().to_string())
            .collect()
    }

    fn last_body(&self, fi: &InstanceId) -> Value {
        let text = self.log_text(fi);
        serde_json::from_str::<Value>(text.lines().last().unwrap()).unwrap()["body"].clone()
    }

    /// Appends `ev` at the head through the storage API, as a fixture.
    fn append(&mut self, fi: &InstanceId, ev: NewEvent) {
        let mut guard = self.ledger.lock().unwrap();
        let seq = guard.log(fi).unwrap().seq();
        guard.append(fi, seq, ev).unwrap();
    }
}

#[test]
fn every_write_needs_an_owner_marker_and_start_records_only_its_digest() {
    // BRO-3624 item 5: no marker, every write verb is refused.
    let mut f = fx(CHAIN);
    let params = json!({ "dir": f.dir.to_str().unwrap() });
    assert_eq!(code(start(&mut f.ledger, &agent("owner", ""), &f.flow, params.clone())), "no_owner_marker");
    let unmarked = VerifiedSession::new("t3:-/owner".parse().unwrap(), "/tmp", None, None, vec![]);
    assert_eq!(code(start(&mut f.ledger, &unmarked, &f.flow, params.clone())), "no_owner_marker");
    assert!(f.ledger.lock().unwrap().instances().unwrap().is_empty(), "nothing was started");

    let fi = f.begin();
    let started: Started = serde_json::from_str::<Value>(f.log_text(&fi).lines().next().unwrap())
        .map(|v| serde_json::from_value(v["body"].clone()).unwrap())
        .unwrap();
    let owner = started.owner.unwrap();
    assert_eq!(owner.session.to_string(), "t3:-/owner");
    assert!(!f.log_text(&fi).contains("marker-1"), "the marker itself is never written");
    assert_eq!(owner.marker_sha256, maestro_flow::kernel::events::marker_digest("marker-1"));

    let c = f.claim(&fi, "head", Timestamp::now());
    let now = Timestamp::now();
    assert_eq!(code(claim(&mut f.ledger, &unmarked, &fi, "head", now)), "no_owner_marker");
    assert_eq!(code(heartbeat(&mut f.ledger, &unmarked, &c.token, now)), "no_owner_marker");
    assert_eq!(code(complete(&mut f.ledger, &unmarked, &c.token, Value::Null, now, &quick())), "no_owner_marker");
    assert_eq!(f.kinds(&fi), ["Started", "Claimed"]);
}

#[test]
fn only_the_owner_session_with_its_marker_writes_its_instance() {
    // BRO-3624 items 5 and 6: a replacement (same session, new marker) and another session (even
    // with the same marker) are refused every write on the instance; a token in their hands
    // grants nothing; a replacement may start its own instance; no transfer or adoption.
    let mut f = fx(CHAIN);
    let fi = f.begin();
    let c = f.claim(&fi, "head", Timestamp::now());
    let now = Timestamp::now();
    for (who, stranger) in
        [("a replacement", agent("owner", "marker-2")), ("another session", agent("other", "marker-1"))]
    {
        assert_eq!(code(claim(&mut f.ledger, &stranger, &fi, "head", now)), "not_owner", "{who}");
        assert_eq!(code(heartbeat(&mut f.ledger, &stranger, &c.token, now)), "not_owner", "{who}");
        let completed = complete(&mut f.ledger, &stranger, &c.token, Value::Null, now, &quick());
        assert_eq!(code(completed), "not_owner", "{who}");
    }
    assert_eq!(f.kinds(&fi), ["Started", "Claimed"]);
    // A restarted process (a new marker) cannot use its old claim even after the lease ends.
    let replacement = agent("owner", "marker-2");
    let later = c.lease_until + secs(1);
    assert_eq!(code(claim(&mut f.ledger, &replacement, &fi, "head", later)), "not_owner");
    let own = start(&mut f.ledger, &replacement, &f.flow, json!({ "dir": f.dir.to_str().unwrap() })).unwrap();
    assert_eq!(code(claim(&mut f.ledger, &replacement, &own.instance, "head", Timestamp::now())), "ok");
    // The owner claims the next attempt after expiry.
    assert_eq!(f.claim(&fi, "head", later).attempt, 2);
}

#[test]
fn an_instance_that_records_no_owner_takes_no_write() {
    let mut f = fx(CHAIN);
    let fi = id("fi_legacy");
    let flow = &f.flow;
    let started = Started {
        flow: flow.name(),
        ir_hash: flow.hash().into(),
        ir: flow.ir().clone(),
        params: json!({ "dir": f.dir.to_str().unwrap() }),
        owner: None,
    };
    f.ledger.lock().unwrap().append(&fi, 0, started.to_event().unwrap()).unwrap();
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "head", Timestamp::now())), "no_owner");
}

#[test]
fn a_token_is_a_handle_kept_only_as_its_hash_and_never_read_back() {
    // BRO-3624 item 6.
    let mut f = fx(CHAIN);
    let fi = f.begin();
    let c = f.claim(&fi, "head", Timestamp::now());
    let text = f.log_text(&fi);
    assert!(!text.contains(&c.token), "the token is not in the log");
    assert!(text.contains(&maestro_flow::log::line_hash(c.token.as_bytes())), "its hash is");
    let reader = Reader::new(&f.root, Options::default()).unwrap();
    let guard = reader.lock().unwrap();
    let reads = [
        serde_json::to_string(&next(&guard, &fi, Timestamp::now()).unwrap()).unwrap(),
        serde_json::to_string(&status(&guard, &fi, Timestamp::now()).unwrap()).unwrap(),
    ];
    let secret = c.token.rsplit_once('.').unwrap().1;
    assert!(reads.iter().all(|r| !r.contains(secret)), "next and status never return it");
    drop(guard);
    // A token for an instance the caller does not own, a made-up one, and malformed ones.
    let other = f.begin();
    let forged = format!("{other}.{secret}");
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &forged, Timestamp::now())), "unknown_token");
    for bad in ["no-dot".to_string(), format!("{fi}.{}", "a".repeat(maestro_flow::kernel::MAX_TOKEN))] {
        assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &bad, Timestamp::now())), "token_malformed", "{bad}");
    }
    let longest = format!("{fi}.{}", "a".repeat(maestro_flow::kernel::MAX_TOKEN - fi.as_str().len() - 1));
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &longest, Timestamp::now())), "unknown_token");
}

#[test]
fn claim_takes_only_an_enabled_node_the_kernel_can_verify() {
    // Spec acceptance (BRO-3624 item 10): out-of-order refusal, and write-time freshness.
    let mut f = fx(CHAIN);
    let fi = f.begin();
    let now = Timestamp::now();
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "build", now)), "not_enabled");
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "ghost", now)), "unknown_node");
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "pick", now)), "not_claimable");
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "owner", now)), "not_claimable");
    let at = f.done(&fi, "head");
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "head", Timestamp::now())), "not_enabled", "done");
    // Once head has gone stale, what consumes it is not enabled.
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "build", at + secs(61))), "not_enabled");
    assert_eq!(f.claim(&fi, "build", Timestamp::now()).attempt, 1);
    assert_eq!(f.kinds(&fi), ["Started", "Claimed", "Completed", "Claimed"]);
}

#[test]
fn a_held_node_is_not_claimed_again_and_the_next_claim_after_expiry_takes_the_next_attempt() {
    // Spec acceptance: a refusal during the lease leaves the attempt; the next claim increments it.
    let mut f = fx(CHAIN);
    let fi = f.begin();
    let now = Timestamp::now();
    let first = f.claim(&fi, "head", now);
    assert_eq!((first.attempt, first.lease_until), (1, now + secs(DEFAULT_LEASE_SECS as i64)));
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "head", first.lease_until - secs(1))), "already_claimed");
    std::fs::write(f.dir.join("head"), "7").unwrap();
    let refused = f.complete(&first.token, Value::Null, now);
    assert_eq!(outcome(&refused), "exit_unmapped");
    assert_eq!(f.last_body(&fi)["attempt"], 1, "a refusal keeps the attempt");
    let second = f.claim(&fi, "head", first.lease_until);
    assert_eq!(second.attempt, 2);
    // The first token is void now, whatever its lease said.
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &first.token, now)), "lease_expired");
}

#[test]
fn a_lease_ends_at_its_time_and_a_heartbeat_moves_it() {
    // Spec acceptance: lease expiry makes complete and heartbeat refuse lease_expired.
    let mut f = fx(CHAIN);
    let fi = f.begin();
    let now = Timestamp::now();
    let c = f.claim(&fi, "head", now);
    let end = c.lease_until;
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &c.token, end)), "lease_expired", "the lease ends at its time");
    let done = complete(&mut f.ledger, &f.me, &c.token, Value::Null, end, &quick());
    assert_eq!(code(done), "lease_expired");
    let beat = heartbeat(&mut f.ledger, &f.me, &c.token, end - secs(1)).unwrap();
    assert_eq!(beat.lease_until, end - secs(1) + secs(DEFAULT_LEASE_SECS as i64));
    assert_eq!(outcome(&f.complete(&c.token, Value::Null, end)), "completed", "the heartbeat kept it");
    assert_eq!(f.kinds(&fi), ["Started", "Claimed", "Heartbeat", "Completed"]);
}

#[test]
fn a_heartbeat_lease_is_the_node_s_and_bounded() {
    let mut f = fx(
        "flow: w\nversion: 1\nparams: [dir]\nworkdir: \"{{params.dir}}\"\nnodes:\n  short: {class: work, verify: [true], wait: {external: ci, heartbeat: 30s}}\n  long: {class: work, verify: [true], wait: {external: ci, heartbeat: 1000000h}}\n",
    );
    let fi = f.begin();
    let now = Timestamp::now();
    assert_eq!(f.claim(&fi, "short", now).lease_until, now + secs(30));
    assert_eq!(f.claim(&fi, "long", now).lease_until, now + secs(maestro_flow::kernel::MAX_LEASE_SECS as i64));
    let fresh = f.begin();
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fresh, "short", Timestamp::MAX)), "clock_out_of_range");
}

#[test]
fn a_refusal_keeps_the_token_and_the_receipt_spends_it() {
    // Spec acceptance: use of a token after Completed spent it. A completion an agent claims is
    // never a receipt: only the verify the kernel ran makes one.
    let mut f = fx(CHAIN);
    let fi = f.begin();
    f.done(&fi, "head");
    f.done(&fi, "build");
    let c = f.claim(&fi, "review", Timestamp::now());
    let claimed = f.complete(&c.token, json!({ "passed": true }), Timestamp::now());
    assert_eq!(outcome(&claimed), "verify_failed");
    assert_eq!(f.status(&fi, Timestamp::now()).nodes["review"].state, State::Ready);
    for bad in [json!([1]), json!("x")] {
        assert_eq!(outcome(&f.complete(&c.token, bad, Timestamp::now())), "outputs_invalid");
    }
    std::fs::write(f.dir.join("ok"), "").unwrap();
    let Completion::Completed { verify, .. } = f.complete(&c.token, json!({ "note": "fixed" }), Timestamp::now())
    else {
        panic!("the repaired work completes on the same token")
    };
    assert_eq!(verify.argv, ["sh", "-c", "test -f ok"]);
    assert_eq!((verify.exit, verify.stdout_sha256.as_str()), (0, maestro_flow::log::line_hash(b"").as_str()));
    let receipt: Completed = serde_json::from_value(f.last_body(&fi)).unwrap();
    assert_eq!((receipt.attempt, receipt.inputs.clone()), (1, c.inputs.clone()));
    assert_eq!(receipt.performer.unwrap().session.as_deref(), Some("t3:-/owner"));
    assert_eq!(f.status(&fi, Timestamp::now()).nodes["review"].state, State::Done);
    let now = Timestamp::now();
    assert_eq!(code(complete(&mut f.ledger, &f.me, &c.token, Value::Null, now, &quick())), "token_spent");
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &c.token, now)), "token_spent");
    let kinds = f.kinds(&fi);
    assert_eq!(kinds[kinds.len() - 5..], ["Claimed", "Refused", "Refused", "Refused", "Completed"]);
}

#[test]
fn an_observation_outside_its_exit_map_fails_closed_and_a_mapped_one_names_its_value() {
    // Spec acceptance: an exit status outside an exit_map fails closed.
    let mut f = fx(CHAIN);
    let fi = f.begin();
    let c = f.claim(&fi, "head", Timestamp::now());
    std::fs::write(f.dir.join("head"), "3").unwrap();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null, Timestamp::now())), "exit_unmapped");
    assert_eq!(f.status(&fi, Timestamp::now()).nodes["head"].state, State::Ready, "no receipt, nothing failed open");
    std::fs::write(f.dir.join("head"), "1").unwrap();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null, Timestamp::now())), "completed");
    let s = f.status(&fi, Timestamp::now());
    assert_eq!(s.nodes["head"].state, State::Done);
    assert_eq!(s.nodes["build"].state, State::Ready, "a mapped status is a value, not a failure");
}

#[test]
fn a_receipt_is_written_only_while_what_it_consumes_is_fresh() {
    // The writer contract: write-time freshness, at the clock the caller passes.
    let mut f = fx(CHAIN);
    let fi = f.begin();
    let at = f.done(&fi, "head");
    let c = f.claim(&fi, "build", Timestamp::now());
    let refused = f.complete(&c.token, Value::Null, at + secs(61));
    assert_eq!(outcome(&refused), "input_stale");
    assert!(!f.dir.join("built").exists(), "nothing ran");
    assert_eq!(outcome(&f.complete(&c.token, Value::Null, Timestamp::now())), "completed", "the token was kept");
}

#[test]
fn a_claim_bound_to_inputs_that_changed_holds_nothing() {
    let mut f = fx(CHAIN);
    let fi = f.begin();
    let at = f.done(&fi, "head");
    let c = f.claim(&fi, "build", Timestamp::now());
    // head goes stale and is re-run with another result: build's inputs change.
    let stale = at + secs(61);
    let rerun = f.claim(&fi, "head", stale);
    assert_eq!(outcome(&f.complete(&rerun.token, json!({ "v": 2 }), stale)), "completed");
    let now = Timestamp::now();
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &c.token, now)), "inputs_changed");
    assert_eq!(code(complete(&mut f.ledger, &f.me, &c.token, Value::Null, now, &quick())), "inputs_changed");
    let again = f.claim(&fi, "build", now);
    assert_eq!(again.attempt, 2, "the node is released at once");
    assert_ne!(again.inputs, c.inputs);
}

/// A fixture receipt for `s` at iteration `{r: i}`, of an attempt no claim took.
fn settle(f: &mut Fx, fi: &InstanceId, i: u32) {
    let other = Completed {
        node: "s".into(),
        iteration: [("r".to_string(), i)].into(),
        attempt: 9,
        inputs: Default::default(),
        outputs: Value::Null,
        verify: None,
        performer: None,
        tool_use_id: None,
    };
    f.append(fi, other.to_event().unwrap());
}

#[test]
fn a_claim_on_an_iteration_that_moved_on_or_ran_out_holds_nothing() {
    let repeat = |max: u32| {
        format!(
            "flow: r\nversion: 1\nparams: [dir]\nworkdir: \"{{{{params.dir}}}}\"\nnodes:\n  s: {{class: work, verify: [true]}}\nrepeats:\n  r: {{max: {max}, body: [s]}}\n"
        )
    };
    // Iteration 0 settles and the repeat moves on to 1.
    let mut f = fx(&repeat(3));
    let fi = f.begin();
    let c = f.claim(&fi, "s", Timestamp::now());
    settle(&mut f, &fi, 0);
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &c.token, Timestamp::now())), "not_current_iteration");
    assert_eq!(f.claim(&fi, "s", Timestamp::now()).iteration, [("r".to_string(), 1)].into());
    // Its only iteration settles and the repeat is exhausted at that same iteration.
    let mut f = fx(&repeat(1));
    let fi = f.begin();
    let c = f.claim(&fi, "s", Timestamp::now());
    settle(&mut f, &fi, 0);
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &c.token, Timestamp::now())), "not_current_iteration");
}

/// A fixture claim on `head` with `attempt` and a lease ending at `until`.
fn claimed(attempt: u32, until: Timestamp, token: &str) -> NewEvent {
    let c = Claimed {
        node: "head".into(),
        iteration: Default::default(),
        attempt,
        lease_until: until.to_string(),
        token_sha256: token.into(),
        inputs: Default::default(),
        performer: None,
        tool_use_id: None,
    };
    c.to_event().unwrap()
}

#[test]
fn the_attempt_counter_never_wraps_or_goes_back() {
    let mut f = fx(CHAIN);
    let fi = f.begin();
    let past = Timestamp::now() - secs(3600);
    f.append(&fi, claimed(u32::MAX, past, "sha256:x"));
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "head", Timestamp::now())), "attempt_overflow");
    // A claim whose attempt does not follow the last one is set aside, live lease or not.
    let fi = f.begin();
    f.append(&fi, claimed(5, past, "sha256:a"));
    f.append(&fi, claimed(3, Timestamp::now() + secs(3600), "sha256:b"));
    assert_eq!(f.claim(&fi, "head", Timestamp::now()).attempt, 6);
}

#[test]
fn a_command_a_signal_ends_is_refused() {
    let mut f = fx(
        "flow: k\nversion: 1\nparams: [dir]\nworkdir: \"{{params.dir}}\"\nnodes:\n  a: {class: pure, run: [sh, -c, 'kill -9 $$']}\n",
    );
    let fi = f.begin();
    let c = f.claim(&fi, "a", Timestamp::now());
    assert_eq!(outcome(&f.complete(&c.token, Value::Null, Timestamp::now())), "verify_failed");
}

#[test]
fn the_flows_lock_is_free_while_a_command_runs_and_the_receipt_lands_after_what_came_meanwhile() {
    // The command sleeps; meanwhile another writer heartbeats the same claim with a short lock
    // wait. The completion is then checked again and appended after that heartbeat.
    let mut f = fx(CHAIN);
    let fi = f.begin();
    f.done(&fi, "head");
    let c = f.claim(&fi, "build", Timestamp::now());
    std::fs::write(f.dir.join("nap"), "0.12").unwrap();
    let (me, token, built) = (f.me.clone(), c.token.clone(), f.dir.join("built"));
    let mut other = Ledger::open(&f.root, &host("mac"), quick_lock()).unwrap();
    let side = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !built.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        heartbeat(&mut other, &me, &token, Timestamp::now()).map(|_| ()).map_err(|e| e.code().to_string())
    });
    let done = f.complete(&c.token, Value::Null, Timestamp::now());
    assert_eq!(side.join().unwrap(), Ok(()), "the heartbeat took the lock while the command ran");
    assert_eq!(outcome(&done), "completed");
    assert_eq!(f.kinds(&fi)[3..], ["Claimed", "Heartbeat", "Completed"]);
}

fn quick_lock() -> Options {
    Options { lock_wait: Duration::from_millis(30) }
}

#[test]
fn a_lease_or_a_freshness_that_ends_while_the_command_runs_refuses_the_receipt() {
    let mut f = fx(CHAIN);
    let fi = f.begin();
    let at = f.done(&fi, "head");
    std::fs::write(f.dir.join("nap"), "0.06").unwrap();
    // head turns stale 20 ms into the run.
    let c = f.claim(&fi, "build", Timestamp::now());
    let refused = f.complete(&c.token, Value::Null, at + secs(60) - SignedDuration::from_millis(20));
    assert_eq!(outcome(&refused), "input_stale");
    assert_eq!(f.kinds(&fi)[3..], ["Claimed", "Refused"], "no receipt");
    // The lease ends 20 ms into the run (head consumes nothing, so only the lease can lapse).
    let fi = f.begin();
    let c = f.claim(&fi, "head", Timestamp::now());
    let end = c.lease_until - SignedDuration::from_millis(20);
    let lapsed = complete(&mut f.ledger, &f.me, &c.token, Value::Null, end, &quick());
    assert_eq!(code(lapsed), "lease_expired");
    assert_eq!(f.kinds(&fi), ["Started", "Claimed"], "no receipt");
}

#[test]
fn a_command_that_hangs_floods_or_cannot_run_is_refused_and_the_token_kept() {
    let mut f = fx(CHAIN);
    let fi = f.begin();
    f.done(&fi, "head");
    let c = f.claim(&fi, "build", Timestamp::now());
    std::fs::write(f.dir.join("nap"), "10").unwrap();
    let limits = Limits { timeout: Duration::from_millis(50), max_output: 1024 };
    let ran = complete(&mut f.ledger, &f.me, &c.token, Value::Null, Timestamp::now(), &limits).unwrap();
    assert_eq!(outcome(&ran), "verify_timeout");
    let bad = Limits { timeout: Duration::ZERO, max_output: 1024 };
    assert_eq!(code(complete(&mut f.ledger, &f.me, &c.token, Value::Null, Timestamp::now(), &bad)), "invalid_limits");
    std::fs::write(f.dir.join("nap"), "0").unwrap();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null, Timestamp::now())), "completed");
}

#[test]
fn a_command_runs_only_in_a_declared_workdir_with_its_params_filled_in() {
    let node = "nodes:\n  a: {class: pure, run: [sh, -c, 'test \"$0\" = ok', \"{{params.word}}\"]}\n";
    let cases = [
        ("", "workdir_invalid"),
        // Relative, though it names a directory from the test process's own working directory.
        ("workdir: src\n", "workdir_invalid"),
        ("workdir: \"{{params.dir}}/missing\"\n", "workdir_invalid"),
        ("workdir: \"{{params.nope}}\"\n", "workdir_invalid"),
        ("workdir: \"{{params.dir}}\"\n", "completed"),
    ];
    for (workdir, want) in cases {
        let mut f = fx(&format!("flow: d\nversion: 1\nparams: [dir, word, big?]\n{workdir}{node}"));
        let params = json!({ "dir": f.dir.to_str().unwrap(), "word": "ok" });
        let fi = start(&mut f.ledger, &f.me, &f.flow, params).unwrap().instance;
        let c = f.claim(&fi, "a", Timestamp::now());
        assert_eq!(outcome(&f.complete(&c.token, Value::Null, Timestamp::now())), want, "{workdir:?}");
    }
}

#[test]
fn a_command_is_refused_when_its_placeholders_name_no_param_or_it_fills_past_its_bound() {
    // A placeholder naming no param; a command at its byte bound once filled in, and one byte over.
    let five =
        "[true, \"{{params.big}}\", \"{{params.big}}\", \"{{params.big}}\", \"{{params.big}}\", \"{{params.big}}\"]";
    let at_bound = (MAX_ARGV - "true".len()) / 5;
    assert_eq!("true".len() + 5 * at_bound, MAX_ARGV);
    for (run, big, want) in [
        ("[echo, \"{{params.nope}}\"]", 0, "template_invalid"),
        (five, at_bound, "completed"),
        (five, at_bound + 1, "template_invalid"),
    ] {
        let mut f = fx(&format!(
            "flow: d\nversion: 1\nparams: [dir, big?]\nworkdir: \"{{{{params.dir}}}}\"\nnodes:\n  a: {{class: pure, run: {run}}}\n"
        ));
        let params = json!({ "dir": f.dir.to_str().unwrap(), "big": "x".repeat(big) });
        let fi = start(&mut f.ledger, &f.me, &f.flow, params).unwrap().instance;
        let c = f.claim(&fi, "a", Timestamp::now());
        assert_eq!(outcome(&f.complete(&c.token, Value::Null, Timestamp::now())), want, "{run} {big}");
    }
}

#[test]
fn a_status_after_the_kernel_writes_replays_with_nothing_set_aside() {
    // The writers' events are the ones replay accepts.
    let mut f = fx(CHAIN);
    let fi = f.begin();
    f.done(&fi, "head");
    f.done(&fi, "build");
    let c = f.claim(&fi, "review", Timestamp::now());
    heartbeat(&mut f.ledger, &f.me, &c.token, Timestamp::now()).unwrap();
    std::fs::write(f.dir.join("ok"), "").unwrap();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null, Timestamp::now())), "completed");
    let s = f.status(&fi, Timestamp::now());
    assert!(s.rejected.is_empty(), "{:?}", s.rejected);
    assert!(["head", "build", "review"].iter().all(|n| s.nodes[*n].state == State::Done));
}

#[test]
#[ignore = "builds a log at the event cap, about 0.8 s in debug; run with --ignored"]
fn claim_and_heartbeat_churn_at_the_event_cap_is_refused_clearly() {
    // Refusals and heartbeats cannot grow a log past the event cap: each writer reports the cap
    // as a refusal, with nothing written.
    use maestro_flow::log::MAX_EVENTS;
    let mut f = fx(CHAIN);
    let fi = f.begin();
    let c = f.claim(&fi, "head", Timestamp::now());
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
    let now = Timestamp::now();
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &c.token, now)), "too_many_events");
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "head", c.lease_until)), "too_many_events");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "nothing was written");
}
