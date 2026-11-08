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
    assert_eq!(code(operator::recover(&root, &quick())), "root_moved");
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

/// A lock file replaced while a guard holds the old one: the root directory's own lock keeps a
/// second writer out, the guard's writes still go to the root it holds, and the log never forks.
/// Its next `lock()` refuses `lock_lost`.
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
    assert_eq!(guard.append(&fi, 1, event("Claimed", 1)).unwrap().event.seq, 2);
    drop(guard);

    assert_eq!(second.lock().unwrap().append(&fi, 2, event("Claimed", 2)).unwrap().event.seq, 3);
    assert_eq!(code(first.lock()), "lock_lost");
    let log = Reader::new(&root, quick()).unwrap().lock().unwrap().log(&fi).unwrap();
    assert_eq!(log.seq(), 3);
}

/// A guard writes through the root directory it verified. With the root moved away, or replaced
/// by a copy or a link, while the guard is held, its writes land in the root it holds and nothing
/// appears at the path; the next `lock()` refuses `root_moved`, and a linked root is refused
/// `symlink` when opened.
#[test]
fn a_held_guard_writes_only_to_the_root_it_verified() {
    for swap in ["moved", "copy", "link"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_in(tmp.path());
        let parent = tmp.path().join(".broomva");
        let held = parent.join("flows-held");
        let mut ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
        append_at_head(&mut ledger, &id("fi_a"), event("Started", 0)).unwrap();

        let mut guard = ledger.lock().unwrap();
        std::fs::rename(&root, &held).unwrap();
        match swap {
            "copy" => {
                for dir in ["instances", "retired"] {
                    std::fs::create_dir_all(root.join(dir)).unwrap();
                }
                for f in ["HOST", "EPOCH"] {
                    std::fs::copy(held.join(f), root.join(f)).unwrap();
                }
            }
            "link" => std::os::unix::fs::symlink(&held, &root).unwrap(),
            _ => {}
        }
        let at_path = if swap == "copy" { snapshot(&root) } else { Default::default() };
        assert_eq!(guard.append(&id("fi_a"), 1, event("Tick", 1)).unwrap().event.seq, 2, "{swap}");
        assert_eq!(guard.append(&id("fi_new"), 0, event("Started", 0)).unwrap().event.seq, 1, "{swap}");
        drop(guard);
        assert_eq!(names(&held.join("instances")), vec!["fi_a", "fi_new"], "{swap}");
        match swap {
            "moved" => assert_eq!(names(&parent), vec!["flows-held", "flows.lock"]),
            "copy" => assert_eq!(snapshot(&root), at_path, "the copy was written"),
            _ => {
                assert_eq!(code(Ledger::open(&root, &host("mac"), quick())), "symlink");
                assert_eq!(code(Reader::new(&root, quick()).unwrap().lock()), "symlink");
            }
        }
        assert_eq!(code(ledger.lock()), if swap == "link" { "symlink" } else { "root_moved" }, "{swap}");
    }
}

/// With the lock deleted beside a live root, a fresh ledger is refused before it touches
/// anything, so the root keeps its epoch and `recover` still restores it.
#[test]
fn a_fresh_ledger_is_refused_beside_a_live_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let lock = lock_of(tmp.path());
    let epoch = Ledger::open(&root, &host("mac"), Options::default()).unwrap().epoch().to_string();
    std::fs::remove_file(&lock).unwrap();
    let before = snapshot(tmp.path());
    assert_eq!(code(operator::create_fresh_ledger(&root, &host("nuc"), &quick())), "operator_refused");
    assert_eq!(snapshot(tmp.path()), before, "a refused fresh ledger changed the tree");
    assert_eq!(operator::recover(&root, &quick()).unwrap(), Recovered::RestoredLock { epoch: epoch.clone() });
    assert_eq!(Ledger::open(&root, &host("mac"), quick()).unwrap().epoch(), epoch);
}

/// An archive that cannot make its destination writes nothing: no marker beside a live ledger.
#[cfg(unix)]
#[test]
fn a_failed_archive_leaves_no_marker() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let parent = tmp.path().join(".broomva");
    drop(Ledger::open(&root, &host("mac"), Options::default()).unwrap());
    let shelf = tmp.path().join("shelf");
    std::fs::create_dir(&shelf).unwrap();
    std::fs::set_permissions(&shelf, std::fs::Permissions::from_mode(0o555)).unwrap();
    // Root makes directories anywhere; the case only means something where permissions hold.
    let refused = std::fs::create_dir(shelf.join("probe")).is_err();
    let result = code(operator::archive(&root, &shelf.join("archive"), &quick()));
    std::fs::set_permissions(&shelf, std::fs::Permissions::from_mode(0o755)).unwrap();
    if refused {
        assert_eq!(result, "io");
        assert_eq!(names(&parent), vec!["flows", "flows.lock"]);
        Ledger::open(&root, &host("mac"), quick()).unwrap();
    }
}

