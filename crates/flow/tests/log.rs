//! BRO-3624 item 8: reads, the log and the projection.

mod common;

use common::*;
use maestro_flow::operator;
use maestro_flow::{CacheState, FlowError, InstanceId, Ledger, Options, Reader};
use sha2::{Digest, Sha256};
use std::io::Write as _;

fn sha(line: &str) -> String {
    format!("sha256:{}", Sha256::digest(line.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>())
}

fn append_raw(path: &std::path::Path, bytes: &str) {
    std::fs::OpenOptions::new().append(true).open(path).unwrap().write_all(bytes.as_bytes()).unwrap();
}

#[test]
fn every_line_is_numbered_and_chained_to_the_one_before() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let fi = id("fi_a");
    let mut ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    for n in 0..4 {
        append_at_head(&mut ledger, &fi, event("Tick", n)).unwrap();
    }
    let text = std::fs::read_to_string(root.join("instances/fi_a/events.jsonl")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4);
    for (i, line) in lines.iter().enumerate() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["seq"], i as u64 + 1);
        assert_eq!(v["kind"], "Tick");
        assert_eq!(v["body"]["n"], i as u64);
        let want = if i == 0 { serde_json::Value::Null } else { sha(lines[i - 1]).into() };
        assert_eq!(v["prev_event"], want, "line {}", i + 1);
    }

    // Line 2 edited by hand: line 3 no longer verifies, and with line 4 after it that is
    // corruption, not a torn tail.
    let tampered = text.replacen("\"n\":1", "\"n\":9", 1);
    std::fs::write(root.join("instances/fi_a/events.jsonl"), tampered).unwrap();
    let refused = ledger.lock().unwrap().log(&fi);
    assert!(matches!(refused, Err(FlowError::ChainBroken { line: 3, .. })), "{refused:?}");
    assert_eq!(code(operator::truncate_torn_tail(&mut ledger, &fi)), "chain_broken");
}

/// Reads take the shared lock and write nothing: no file, no directory entry, no cache.
#[test]
fn a_read_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let parent = tmp.path().join(".broomva");
    let mut ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    for n in 0..3 {
        append_at_head(&mut ledger, &id("fi_a"), event("Tick", n)).unwrap();
        append_at_head(&mut ledger, &id("fi_b"), event("Started", n)).unwrap();
    }
    append_raw(&root.join("instances/fi_b/events.jsonl"), "{\"seq\":4,");
    drop(ledger);
    let read_everything = || {
        let reader = Reader::new(&root, Options::default()).unwrap();
        let guard = reader.lock().unwrap();
        assert_eq!(guard.instances().unwrap(), vec![id("fi_a"), id("fi_b")]);
        assert_eq!(guard.log(&id("fi_a")).unwrap().seq(), 3);
        assert_eq!(code(guard.log(&id("fi_b"))), "torn_tail");
        assert_eq!(code(guard.summary(&id("fi_b"))), "torn_tail");
        assert_eq!(code(guard.log(&id("fi_none"))), "unknown_instance");
        let summary = guard.summary(&id("fi_a")).unwrap();
        guard.projection_dump().unwrap();
        summary.cache
    };

    let before = snapshot(&parent);
    assert!(root.join("projection.sqlite").exists());
    assert_eq!(read_everything(), CacheState::Agrees);
    assert_eq!(snapshot(&parent), before);

    // With the cache gone, a read reports it missing and does not make one.
    std::fs::remove_file(root.join("projection.sqlite")).unwrap();
    let before = snapshot(&parent);
    assert_eq!(read_everything(), CacheState::Missing);
    assert_eq!(snapshot(&parent), before);
    assert!(!root.join("projection.sqlite").exists());

    // A read of an empty tree is no ledger, and creates none.
    let empty = tempfile::tempdir().unwrap();
    let reader = Reader::new(&root_in(empty.path()), Options::default()).unwrap();
    assert_eq!(code(reader.lock()), "no_ledger");
    assert!(names(empty.path()).is_empty());
}

