//! BRO-3633 PR 3b2a: `claim`, `heartbeat` and `complete`, with the commands the kernel runs.
//! Each test names the BRO-3624 items it covers. The writers read the wall clock themselves.
//! This binary links the library as every caller does, so it reads the production clock and
//! cannot set or shift it: the tests that must move the clock are the crate's own unit tests
//! (`src/kernel/clocked.rs`). Commands are short `sh` lines.

use crate::claiming::*;
use jiff::Timestamp;
use maestro_core::session::VerifiedSession;
use maestro_flow::clock;
use maestro_flow::kernel::events::{Claimed, Completed, KernelEvent, Started};
use maestro_flow::kernel::{
    Completion, DEFAULT_LEASE_SECS, Limits, MAX_ARGV, State, claim, compile, complete, heartbeat, next, start, status,
};
use maestro_flow::{InstanceId, Ledger, NewEvent, Options, Reader};
use serde_json::{Value, json};
use std::time::Duration;

#[test]
fn the_clock_every_build_reads_is_the_wall_clock() {
    // P20 round 3: `clock::now` is one code path in every build, and this binary is built without
    // `cfg(test)` on the library, as every caller is, so this is the production clock. A frozen
    // clock, or one an hour ahead or behind, fails here.
    let before = Timestamp::now();
    let now = clock::now();
    let after = Timestamp::now();
    assert!(before <= now && now <= after, "{before} <= {now} <= {after}");
    // The writers judge by it: a claim's lease runs from the wall clock at the claim.
    let mut f = fx(CHAIN);
    let fi = f.begin();
    let lease = secs(DEFAULT_LEASE_SECS as i64);
    let before = Timestamp::now();
    let c = f.claim(&fi, "head");
    let after = Timestamp::now();
    assert!(before + lease <= c.lease_until && c.lease_until <= after + lease, "{before} {} {after}", c.lease_until);
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

    let c = f.claim(&fi, "head");
    assert_eq!(code(claim(&mut f.ledger, &unmarked, &fi, "head")), "no_owner_marker");
    assert_eq!(code(heartbeat(&mut f.ledger, &unmarked, &c.token)), "no_owner_marker");
    assert_eq!(code(complete(&mut f.ledger, &unmarked, &c.token, Value::Null, &quick())), "no_owner_marker");
    assert_eq!(f.kinds(&fi), ["Started", "Claimed"]);
}

#[test]
fn only_the_owner_session_with_its_marker_writes_its_instance() {
    // BRO-3624 items 5 and 6: a replacement (same session, new marker) and another session (even
    // with the same marker) are refused every write on the instance; a token in their hands
    // grants nothing; a replacement may start its own instance; no agent transfers an instance
    // (adoption needs the owner's approval, `kernel::adoption`).
    let mut f = fx(CHAIN);
    let fi = f.begin();
    let c = f.claim(&fi, "head");
    for (who, stranger) in
        [("a replacement", agent("owner", "marker-2")), ("another session", agent("other", "marker-1"))]
    {
        assert_eq!(code(claim(&mut f.ledger, &stranger, &fi, "head")), "not_owner", "{who}");
        assert_eq!(code(heartbeat(&mut f.ledger, &stranger, &c.token)), "not_owner", "{who}");
        let completed = complete(&mut f.ledger, &stranger, &c.token, Value::Null, &quick());
        assert_eq!(code(completed), "not_owner", "{who}");
    }
    assert_eq!(f.kinds(&fi), ["Started", "Claimed"]);
    // A replacement may start an instance of its own, and write that one.
    let replacement = agent("owner", "marker-2");
    let own = start(&mut f.ledger, &replacement, &f.flow, json!({ "dir": f.dir.to_str().unwrap() })).unwrap();
    assert_eq!(code(claim(&mut f.ledger, &replacement, &own.instance, "head")), "ok");
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
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "head")), "no_owner");
}

