//! Tests that need a pause point (`hooks`): a race window held open, or a child process killed at
//! a crash point. They live in the crate under `cfg(test)` because pause points exist in no other
//! build. A child is this unit-test binary run again (`tests/common`).

// Loaded once in this binary; `kernel::clocked` uses it too.
#[path = "../tests/common/mod.rs"]
pub(crate) mod common;

use crate::operator::{self, Recovered};
use crate::{Ledger, Options, Reader};
use common::*;
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

/// A genesis killed between linking its lock and moving its staged root in: every writer and
/// reader refuses `genesis_incomplete` and changes nothing, and `recover` moves the stage in.
#[test]
fn a_crash_mid_genesis_is_refused_then_recovered() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let parent = tmp.path().join(".broomva");
    let go = tmp.path().join("go");
    std::fs::write(&go, "").unwrap();
    let pause = Pause::at("genesis_linked");
    let mut child =
        spawn_child_pausing(&["genesis", root.to_str().unwrap(), "mac", go.to_str().unwrap()], Some(&pause));
    pause.reached();
    child.kill().unwrap();
    child.wait().unwrap();
    let epoch = std::fs::read_to_string(lock_of(tmp.path())).unwrap().trim().to_string();
    let before = snapshot(&parent);
    assert_eq!(code(Ledger::open(&root, &host("mac"), quick())), "genesis_incomplete");
    assert_eq!(code(Reader::new(&root, quick()).unwrap().lock()), "genesis_incomplete");
    assert_eq!(snapshot(&parent), before, "a refused writer changed the tree");
    assert_eq!(operator::recover(&root, &quick()).unwrap(), Recovered::FinishedGenesis { epoch: epoch.clone() });
    let mut ledger = Ledger::open(&root, &host("mac"), quick()).unwrap();
    assert_eq!(ledger.epoch(), epoch);
    append_at_head(&mut ledger, &id("fi_a"), event("Started", 0)).unwrap();
    assert_eq!(names(&parent), vec!["flows", "flows.lock"]);
}

/// A write paused after its checks while the operator moves the root away by hand, deletes the
/// lock and another host's writer starts a new ledger there: the write lands in the directory it
/// verified (the moved root), through its descriptor, and never in the new ledger. Both a new
/// instance and an existing one.
#[test]
fn a_paused_write_lands_only_in_the_root_it_verified() {
    for (instance, seq) in [("fi_new", 1), ("fi_a", 2)] {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_in(tmp.path());
        let parent = tmp.path().join(".broomva");
        let mut mac = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
        append_at_head(&mut mac, &id("fi_a"), event("Started", 0)).unwrap();
        let pause = Pause::at("append_checked");
        let child =
            spawn_child_pausing(&["append", root.to_str().unwrap(), "mac", instance, "1", "5000"], Some(&pause));
        pause.reached();
        std::fs::rename(&root, parent.join("flows-moved")).unwrap();
        std::fs::remove_file(lock_of(tmp.path())).unwrap();
        let nuc = Ledger::open(&root, &host("nuc"), quick()).unwrap();
        pause.go();
        assert_eq!(child_result(child), "ok", "{instance}");
        let text =
            std::fs::read_to_string(parent.join("flows-moved/instances").join(instance).join("events.jsonl")).unwrap();
        assert_eq!(text.lines().count() as u64, seq, "{instance}");
        assert!(names(&root.join("instances")).is_empty(), "{instance}: the write reached the new ledger");
        assert_eq!(names(&root), vec!["EPOCH", "HOST", "instances", "retired"], "{instance}");
        let new = Reader::new(&root, quick()).unwrap();
        assert_eq!(code(new.lock().unwrap().log(&id(instance))), "unknown_instance", "{instance}");
        drop(nuc);
    }
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

/// A lock is never rewritten in place: a new one is written whole under a private name and
/// renamed over it. Killed between the two, a fresh ledger leaves the old lock as it was (same
/// file, same epoch), so no writer on any host starts a ledger, and `recover` killed there leaves
/// the lock it was repairing. Finished, the lock is a new file.
#[test]
fn a_crash_before_a_new_lock_is_renamed_in_leaves_the_old_one() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let parent = tmp.path().join(".broomva");
    let lock = lock_of(tmp.path());
    let kill_at_rename = |spec: &[&str]| {
        let pause = Pause::at("lock_written");
        let mut child = spawn_child_pausing(spec, Some(&pause));
        pause.reached();
        child.kill().unwrap();
        child.wait().unwrap();
    };
    let ino = |p: &std::path::Path| std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(p).unwrap());

    // recover, restoring a lock emptied beside a live root.
    let epoch = Ledger::open(&root, &host("mac"), Options::default()).unwrap().epoch().to_string();
    std::fs::write(&lock, "").unwrap();
    let emptied = ino(&lock);
    kill_at_rename(&["recover", root.to_str().unwrap()]);
    assert_eq!((std::fs::read_to_string(&lock).unwrap().as_str(), ino(&lock)), ("", emptied));
    assert_eq!(operator::recover(&root, &quick()).unwrap(), Recovered::RestoredLock { epoch: epoch.clone() });
    assert_ne!(ino(&lock), emptied, "the lock was rewritten in place");

    // A fresh ledger after the root was moved away by hand.
    let mut old = Ledger::open(&root, &host("mac"), quick()).unwrap();
    std::fs::rename(&root, parent.join("flows-moved")).unwrap();
    let before = ino(&lock);
    kill_at_rename(&["fresh", root.to_str().unwrap(), "nuc"]);
    assert_eq!((std::fs::read_to_string(&lock).unwrap().trim(), ino(&lock)), (epoch.as_str(), before));
    for name in ["mac", "nuc", "other"] {
        assert_eq!(code(Ledger::open(&root, &host(name), quick())), "root_moved", "{name}");
    }
    assert_eq!(code(operator::recover(&root, &quick())), "root_moved");
    assert_eq!(names(&parent), vec!["flows-moved", "flows.lock"], "debris left");
    let fresh = operator::create_fresh_ledger(&root, &host("nuc"), &quick()).unwrap();
    assert_ne!(ino(&lock), before, "the lock was rewritten in place");
    assert_eq!(std::fs::read_to_string(&lock).unwrap().trim(), fresh);
    assert_eq!(code(old.lock()), "epoch_mismatch");
    assert_eq!(names(&parent), vec!["flows", "flows-moved", "flows.lock"]);
}