#[test]
fn reads_take_a_shared_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let mut ledger = Ledger::open(&root, &host("mac"), quick()).unwrap();
    let reader = Reader::new(&root, quick()).unwrap();

    let writing = ledger.lock().unwrap();
    assert_eq!(code(reader.lock()), "busy", "a read went ahead under a writer");
    drop(writing);

    let one = reader.lock().unwrap();
    let two = reader.lock().unwrap();
    assert_eq!(code(ledger.lock()), "busy", "a write went ahead under readers");
    drop((one, two));
    drop(ledger.lock().unwrap());
}

#[test]
fn a_torn_tail_reads_as_unread_and_blocks_appends_until_truncated() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let fi = id("fi_a");
    let path = root.join("instances/fi_a/events.jsonl");
    let mut ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    for n in 0..3 {
        append_at_head(&mut ledger, &fi, event("Tick", n)).unwrap();
    }
    let whole = std::fs::read(&path).unwrap();
    let reader = Reader::new(&root, Options::default()).unwrap();

    // Cut short by a crash.
    append_raw(&path, "{\"seq\":4,\"prev_ev");
    let read = reader.lock().unwrap().log(&fi);
    assert!(matches!(read, Err(FlowError::TornTail { verified_seq: 3, .. })), "{read:?}");
    assert_eq!(code(ledger.lock().unwrap().append(&fi, 3, event("Tick", 3))), "torn_tail");
    assert_eq!(operator::truncate_torn_tail(&mut ledger, &fi).unwrap(), 17);
    assert_eq!(std::fs::read(&path).unwrap(), whole);
    assert_eq!(ledger.lock().unwrap().append(&fi, 3, event("Tick", 3)).unwrap().event.seq, 4);

    // A whole last line, newline and all, is never what a crash leaves: whether it does not chain
    // or does not parse, it is a broken chain, which truncation refuses rather than "repair".
    let whole = std::fs::read(&path).unwrap();
    let unchained = serde_json::json!({"seq": 5, "prev_event": "sha256:00", "at": "2026-11-07T00:00:00Z", "kind": "Tick", "body": {}});
    for bad in [format!("{unchained}\n"), "{\"seq\":5,garbage\n".to_string()] {
        append_raw(&path, &bad);
        let damaged = std::fs::read(&path).unwrap();
        assert_eq!(code(reader.lock().unwrap().log(&fi)), "chain_broken", "{bad}");
        assert_eq!(code(ledger.lock().unwrap().append(&fi, 4, event("Tick", 4))), "chain_broken", "{bad}");
        assert_eq!(code(operator::truncate_torn_tail(&mut ledger, &fi)), "chain_broken", "{bad}");
        assert_eq!(std::fs::read(&path).unwrap(), damaged, "{bad}");
        std::fs::write(&path, &whole).unwrap();
    }
    assert_eq!(ledger.lock().unwrap().append(&fi, 4, event("Tick", 4)).unwrap().event.seq, 5);
    assert_eq!(reader.lock().unwrap().log(&fi).unwrap().seq(), 5);
    assert_eq!(code(operator::truncate_torn_tail(&mut ledger, &fi)), "operator_refused");

    // A corrupt line before a good last one: broken, and truncation keeps both.
    let text = std::fs::read_to_string(&path).unwrap();
    let corrupt = text.replacen("{\"seq\":4", "{\"seq\":4,", 1);
    std::fs::write(&path, &corrupt).unwrap();
    assert_eq!(code(reader.lock().unwrap().log(&fi)), "chain_broken");
    assert_eq!(code(operator::truncate_torn_tail(&mut ledger, &fi)), "chain_broken");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), corrupt);
}

