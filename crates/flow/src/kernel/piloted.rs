//! `verify@1` on the kernel's clock (BRO-3636): its own definition, on a scratch git repository,
//! driven claim by claim with the kernel running every command. Contract V2 needs the clock to
//! move past pin's `fresh_for`, which only the crate's unit tests can make it do (see `clock`), so
//! V2 is here, with the snapshot `check` runs in; V1, V3 and V4, which need the built binary's
//! `maestro dogfood verify-receipt`, run through it in `crates/cli/tests/pilot.rs`.
//!
//! Every repository here has an empty `core.hooksPath` of its own, so no hook of the host's runs
//! in it, and no assertion reads a command's standard error that a host's git could add to.

use crate::clock::TestClock;
use crate::kernel::{Completion, Limits, Reason, State, claim, complete, heartbeat, start};
// The claim tests' fixture, loaded once in this binary (`clocked`).
use super::clocked::claiming::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

const VERIFY: &str = include_str!("../../flows/verify.yaml");

/// A pure node that runs the project's command in a snapshot of `head`, alone.
const SNAPSHOT: &str = r#"
flow: snap
version: 1
params: [repo, head, check]
workdir: "{{params.repo}}"
nodes:
  check: {class: pure, snapshot: "{{params.head}}", run: [sh, -c, "{{params.check}}"]}
  plain: {class: pure, run: [sh, -c, "{{params.check}}"]}
"#;

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

/// A commit of everything in the working tree; its id.
fn commit(repo: &Path, message: &str) -> String {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "--allow-empty", "-m", message]);
    git(repo, &["rev-parse", "HEAD"])
}

/// `dir` made a repository whose hooks are an empty folder of its own; its first commit.
fn repository(dir: &Path) -> String {
    git(dir, &["init", "-q"]);
    let hooks = dir.parent().unwrap().join("no-hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    git(dir, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
    std::fs::write(dir.join("at-head"), "content of the head\n").unwrap();
    commit(dir, "one")
}

/// The fixture on `yaml`, its workdir a repository with one commit, and the test clock.
fn pilot(yaml: &str) -> (Fx, TestClock, String) {
    let f = fx(yaml);
    let head = repository(&f.dir);
    (f, TestClock::install(), head)
}

fn begin(f: &mut Fx, head: &str, check: &str) -> crate::InstanceId {
    let params = json!({ "repo": f.dir.to_str().unwrap(), "head": head, "check": check });
    start(&mut f.ledger, &f.me, &f.flow, params).unwrap().instance
}

fn state(f: &Fx, fi: &crate::InstanceId, node: &str) -> State {
    f.status(fi, crate::clock::now()).nodes[node].state
}

/// The refusal's code and reason.
fn refused(c: &Completion) -> (&str, &str) {
    match c {
        Completion::Refused { code, reason, .. } => (code, reason),
        other => panic!("{other:?}"),
    }
}

/// What a repository holds that a command could change: HEAD, every ref, the worktrees, the
/// working tree's state and its config.
fn untouched(repo: &Path) -> Vec<String> {
    let config = std::fs::read_to_string(repo.join(".git/config")).unwrap();
    let mut files: Vec<String> =
        std::fs::read_dir(repo).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    files.sort();
    vec![
        git(repo, &["rev-parse", "HEAD"]),
        git(repo, &["for-each-ref"]),
        git(repo, &["worktree", "list", "--porcelain"]),
        git(repo, &["status", "--porcelain", "--untracked-files=all"]),
        config,
        files.join(","),
    ]
}

#[test]
fn v2_a_lapsed_pin_holds_check_until_the_kernel_runs_pin_again() {
    let (mut f, test_clock, head) = pilot(VERIFY);
    let fi = begin(&mut f, &head, "true");
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
fn v2_the_check_runs_on_the_heads_content_never_the_working_tree() {
    let (mut f, _clock, head) = pilot(VERIFY);
    // The working tree differs from the head: a file of the head's deleted, one it lacks added.
    std::fs::remove_file(f.dir.join("at-head")).unwrap();
    std::fs::write(f.dir.join("untracked"), "").unwrap();
    let before = untouched(&f.dir);
    let fi = begin(&mut f, &head, "test -f at-head && test ! -f untracked && touch made");
    f.done(&fi, "pin");
    f.done(&fi, "check");
    // The user's repository is as it was: no file made, no ref, worktree or config added.
    assert_eq!(untouched(&f.dir), before);
    assert!(!f.dir.join("made").exists());
}

#[test]
fn v2_a_check_that_moves_away_and_back_does_not_pass() {
    // The check checks out another commit, where it would pass, and comes back. The other commit
    // is the head's own parent, so a snapshot with the head's history would have it; the snapshot
    // holds the head alone, so there is nothing to move to, and the check fails.
    let (mut f, _clock, _) = pilot(VERIFY);
    std::fs::write(f.dir.join("passed"), "").unwrap();
    let other = commit(&f.dir, "two");
    std::fs::remove_file(f.dir.join("passed")).unwrap();
    let head = commit(&f.dir, "three");
    let before = untouched(&f.dir);
    let check = format!("git checkout -q {other} && test -f passed; s=$?; git checkout -q -; exit $s");
    let fi = begin(&mut f, &head, &check);
    f.done(&fi, "pin");
    let c = f.claim(&fi, "check");
    let done = f.complete(&c.token, Value::Null);
    assert_eq!(refused(&done).0, "verify_failed");
    assert_ne!(state(&f, &fi, "check"), State::Done);
    assert_eq!(untouched(&f.dir), before);
    // The same check in the working tree would have passed: that is what it guards.
    git(&f.dir, &["checkout", "-q", &other]);
    assert!(f.dir.join("passed").exists());
}

#[test]
fn v2_once_pin_lapses_a_head_that_moved_holds_check() {
    let (mut f, test_clock, head) = pilot(VERIFY);
    let fi = begin(&mut f, &head, "test -f at-head");
    let at = f.done(&fi, "pin");
    // HEAD moves while pin is still fresh: the check runs on the head's content and passes for
    // that commit, the one pinned.
    commit(&f.dir, "two");
    f.done(&fi, "check");
    // Once pin lapses the kernel runs it again: it records moved, and the check no longer stands.
    test_clock.set(at + secs(601));
    assert_eq!(state(&f, &fi, "check"), State::Stale);
    let pin = f.claim(&fi, "pin");
    assert_eq!(outcome(&f.complete(&pin.token, Value::Null)), "completed");
    assert_eq!(f.last_body(&fi)["verify"]["exit"], 1);
    let s = f.status(&fi, crate::clock::now());
    assert_eq!(s.nodes["check"].state, State::Blocked);
    assert_eq!(
        s.nodes["check"].unmet,
        vec![Reason::Switch { on: "pin".into(), took: Some("moved".into()), case: "at_head".into() }]
    );
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "check")), "not_enabled");
    // Back at the head, check waits for the next run of pin, which re-validates its receipt.
    git(&f.dir, &["reset", "-q", "--hard", &head]);
    assert_eq!(code(claim(&mut f.ledger, &f.me, &fi, "check")), "not_enabled", "pin still says moved");
    test_clock.set(at + secs(1202));
    f.done(&fi, "pin");
    assert_eq!(state(&f, &fi, "check"), State::Done);
}

