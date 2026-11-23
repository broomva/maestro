//! Owner context remains read-only and distinguishes observations from action outcomes.
use maestro_store::{Paths, Store, TreeSource};
use maestro_view::{render_web, validate};
use serde_json::json;
use std::path::Path;

fn machine() -> tempfile::TempDir {
    let m = tempfile::tempdir().unwrap();
    for dir in [
        "workspace/.control/asks",
        "workspace/.control/loops",
        "workspace/.control/initiatives",
        "work",
        "broomva-home/ledger/loops",
        "fleet-reconcile",
    ] {
        std::fs::create_dir_all(m.path().join(dir)).unwrap();
    }
    std::fs::write(m.path().join("broomva-home/ledger/loops/ticks.jsonl"), "").unwrap();
    m
}

fn render(root: &Path, kind: &str) -> String {
    let store = Store::new(Paths::under(root), TreeSource::WorkingTree);
    let spec = json!({"root":"page","blocks":{"page":{"type":kind}}});
    let v = validate(&spec).unwrap();
    assert!(v.drops.is_empty());
    render_web(&store, &v, "2026-10-23T12:00:00Z".parse().unwrap(), maestro_view::render::Owner::Off("test".into()))
        .0
        .html
}

fn work(root: &Path, slug: &str, id: &str, state: &str, reason: Option<&str>) {
    let dir = root.join("work").join(slug);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("_work.md"), format!("---\nid: {id}\nkind: task\nstate: {state}\nowner: fixture-owner\nrepo: fixture/repo\n---\n\n# {slug}\n\nPrivate work body is not part of the view.\n")).unwrap();
    if let Some(reason) = reason {
        let event = json!({"ts":"2026-10-23T10:00:00Z","type":"loop.flagged","actor":"maestro","kind":"yours","reason":reason,"since":"2026-10-23T10:00:00Z"});
        std::fs::write(dir.join("_events.jsonl"), format!("{event}\n")).unwrap();
    }
}

fn ledger(root: &Path, file: &str, content: &str) {
    std::fs::write(root.join("workspace/.control/asks").join(file), content).unwrap();
}

#[test]
fn owner_hold_has_recorded_context_without_a_work_mutation_control() {
    let m = machine();
    work(m.path(), "held-item", "work-1", "proposed", Some("Owner must confirm <access>"));
    for kind in ["needs", "today", "decisions"] {
        let html = render(m.path(), kind);
        assert!(html.contains("<details class=\"mv-work-context\">"), "{kind}: {html}");
        assert!(html.contains("Owner must confirm &lt;access&gt;"), "{kind}: {html}");
        assert!(html.contains("fixture-owner"));
        assert!(html.contains("2026-10-23T10:00:00Z"));
        assert!(html.contains("does not change work state here"));
        assert!(!html.contains("Private work body"));
        assert!(!html.contains("/work/start"));
        assert!(!html.contains("/work/complete"));
    }
}

#[test]
fn a_duplicate_work_identity_cannot_attach_an_arbitrary_hold_detail() {
    let m = machine();
    work(m.path(), "first-item", "same-id", "proposed", Some("First private hold"));
    work(m.path(), "second-item", "same-id", "proposed", Some("Second private hold"));
    let html = render(m.path(), "needs");
    assert!(html.contains("Work details could not be identified uniquely."));
    assert!(!html.contains("First private hold"));
    assert!(!html.contains("Second private hold"));
}

#[test]
fn recent_asks_link_to_their_exact_ledger_and_encoded_handle() {
    let m = machine();
    ledger(
        m.path(),
        "specific.yaml",
        "arc: demo\nasks:\n  - id: 'second ask?#'\n    ask: Specific recent ask\n    class: decision\n    asked_at: '2026-10-23T11:00:00Z'\n",
    );
    let html = render(m.path(), "today");
    assert!(html.contains("href=\"/decisions/specific.yaml/second%20ask%3F%23\">Decide</a>"), "{html}");
}