#[test]
fn a_token_is_a_handle_kept_only_as_its_hash_and_never_read_back() {
    // BRO-3624 item 6.
    let mut f = fx(CHAIN);
    let fi = f.begin();
    let c = f.claim(&fi, "head");
    let text = f.log_text(&fi);
    assert!(!text.contains(&c.token), "the token is not in the log");
    assert!(text.contains(&maestro_flow::log::line_hash(c.token.as_bytes())), "its hash is");
    let reader = Reader::new(&f.root, Options::default()).unwrap();
    let guard = reader.lock().unwrap();
    let reads = [
        serde_json::to_string(&next(&guard, &fi, clock::now()).unwrap()).unwrap(),
        serde_json::to_string(&status(&guard, &fi, clock::now()).unwrap()).unwrap(),
    ];
    let secret = c.token.rsplit_once('.').unwrap().1;
    assert!(reads.iter().all(|r| !r.contains(secret)), "next and status never return it");
    drop(guard);
    // A token for an instance the caller does not own, a made-up one, and malformed ones.
    let other = f.begin();
    let forged = format!("{other}.{secret}");
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &forged)), "unknown_token");
    for bad in ["no-dot".to_string(), format!("{fi}.{}", "a".repeat(maestro_flow::kernel::MAX_TOKEN))] {
        assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &bad)), "token_malformed", "{bad}");
    }
    let longest = format!("{fi}.{}", "a".repeat(maestro_flow::kernel::MAX_TOKEN - fi.as_str().len() - 1));
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &longest)), "unknown_token");
}

#[test]
fn a_refusal_keeps_the_token_and_the_receipt_spends_it() {
    // Spec acceptance: use of a token after Completed spent it. A completion an agent claims is
    // never a receipt: only the verify the kernel ran makes one.
    let mut f = fx(CHAIN);
    let fi = f.begin();
    f.done(&fi, "head");
    f.done(&fi, "build");
    let c = f.claim(&fi, "review");
    let claimed = f.complete(&c.token, json!({ "passed": true }));
    assert_eq!(outcome(&claimed), "verify_failed");
    assert_eq!(f.status(&fi, clock::now()).nodes["review"].state, State::Ready);
    for bad in [json!([1]), json!("x")] {
        assert_eq!(outcome(&f.complete(&c.token, bad)), "outputs_invalid");
    }
    std::fs::write(f.dir.join("ok"), "").unwrap();
    let Completion::Completed { verify, .. } = f.complete(&c.token, json!({ "note": "fixed" })) else {
        panic!("the repaired work completes on the same token")
    };
    assert_eq!(verify.argv, ["sh", "-c", "test -f ok"]);
    assert_eq!((verify.exit, verify.stdout_sha256.as_str()), (0, maestro_flow::log::line_hash(b"").as_str()));
    let receipt: Completed = serde_json::from_value(f.last_body(&fi)).unwrap();
    assert_eq!((receipt.attempt, receipt.inputs.clone()), (1, c.inputs.clone()));
    assert_eq!(receipt.performer.unwrap().session.as_deref(), Some("t3:-/owner"));
    assert_eq!(f.status(&fi, clock::now()).nodes["review"].state, State::Done);
    assert_eq!(code(complete(&mut f.ledger, &f.me, &c.token, Value::Null, &quick())), "token_spent");
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &c.token)), "token_spent");
    let kinds = f.kinds(&fi);
    assert_eq!(kinds[kinds.len() - 5..], ["Claimed", "Refused", "Refused", "Refused", "Completed"]);
}

#[test]
fn an_observation_outside_its_exit_map_fails_closed_and_a_mapped_one_names_its_value() {
    // Spec acceptance: an exit status outside an exit_map fails closed.
    let mut f = fx(CHAIN);
    let fi = f.begin();
    let c = f.claim(&fi, "head");
    std::fs::write(f.dir.join("head"), "3").unwrap();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "exit_unmapped");
    assert_eq!(f.status(&fi, clock::now()).nodes["head"].state, State::Ready, "no receipt, nothing failed open");
    std::fs::write(f.dir.join("head"), "1").unwrap();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "completed");
    let s = f.status(&fi, clock::now());
    assert_eq!(s.nodes["head"].state, State::Done);
    assert_eq!(s.nodes["build"].state, State::Ready, "a mapped status is a value, not a failure");
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
    let c = f.claim(&fi, "s");
    settle(&mut f, &fi, 0);
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &c.token)), "not_current_iteration");
    assert_eq!(f.claim(&fi, "s").iteration, [("r".to_string(), 1)].into());
    // Its only iteration settles and the repeat is exhausted at that same iteration.
    let mut f = fx(&repeat(1));
    let fi = f.begin();
    let c = f.claim(&fi, "s");
    settle(&mut f, &fi, 0);
    assert_eq!(code(heartbeat(&mut f.ledger, &f.me, &c.token)), "not_current_iteration");
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
        cancel_at: None,
    };
    c.to_event().unwrap()
}