/// The stored `seq` is checked, not only counted: a line numbered out of turn, chained correctly,
/// breaks the log.
#[test]
fn a_line_numbered_out_of_turn_breaks_the_chain() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    drop(Ledger::open(&root, &host("mac"), Options::default()).unwrap());
    let line = |seq: u64, prev: Option<String>| {
        serde_json::json!({"seq": seq, "prev_event": prev, "at": "2026-11-07T00:00:00Z", "kind": "Tick", "body": {}})
            .to_string()
    };
    let l1 = line(1, None);
    let l2 = line(5, Some(sha(&l1)));
    let l3 = line(3, Some(sha(&l2)));
    std::fs::create_dir(root.join("instances/fi_seq")).unwrap();
    std::fs::write(root.join("instances/fi_seq/events.jsonl"), format!("{l1}\n{l2}\n{l3}\n")).unwrap();
    let read = Reader::new(&root, quick()).unwrap().lock().unwrap().log(&id("fi_seq"));
    assert!(matches!(read, Err(FlowError::ChainBroken { line: 2, .. })), "{read:?}");
}

/// No path under the root is followed out of it: a linked instance directory, a linked log and
/// a linked `instances/` are each refused, and nothing outside the root is written or made.
#[cfg(unix)]
#[test]
fn symbolic_links_under_the_root_are_refused() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("events.jsonl"), "x").unwrap();
    let mut ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    append_at_head(&mut ledger, &id("fi_a"), event("Started", 0)).unwrap();
    let instances = root.join("instances");

    symlink(&outside, instances.join("fi_dir")).unwrap();
    std::fs::create_dir(instances.join("fi_file")).unwrap();
    symlink(outside.join("events.jsonl"), instances.join("fi_file/events.jsonl")).unwrap();
    std::fs::create_dir(instances.join("fi_dangling")).unwrap();
    symlink(outside.join("new.jsonl"), instances.join("fi_dangling/events.jsonl")).unwrap();
    let before = snapshot(&outside);
    {
        let mut guard = ledger.lock().unwrap();
        for (name, seq) in [("fi_dir", 0), ("fi_file", 0), ("fi_file", 1), ("fi_dangling", 0)] {
            assert_eq!(code(guard.append(&id(name), seq, event("Started", 0))), "symlink", "{name} at {seq}");
        }
    }
    let reader = Reader::new(&root, quick()).unwrap();
    for name in ["fi_dir", "fi_file", "fi_dangling"] {
        assert_eq!(code(reader.lock().unwrap().log(&id(name))), "symlink", "{name}");
    }
    assert_eq!(snapshot(&outside), before, "a write went through a link");

    // `instances/` itself replaced by a link to a copy.
    std::fs::rename(&instances, root.join("instances-real")).unwrap();
    symlink(root.join("instances-real"), &instances).unwrap();
    assert_eq!(code(ledger.lock().unwrap().append(&id("fi_a"), 1, event("Tick", 1))), "symlink");
    assert_eq!(code(reader.lock().unwrap().log(&id("fi_a"))), "symlink");
}

/// The projection is never opened through a link, by a writer or a reader, and SQLite writes no
/// journal through one: a link to an outside file leaves that file as it was, and one to an
/// absent file makes nothing.
#[cfg(unix)]
#[test]
fn the_projection_is_never_opened_through_a_link() {
    use std::os::unix::fs::symlink;
    for name in ["projection.sqlite", "projection.sqlite-journal"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_in(tmp.path());
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let mut ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
        append_at_head(&mut ledger, &id("fi_a"), event("Started", 0)).unwrap();
        let db = root.join("projection.sqlite");
        std::fs::copy(&db, outside.join("existing")).unwrap();
        for target in ["existing", "absent"] {
            let link = root.join(name);
            let _ = std::fs::remove_file(&link);
            if name == "projection.sqlite" {
                std::fs::remove_file(&db).ok();
            }
            symlink(outside.join(target), &link).unwrap();
            let before = snapshot(&outside);
            let seq = ledger.lock().unwrap().log(&id("fi_a")).unwrap().seq();
            let appended = ledger.lock().unwrap().append(&id("fi_a"), seq, event("Tick", seq)).unwrap();
            assert!(matches!(appended.cache, maestro_flow::CacheWrite::Failed(_)), "{name} -> {target}");
            let guard = Reader::new(&root, quick()).unwrap().lock().unwrap();
            let cache = guard.summary(&id("fi_a")).unwrap().cache;
            if name == "projection.sqlite" {
                assert!(matches!(cache, CacheState::Unreadable(_)), "{name} -> {target}");
                assert!(guard.projection_dump().is_err(), "{name} -> {target}");
            }
            drop(guard);
            assert_eq!(snapshot(&outside), before, "{name} -> {target}: written through the link");
        }
    }
}

