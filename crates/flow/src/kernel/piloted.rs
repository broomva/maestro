//! `verify@1` on the kernel's clock (BRO-3636): its own definition, on a scratch git repository,
//! driven claim by claim with the kernel running every command. Contract V2 needs the clock to
//! move past pin's `fresh_for`, which only the crate's unit tests can make it do (see `clock`), so
//! V2 is here; V1, V3 and V4, which need the built binary's `maestro dogfood verify-receipt`, run
//! through it in `crates/cli/tests/pilot.rs`.

use crate::clock::TestClock;
use crate::kernel::{Completion, Reason, State, claim, heartbeat, start};
// The claim tests' fixture, loaded once in this binary (`clocked`).
use super::clocked::claiming::*;
use serde_json::{Value, json};
use std::path::Path;

const VERIFY: &str = include_str!("../../flows/verify.yaml");

fn git(repo: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@localhost", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A commit on top of the repository's HEAD; its id.
fn commit(repo: &Path, message: &str) -> String {
    git(repo, &["commit", "-q", "--allow-empty", "-m", message]);
    git(repo, &["rev-parse", "HEAD"])
}

/// The fixture on `verify@1`, its workdir a git repository with one commit, and the test clock.
fn pilot() -> (Fx, TestClock, String) {
    let f = fx(VERIFY);
    git(&f.dir, &["init", "-q"]);
    let head = commit(&f.dir, "one");
    (f, TestClock::install(), head)
}

fn begin(f: &mut Fx, head: &str) -> crate::InstanceId {
    // The project's check leaves a mark, so a test sees whether it ran at all.
    let params = json!({ "repo": f.dir.to_str().unwrap(), "head": head, "check": "touch checked" });
    start(&mut f.ledger, &f.me, &f.flow, params).unwrap().instance
}

fn state(f: &Fx, fi: &crate::InstanceId, node: &str) -> State {
    f.status(fi, crate::clock::now()).nodes[node].state
}

#[test]
fn v2_a_lapsed_pin_holds_check_until_the_kernel_runs_pin_again() {
    let (mut f, test_clock, head) = pilot();
    let fi = begin(&mut f, &head);
    let at = f.done(&fi, "pin");
    // A claim taken while pin is fresh, kept alive past fresh_for by its heartbeat: its completion
    // is refused once pin has lapsed on the kernel clock, and the kernel never runs the check.
    let c = f.claim(&fi, "check");
    test_clock.set(at + secs(590));
    heartbeat(&mut f.ledger, &f.me, &c.token).unwrap();
    test_clock.set(at + secs(601));
    assert_eq!(state(&f, &fi, "pin"), State::Stale);
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "input_stale");
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "check")), "not_enabled");
    let s = f.status(&fi, crate::clock::now());
    assert_eq!(s.nodes["check"].unmet, vec![Reason::Stale { node: "pin".into() }]);
    // The kernel runs pin again; at the same head, check completes on the same claim.
    f.done(&fi, "pin");
    assert_eq!(outcome(&f.complete(&c.token, Value::Null)), "completed");
    assert_eq!(
        f.kinds(&fi),
        ["Started", "Claimed", "Completed", "Claimed", "Heartbeat", "Refused", "Claimed", "Completed", "Completed"]
    );
}

#[test]
fn v2_a_head_that_moved_while_pin_was_fresh_fails_check_until_pin_runs_again() {
    let (mut f, test_clock, head) = pilot();
    let fi = begin(&mut f, &head);
    let at = f.done(&fi, "pin");
    // HEAD moves while pin is still fresh: the kernel cannot see it yet, so check is enabled, but
    // its own command looks at HEAD and fails; it never passes against the moved head.
    let moved = commit(&f.dir, "two");
    let c = f.claim(&fi, "check");
    match f.complete(&c.token, Value::Null) {
        Completion::Refused { code, reason, stderr, .. } => {
            assert_eq!((code, reason.as_str()), ("verify_failed", "the command exited 125"));
            assert_eq!(stderr, format!("the repository is not at {head}\n"));
        }
        other => panic!("{other:?}"),
    }
    assert!(!f.dir.join("checked").exists(), "the project's check never ran against the moved head");
    // Once pin lapses the kernel runs it again: it records moved, and check never runs.
    test_clock.set(at + secs(601));
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "check")), "not_enabled");
    let pin = f.claim(&fi, "pin");
    assert_eq!(outcome(&f.complete(&pin.token, Value::Null)), "completed");
    assert_eq!(f.last_body(&fi)["verify"]["exit"], 1, "pin observed {moved}");
    let s = f.status(&fi, crate::clock::now());
    assert_eq!(s.nodes["check"].state, State::Blocked);
    assert_eq!(
        s.nodes["check"].unmet,
        vec![Reason::Switch { on: "pin".into(), took: Some("moved".into()), case: "at_head".into() }]
    );
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "check")), "not_enabled");
    // Back at the head, check waits for the next run of pin, then passes.
    git(&f.dir, &["reset", "-q", "--hard", &head]);
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "check")), "not_enabled", "pin still says moved");
    test_clock.set(at + secs(1202));
    f.done(&fi, "pin");
    assert_eq!(state(&f, &fi, "check"), State::Ready);
    f.done(&fi, "check");
    assert_eq!(state(&f, &fi, "check"), State::Done);
    assert!(f.dir.join("checked").exists());
}

#[test]
fn v2_a_check_that_moves_head_while_it_runs_fails() {
    let (mut f, _clock, head) = pilot();
    let moves = "git -c user.name=t -c user.email=t@localhost -c commit.gpgsign=false commit -q --allow-empty -m moved";
    let params = json!({ "repo": f.dir.to_str().unwrap(), "head": head, "check": moves });
    let fi = start(&mut f.ledger, &f.me, &f.flow, params).unwrap().instance;
    f.done(&fi, "pin");
    let c = f.claim(&fi, "check");
    match f.complete(&c.token, Value::Null) {
        Completion::Refused { code, reason, stderr, .. } => {
            assert_eq!((code, reason.as_str()), ("verify_failed", "the command exited 125"));
            assert_eq!(stderr, format!("the repository is not at {head}\n"));
        }
        other => panic!("{other:?}"),
    }
    assert_ne!(git(&f.dir, &["rev-parse", "HEAD"]), head, "the check did move HEAD");
}
