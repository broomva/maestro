//! BRO-3624 item 3: retiring an instance is an operator step under the lock that keeps its log.

mod common;

use common::*;
use maestro_flow::operator;
use maestro_flow::{FlowError, Ledger, Options, Reader};
use std::io::Write as _;

#[test]
fn a_retired_instance_keeps_its_log_and_is_never_written_again() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let fi = id("fi_a");
    let mut ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    for n in 0..3 {
        append_at_head(&mut ledger, &fi, event("Tick", n)).unwrap();
    }
    let log = std::fs::read(root.join("instances/fi_a/events.jsonl")).unwrap();
    operator::retire(&mut ledger, &fi).unwrap();
    assert_eq!(std::fs::read(root.join("retired/fi_a/events.jsonl")).unwrap(), log);
    assert!(!root.join("instances/fi_a").exists());

    let mut guard = ledger.lock().unwrap();
    assert!(matches!(guard.append(&fi, 3, event("Tick", 3)), Err(FlowError::Retired { .. })));
    // Never reopened as a new instance either.
    assert!(matches!(guard.append(&fi, 0, event("Started", 0)), Err(FlowError::Retired { .. })));
    assert!(!root.join("instances/fi_a").exists());
    assert!(guard.instances().unwrap().is_empty());
    drop(guard);
    assert_eq!(code(Reader::new(&root, quick()).unwrap().lock().unwrap().log(&fi)), "retired");
    assert_eq!(code(operator::retire(&mut ledger, &fi)), "retired");
    assert_eq!(code(operator::retire(&mut ledger, &id("fi_none"))), "unknown_instance");
    assert_eq!(std::fs::read(root.join("retired/fi_a/events.jsonl")).unwrap(), log);
}

#[test]
fn retiring_waits_for_the_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let fi = id("fi_a");
    let mut ledger = Ledger::open(&root, &host("mac"), quick()).unwrap();
    append_at_head(&mut ledger, &fi, event("Started", 0)).unwrap();
    let mut other = Ledger::open(&root, &host("mac"), quick()).unwrap();
    let guard = other.lock().unwrap();
    assert_eq!(code(operator::retire(&mut ledger, &fi)), "busy");
    assert!(root.join("instances/fi_a/events.jsonl").exists());
    drop(guard);
    operator::retire(&mut ledger, &fi).unwrap();
}

/// A `retired/` replaced by a link would move an instance out of the root: refused, and the
/// instance stays where it was.
#[cfg(unix)]
#[test]
fn retiring_through_a_linked_retired_dir_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let fi = id("fi_a");
    let mut ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    append_at_head(&mut ledger, &fi, event("Started", 0)).unwrap();
    std::fs::remove_dir(root.join("retired")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("retired")).unwrap();
    assert_eq!(code(operator::retire(&mut ledger, &fi)), "symlink");
    assert!(root.join("instances/fi_a/events.jsonl").exists());
    assert!(names(&outside).is_empty());
}

/// The kernel stats `retired/<id>` and never opens anything inside it: with the retired log and
/// its directory unreadable, every write and read still refuses cleanly with `retired`.
#[cfg(unix)]
#[test]
fn the_kernel_never_reads_a_retired_log() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let fi = id("fi_a");
    let mut ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    append_at_head(&mut ledger, &fi, event("Started", 0)).unwrap();
    operator::retire(&mut ledger, &fi).unwrap();
    let dir = root.join("retired/fi_a");
    let file = dir.join("events.jsonl");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();

    let results = {
        let mut guard = ledger.lock().unwrap();
        let writes = [code(guard.append(&fi, 1, event("Tick", 1))), code(guard.append(&fi, 0, event("Started", 0)))];
        let logged = code(guard.log(&fi));
        drop(guard);
        let reader = Reader::new(&root, quick()).unwrap();
        let read = reader.lock().unwrap();
        let reads = (code(read.log(&fi)), code(read.summary(&fi)));
        drop(read);
        (writes, logged, reads.0, reads.1, code(operator::truncate_torn_tail(&mut ledger, &fi)))
    };

    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        results,
        (
            ["retired".to_string(), "retired".to_string()],
            "retired".into(),
            "retired".into(),
            "retired".into(),
            "retired".into()
        )
    );
}

/// Copies a directory tree, as an operator restoring a backup over the root would.
fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to.join(entry.file_name()));
        } else {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

/// Opens a ledger with one instance whose log has a torn tail, then swaps the root for a
/// same-epoch copy, as an operator restoring a backup over it would. Runs `op` on the opened
/// ledger: `lock()` still finds a whole ledger with the right epoch, but the write checks the root
/// is the one this writer opened, so it must refuse `root_moved` and change nothing in either tree.
fn refuses_a_root_swapped_for_a_copy(op: fn(&mut Ledger, &maestro_flow::InstanceId) -> String) {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let parent = tmp.path().join(".broomva");
    let fi = id("fi_a");
    let mut ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    append_at_head(&mut ledger, &fi, event("Started", 0)).unwrap();
    let log = root.join("instances/fi_a/events.jsonl");
    std::fs::OpenOptions::new().append(true).open(&log).unwrap().write_all(b"{\"seq\":2,").unwrap();
    std::fs::rename(&root, parent.join("flows-original")).unwrap();
    copy_tree(&parent.join("flows-original"), &root);
    let before = snapshot(tmp.path());
    assert_eq!(op(&mut ledger, &fi), "root_moved");
    assert_eq!(snapshot(tmp.path()), before, "a refused operator write changed a tree");
}

#[test]
fn retire_refuses_a_root_swapped_for_a_copy() {
    refuses_a_root_swapped_for_a_copy(|ledger, fi| code(operator::retire(ledger, fi)));
}

#[test]
fn truncate_refuses_a_root_swapped_for_a_copy() {
    refuses_a_root_swapped_for_a_copy(|ledger, fi| code(operator::truncate_torn_tail(ledger, fi)));
}