/// Every row of the projection is a function of its log alone, so the cache kept up write by
/// write equals a rebuild from scratch: after appends, a torn tail, an unreadable log, a retire
/// and a truncation. A rebuild never drops an instance it cannot read; it records it.
#[test]
fn the_incremental_projection_equals_a_rebuild() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let db = root.join("projection.sqlite");
    let mut ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    for (name, kind) in [("fi_a", "Started"), ("fi_a", "Claimed"), ("fi_b", "Started"), ("fi_c", "Started")] {
        append_at_head(&mut ledger, &id(name), event(kind, 0)).unwrap();
    }
    append_at_head(&mut ledger, &id("fi_d"), event("Started", 0)).unwrap();
    let reader = Reader::new(&root, Options::default()).unwrap();
    let dump = |reader: &Reader| reader.lock().unwrap().projection_dump().unwrap().unwrap();
    // Compares the cache as kept with a rebuild, then puts the kept one back.
    let same_as_rebuild = |ledger: &mut Ledger, step: &str| {
        let kept = dump(&reader);
        let saved = std::fs::read(&db).unwrap();
        std::fs::remove_file(&db).unwrap();
        operator::rebuild_projection(ledger).unwrap();
        assert_eq!(kept, dump(&reader), "after {step}");
        std::fs::write(&db, saved).unwrap();
        kept
    };
    same_as_rebuild(&mut ledger, "appends");

    append_raw(&root.join("instances/fi_b/events.jsonl"), "{\"seq\":2,");
    append_at_head(&mut ledger, &id("fi_a"), event("Tick", 1)).unwrap();
    let kept = same_as_rebuild(&mut ledger, "a torn tail");
    let row = kept.instances.iter().find(|r| r.0 == "fi_b").unwrap();
    assert_eq!((row.1.as_str(), row.2), ("torn_tail", 1));

    operator::retire(&mut ledger, &id("fi_c")).unwrap();
    let kept = same_as_rebuild(&mut ledger, "a retire");
    assert!(kept.instances.iter().all(|r| r.0 != "fi_c"));
    assert!(kept.kind_counts.iter().all(|r| r.0 != "fi_c"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let log = root.join("instances/fi_d/events.jsonl");
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads anything; the unreadable case only means something where permissions hold.
        if std::fs::read(&log).is_err() {
            append_at_head(&mut ledger, &id("fi_a"), event("Tick", 2)).unwrap();
            let kept = same_as_rebuild(&mut ledger, "an unreadable log");
            let row = kept.instances.iter().find(|r| r.0 == "fi_d").unwrap();
            assert_eq!((row.1.as_str(), row.2), ("unreadable", 0));
        }
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    operator::truncate_torn_tail(&mut ledger, &id("fi_b")).unwrap();
    let kept = same_as_rebuild(&mut ledger, "a truncation");
    let row = kept.instances.iter().find(|r| r.0 == "fi_b").unwrap();
    assert_eq!((row.1.as_str(), row.2), ("ok", 1));
}

#[test]
fn the_projection_replays_exactly_and_the_log_wins() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let db = root.join("projection.sqlite");
    let (a, b) = (id("fi_a"), id("fi_b"));
    let mut ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    for (fi, kind) in [(&a, "Started"), (&a, "Claimed"), (&a, "Claimed"), (&b, "Started"), (&a, "Completed")] {
        append_at_head(&mut ledger, fi, event(kind, 0)).unwrap();
    }
    let reader = Reader::new(&root, Options::default()).unwrap();
    let dump = || reader.lock().unwrap().projection_dump().unwrap().unwrap();
    let built = dump();
    assert_eq!(built.instances.len(), 2);
    assert!(built.kind_counts.contains(&("fi_a".into(), "Claimed".into(), 2)));

    // Deleted and replayed: the same rows.
    std::fs::remove_file(&db).unwrap();
    operator::rebuild_projection(&mut ledger).unwrap();
    assert_eq!(dump(), built);

    // Deleted, then the next append rebuilds it whole, not only the instance it wrote.
    std::fs::remove_file(&db).unwrap();
    append_at_head(&mut ledger, &a, event("Heartbeat", 0)).unwrap();
    let after = dump();
    assert!(after.instances.iter().any(|r| r.0 == "fi_b"), "{after:?}");
    operator::rebuild_projection(&mut ledger).unwrap();
    assert_eq!(dump(), after);

    // A cache that disagrees with the log loses, on read and on the next write.
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("UPDATE instances SET seq = 99 WHERE id = 'fi_a'", []).unwrap();
    conn.execute("UPDATE kind_counts SET n = 7 WHERE instance = 'fi_b'", []).unwrap();
    drop(conn);
    let guard = reader.lock().unwrap();
    let (sa, sb) = (guard.summary(&a).unwrap(), guard.summary(&b).unwrap());
    drop(guard);
    assert_eq!(sa.summary, ledger.lock().unwrap().log(&a).unwrap().summary());
    assert_eq!(sa.summary.seq, 5);
    assert!(matches!(sa.cache, CacheState::Disagrees(Some(ref c)) if c.seq == 99), "{:?}", sa.cache);
    assert_eq!(sb.summary.counts["Started"], 1);
    assert!(matches!(sb.cache, CacheState::Disagrees(_)));
    append_at_head(&mut ledger, &a, event("Heartbeat", 1)).unwrap();
    append_at_head(&mut ledger, &b, event("Heartbeat", 1)).unwrap();
    let guard = reader.lock().unwrap();
    assert_eq!(guard.summary(&a).unwrap().cache, CacheState::Agrees);
    assert_eq!(guard.summary(&b).unwrap().cache, CacheState::Agrees);
}