/// An archive to another filesystem is refused before anything is written: a rename there would
/// fail after the marker. The test forces the device comparison for one path
/// (`hooks::other_device`).
#[test]
fn an_archive_to_another_filesystem_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    drop(Ledger::open(&root, &host("mac"), Options::default()).unwrap());
    let dest = tmp.path().join("archive");
    // An absent `dest` is judged by its parent; an empty one being reused (a mount point, say)
    // by itself.
    let parent = std::fs::canonicalize(tmp.path()).unwrap();
    for (other, existing) in [(parent.as_path(), false), (dest.as_path(), true)] {
        if existing {
            std::fs::create_dir(&dest).unwrap();
        }
        let before = snapshot(tmp.path());
        let spec = ["archive", root.to_str().unwrap(), dest.to_str().unwrap()];
        let child = spawn_child_with(&spec, &[(OTHER_DEVICE_ENV, other.to_str().unwrap())]);
        assert_eq!(child_result(child), "operator_refused", "existing {existing}");
        assert_eq!(snapshot(tmp.path()), before, "existing {existing}");
    }
    operator::archive(&root, &dest, &quick()).unwrap();
}

/// A writer that opened the root and is still waiting for its lock while the root is moved by
/// hand, or archived through a replaced lock file: once it gets the lock it sees the path no
/// longer names that directory and refuses `root_moved`, writing nothing.
#[test]
fn a_writer_queued_on_a_root_that_moves_refuses_root_moved() {
    for how in ["moved", "archived"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_in(tmp.path());
        let parent = tmp.path().join(".broomva");
        let dest = tmp.path().join("archive");
        let mut mac = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
        append_at_head(&mut mac, &id("fi_a"), event("Started", 0)).unwrap();
        let pause = Pause::at("root_opened");
        let child = spawn_child_pausing(&["append", root.to_str().unwrap(), "mac", "fi_a", "1", "5000"], Some(&pause));
        pause.reached();
        let moved = if how == "moved" {
            std::fs::rename(&root, parent.join("flows-moved")).unwrap();
            parent.join("flows-moved")
        } else {
            replace_with_copy(&lock_of(tmp.path()));
            operator::archive(&root, &dest, &quick()).unwrap();
            dest.join("flows")
        };
        pause.go();
        assert_eq!(child_result(child), "root_moved", "{how}");
        let log = std::fs::read_to_string(moved.join("instances/fi_a/events.jsonl")).unwrap();
        assert_eq!(log.lines().count(), 1, "{how}: the queued writer wrote");
    }
}

/// `archive` checks the lock file it locked is still the lock: one replaced between its open and
/// its lock is refused `lock_lost`, and nothing moves.
#[test]
fn an_archive_refuses_a_lock_replaced_while_opening() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let dest = tmp.path().join("archive");
    drop(Ledger::open(&root, &host("mac"), Options::default()).unwrap());
    let pause = Pause::at("lock_opened");
    let child = spawn_child_pausing(&["archive", root.to_str().unwrap(), dest.to_str().unwrap()], Some(&pause));
    pause.reached();
    replace_with_copy(&lock_of(tmp.path()));
    pause.go();
    assert_eq!(child_result(child), "lock_lost");
    assert!(root.exists() && !dest.exists());
}

