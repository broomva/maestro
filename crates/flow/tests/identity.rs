//! BRO-3624 item 2: host binding and ledger identity, checked when the lock is taken and again
//! at every write.

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

fn race(pause: Option<&Pause>) {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let go = tmp.path().join("go");
    let hosts: Vec<String> = (0..6).map(|i| format!("host-{i}")).collect();
    let children: Vec<_> = hosts
        .iter()
        .map(|h| spawn_child_pausing(&["genesis", root.to_str().unwrap(), h, go.to_str().unwrap()], pause))
        .collect();
    std::fs::write(&go, "").unwrap();
    if let Some(p) = pause {
        // The winner holds the window between linking its lock and moving its root in; the
        // losers pile up on the lock meanwhile.
        p.reached();
        std::thread::sleep(Duration::from_millis(300));
        p.go();
    }
    let results: Vec<String> = children.into_iter().map(child_result).collect();
    let winners: Vec<&String> = hosts.iter().zip(&results).filter(|(_, r)| *r == "ok").map(|(h, _)| h).collect();
    assert_eq!(winners.len(), 1, "{results:?}");
    assert!(results.iter().filter(|r| *r != "ok").all(|r| r == "foreign_host"), "{results:?}");
    assert_eq!(std::fs::read_to_string(root.join("HOST")).unwrap().trim(), winners[0].as_str());
    let epoch = std::fs::read_to_string(root.join("EPOCH")).unwrap();
    assert_eq!(std::fs::read_to_string(lock_of(tmp.path())).unwrap(), epoch);
    // The losers' staged roots and lock stages are gone.
    assert_eq!(names(&tmp.path().join(".broomva")), vec!["flows", "flows.lock"]);
}

#[test]
fn racing_first_writers_elect_exactly_one_host() {
    for _ in 0..3 {
        race(None);
    }
}

/// The window between the winner linking its lock and moving its root in, held open: every loser
/// must wait on the lock and then refuse `foreign_host`, never see a half-made ledger.
#[test]
fn racing_losers_wait_for_the_winners_root() {
    race(Some(&Pause::at("genesis_linked")));
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
    assert_eq!(operator::recover(&root, &quick()).unwrap(), Recovered::RestoredLock { epoch: epoch.clone() });

    // A writer still holding a guard through a deleted lock holds the root directory's lock too:
    // recovering waits for it.
    let mut current = Ledger::open(&root, &host("mac"), quick()).unwrap();
    {
        let guard = current.lock().unwrap();
        std::fs::remove_file(&lock).unwrap();
        assert_eq!(code(operator::recover(&root, &quick())), "busy");
        assert!(!lock.exists());
        drop(guard);
    }
    assert_eq!(operator::recover(&root, &quick()).unwrap(), Recovered::RestoredLock { epoch: epoch.clone() });
    let reopened = Ledger::open(&root, &host("mac"), quick()).unwrap();
    assert_eq!(reopened.epoch(), epoch);
    // A writer still holding the deleted lock file would lock a file nobody else locks: refused.
    assert_eq!(code(old.lock()), "lock_lost");
}

/// A genesis's lock says `genesis <epoch>` until its root is in place: a crash at any point
/// after the lock is linked is refused, then recovered forward (root in place) or cleared (not).
#[test]
fn a_crash_mid_genesis_is_refused_then_recovered() {
    for root_in_place in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_in(tmp.path());
        let parent = tmp.path().join(".broomva");
        let lock = lock_of(tmp.path());
        let epoch = if root_in_place {
            // Crashed after moving its root in, before the lock named the epoch.
            let epoch = "0123456789abcdef0123456789abcdef".to_string();
            for dir in ["instances", "retired"] {
                std::fs::create_dir_all(root.join(dir)).unwrap();
            }
            std::fs::write(root.join("HOST"), "mac\n").unwrap();
            std::fs::write(root.join("EPOCH"), format!("{epoch}\n")).unwrap();
            std::fs::write(&lock, format!("genesis {epoch}\n")).unwrap();
            epoch
        } else {
            // A real genesis killed right after linking its lock.
            let go = tmp.path().join("go");
            std::fs::write(&go, "").unwrap();
            let pause = Pause::at("genesis_linked");
            let mut child =
                spawn_child_pausing(&["genesis", root.to_str().unwrap(), "mac", go.to_str().unwrap()], Some(&pause));
            pause.reached();
            child.kill().unwrap();
            child.wait().unwrap();
            let text = std::fs::read_to_string(&lock).unwrap();
            text.trim().strip_prefix("genesis ").expect("the lock says the genesis did not finish").to_string()
        };
        let before = snapshot(&parent);
        assert_eq!(code(Ledger::open(&root, &host("mac"), quick())), "genesis_incomplete");
        assert_eq!(code(Reader::new(&root, quick()).unwrap().lock()), "genesis_incomplete");
        assert_eq!(snapshot(&parent), before, "a refused writer changed the tree");
        let recovered = operator::recover(&root, &quick()).unwrap();
        let mut ledger = Ledger::open(&root, &host("mac"), quick()).unwrap();
        if root_in_place {
            assert_eq!(recovered, Recovered::FinishedGenesis { epoch: epoch.clone() });
            assert_eq!(ledger.epoch(), epoch);
        } else {
            assert_eq!(recovered, Recovered::ClearedGenesis);
            assert_ne!(ledger.epoch(), epoch);
        }
        append_at_head(&mut ledger, &id("fi_a"), event("Started", 0)).unwrap();
        assert_eq!(names(&parent), vec!["flows", "flows.lock"]);
    }
}