#[test]
fn shared_ask_handles_do_not_offer_an_ambiguous_recent_link() {
    let m = machine();
    ledger(
        m.path(),
        "shared.yaml",
        "arc: demo\nasks:\n  - id: same\n    ask: First recent ask\n    class: decision\n    asked_at: '2026-10-23T11:00:00Z'\n  - id: same\n    ask: Second recent ask\n    class: decision\n    asked_at: '2026-10-23T10:00:00Z'\n",
    );
    let html = render(m.path(), "today");
    assert!(html.contains("Decision link unavailable: shared handle"));
    assert!(!html.contains("href=\"/decisions/shared.yaml/same\""));
}

#[test]
fn answered_does_not_claim_an_agent_started_or_finished() {
    let m = machine();
    ledger(
        m.path(),
        "answered.yaml",
        "arc: demo\nasks:\n  - id: answered\n    ask: A recorded answer\n    class: decision\n    answer: yes\n    answered_at: '2026-10-23T10:00:00Z'\n",
    );
    let html = render(m.path(), "today");
    assert!(html.contains("Agent outcome not reported"), "{html}");
    assert!(!html.contains("> Acting"));
    assert!(!html.contains("> Acted"));
    assert!(html.contains("href=\"/decisions/answered.yaml/answered\">A recorded answer</a>"));
}

#[test]
fn work_counts_expand_to_the_same_recorded_items() {
    let m = machine();
    work(m.path(), "review-item", "review-id", "review", None);
    work(m.path(), "blocked-item", "blocked-id", "blocked", None);
    work(m.path(), "running-item", "running-id", "running", None);
    let html = render(m.path(), "today");
    let stats = html.split("class=\"today-stats\"").nth(1).unwrap();
    assert!(stats.contains("<strong>1</strong><span>work items in review</span>"), "{stats}");
    assert!(stats.contains("<strong>1</strong><span>blocked</span>"));
    assert!(stats.contains("review-item"));
    assert!(stats.contains("blocked-item"));
    assert!(!stats.contains("running-item"));
    assert!(stats.contains("<details class=\"today-stat\">"));
}

#[test]
fn unread_work_does_not_become_an_empty_drilldown() {
    let m = machine();
    std::fs::remove_dir(m.path().join("work")).unwrap();
    let html = render(m.path(), "today");
    assert!(html.contains("<strong>Unavailable</strong><span>blocked</span>"));
    assert!(html.contains("Work items are unavailable."));
    assert!(!html.contains("No work items in this state."));
}

#[test]
fn every_web_page_exposes_read_time_scope_and_errors_as_a_disclosure() {
    let m = machine();
    ledger(m.path(), "broken.yaml", "not: [valid\n");
    for kind in ["today", "decisions", "health"] {
        let html = render(m.path(), kind);
        assert!(html.contains("<details class=\"mv-provenance\"><summary>Data sources"), "{kind}: {html}");
        assert!(html.contains("datetime=\"2026-10-23T12:00:00Z\""));
        assert!(html.contains("does not establish when each producer last updated them"));
        assert!(html.contains("Workspace revision:"));
        assert!(html.contains("broken.yaml"));
        assert!(html.contains("data-source-state=\"partial\""));
    }
}

fn delivery(id: &str, stage: &str, woke: Option<&str>) -> serde_json::Value {
    json!({"id":id,"intent":{"verb":"ask.answer","ledger":"delivery.yaml","handle":"delivery-ask","hash":"old-entry","option":"yes","note":"Private receipt note must not render"},"stage":stage,"signed_at":"2026-10-23T10:00:00Z","commit":"0123456789abcdef","woke":woke,"landing":"landing not yet recorded","error":null})
}