#[test]
fn names_cannot_leave_their_directory() {
    for bad in ["", "../fi", "a/b", ".hidden", "-x", "fi a", "fi\n", &"x".repeat(129)] {
        assert!(matches!(InstanceId::new(bad), Err(FlowError::Invalid { .. })), "{bad:?}");
    }
    assert!(InstanceId::new("fi_01J-abc").is_ok());
    let tmp = tempfile::tempdir().unwrap();
    let mut ledger = Ledger::open(&root_in(tmp.path()), &host("mac"), Options::default()).unwrap();
    let refused = ledger.lock().unwrap().append(&id("fi_a"), 0, event("../kind", 0));
    assert_eq!(code(refused), "invalid");
}

#[test]
fn an_event_line_or_a_log_over_its_bound_is_refused_and_never_read() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(tmp.path());
    let mut ledger = Ledger::open(&root, &host("mac"), Options::default()).unwrap();
    let body = serde_json::json!({ "s": "x".repeat(maestro_flow::log::MAX_LINE as usize) });
    let huge = maestro_flow::NewEvent::new("Started", body);
    assert_eq!(code(ledger.lock().unwrap().append(&id("fi_big"), 0, huge)), "too_large");
    assert!(ledger.lock().unwrap().instances().unwrap().is_empty(), "nothing was written");

    let fi = id("fi_a");
    append_at_head(&mut ledger, &fi, event("Started", 0)).unwrap();
    let log = root.join("instances/fi_a/events.jsonl");
    let reader = Reader::new(&root, Options::default()).unwrap();
    // At the bound the log is read (its zeros are a torn tail); a byte over, it is refused unread.
    let resize = |len: u64| std::fs::OpenOptions::new().write(true).open(&log).unwrap().set_len(len).unwrap();
    resize(maestro_flow::log::MAX_LOG);
    assert_eq!(code(reader.lock().unwrap().log(&fi)), "torn_tail");
    resize(maestro_flow::log::MAX_LOG + 1);
    assert_eq!(code(reader.lock().unwrap().log(&fi)), "too_large");
    assert_eq!(code(append_at_head(&mut ledger, &fi, event("Tick", 1))), "too_large");
}