#[test]
fn a_root_moved_by_hand_refuses_waiting_and_new_writers_and_creates_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let parent = tmp.path().join(".broomva");
    let mut archiver = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    append_at_head(&mut archiver, &id("fi_a"), event("Started", 0)).unwrap();
    let mut waiting = Ledger::open(&root, &host("mac"), Options { lock_wait: Duration::from_secs(10) }).unwrap();

    let guard = archiver.lock().unwrap();
    let waiter = std::thread::spawn(move || code(waiting.lock()));
    std::thread::sleep(Duration::from_millis(200));
    std::fs::rename(&root, parent.join("flows-moved")).unwrap();
    drop(guard);
    assert_eq!(waiter.join().unwrap(), "root_moved");

    let before = snapshot(&parent);
    assert_eq!(code(Ledger::open(&root, &host("mac"), quick())), "root_moved");
    assert_eq!(code(Ledger::open(&root, &host("nuc"), quick())), "root_moved");
    assert_eq!(code(Reader::new(&root, quick()).unwrap().lock()), "root_moved");
    assert_eq!(code(archiver.lock()), "root_moved");
    assert_eq!(snapshot(&parent), before, "a refused writer created something");
    assert_eq!(names(&parent), vec!["flows-moved", "flows.lock"]);
}

/// The operator's archive moves the root and its lock together, under the lock, and leaves a
/// marker: no writer, waiting or new, writes or makes anything there until a fresh ledger.
#[test]
fn an_archived_ledger_refuses_every_writer_until_a_fresh_one() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let parent = tmp.path().join(".broomva");
    let dest = tmp.path().join("archive-1");
    let mut holder = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    append_at_head(&mut holder, &id("fi_a"), event("Started", 0)).unwrap();
    let log = std::fs::read(root.join("instances/fi_a/events.jsonl")).unwrap();
    let mut waiting = Ledger::open(&root, &host("mac"), Options { lock_wait: Duration::from_secs(10) }).unwrap();

    let guard = holder.lock().unwrap();
    let (r, d) = (root.clone(), dest.clone());
    let archiving = std::thread::spawn(move || code(operator::archive(&r, &d, &Options::default())));
    std::thread::sleep(Duration::from_millis(200));
    assert!(root.exists(), "the archive went ahead under a guard");
    drop(guard);
    assert_eq!(archiving.join().unwrap(), "ok");
    assert_eq!(names(&dest), vec!["flows", "flows.lock"]);
    assert_eq!(std::fs::read(dest.join("flows/instances/fi_a/events.jsonl")).unwrap(), log);

    let before = snapshot(&parent);
    assert_eq!(code(waiting.lock()), "root_moved");
    assert_eq!(code(holder.lock()), "root_moved");
    assert_eq!(code(Ledger::open(&root, &host("mac"), quick())), "root_moved");
    assert_eq!(code(Reader::new(&root, quick()).unwrap().lock()), "root_moved");
    assert_eq!(code(operator::recover(&root, &quick())), "operator_refused");
    assert_eq!(snapshot(&parent), before, "a refused writer created something");
    assert_eq!(names(&parent), vec!["flows.archived"]);
    assert_eq!(code(operator::archive(&root, &tmp.path().join("archive-2"), &quick())), "root_moved");

    let fresh = operator::create_fresh_ledger(&root, &host("nuc"), &quick()).unwrap();
    assert_eq!(names(&parent), vec!["flows", "flows.lock"]);
    assert_eq!(Ledger::open(&root, &host("nuc"), quick()).unwrap().epoch(), fresh);
    assert_eq!(code(waiting.lock()), "epoch_mismatch");
    assert_eq!(code(operator::archive(&root, &dest, &quick())), "operator_refused");
}