#[test]
fn the_attempt_counter_never_wraps_or_goes_back() {
    let mut f = fx(CHAIN);
    let fi = f.begin();
    let past = clock::now() - secs(3600);
    f.append(&fi, claimed(u32::MAX, past, "sha256:x"));
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "head")), "attempt_overflow");
    // A claim whose attempt does not follow the last one is set aside, live lease or not.
    let fi = f.begin();
    f.append(&fi, claimed(5, past, "sha256:a"));
    f.append(&fi, claimed(3, clock::now() + secs(3600), "sha256:b"));
    assert_eq!(f.claim(&fi, "head").attempt, 6);
}

#[test]
fn a_command_a_signal_ends_is_refused() {
    let mut f = fx(
        "flow: k\nversion: 1\nparams: [dir]\nworkdir: \"{{params.dir}}\"\nnodes:\n  a: {class: pure, run: [sh, -c, 'kill -9 $$']}\n",
    );
    let fi = f.begin();
    let c = f.claim(&fi, "a");
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "verify_failed");
}

#[test]
fn the_flows_lock_is_free_while_a_command_runs_and_the_receipt_lands_after_what_came_meanwhile() {
    // The command sleeps; meanwhile another writer heartbeats the same claim with a short lock
    // wait. The completion is then checked again and appended after that heartbeat.
    let mut f = fx(CHAIN);
    let fi = f.begin();
    f.done(&fi, "head");
    let c = f.claim(&fi, "build");
    std::fs::write(f.dir.join("nap"), "0.12").unwrap();
    let (me, token, built) = (f.me.clone(), c.token.clone(), f.dir.join("built"));
    let mut other = Ledger::open(&f.root, &host("mac"), quick_lock()).unwrap();
    let side = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !built.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        heartbeat(&mut other, &me, &token).map(|_| ()).map_err(|e| e.code().to_string())
    });
    let done = f.complete(&c.token, Value::Null);
    assert_eq!(side.join().unwrap(), Ok(()), "the heartbeat took the lock while the command ran");
    assert_eq!(outcome(&done), "completed");
    assert_eq!(f.kinds(&fi)[3..], ["Claimed", "Heartbeat", "Completed"]);
}

fn quick_lock() -> Options {
    Options { lock_wait: Duration::from_millis(30) }
}

#[test]
fn a_command_that_hangs_floods_or_cannot_run_is_refused_and_the_token_kept() {
    let mut f = fx(CHAIN);
    let fi = f.begin();
    f.done(&fi, "head");
    let c = f.claim(&fi, "build");
    std::fs::write(f.dir.join("nap"), "10").unwrap();
    let limits = Limits { timeout: Duration::from_millis(50), max_output: 1024 };
    let ran = complete(&mut f.ledger, &f.me, &c.token, Value::Null, &limits).unwrap();
    assert_eq!(outcome(&ran), "verify_timeout");
    let bad = Limits { timeout: Duration::ZERO, max_output: 1024 };
    assert_eq!(code(complete(&mut f.ledger, &f.me, &c.token, Value::Null, &bad)), "invalid_limits");
    std::fs::write(f.dir.join("nap"), "0").unwrap();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "completed");
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
        let c = f.claim(&fi, "a");
        assert_eq!(outcome(&f.complete(&c.token, Value::Null)), want, "{workdir:?}");
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
        let c = f.claim(&fi, "a");
        assert_eq!(outcome(&f.complete(&c.token, Value::Null)), want, "{run} {big}");
    }
}

