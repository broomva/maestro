//! The human snapshot's landing clause (BRO-3269): a count of answers not yet landed only where
//! one is made and known, "not known" otherwise, never 0 or null in their place.

use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// The asks line of `maestro snapshot` on a workspace with one open ask committed on main.
fn asks_line(tree: &str, branch: impl FnOnce(&Path)) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join(".control/asks")).unwrap();
    std::fs::write(ws.join(".control/asks/a.yaml"), "asks:\n  - id: q\n    ask: which?\n").unwrap();
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "open"]);
    branch(&ws);
    let out = Command::new(env!("CARGO_BIN_EXE_maestro"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", tmp.path().join("home"))
        .env("BROOMVA_WORKSPACE", &ws)
        .env("MAESTRO_TREE", tree)
        .env("MAESTRO_FLEET_STATE", tmp.path().join("fleet"))
        .arg("snapshot")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    text.lines().find(|l| l.trim_start().starts_with("asks ")).unwrap_or_default().to_string()
}

#[test]
fn a_count_that_could_not_be_read_says_not_known_never_null() {
    let line = asks_line("main", |ws| {
        // maestro/answers names an object the repo does not hold.
        std::fs::create_dir_all(ws.join(".git/refs/heads/maestro")).unwrap();
        std::fs::write(ws.join(".git/refs/heads/maestro/answers"), format!("{}\n", "1".repeat(40))).unwrap();
    });
    assert!(
        line.contains(
            "answered in this checkout or on maestro/answers (this clone's, or origin's as last fetched): not known"
        ),
        "{line}"
    );
    assert!(!line.contains("null") && !line.contains("0 answered in this checkout"), "{line}");
}

#[test]
fn the_working_tree_makes_no_landing_count() {
    let line = asks_line("worktree", |_| {});
    assert!(line.contains("1 open"), "{line}");
    assert!(!line.contains("maestro/answers") && !line.contains("not yet landed"), "{line}");
}

#[test]
fn a_waiting_answer_is_counted_exactly() {
    let line = asks_line("main", |ws| {
        git(ws, &["checkout", "-q", "-b", "maestro/answers"]);
        std::fs::write(ws.join(".control/asks/a.yaml"), "asks:\n  - id: q\n    ask: which?\n    answer: this\n")
            .unwrap();
        git(ws, &["commit", "-q", "-am", "answer"]);
        git(ws, &["checkout", "-q", "main"]);
    });
    assert!(line.contains(", 1 answered in this checkout or on maestro/answers (this clone's, or origin's as last fetched), not yet landed"), "{line}");
}

#[test]
fn a_count_from_a_partial_read_is_not_known() {
    let line = asks_line("main", |ws| {
        git(ws, &["checkout", "-q", "-b", "maestro/answers"]);
        std::fs::write(ws.join(".control/asks/b.yaml"), "asks: [unclosed\n").unwrap();
        git(ws, &["add", "."]);
        git(ws, &["commit", "-q", "-m", "broken"]);
        git(ws, &["checkout", "-q", "main"]);
    });
    // A ledger that did not read may hold an answer: the count is not known, never a bound.
    assert!(
        line.contains(
            "answered in this checkout or on maestro/answers (this clone's, or origin's as last fetched): not known"
        ),
        "{line}"
    );
}