/// The destination is checked before anything is written: its parent must exist, it must not,
/// and it must be outside the root.
#[test]
fn an_archive_checks_its_destination_first() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let parent = tmp.path().join(".broomva");
    drop(Ledger::open(&root, &host("mac"), Options::default()).unwrap());
    std::fs::create_dir(tmp.path().join("taken")).unwrap();
    let before = snapshot(tmp.path());
    for dest in
        [tmp.path().join("missing/archive"), tmp.path().join("taken"), root.join("inside"), root.join("instances/x")]
    {
        assert_eq!(code(operator::archive(&root, &dest, &quick())), "operator_refused", "{}", dest.display());
    }
    assert_eq!(snapshot(tmp.path()), before, "a refused archive wrote something");
    assert_eq!(names(&parent), vec!["flows", "flows.lock"]);
}

#[test]
fn a_recreated_root_with_another_epoch_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let parent = tmp.path().join(".broomva");
    let mut old = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    assert!(matches!(operator::create_fresh_ledger(&root, &host("nuc"), &quick()), Err(FlowError::OperatorRefused(_))));

    std::fs::rename(&root, parent.join("flows-moved")).unwrap();
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

/// A lock file replaced while a guard holds the old one: the root directory's own lock still
/// keeps a second writer out, the first guard's next write refuses, and the log never forks.
#[test]
fn a_lock_file_replaced_under_a_guard_never_forks_the_log() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let fi = id("fi_a");
    let mut first = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    append_at_head(&mut first, &fi, event("Started", 0)).unwrap();

    let mut guard = first.lock().unwrap();
    replace_with_copy(&lock_of(tmp.path()));
    let mut second = Ledger::open(&root, &host("mac"), quick()).unwrap();
    assert_eq!(code(second.lock()), "busy", "a second writer went ahead beside a held guard");
    assert_eq!(code(guard.append(&fi, 1, event("Claimed", 1))), "lock_lost");
    drop(guard);

    assert_eq!(second.lock().unwrap().append(&fi, 1, event("Claimed", 2)).unwrap().event.seq, 2);
    assert_eq!(code(first.lock()), "lock_lost");
    let log = Reader::new(&root, quick()).unwrap().lock().unwrap().log(&fi).unwrap();
    assert_eq!(log.seq(), 2);
    assert_eq!(log.events[1].body["n"], 2);
}

/// A guard held while the root is moved away, replaced by a copy or a link, or its epoch rewritten:
/// the next write refuses and recreates nothing.
#[cfg(unix)]
#[test]
fn a_write_checks_the_root_it_was_given() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let parent = tmp.path().join(".broomva");
    let mut ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    append_at_head(&mut ledger, &id("fi_a"), event("Started", 0)).unwrap();

    let mut guard = ledger.lock().unwrap();
    std::fs::rename(&root, parent.join("flows-moved")).unwrap();
    assert_eq!(code(guard.append(&id("fi_a"), 1, event("Tick", 1))), "root_moved");
    assert_eq!(code(guard.append(&id("fi_new"), 0, event("Started", 0))), "root_moved");
    assert_eq!(names(&parent), vec!["flows-moved", "flows.lock"]);

    // A copy put in its place is another directory.
    std::fs::create_dir_all(root.join("instances")).unwrap();
    std::fs::create_dir_all(root.join("retired")).unwrap();
    for f in ["HOST", "EPOCH"] {
        std::fs::copy(parent.join("flows-moved").join(f), root.join(f)).unwrap();
    }
    assert_eq!(code(guard.append(&id("fi_new"), 0, event("Started", 0))), "root_moved");
    assert!(names(&root.join("instances")).is_empty());
    drop(guard);
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::rename(parent.join("flows-moved"), &root).unwrap();

    // A link to the real root put in its place: `lstat` never takes the link for the root.
    let mut guard = ledger.lock().unwrap();
    std::fs::rename(&root, parent.join("flows-real")).unwrap();
    std::os::unix::fs::symlink(parent.join("flows-real"), &root).unwrap();
    assert_eq!(code(guard.append(&id("fi_new"), 0, event("Started", 0))), "root_moved");
    assert!(!parent.join("flows-real/instances/fi_new").exists());
    drop(guard);
    assert_eq!(code(Ledger::open(&root, &host("mac"), quick())), "symlink");
    assert_eq!(code(Reader::new(&root, quick()).unwrap().lock()), "symlink");
    std::fs::remove_file(&root).unwrap();
    std::fs::rename(parent.join("flows-real"), &root).unwrap();

    // The same root with its epoch rewritten under the guard.
    let mut guard = ledger.lock().unwrap();
    let epoch = std::fs::read_to_string(root.join("EPOCH")).unwrap();
    std::fs::write(root.join("EPOCH"), "ffffffffffffffffffffffffffffffff\n").unwrap();
    assert_eq!(code(guard.append(&id("fi_a"), 1, event("Tick", 1))), "epoch_mismatch");
    std::fs::write(root.join("EPOCH"), epoch).unwrap();
    assert_eq!(guard.append(&id("fi_a"), 1, event("Tick", 1)).unwrap().event.seq, 2);
}