#[test]
fn only_a_work_nodes_verify_reads_the_outputs_it_judges_and_only_on_its_standard_input() {
    // BRO-3636: a work node's verify reads the outputs it judges as JSON on its standard input,
    // never in its argv; every other command reads an empty input.
    let mut f = fx(r#"
flow: o
version: 1
params: [dir]
workdir: "{{params.dir}}"
nodes:
  work: {class: work, verify: [sh, -c, 'cat > seen-work; grep -q shown seen-work']}
  pure: {class: pure, run: [sh, -c, 'cat > seen-pure; test ! -s seen-pure']}
  observe: {class: observe, run: [sh, -c, 'cat > seen-observe; test ! -s seen-observe'], fresh_for: 1h, exit_map: {0: ok}}
"#);
    let fi = f.begin();
    let c = f.claim(&fi, "work");
    assert_eq!(outcome(&f.complete(&c.token, json!({ "word": "hidden" }))), "verify_failed");
    assert_eq!(outcome(&f.complete(&c.token, json!({ "word": "shown" }))), "completed");
    let seen: Value = serde_json::from_slice(&std::fs::read(f.dir.join("seen-work")).unwrap()).unwrap();
    assert_eq!(seen, json!({ "word": "shown" }), "the outputs, whole, as JSON");
    assert_eq!(f.last_body(&fi)["verify"]["argv"], json!(["sh", "-c", "cat > seen-work; grep -q shown seen-work"]));
    for node in ["pure", "observe"] {
        let c = f.claim(&fi, node);
        assert_eq!(outcome(&f.complete(&c.token, json!({ "word": "shown" }))), "completed", "{node}");
    }
}

#[test]
fn a_performers_text_is_never_read_as_code() {
    // The injection BRO-3636 round 1 found: an outputs placeholder in a shell's argv. It is refused
    // at compile time, in every command and the workdir.
    let injectable = [
        "nodes:\n  w: {class: work, verify: [sh, -c, \"test '{{outputs.value}}' = safe\"]}\n",
        "nodes:\n  p: {class: pure, run: [echo, \"{{outputs.value}}\"]}\n",
        "nodes:\n  o: {class: observe, run: [echo, \"{{outputs.value}}\"], fresh_for: 1h, exit_map: {0: ok}}\n",
        "nodes:\n  e: {class: effect, run: ['true'], reconcile: [echo, \"{{outputs.value}}\"]}\n",
        "nodes:\n  e: {class: effect, run: [echo, \"{{outputs.value}}\"], reconcile: ['true']}\n",
        "nodes:\n  p: {class: pure, run: ['true'], snapshot: \"{{outputs.value}}\"}\n",
        "workdir: \"{{outputs.value}}\"\nnodes:\n  p: {class: pure, run: ['true']}\n",
    ];
    for yaml in injectable {
        let e = compile(&format!("flow: i\nversion: 1\n{yaml}")).unwrap_err();
        assert_eq!(e.code(), "definition_invalid", "{yaml}");
        assert!(e.to_string().contains("standard input"), "{e}");
    }
    // The same check, reading its input as data: the performer's text cannot end the test.
    let mut f = fx(r#"
flow: i
version: 1
params: [dir]
workdir: "{{params.dir}}"
nodes:
  w: {class: work, verify: [sh, -c, 'cat > got; printf %s "$0" > want; cmp -s got want', '{"value":"safe"}']}
"#);
    let fi = f.begin();
    let c = f.claim(&fi, "w");
    assert_eq!(outcome(&f.complete(&c.token, json!({ "value": "'; exit 0; #" }))), "verify_failed");
    assert_eq!(outcome(&f.complete(&c.token, json!({ "value": "safe" }))), "completed");
}

#[test]
fn an_outputs_placeholder_pinned_before_the_rule_is_refused_as_it_was() {
    // An IR pinned before the compile-time rule keeps what it did: its placeholder names no param,
    // so the command is refused template_invalid before it runs, and the outputs fill nothing.
    let mut f = fx(
        "flow: o\nversion: 1\nparams: [dir]\nworkdir: \"{{params.dir}}\"\nnodes:\n  w: {class: work, verify: ['true']}\n",
    );
    let mut ir = f.flow.ir().clone();
    let maestro_flow::kernel::ir::Class::Work { verify, .. } = &mut ir.nodes.get_mut("w").unwrap().class else {
        panic!()
    };
    *verify = vec!["test".into(), "{{outputs.value}}".into(), "=".into(), "safe".into()];
    let started = Started {
        flow: "o@1".into(),
        ir_hash: maestro_flow::kernel::ir::content_hash(&ir).unwrap(),
        ir,
        params: json!({ "dir": f.dir.to_str().unwrap() }),
        owner: Some(maestro_flow::kernel::events::Owner {
            session: "t3:-/owner".parse().unwrap(),
            marker_sha256: maestro_flow::kernel::events::marker_digest("marker-1"),
        }),
    };
    let fi = InstanceId::new("fi_pinned_outputs").unwrap();
    f.ledger.lock().unwrap().append(&fi, 0, started.to_event().unwrap()).unwrap();
    let c = f.claim(&fi, "w");
    assert_eq!(outcome(&f.complete(&c.token, json!({ "value": "safe" }))), "template_invalid");
}

#[test]
fn a_status_after_the_kernel_writes_replays_with_nothing_set_aside() {
    // The writers' events are the ones replay accepts.
    let mut f = fx(CHAIN);
    let fi = f.begin();
    f.done(&fi, "head");
    f.done(&fi, "build");
    let c = f.claim(&fi, "review");
    heartbeat(&mut f.ledger, &f.me, &c.token).unwrap();
    std::fs::write(f.dir.join("ok"), "").unwrap();
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "completed");
    let s = f.status(&fi, clock::now());
    assert!(s.rejected.is_empty(), "{:?}", s.rejected);
    assert!(["head", "build", "review"].iter().all(|n| s.nodes[*n].state == State::Done));
}

/// A pure node `a` that runs `python3 -c <script>`.
fn python(script: &str) -> String {
    let run = serde_json::to_string(&["python3", "-c", script]).unwrap();
    format!(
        "flow: p\nversion: 1\nparams: [dir]\nworkdir: \"{{{{params.dir}}}}\"\nnodes:\n  a: {{class: pure, run: {run}}}\n"
    )
}

/// The command forks a child that leaves its process group (`setsid`, so the group kill misses
/// it) and keeps standard output open, writing a byte every 10 ms. When a write fails, the run's
/// end of the pipe is closed: the child records that in `closed` and exits.
const ESCAPE: &str = "import os, time
r, w = os.pipe()
if os.fork() == 0:
    os.setsid()
    os.write(w, b'.')
    end = time.time() + 5
    while time.time() < end:
        try:
            os.write(1, b'x')
        except BrokenPipeError:
            open('closed', 'w').close()
            break
        time.sleep(0.01)
    os._exit(0)
os.read(r, 1)
";

#[test]
fn a_process_that_leaves_the_group_holding_the_output_fails_at_the_grace_and_holds_nothing() {
    use maestro_flow::kernel::exec::READ_GRACE;
    let mut f = fx(&python(ESCAPE));
    let fi = f.begin();
    let c = f.claim(&fi, "a");
    // `complete` runs on its own thread, so a run that does not return fails this test at a fixed
    // bound instead of hanging the suite.
    let (root, me, token) = (f.root.clone(), f.me.clone(), c.token.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
        let started = std::time::Instant::now();
        let done = complete(&mut ledger, &me, &token, Value::Null, &quick());
        let _ = tx.send((done.map(|c| outcome(&c).to_string()).map_err(|e| e.code().to_string()), started.elapsed()));
    });
    let (done, took) = rx.recv_timeout(Duration::from_secs(6)).expect("complete returned");
    assert_eq!(done, Ok("output_unclosed".to_string()));
    // The escaped process holds the pipe for 5 s: a run that returns well before that returned at
    // the grace. The slack is wide so a loaded machine (mutation shards) does not fail it.
    assert!(took >= READ_GRACE && took < READ_GRACE + Duration::from_secs(3), "{took:?}");
    // Nothing of the run outlives it: no descriptor holds the pipe, so the child's next write
    // fails at once (and no reader thread exists to hold one).
    let closed = f.dir.join("closed");
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while !closed.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(closed.exists(), "the run closed its end of the pipe when it returned");
    assert_eq!(f.kinds(&fi), ["Started", "Claimed", "Refused"], "the token is kept");
}

