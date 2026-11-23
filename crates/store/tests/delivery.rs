use maestro_store::delivery::Stage;
use maestro_store::{Paths, Store, TreeSource};
use serde_json::{Value, json};

fn machine() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::under(dir.path());
    std::fs::create_dir_all(&paths.owner_dir).unwrap();
    let store = Store::new(paths, TreeSource::WorkingTree);
    (dir, store)
}

fn receipt(id: &str, stage: &str) -> Value {
    json!({"id":id,"intent":{"verb":"ask.answer","ledger":"demo.yaml","handle":"ask","hash":"entry-hash","option":"yes","note":"private note"},"stage":stage,"signed_at":"2026-11-23T12:00:00Z","commit":null,"woke":null,"landing":null,"error":null})
}

fn write(store: &Store, rows: &[Value]) {
    let text = rows.iter().map(Value::to_string).collect::<Vec<_>>().join("\n") + "\n";
    std::fs::write(store.paths.owner_dir.join("receipts.jsonl"), text).unwrap();
}

#[test]
fn a_missing_receipt_file_is_unavailable_not_empty() {
    let (_dir, store) = machine();
    let read = store.answer_deliveries();
    assert!(read.data.is_none());
    assert!(!read.errors.is_empty());
    assert!(read.source.ends_with("receipts.jsonl"));
}

#[test]
fn latest_record_replaces_prior_stage_and_delivery_failure() {
    let (_dir, store) = machine();
    let id = "abcdefghijklmnop";
    let mut written = receipt(id, "written");
    written["commit"] = json!("abcdef0123456789");
    written["woke"] = json!("T3 did not take it (exit Some(1)): unavailable");
    let mut notified = written.clone();
    notified["woke"] = json!("told T3 thread test-thread");
    write(&store, &[receipt(id, "signed"), receipt(id, "applying"), written, notified]);
    let read = store.answer_deliveries();
    assert!(read.errors.is_empty());
    let rows = read.data.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].stage, Stage::Written);
    assert!(rows[0].session_notified());
}

#[test]
fn a_damaged_latest_stage_invalidates_the_earlier_success_for_that_id() {
    let (_dir, store) = machine();
    write(&store, &[receipt("abcdefghijklmnop", "written"), receipt("abcdefghijklmnop", "invented-success")]);
    let read = store.answer_deliveries();
    assert!(!read.errors.is_empty());
    assert!(read.data.unwrap().is_empty());
}

#[test]
fn bad_timestamps_and_unknown_malformed_rows_keep_the_read_partial() {
    let (_dir, store) = machine();
    let mut bad = receipt("badtimeabcdefghi", "written");
    bad["signed_at"] = json!("yesterday");
    write(&store, &[receipt("abcdefghijklmnop", "undone"), bad]);
    use std::io::Write;
    writeln!(
        std::fs::OpenOptions::new().append(true).open(store.paths.owner_dir.join("receipts.jsonl")).unwrap(),
        "{{truncated"
    )
    .unwrap();
    let read = store.answer_deliveries();
    assert_eq!(read.errors.len(), 2);
    assert_eq!(read.data.unwrap().len(), 1);
}

#[test]
fn only_answer_receipts_with_a_valid_receipt_identity_are_accepted() {
    let (_dir, store) = machine();
    let invalid_id = receipt("short", "written");
    let mut wrong_verb = receipt("abcdefghijklmnop", "written");
    wrong_verb["intent"]["verb"] = json!("work.complete");
    write(&store, &[invalid_id, wrong_verb]);
    let read = store.answer_deliveries();
    assert_eq!(read.errors.len(), 2);
    assert!(read.data.unwrap().is_empty());
}

#[test]
fn notification_requires_the_exact_success_record_and_written_stage() {
    let (_dir, store) = machine();
    for (stage, woke) in [
        ("signed", "told T3 thread thread"),
        ("written", "told T3 thread "),
        ("written", "told T3 thread thread extra"),
        ("written", "T3 did not run: denied"),
    ] {
        let mut row = receipt("abcdefghijklmnop", stage);
        row["woke"] = json!(woke);
        write(&store, &[row]);
        assert!(!store.answer_deliveries().data.unwrap()[0].session_notified(), "{stage}: {woke}");
    }
}

#[test]
fn invalid_utf8_in_a_retained_record_is_unavailable() {
    let (_dir, store) = machine();
    std::fs::write(store.paths.owner_dir.join("receipts.jsonl"), [b'{', 0xff, b'}', b'\n']).unwrap();
    let read = store.answer_deliveries();
    assert!(read.data.is_none());
    assert!(read.errors.iter().any(|e| e.reason.contains("UTF-8")));
}

#[test]
fn a_bounded_tail_marks_older_history_unread_and_keeps_complete_late_rows() {
    let (_dir, store) = machine();
    let row = receipt("abcdefghijklmnop", "signed").to_string();
    let late = receipt("ponmlkjihgfedcba", "written").to_string();
    let text = format!("{}{}\n", format!("{row}\n").repeat(1200), late);
    assert!(text.len() > 256 * 1024);
    std::fs::write(store.paths.owner_dir.join("receipts.jsonl"), text).unwrap();
    let read = store.answer_deliveries();
    assert!(read.source.contains("256 KiB tail"));
    assert!(read.errors.iter().any(|e| e.reason.contains("Earlier receipt history")));
    let rows = read.data.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, "ponmlkjihgfedcba");
}

#[test]
fn an_empty_existing_log_is_a_known_empty_read() {
    let (_dir, store) = machine();
    write(&store, &[]);
    let read = store.answer_deliveries();
    assert!(read.errors.is_empty());
    assert!(read.data.unwrap().is_empty());
}