/// `archive` moves the root by its path, so it re-checks the path still names the directory it
/// locked: a directory put in its place meanwhile is refused `root_moved` and left alone.
#[test]
fn an_archive_refuses_a_root_replaced_after_its_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let parent = tmp.path().join(".broomva");
    let dest = tmp.path().join("archive");
    drop(Ledger::open(&root, &host("mac"), Options::default()).unwrap());
    let pause = Pause::at("archive_locked");
    let child = spawn_child_pausing(&["archive", root.to_str().unwrap(), dest.to_str().unwrap()], Some(&pause));
    pause.reached();
    std::fs::rename(&root, parent.join("flows-real")).unwrap();
    std::fs::create_dir(&root).unwrap();
    pause.go();
    assert_eq!(child_result(child), "root_moved");
    assert!(names(&root).is_empty() && !dest.exists());
    assert_eq!(names(&parent.join("flows-real")), vec!["EPOCH", "HOST", "instances", "retired"]);
}

/// A fresh ledger killed after its root went in and before its archive marker went: the marker
/// sits beside a whole ledger, and `recover` removes it.
#[test]
fn recover_removes_a_marker_beside_a_whole_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let parent = tmp.path().join(".broomva");
    drop(Ledger::open(&root, &host("mac"), Options::default()).unwrap());
    operator::archive(&root, &tmp.path().join("archive"), &quick()).unwrap();
    let pause = Pause::at("root_in_place");
    let mut child = spawn_child_pausing(&["fresh", root.to_str().unwrap(), "nuc"], Some(&pause));
    pause.reached();
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(names(&parent), vec!["flows", "flows.archived", "flows.lock"]);
    assert_eq!(operator::recover(&root, &quick()).unwrap(), Recovered::RemovedDebris { count: 1 });
    assert_eq!(names(&parent), vec!["flows", "flows.lock"]);
    Ledger::open(&root, &host("nuc"), quick()).unwrap();
}

/// SQLite opens the cache by path after the root check, and its journal by path when it writes: a
/// root moved in between fails the write, reported as a skipped cache, never an updated one. The
/// log wins either way.
#[test]
fn a_cache_written_while_the_root_moved_is_reported_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let parent = tmp.path().join(".broomva");
    drop(Ledger::open(&root, &host("mac"), Options::default()).unwrap());
    let pause = Pause::at("projection_open");
    let child = spawn_child_pausing(&["append-cache", root.to_str().unwrap(), "mac", "fi_a"], Some(&pause));
    pause.reached();
    std::fs::rename(&root, parent.join("flows-moved")).unwrap();
    pause.go();
    assert_eq!(child_result(child), "cache_failed");
    assert!(parent.join("flows-moved/instances/fi_a/events.jsonl").exists());
}

/// A genesis paused between seeing an empty tree and linking its lock, while another writer makes
/// a ledger there and the operator archives it, or a root is put in place with no lock: once
/// linked it re-checks, takes its lock back and refuses `root_moved`, creating nothing.
#[test]
fn a_genesis_racing_an_archive_creates_nothing() {
    for case in ["archived", "root"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_in(tmp.path());
        let parent = tmp.path().join(".broomva");
        let go = tmp.path().join("go");
        std::fs::write(&go, "").unwrap();
        let pause = Pause::at("before_genesis_link");
        let spec = ["genesis", root.to_str().unwrap(), "late", go.to_str().unwrap()];
        let child = spawn_child_pausing(&spec, Some(&pause));
        pause.reached();
        drop(Ledger::open(&root, &host("winner"), Options::default()).unwrap());
        if case == "archived" {
            operator::archive(&root, &tmp.path().join("archive"), &quick()).unwrap();
        } else {
            std::fs::remove_file(lock_of(tmp.path())).unwrap();
        }
        pause.go();
        assert_eq!(child_result(child), "root_moved", "{case}");
        let left = if case == "archived" { vec!["flows.archived"] } else { vec!["flows"] };
        assert_eq!(names(&parent), left, "{case}");
        if case == "root" {
            assert_eq!(std::fs::read_to_string(root.join("HOST")).unwrap().trim(), "winner");
        }
    }
}

/// Two recoveries of a deleted lock: the one that waited on the root directory's lock finds the
/// other already restored it, and changes nothing.
#[test]
fn a_recovery_that_waited_finds_the_lock_restored() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let lock = lock_of(tmp.path());
    let epoch = Ledger::open(&root, &host("mac"), Options::default()).unwrap().epoch().to_string();
    std::fs::remove_file(&lock).unwrap();
    let pause = Pause::at("root_opened");
    let child = spawn_child_pausing(&["recover", root.to_str().unwrap()], Some(&pause));
    pause.reached();
    assert_eq!(operator::recover(&root, &quick()).unwrap(), Recovered::RestoredLock { epoch: epoch.clone() });
    let restored = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&lock).unwrap());
    pause.go();
    assert_eq!(child_result(child), "operator_refused");
    assert_eq!(std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&lock).unwrap()), restored);
}