#[test]
fn a_snapshot_that_cannot_be_made_is_its_own_refusal_never_the_projects_exit() {
    let (mut f, _clock, head) = pilot(SNAPSHOT);
    // No such commit in the repository: the fetch fails, before the command runs.
    let fi = begin(&mut f, &"0".repeat(40), "touch ran");
    let c = f.claim(&fi, "check");
    let done = f.complete(&c.token, Value::Null);
    let (code, reason) = refused(&done);
    assert_eq!(code, "snapshot_failed");
    assert!(reason.contains("git fetch"), "{reason}");
    // Not a full object id: refused before anything runs, whatever git would make of it.
    for commit in ["HEAD", "--upload-pack=touch pwned", &head[..12], &head.to_uppercase()] {
        let fi = begin(&mut f, commit, "touch ran");
        let c = f.claim(&fi, "check");
        assert_eq!(refused(&f.complete(&c.token, Value::Null)).0, "snapshot_invalid", "{commit}");
    }
    assert!(!f.dir.join("pwned").exists());
    // A project's command that exits 125 is a check that failed with 125, nothing else.
    let fi = begin(&mut f, &head, "exit 125");
    let c = f.claim(&fi, "check");
    assert_eq!(refused(&f.complete(&c.token, Value::Null)), ("verify_failed", "the command exited 125"));
}

#[test]
fn a_snapshot_is_removed_whatever_its_command_came_to() {
    let (mut f, _clock, head) = pilot(SNAPSHOT);
    let out = f.root.parent().unwrap().to_path_buf();
    let short = Limits { timeout: Duration::from_millis(1500), max_output: 64 << 10 };
    let cases: [(&str, &str, Limits); 4] = [
        ("passed", "true", quick()),
        ("failed", "exit 1", quick()),
        ("timed out", "sleep 30", short.clone()),
        ("read only", "chmod -R a-w .", quick()),
    ];
    for (i, (case, check, limits)) in cases.into_iter().enumerate() {
        let seen = out.join(format!("snapshot-{i}"));
        let fi = begin(&mut f, &head, &format!("pwd > {}; {check}", seen.display()));
        let c = f.claim(&fi, "check");
        complete(&mut f.ledger, &f.me, &c.token, Value::Null, &limits).unwrap();
        let dir = PathBuf::from(std::fs::read_to_string(&seen).unwrap().trim());
        assert!(
            dir.starts_with(std::env::temp_dir().canonicalize().unwrap()) || dir.starts_with(std::env::temp_dir()),
            "{case}: {dir:?}"
        );
        assert!(!dir.exists(), "{case}: the snapshot {dir:?} is still there");
    }
}

#[test]
fn only_the_projects_command_in_a_snapshot_searches_the_callers_path() {
    let (mut f, _clock, head) = pilot(SNAPSHOT);
    let out = f.root.parent().unwrap().to_path_buf();
    let fi = begin(&mut f, &head, &format!("printf %s \"$PATH\" > {}", out.join("path-seen").display()));
    f.done(&fi, "check");
    let seen = std::fs::read_to_string(out.join("path-seen")).unwrap();
    assert_eq!(seen, std::env::var("PATH").unwrap(), "the project's toolchain is on the caller's PATH");
    f.done(&fi, "plain");
    let seen = std::fs::read_to_string(out.join("path-seen")).unwrap();
    assert_eq!(seen, maestro_store::tree::git_path(), "every other command searches the system's");
}
