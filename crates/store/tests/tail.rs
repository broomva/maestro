//! The ticks ledger is read by its tail, as the plugin reads it (`readTail`, `STREAM_TAIL`).

use maestro_core::tick::{LedgerEvent, pair_ticks};
use maestro_store::{Paths, STREAM_TAIL, Store, TreeSource, read_tail};
use serde::Deserialize;
use std::path::Path;

#[derive(Deserialize)]
struct Case {
    text: String,
    limit: usize,
    tail: String,
    from: u64,
}

/// `fixtures/parity/tail-golden.json` is the plugin's own cut, run under bun (`scripts/tail-golden.ts`).
#[test]
fn the_tail_is_cut_where_the_plugin_cuts_it() {
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/parity/tail-golden.json");
    let cases: Vec<Case> = serde_json::from_str(&std::fs::read_to_string(golden).unwrap()).unwrap();
    assert!(cases.len() > 400);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("stream.jsonl");
    for c in &cases {
        std::fs::write(&file, &c.text).unwrap();
        let (tail, from) = read_tail(&file, c.limit).unwrap();
        assert_eq!((tail.as_str(), from), (c.tail.as_str(), c.from), "{:?} at limit {}", c.text, c.limit);
    }
}

fn line(id: &str, kind: &str, subject: &str, cause: Option<&str>, ts: &str) -> String {
    serde_json::json!({
        "v": 1, "id": id, "ts": ts, "stream": "loops/ticks", "type": kind, "actor": "agent:aaaa1111",
        "subject": subject, "refs": {}, "cause": cause, "data": {"outcome": "ok"}, "prev": "sha256:0",
    })
    .to_string()
        + "\n"
}

/// A tick that started before the tail is not read, though its end is inside it; a tick wholly
/// inside is. The line the cut falls in is dropped, not reported as torn.
#[test]
fn a_tick_that_started_before_the_tail_is_not_read() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::under(root.path());
    let ledger = paths.ticks_ledger();
    std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    let mut text = line("OLD", "loop.tick.started", "loop:boundary", None, "2026-10-01T00:00:00Z");
    let mut n = 0;
    while text.len() < 2 * STREAM_TAIL {
        n += 1;
        text += &line(&format!("F{n}"), "loop.tick.started", "loop:filler", None, "2026-10-01T00:01:00Z");
    }
    text += &line("OLD-END", "loop.tick.ended", "loop:boundary", Some("OLD"), "2026-10-01T09:00:00Z");
    text += &line("NEW", "loop.tick.started", "loop:boundary", None, "2026-10-01T10:00:00Z");
    text += &line("NEW-END", "loop.tick.ended", "loop:boundary", Some("NEW"), "2026-10-01T10:05:00Z");
    std::fs::write(&ledger, &text).unwrap();

    let read = Store::new(paths, TreeSource::WorkingTree).ticks();
    assert!(read.errors.is_empty(), "{:?}", read.errors);
    assert!(read.source.contains(" from byte "), "{}", read.source);
    let ticks = read.data.unwrap();
    let boundary: Vec<_> = ticks.iter().filter(|t| t.loop_id == "boundary").map(|t| t.start_id.as_str()).collect();
    assert_eq!(boundary, ["NEW"]);

    // The full read sees OLD; the tail holds exactly the full read's ticks that start inside it.
    let events: Vec<LedgerEvent> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let full = pair_ticks(&events);
    assert!(full.iter().any(|t| t.start_id == "OLD" && t.is_closed()));
    let (tail, _) = read_tail(&ledger, STREAM_TAIL).unwrap();
    let ids: std::collections::HashSet<String> =
        tail.lines().map(|l| serde_json::from_str::<LedgerEvent>(l).unwrap().id).collect();
    let inside: Vec<_> = full.iter().filter(|t| ids.contains(&t.start_id)).collect();
    assert_eq!(ticks.len(), inside.len());
    for (a, b) in ticks.iter().zip(inside) {
        assert_eq!(serde_json::to_value(a).unwrap(), serde_json::to_value(b).unwrap());
    }
}

/// At or under the limit the whole ledger is read and line numbers are the file's.
#[test]
fn a_short_ledger_is_read_whole() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::under(root.path());
    let ledger = paths.ticks_ledger();
    std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    let text = line("A", "loop.tick.started", "loop:x", None, "2026-10-01T00:00:00Z") + "torn{\n";
    std::fs::write(&ledger, &text).unwrap();
    let read = Store::new(paths, TreeSource::WorkingTree).ticks();
    assert_eq!(read.source, format!("fs:{}", ledger.display()));
    assert_eq!(read.data.unwrap().len(), 1);
    let at: Vec<_> = read.errors.iter().map(|e| e.at.clone()).collect();
    assert_eq!(at, [format!("fs:{}:2", ledger.display())]);
}
