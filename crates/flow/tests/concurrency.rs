//! BRO-3624 item 1: single writer and concurrency. One lock, and under it one rule: an append
//! names the sequence number it expects.

mod common;

use common::*;
use maestro_flow::{FlowError, Ledger, Options, Reader};

/// Not a test: the entry a spawned child process runs (see `common::child_main`).
#[test]
fn child_process_entry() {
    child_main();
}

#[test]
fn two_processes_appending_at_once_are_serialized() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    drop(Ledger::open(&root, &host("mac"), Options::default()).unwrap());
    let r = root.to_str().unwrap();
    let a = spawn_child(&["append", r, "mac", "fi_shared", "40", "20000"]);
    let b = spawn_child(&["append", r, "mac", "fi_shared", "40", "20000"]);
    assert_eq!(child_result(a), "ok");
    assert_eq!(child_result(b), "ok");
    let reader = Reader::new(&root, Options::default()).unwrap();
    let log = reader.lock().unwrap().log(&id("fi_shared")).unwrap();
    // Verified end to end: 80 events numbered 1..=80, each chained to the one before.
    assert_eq!(log.seq(), 80);
    let mut pids: Vec<u64> = log.events.iter().map(|e| e.body["pid"].as_u64().unwrap()).collect();
    pids.sort();
    pids.dedup();
    assert_eq!(pids.len(), 2, "both processes wrote");
}

#[test]
fn a_held_write_lock_makes_another_process_wait() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let mut ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    let r = root.to_str().unwrap();
    let guard = ledger.lock().unwrap();
    let blocked = spawn_child(&["append", r, "mac", "fi_a", "1", "300"]);
    assert_eq!(child_result(blocked), "busy");
    drop(guard);
    let after = spawn_child(&["append", r, "mac", "fi_a", "1", "5000"]);
    assert_eq!(child_result(after), "ok");
}

/// The kernel drops the guard before slow external work, such as filing an ask: the lock must
/// then be free for everyone else while this writer's handle stays open.
#[test]
fn dropping_the_guard_releases_the_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let mut ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    append_at_head(&mut ledger, &id("fi_a"), event("Started", 0)).unwrap();
    let guard = ledger.lock().unwrap();
    drop(guard);
    // Another process gets it within a short wait while `ledger` is still open here.
    let other = spawn_child(&["append", root.to_str().unwrap(), "mac", "fi_a", "1", "300"]);
    assert_eq!(child_result(other), "ok");
    // So does another handle in this process, and this one again afterwards.
    let mut second = Ledger::open(&root, &host("mac"), quick()).unwrap();
    drop(second.lock().unwrap());
    drop(ledger.lock().unwrap());
}

#[test]
fn a_stale_sequence_number_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let fi = id("fi_a");
    let mut a = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    let mut b = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    append_at_head(&mut a, &fi, event("Started", 0)).unwrap();

    // A reads the head and releases the lock (as before filing an ask); B appends meanwhile.
    let seen = a.lock().unwrap().log(&fi).unwrap().seq();
    assert_eq!(seen, 1);
    b.lock().unwrap().append(&fi, seen, event("Claimed", 1)).unwrap();

    // A comes back with what it saw: refused, and nothing is written.
    let mut guard = a.lock().unwrap();
    let refused = guard.append(&fi, seen, event("Claimed", 2));
    assert!(matches!(refused, Err(FlowError::StaleSeq { expected: 1, current: 2, .. })), "{refused:?}");
    assert!(matches!(guard.append(&fi, 7, event("Claimed", 2)), Err(FlowError::StaleSeq { .. })));
    // Creating an instance that exists is a stale sequence number too, not a second log.
    assert!(matches!(guard.append(&fi, 0, event("Started", 2)), Err(FlowError::StaleSeq { .. })));
    assert_eq!(guard.log(&fi).unwrap().seq(), 2);
    // With the head it now reads, the same writer succeeds: the rule is the sequence number, nothing else.
    assert_eq!(guard.append(&fi, 2, event("Claimed", 2)).unwrap().event.seq, 3);
    // An append naming an instance that does not exist, with a nonzero sequence number, creates nothing.
    assert!(matches!(guard.append(&id("fi_none"), 1, event("Claimed", 0)), Err(FlowError::UnknownInstance { .. })));
    assert!(!root.join("instances/fi_none").exists());
}