/// A flow whose pure node `a` runs `sh -c 'touch ran'` with 25 copies of param `big` after it.
fn wide() -> String {
    let words = vec!["\"{{params.big}}\""; 25].join(", ");
    format!(
        "flow: d\nversion: 1\nparams: [dir, big?]\nworkdir: \"{{{{params.dir}}}}\"\nnodes:\n  a: {{class: pure, run: [sh, -c, 'touch ran', {words}]}}\n"
    )
}

#[test]
fn work_whose_receipt_cannot_fit_an_event_line_is_refused_before_it_runs() {
    // 25 copies of a 10,000-character param fit the argv bound (250,000 bytes), but each control
    // character is six bytes once JSON escapes it: the receipt would be about 1.5 MB, more than
    // an event line takes, so no append could ever record it.
    let mut f = fx(&wide());
    let params = json!({ "dir": f.dir.to_str().unwrap(), "big": "\u{1}".repeat(10_000) });
    let fi = start(&mut f.ledger, &f.me, &f.flow, params).unwrap().instance;
    let c = f.claim(&fi, "a");
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "receipt_too_large");
    assert!(!f.dir.join("ran").exists(), "nothing ran");
    assert_eq!(f.kinds(&fi), ["Started", "Claimed", "Refused"]);
}

#[test]
fn a_receipt_at_its_size_bound_is_written_and_one_byte_more_is_refused_before_it_runs() {
    // The bound `complete` checks before the run: the receipt's body with the widest exit status
    // (i32::MIN) and an empty `ran_at`, its outputs counted, plus a 256-byte margin for the
    // envelope and `ran_at`, at most an event line. At that bound the receipt is written (the
    // margin holds the real envelope); one byte over, it is refused and nothing runs.
    const MARGIN: usize = 256;
    let mut f = fx(&wide());
    // 25 copies of 6,900 control characters: about 1 MB of argv once JSON escapes it.
    let params = json!({ "dir": f.dir.to_str().unwrap(), "big": "\u{1}".repeat(6_900) });
    let take = |f: &mut Fx, pad: usize| {
        let fi = start(&mut f.ledger, &f.me, &f.flow, params.clone()).unwrap().instance;
        let c = f.claim(&fi, "a");
        let _ = std::fs::remove_file(f.dir.join("ran"));
        let done = f.complete(&c.token, json!({ "s": "x".repeat(pad) }));
        (fi, done)
    };
    // The receipt with no padding, measured as the bound measures it.
    let (fi, done) = take(&mut f, 0);
    assert_eq!(outcome(&done), "completed");
    let mut receipt: Completed = serde_json::from_value(f.last_body(&fi)).unwrap();
    let verify = receipt.verify.as_mut().unwrap();
    (verify.exit, verify.ran_at) = (i32::MIN, String::new());
    let bare = serde_json::to_vec(&receipt).unwrap().len();
    let pad = maestro_flow::log::MAX_LINE as usize - MARGIN - bare;
    assert!(pad > 0 && pad < maestro_flow::kernel::MAX_OUTPUTS, "{pad}");

    let (fi, done) = take(&mut f, pad);
    assert_eq!(outcome(&done), "completed", "at the bound");
    let line = f.log_text(&fi).lines().last().unwrap().len() + 1;
    assert!(line <= maestro_flow::log::MAX_LINE as usize, "{line}");
    let (fi, done) = take(&mut f, pad + 1);
    assert_eq!(outcome(&done), "receipt_too_large", "one byte over");
    assert!(!f.dir.join("ran").exists(), "nothing ran");
    assert_eq!(f.kinds(&fi), ["Started", "Claimed", "Refused"]);
}
