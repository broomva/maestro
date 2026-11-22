//! `tree::read_verified`'s own bounds (BRO-3635): every object it reads is cut at its bound before
//! it is read in full, and every git call it makes is given a deadline, so neither an oversized
//! object nor a FIFO planted at an object's path holds the answer read.

use maestro_store::tree::{MAX_OBJECT, has_object, local_commit, read_verified};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn git(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repository holding `f` (`text`), committed; its root and the commit.
fn repo(text: &str) -> (tempfile::TempDir, PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("r");
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("f"), text).unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", "x"]);
    let head = git(&root, &["rev-parse", "HEAD"]);
    (tmp, root, head)
}

const T: Duration = Duration::from_secs(20);

#[test]
fn a_file_over_its_bound_is_cut_at_the_bound() {
    let (_tmp, root, head) = repo(&"x".repeat(100));
    assert_eq!(read_verified(&root, &head, "f", 100, T).unwrap().map(|t| t.len()), Some(100));
    let e = read_verified(&root, &head, "f", 10, T).unwrap_err();
    assert!(e.contains("said more than 10 bytes (read 11, then stopped)"), "{e}");
}

#[test]
fn a_commit_over_the_object_bound_is_cut_at_the_bound() {
    let (_tmp, root, head) = repo("x");
    let tree = git(&root, &["rev-parse", "HEAD^{tree}"]);
    let message = root.join("message");
    std::fs::write(&message, "m".repeat(2 * MAX_OBJECT)).unwrap();
    let big = git(&root, &["commit-tree", &tree, "-p", &head, "-F", message.to_str().unwrap()]);
    let e = read_verified(&root, &big, "f", 100, T).unwrap_err();
    assert!(e.contains(&format!("said more than {MAX_OBJECT} bytes (read {}, then stopped)", MAX_OBJECT + 1)), "{e}");
}

#[test]
fn a_tree_over_the_object_bound_is_cut_at_the_bound() {
    let (_tmp, root, _head) = repo("x");
    let blob = git(&root, &["rev-parse", "HEAD:f"]);
    // About 40 000 entries of ~34 bytes: past MAX_OBJECT (1 MiB).
    let entries: String = (0..40_000).map(|i| format!("100644 blob {blob}\tf{i:05}\n")).collect();
    let listing = root.join("entries");
    std::fs::write(&listing, entries).unwrap();
    let out = Command::new("sh").args(["-c", "git mktree < entries"]).current_dir(&root).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let tree = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let big = git(&root, &["commit-tree", &tree, "-m", "big"]);
    let e = read_verified(&root, &big, "f00000", 100, T).unwrap_err();
    assert!(e.contains(&format!("said more than {MAX_OBJECT} bytes (read {}, then stopped)", MAX_OBJECT + 1)), "{e}");
}

#[test]
fn a_fifo_at_an_objects_path_is_given_up_at_the_deadline() {
    let (_tmp, root, head) = repo("x");
    let blob = git(&root, &["rev-parse", "HEAD:f"]);
    let loose = root.join(".git/objects").join(&blob[..2]).join(&blob[2..]);
    std::fs::remove_file(&loose).unwrap();
    let made = Command::new("mkfifo").arg(&loose).status().unwrap();
    assert!(made.success());
    let (tx, rx) = std::sync::mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        let _ = tx.send(read_verified(&root, &head, "f", 100, Duration::from_secs(1)));
    });
    let got = rx.recv_timeout(Duration::from_secs(15)).expect("the read never gave up");
    let e = got.unwrap_err();
    assert!(e.contains("did not answer within 1s"), "{e}");
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}

#[test]
fn a_fifo_where_git_looks_cannot_hang_the_presence_check() {
    // A FIFO at the alternates file: `cat-file -e` opens it and would wait forever.
    let (_tmp, root, head) = repo("x");
    assert_eq!(has_object(&root, &head, T), Ok(true));
    let made = Command::new("mkfifo").arg(root.join(".git/objects/info/alternates")).status().unwrap();
    assert!(made.success());
    let (tx, rx) = std::sync::mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        let _ = tx.send(has_object(&root, &head, Duration::from_secs(1)));
    });
    let got = rx.recv_timeout(Duration::from_secs(15)).expect("the presence check never gave up");
    let e = got.unwrap_err();
    assert!(e.contains("did not answer within 1s"), "{e}");
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}

