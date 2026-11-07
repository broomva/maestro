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

    // Whole but failing its hash.
    let whole = std::fs::read(&path).unwrap();
    let bad = serde_json::json!({"seq": 5, "prev_event": "sha256:00", "at": "2026-11-07T00:00:00Z", "kind": "Tick", "body": {}});
    append_raw(&path, &format!("{bad}\n"));
    assert_eq!(code(reader.lock().unwrap().log(&fi)), "torn_tail");
    assert_eq!(code(ledger.lock().unwrap().append(&fi, 4, event("Tick", 4))), "torn_tail");
    operator::truncate_torn_tail(&mut ledger, &fi).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), whole);
    assert_eq!(ledger.lock().unwrap().append(&fi, 4, event("Tick", 4)).unwrap().event.seq, 5);
    assert_eq!(reader.lock().unwrap().log(&fi).unwrap().seq(), 5);
    assert_eq!(code(operator::truncate_torn_tail(&mut ledger, &fi)), "operator_refused");
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
    assert!(after.instances.iter().any(|(i, _, _)| i == "fi_b"), "{after:?}");
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
