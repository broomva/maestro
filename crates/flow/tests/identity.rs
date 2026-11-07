//! BRO-3624 item 2: host binding and ledger identity.

mod common;

use common::*;
use maestro_flow::operator::{self, Recovered};
use maestro_flow::{FlowError, Ledger, Options, Reader};
use std::time::Duration;

/// Not a test: the entry a spawned child process runs (see `common::child_main`).
#[test]
fn child_process_entry() {
    child_main();
}

#[test]
fn racing_first_writers_elect_exactly_one_host() {
    for round in 0..3 {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_in(tmp.path());
        let go = tmp.path().join("go");
        let hosts: Vec<String> = (0..6).map(|i| format!("host-{round}-{i}")).collect();
        let children: Vec<_> =
            hosts.iter().map(|h| spawn_child(&["genesis", root.to_str().unwrap(), h, go.to_str().unwrap()])).collect();
        std::fs::write(&go, "").unwrap();
        let results: Vec<String> = children.into_iter().map(child_result).collect();
        let winners: Vec<&String> = hosts.iter().zip(&results).filter(|(_, r)| *r == "ok").map(|(h, _)| h).collect();
        assert_eq!(winners.len(), 1, "round {round}: {results:?}");
        assert!(results.iter().filter(|r| *r != "ok").all(|r| r == "foreign_host"), "round {round}: {results:?}");
        assert_eq!(std::fs::read_to_string(root.join("HOST")).unwrap().trim(), winners[0].as_str());
        let epoch = std::fs::read_to_string(root.join("EPOCH")).unwrap();
        assert_eq!(std::fs::read_to_string(lock_of(tmp.path())).unwrap(), epoch);
        // The losers' staged roots and lock files are gone.
        assert_eq!(names(&tmp.path().join(".broomva")), vec!["flows", "flows.lock"]);
    }
}

#[test]
fn genesis_builds_the_whole_root_and_a_second_open_reuses_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let first = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    assert_eq!(names(&root), vec!["EPOCH", "HOST", "instances", "retired"]);
    assert_eq!(std::fs::read_to_string(root.join("EPOCH")).unwrap().trim(), first.epoch());
    assert_eq!(std::fs::read_to_string(lock_of(tmp.path())).unwrap().trim(), first.epoch());
    let second = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    assert_eq!(second.epoch(), first.epoch());
    assert_eq!(code(Ledger::open(&root, &host("nuc"), Options::default())), "foreign_host");
}

#[test]
fn a_deleted_or_emptied_lock_never_re_epochs_a_live_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let lock = lock_of(tmp.path());
    let mut old = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    let epoch = old.epoch().to_string();
    append_at_head(&mut old, &id("fi_a"), event("Started", 0)).unwrap();

    std::fs::remove_file(&lock).unwrap();
    assert_eq!(code(Ledger::open(&root, &host("mac"), quick())), "lock_lost");
    assert_eq!(code(Reader::new(&root, quick()).unwrap().lock()), "lock_lost");
    assert!(!lock.exists(), "a writer recreated the lock");
    assert_eq!(std::fs::read_to_string(root.join("EPOCH")).unwrap().trim(), epoch);

    std::fs::write(&lock, "").unwrap();
    assert_eq!(code(Ledger::open(&root, &host("mac"), quick())), "lock_lost");
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), "");
    assert_eq!(std::fs::read_to_string(root.join("EPOCH")).unwrap().trim(), epoch);

    // The operator's recovery restores the root's own epoch, never a new one.
    std::fs::remove_file(&lock).unwrap();
    assert_eq!(operator::recover(&root, &quick()).unwrap(), Recovered::RestoredLock { epoch: epoch.clone() });
    std::fs::write(&lock, "").unwrap();
    assert_eq!(operator::recover(&root, &quick()).unwrap(), Recovered::RestoredLock { epoch: epoch.clone() });
    let reopened = Ledger::open(&root, &host("mac"), quick()).unwrap();
    assert_eq!(reopened.epoch(), epoch);
    // A writer still holding the deleted lock file would lock a file nobody else locks: refused.
    assert_eq!(code(old.lock()), "lock_lost");
}