/// The root moved away after the write's checks and before it lands: the write path never makes
/// the root or `instances/`, so nothing comes back.
#[test]
fn a_root_moved_mid_write_is_not_recreated() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let parent = tmp.path().join(".broomva");
    drop(Ledger::open(&root, &host("mac"), Options::default()).unwrap());
    let pause = Pause::at("append_checked");
    let child = spawn_child_pausing(&["append", root.to_str().unwrap(), "mac", "fi_new", "1", "5000"], Some(&pause));
    pause.reached();
    std::fs::rename(&root, parent.join("flows-moved")).unwrap();
    pause.go();
    assert_eq!(child_result(child), "root_moved");
    assert_eq!(names(&parent), vec!["flows-moved", "flows.lock"]);
    assert!(names(&parent.join("flows-moved/instances")).is_empty());
}

/// The lock file replaced between a writer or reader opening it and locking it: they lock a
/// file nobody else locks, so they refuse.
#[test]
fn a_lock_replaced_while_opening_is_refused() {
    for spec in [&["open", "", "mac"][..], &["read", ""][..]] {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_in(tmp.path());
        drop(Ledger::open(&root, &host("mac"), Options::default()).unwrap());
        let mut spec: Vec<&str> = spec.to_vec();
        spec[1] = root.to_str().unwrap();
        let pause = Pause::at("lock_opened");
        let child = spawn_child_pausing(&spec, Some(&pause));
        pause.reached();
        replace_with_copy(&lock_of(tmp.path()));
        pause.go();
        assert_eq!(child_result(child), "lock_lost", "{}", spec[0]);
    }
}

/// A fresh ledger re-checks, under the lock, that the old root is still gone, and that the lock
/// it holds is the lock: a root back in place or a lock replaced meanwhile leaves everything as
/// it was.
#[test]
fn a_fresh_ledger_rechecks_under_the_lock() {
    for (point, expected) in [("fresh_checked", "operator_refused"), ("lock_opened", "lock_lost")] {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_in(tmp.path());
        let parent = tmp.path().join(".broomva");
        let lock = lock_of(tmp.path());
        let epoch = Ledger::open(&root, &host("mac"), Options::default()).unwrap().epoch().to_string();
        std::fs::rename(&root, parent.join("flows-moved")).unwrap();
        let pause = Pause::at(point);
        let child = spawn_child_pausing(&["fresh", root.to_str().unwrap(), "nuc"], Some(&pause));
        pause.reached();
        if point == "fresh_checked" {
            std::fs::rename(parent.join("flows-moved"), &root).unwrap();
        } else {
            replace_with_copy(&lock);
        }
        pause.go();
        assert_eq!(child_result(child), expected, "{point}");
        assert_eq!(std::fs::read_to_string(&lock).unwrap().trim(), epoch, "{point}");
        let in_place = if point == "fresh_checked" { root.clone() } else { parent.join("flows-moved") };
        assert_eq!(std::fs::read_to_string(in_place.join("EPOCH")).unwrap().trim(), epoch, "{point}");
        assert_eq!(root.exists(), point == "fresh_checked", "{point}");
    }
}