#[test]
fn a_local_ref_is_absent_a_commit_or_an_error() {
    let (_tmp, root, head) = repo("x");
    assert_eq!(local_commit(&root, "refs/heads/main", T), Ok(Some(head.clone())));
    assert_eq!(local_commit(&root, "refs/heads/maestro/answers", T), Ok(None));
    // There but naming a blob: an error, never absent.
    let blob = git(&root, &["rev-parse", "HEAD:f"]);
    std::fs::create_dir_all(root.join(".git/refs/heads/maestro")).unwrap();
    std::fs::write(root.join(".git/refs/heads/maestro/answers"), format!("{blob}\n")).unwrap();
    assert!(local_commit(&root, "refs/heads/maestro/answers", T).is_err());
    // There and not a ref at all: an error.
    std::fs::write(root.join(".git/refs/heads/maestro/answers"), "not an id\n").unwrap();
    assert!(local_commit(&root, "refs/heads/maestro/answers", T).is_err());
}

#[test]
fn a_fifo_where_git_looks_cannot_hang_the_ref_resolution() {
    // A FIFO at the head commit's own loose object, its only copy in this fresh repo (no pack, no
    // commit-graph): `rev-parse <ref>^{commit}` opens it to peel the ref (checked on the Mac's git
    // and Linux git 2.43). A FIFO at the alternates file is not opened by every git, which let a
    // mutant lengthening the deadline survive on CI (maestro#62). The assertion names the
    // rev-parse step, so a git that does not open the object fails this test, never passes it.
    let (_tmp, root, head) = repo("x");
    let loose = root.join(".git/objects").join(&head[..2]).join(&head[2..]);
    std::fs::remove_file(&loose).unwrap();
    let made = Command::new("mkfifo").arg(&loose).status().unwrap();
    assert!(made.success());
    let (tx, rx) = std::sync::mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        let _ = tx.send(local_commit(&root, "refs/heads/main", Duration::from_secs(1)));
    });
    let got = rx.recv_timeout(Duration::from_secs(15)).expect("the ref resolution never gave up");
    let e = got.unwrap_err();
    assert!(e.contains("git rev-parse did not answer within 1s"), "{e}");
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}

#[test]
fn verified_commit_reads_the_exact_message_and_parent() {
    let (_tmp, root, parent) = repo("x");
    std::fs::write(root.join("f"), "y").unwrap();
    git(&root, &["commit", "-qam", "answer\n\nMaestro-Owner-Proof: {}"]);
    let head = git(&root, &["rev-parse", "HEAD"]);
    let commit = maestro_store::tree::verified_commit(&root, &head, T).unwrap();
    assert_eq!(commit.parents, [parent]);
    assert!(commit.message.contains("Maestro-Owner-Proof: {}"));
    let forged = "1".repeat(40);
    let objects = root.join(".git/objects");
    let from = objects.join(&head[..2]).join(&head[2..]);
    let to = objects.join(&forged[..2]).join(&forged[2..]);
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    std::fs::copy(from, to).unwrap();
    let error = maestro_store::tree::verified_commit(&root, &forged, T).unwrap_err();
    assert!(error.contains("does not hash to its id"), "{error}");
    let malformed = root.join("bad-commit");
    let tree = git(&root, &["rev-parse", "HEAD^{tree}"]);
    std::fs::write(&malformed, format!("tree {tree}\nparent --bad\n\nmessage\n")).unwrap();
    let bad = git(&root, &["hash-object", "--literally", "-t", "commit", "-w", malformed.to_str().unwrap()]);
    assert!(maestro_store::tree::verified_commit(&root, &bad, T).unwrap_err().contains("invalid parent"));
}