#[test]
fn a_crash_mid_genesis_is_refused_then_recovered() {
    // Crash after the lock was linked, before the staged root moved into place.
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let parent = tmp.path().join(".broomva");
    let epoch = "0123456789abcdef0123456789abcdef";
    let stage = parent.join(format!(".flows.genesis-{epoch}"));
    std::fs::create_dir_all(stage.join("instances")).unwrap();
    std::fs::create_dir_all(stage.join("retired")).unwrap();
    std::fs::write(stage.join("HOST"), "mac\n").unwrap();
    std::fs::write(stage.join("EPOCH"), format!("{epoch}\n")).unwrap();
    std::fs::write(lock_of(tmp.path()), format!("{epoch}\n")).unwrap();
    let before = snapshot(&parent);
    assert_eq!(code(Ledger::open(&root, &host("mac"), quick())), "genesis_incomplete");
    assert_eq!(code(Reader::new(&root, quick()).unwrap().lock()), "genesis_incomplete");
    assert_eq!(snapshot(&parent), before, "a refused writer changed the tree");
    assert_eq!(operator::recover(&root, &quick()).unwrap(), Recovered::FinishedGenesis { epoch: epoch.into() });
    let mut ledger = Ledger::open(&root, &host("mac"), quick()).unwrap();
    assert_eq!(ledger.epoch(), epoch);
    append_at_head(&mut ledger, &id("fi_a"), event("Started", 0)).unwrap();

    // An empty lock with no root: refused, and the recovery returns the tree to empty.
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    std::fs::create_dir_all(tmp.path().join(".broomva")).unwrap();
    std::fs::write(lock_of(tmp.path()), "").unwrap();
    assert_eq!(code(Ledger::open(&root, &host("mac"), quick())), "genesis_incomplete");
    assert_eq!(operator::recover(&root, &quick()).unwrap(), Recovered::ClearedEmptyLock);
    Ledger::open(&root, &host("mac"), quick()).unwrap();

    // A crash before the lock was linked leaves only a stage, which the next genesis ignores.
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    std::fs::create_dir_all(tmp.path().join(".broomva/.flows.genesis-ffffffffffffffffffffffffffffffff")).unwrap();
    Ledger::open(&root, &host("mac"), quick()).unwrap();
}

#[test]
fn an_archived_root_refuses_waiting_and_new_writers_and_creates_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let parent = tmp.path().join(".broomva");
    let mut archiver = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    append_at_head(&mut archiver, &id("fi_a"), event("Started", 0)).unwrap();
    let mut waiting = Ledger::open(&root, &host("mac"), Options { lock_wait: Duration::from_secs(10) }).unwrap();

    let guard = archiver.lock().unwrap();
    let waiter = std::thread::spawn(move || code(waiting.lock()));
    std::thread::sleep(Duration::from_millis(200));
    std::fs::rename(&root, parent.join("flows.archived")).unwrap();
    drop(guard);
    assert_eq!(waiter.join().unwrap(), "root_moved");

    let before = snapshot(&parent);
    assert_eq!(code(Ledger::open(&root, &host("mac"), quick())), "root_moved");
    assert_eq!(code(Ledger::open(&root, &host("nuc"), quick())), "root_moved");
    assert_eq!(code(Reader::new(&root, quick()).unwrap().lock()), "root_moved");
    assert_eq!(code(archiver.lock()), "root_moved");
    assert_eq!(snapshot(&parent), before, "a refused writer created something");
    assert_eq!(names(&parent), vec!["flows.archived", "flows.lock"]);
}

#[test]
fn a_recreated_root_with_another_epoch_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let parent = tmp.path().join(".broomva");
    let mut old = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    assert!(matches!(operator::create_fresh_ledger(&root, &host("nuc"), &quick()), Err(FlowError::OperatorRefused(_))));

    std::fs::rename(&root, parent.join("flows.archived")).unwrap();
    // A fresh ledger for a new host is the operator's step.
    let fresh = operator::create_fresh_ledger(&root, &host("nuc"), &quick()).unwrap();
    assert_ne!(fresh, old.epoch());
    let refused = old.lock();
    assert!(matches!(refused, Err(FlowError::EpochMismatch { .. })), "{refused:?}");
    assert_eq!(code(Ledger::open(&root, &host("mac"), quick())), "foreign_host");
    let nuc = Ledger::open(&root, &host("nuc"), quick()).unwrap();
    assert_eq!(nuc.epoch(), fresh);

    // A root swapped in by hand, with an epoch its lock does not name, is refused to everyone.
    std::fs::write(root.join("EPOCH"), "ffffffffffffffffffffffffffffffff\n").unwrap();
    assert_eq!(code(Ledger::open(&root, &host("nuc"), quick())), "epoch_mismatch");
    assert_eq!(code(Reader::new(&root, quick()).unwrap().lock()), "epoch_mismatch");
}
