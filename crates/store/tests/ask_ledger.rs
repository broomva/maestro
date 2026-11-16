//! `Store::ask_ledger`, the one-ledger read the flow kernel's human steps use (BRO-3635), and the
//! bounded read under it: a ledger over its limit is refused at any revision without being taken
//! in, and an entry that does not read is an error, never skipped.

use maestro_store::tree::MAX_READ;
use maestro_store::{LEDGER_LIMIT, Paths, Store, TreeSource};
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@localhost", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

const HEAD: &str = "arc: a\nlanes: [x]\ntz: UTC\nopened: '2026-11-09'\nasks:\n  - id: one\n    ask: \"q?\"\n";

/// A workspace holding `ledger.yaml` (committed) and `big.bin`, as `text` and `big` bytes.
fn workspace(text: &str, big: usize) -> (tempfile::TempDir, Paths) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path());
    let asks = paths.workspace.join(".control/asks");
    std::fs::create_dir_all(&asks).unwrap();
    std::fs::write(asks.join("ledger.yaml"), text).unwrap();
    std::fs::write(paths.workspace.join("big.bin"), vec![b'x'; big]).unwrap();
    git(&paths.workspace, &["init", "-q", "-b", "main"]);
    git(&paths.workspace, &["add", "-A"]);
    git(&paths.workspace, &["commit", "-qm", "seed"]);
    (tmp, paths)
}

#[test]
fn a_ledger_over_its_limit_is_refused_at_the_read_in_the_working_tree_and_at_a_revision() {
    // Over LEDGER_LIMIT and under MAX_READ: the ledger's own limit refuses it.
    let text = format!("{HEAD}    pad: \"{}\"\n", "p".repeat(LEDGER_LIMIT));
    let (_tmp, paths) = workspace(&text, 1);
    for source in [TreeSource::WorkingTree, TreeSource::GitRev("HEAD".into())] {
        let e = Store::new(paths.clone(), source.clone()).ask_ledger("ledger.yaml").unwrap_err();
        assert!(e.contains(&format!("over the {LEDGER_LIMIT}-byte read limit")), "{source}: {e}");
    }
    // At a revision the blob's size is asked before it is read: the refusal names its true size,
    // which a read cut at the limit could not know.
    let e = Store::new(paths, TreeSource::GitRev("HEAD".into())).ask_ledger("ledger.yaml").unwrap_err();
    assert!(e.contains(&format!("is {} bytes", text.len())), "{e}");
}

#[test]
fn a_blob_over_max_read_at_a_revision_is_refused_by_its_size() {
    let (_tmp, paths) = workspace(HEAD, MAX_READ + 7);
    let store = Store::new(paths, TreeSource::GitRev("HEAD".into()));
    let e = store.tree.read("big.bin").unwrap_err().to_string();
    assert!(e.contains(&format!("is {} bytes", MAX_READ + 7)), "{e}");
}

#[test]
fn an_entry_that_does_not_read_is_an_error_not_skipped() {
    let (_tmp, paths) = workspace(&format!("{HEAD}  - ask: \"no id\"\n"), 1);
    let e = Store::new(paths.clone(), TreeSource::WorkingTree).ask_ledger("ledger.yaml").unwrap_err();
    assert!(e.contains("ledger.yaml#1"), "{e}");
    // A ledger that reads whole is its asks; one that is not there is none.
    let (_tmp, paths) = workspace(HEAD, 1);
    let store = Store::new(paths, TreeSource::WorkingTree);
    assert_eq!(store.ask_ledger("ledger.yaml").unwrap().unwrap().asks.len(), 1);
    assert!(store.ask_ledger("absent.yaml").unwrap().is_none());
}