/// `recover` removes what crashed geneses and recoveries leave beside the root, except a private
/// lock file a process still holds (a genesis in flight).
#[test]
fn recover_removes_crash_debris_but_not_a_genesis_in_flight() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let parent = tmp.path().join(".broomva");
    drop(Ledger::open(&root, &host("mac"), Options::default()).unwrap());
    let e = "0123456789abcdef0123456789abcdef";
    std::fs::create_dir_all(parent.join(format!(".flows.genesis-{e}/instances"))).unwrap();
    std::fs::write(parent.join(format!(".flows.lock.genesis-{e}")), format!("{e}\n")).unwrap();
    std::fs::write(parent.join(format!(".flows.lock.new-{e}")), format!("{e}\n")).unwrap();
    // A second name for the live lock, left by a crash between linking and unlinking it.
    std::fs::hard_link(lock_of(tmp.path()), parent.join(".flows.lock.genesis-1111111111111111111111111111111a"))
        .unwrap();
    let held = parent.join(".flows.lock.genesis-ffffffffffffffffffffffffffffffff");
    std::fs::write(&held, "").unwrap();
    let holder = std::fs::File::open(&held).unwrap();
    holder.lock().unwrap();
    // Not the shapes a crash leaves: an operator's own directory, a file under a stage name, a
    // directory under a private lock name, and a name that is not an epoch.
    let kept = [
        ".flows.genesis-old",
        ".flows.genesis-3333333333333333333333333333333c",
        ".flows.lock.new-2222222222222222222222222222222b",
        ".flows.lock.new-zz",
    ];
    std::fs::create_dir(parent.join(kept[0])).unwrap();
    std::fs::write(parent.join(kept[1]), "").unwrap();
    std::fs::create_dir(parent.join(kept[2])).unwrap();
    std::fs::write(parent.join(kept[3]), "").unwrap();
    assert_eq!(operator::recover(&root, &quick()).unwrap(), Recovered::RemovedDebris { count: 4 });
    let mut left = kept.iter().map(|k| k.to_string()).collect::<Vec<_>>();
    left.extend([
        ".flows.lock.genesis-ffffffffffffffffffffffffffffffff".to_string(),
        "flows".into(),
        "flows.lock".into(),
    ]);
    left.sort();
    assert_eq!(names(&parent), left);
    assert_eq!(code(operator::recover(&root, &quick())), "operator_refused");
    Ledger::open(&root, &host("mac"), quick()).unwrap();
    drop(holder);
}

/// The `EPOCH` a root carries is 32 hex digits; anything else is a damaged root.
#[test]
fn a_root_whose_epoch_is_not_32_hex_is_damaged() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    drop(Ledger::open(&root, &host("mac"), Options::default()).unwrap());
    std::fs::write(root.join("EPOCH"), "0123456789abcdef\n").unwrap();
    std::fs::write(lock_of(tmp.path()), "0123456789abcdef\n").unwrap();
    assert_eq!(code(Ledger::open(&root, &host("mac"), quick())), "ledger_damaged");
    assert_eq!(code(Reader::new(&root, quick()).unwrap().lock()), "ledger_damaged");
}

/// `archive` takes the root directory's own lock, as a writer does: with a guard held through a
/// lock file replaced since, it waits and refuses `busy` rather than move the root from under it.
#[test]
fn an_archive_waits_for_a_guard_held_through_a_replaced_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let mut ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    let guard = ledger.lock().unwrap();
    replace_with_copy(&lock_of(tmp.path()));
    let before = snapshot(tmp.path());
    assert_eq!(code(operator::archive(&root, &tmp.path().join("archive"), &quick())), "busy");
    assert_eq!(snapshot(tmp.path()), before, "a refused archive changed the tree");
    drop(guard);
}

/// A fresh ledger that crashed after its lock named its stage: `recover` moves the stage in and
/// removes the archive marker, so the finished ledger has none beside it.
#[test]
fn recovering_a_fresh_ledger_removes_the_archive_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let parent = tmp.path().join(".broomva");
    drop(Ledger::open(&root, &host("mac"), Options::default()).unwrap());
    operator::archive(&root, &tmp.path().join("archive"), &quick()).unwrap();
    let e = "0123456789abcdef0123456789abcdef";
    let stage = parent.join(format!(".flows.genesis-{e}"));
    for dir in ["instances", "retired"] {
        std::fs::create_dir_all(stage.join(dir)).unwrap();
    }
    std::fs::write(stage.join("HOST"), "nuc\n").unwrap();
    std::fs::write(stage.join("EPOCH"), format!("{e}\n")).unwrap();
    std::fs::write(lock_of(tmp.path()), format!("{e}\n")).unwrap();
    assert_eq!(code(Ledger::open(&root, &host("nuc"), quick())), "genesis_incomplete");
    assert_eq!(operator::recover(&root, &quick()).unwrap(), Recovered::FinishedGenesis { epoch: e.into() });
    assert_eq!(names(&parent), vec!["flows", "flows.lock"]);
    assert_eq!(Ledger::open(&root, &host("nuc"), quick()).unwrap().epoch(), e);
}

/// Only real directories under `instances/` are instances: a stray file is not listed.
#[test]
fn a_stray_file_under_instances_is_not_an_instance() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let mut ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    append_at_head(&mut ledger, &id("fi_a"), event("Started", 0)).unwrap();
    std::fs::write(root.join("instances/notes"), "").unwrap();
    assert_eq!(ledger.lock().unwrap().instances().unwrap(), vec![id("fi_a")]);
    assert_eq!(Reader::new(&root, quick()).unwrap().lock().unwrap().instances().unwrap(), vec![id("fi_a")]);
}