fn delivery_log(root: &Path, rows: &[serde_json::Value]) {
    let owner = root.join("maestro-owner");
    std::fs::create_dir_all(&owner).unwrap();
    let text = rows.iter().map(serde_json::Value::to_string).collect::<Vec<_>>().join("\n") + "\n";
    std::fs::write(owner.join("receipts.jsonl"), text).unwrap();
}

#[test]
fn writer_delivery_is_separate_from_answer_execution_and_shows_only_latest_state() {
    let m = machine();
    ledger(
        m.path(),
        "delivery.yaml",
        "arc: demo\nasks:\n  - id: delivery-ask\n    ask: A decision with a delivery record\n    class: decision\n    answer: 'yes'\n    answered_at: '2026-10-23T10:00:00Z'\n",
    );
    delivery_log(
        m.path(),
        &[
            delivery("abcdefghijklmnop", "signed", None),
            delivery("abcdefghijklmnop", "written", Some("told T3 thread fixture-thread")),
        ],
    );
    let html = render(m.path(), "today");
    let activity = html.split("aria-label=\"Answer delivery\"").nth(1).unwrap();
    assert!(activity.contains("Recorded written"));
    assert!(activity.contains("Session notified"));
    assert!(!activity.contains("Signed, awaiting writer"));
    assert_eq!(activity.matches("data-delivery=\"abcdefghijklmnop\"").count(), 1);
    assert!(activity.contains("does not prove that an agent started or completed the work"));
    assert!(activity.contains("does not establish that the answer has landed on main"));
    assert!(activity.contains("href=\"/decisions/delivery.yaml/delivery-ask\">View decision</a>"));
    assert!(!html.contains("Private receipt note must not render"));
}

#[test]
fn missing_or_partial_delivery_history_never_looks_like_a_known_empty_log() {
    let m = machine();
    let html = render(m.path(), "today");
    assert!(html.contains("Answer delivery receipts are unavailable."));
    assert!(!html.contains("No answer delivery receipts recorded."));
    delivery_log(
        m.path(),
        &[delivery("abcdefghijklmnop", "written", None), delivery("abcdefghijklmnop", "made-up", None)],
    );
    let html = render(m.path(), "today");
    assert!(html.contains("Delivery history is partial."));
    assert!(!html.contains("No answer delivery receipts recorded."));
    assert!(!html.contains("data-delivery=\"abcdefghijklmnop\""));
    assert!(html.contains("Delivery source"));
}

#[test]
fn failed_notification_is_reported_without_a_success_badge_and_activity_is_bounded() {
    let m = machine();
    let rows: Vec<_> = (0..10)
        .map(|i| {
            delivery(&format!("receipt{i:09}"), "written", Some("T3 did not take it (exit Some(1)): fixture refusal"))
        })
        .collect();
    delivery_log(m.path(), &rows);
    let html = render(m.path(), "today");
    assert_eq!(html.matches("data-delivery=").count(), 8);
    assert!(html.contains("Recorded delivery result: T3 did not take it"));
    assert!(!html.contains("Session notified"));
    assert!(html.contains("Showing the 8 most recently signed receipts"));
    assert!(!html.contains(">View decision</a>"));
}

#[test]
fn delivery_error_display_is_bounded_even_for_large_malformed_records() {
    let m = machine();
    let rows: Vec<_> = (0..12)
        .map(|i| {
            let mut row = delivery(&format!("receipt{i:09}"), "invalid", None);
            row["stage"] = json!("x".repeat(4000));
            row
        })
        .collect();
    delivery_log(m.path(), &rows);
    let html = render(m.path(), "today");
    let source = html.split("<summary>Delivery source</summary>").nth(1).unwrap();
    assert!(source.contains("Showing 8 of 12 receipt read errors."));
    let source = source.split("Showing 8 of 12 receipt read errors.").next().unwrap();
    assert_eq!(source.matches("class=\"mv-source-error\"").count(), 8);
    assert!(source.contains("message shortened"));
    assert!(!source.contains(&"x".repeat(300)));
}
