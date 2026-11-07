//! BRO-3624 item 3: retiring an instance is an operator step under the lock that keeps its log.

mod common;

use common::*;
use maestro_flow::operator;
use maestro_flow::{FlowError, Ledger, Options, Reader};

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
