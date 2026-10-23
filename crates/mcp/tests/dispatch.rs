use maestro_core::authority::{Authority, OwnerVerb};
use maestro_mcp::{MaestroServer, TOOLS, call};
use maestro_store::{Paths, Store, TreeSource};
use serde_json::json;
use std::path::Path;

fn store() -> Store {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/machine");
    Store::new(Paths::under(root), TreeSource::WorkingTree)
}

#[test]
fn every_exposed_tool_is_read_only() {
    for spec in TOOLS {
        assert_eq!(spec.authority, Authority::Read, "{}", spec.name);
    }
    for tool in MaestroServer::tools() {
        let ann = tool.annotations.as_ref().expect("annotations");
        assert_eq!(ann.read_only_hint, Some(true), "{}", tool.name);
        assert_eq!(tool.input_schema.get("type").and_then(|t| t.as_str()), Some("object"), "{}", tool.name);
    }
}

#[test]
fn no_owner_verb_is_reachable() {
    let s = store();
    for verb in OwnerVerb::ALL {
        let name = serde_json::to_value(verb).unwrap().as_str().unwrap().replace('.', "_");
        assert!(call(&s, &format!("maestro_{name}"), json!({})).is_err(), "{name}");
    }
    assert!(call(&s, "maestro_answer_ask", json!({})).is_err());
}

#[test]
fn every_tool_answers_on_the_fixture() {
    let s = store();
    let args = |name: &str| match name {
        "maestro_ask_get" => json!({ "ledger": "demo-arc.yaml", "id": "merge-now" }),
        "maestro_ui_render" => json!({ "root": "s", "blocks": { "s": { "type": "snapshot" } } }),
        "maestro_ui_poll" => json!({ "spec": { "root": "s", "blocks": { "s": { "type": "snapshot" } } } }),
        _ => json!({}),
    };
    for spec in TOOLS {
        let v = call(&s, spec.name, args(spec.name)).unwrap_or_else(|e| panic!("{}: {}", spec.name, e.message));
        assert!(!v.is_null(), "{}", spec.name);
    }
}

#[test]
fn asks_list_filters_and_hashes() {
    let s = store();
    let open = call(&s, "maestro_asks_list", json!({})).unwrap();
    assert_eq!(open["readable"], true);
    assert_eq!(open["asks"].as_array().unwrap().len(), 3);
    let blocking = call(&s, "maestro_asks_list", json!({ "blocking": true, "project": "demo" })).unwrap();
    let rows = blocking["asks"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let rows: Vec<_> = rows.iter().filter(|r| r["ledger"] == "demo-arc.yaml").collect();
    assert_eq!(rows[0]["recommended"], "merge");
    let full = call(&s, "maestro_ask_get", json!({ "ledger": "demo-arc.yaml", "id": "merge-now" })).unwrap();
    assert_eq!(full["hash"], rows[0]["hash"]);
    assert!(call(&s, "maestro_asks_list", json!({ "state": "bogus" })).is_err());
}

#[test]
fn work_state_filter_uses_canonical_labels() {
    let s = store();
    let v = call(&s, "maestro_work_list", json!({ "state": "Needs you" })).unwrap();
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["finished"], true);
    let standing = call(&s, "maestro_work_list", json!({ "state": "Standing" })).unwrap();
    assert_eq!(standing["items"].as_array().unwrap().len(), 1);
    assert!(call(&s, "maestro_work_list", json!({ "state": "review" })).is_err());
}

#[test]
fn context_returns_markdown_and_packet() {
    let s = store();
    let v = call(&s, "maestro_context", json!({ "project": "demo" })).unwrap();
    assert!(v["markdown"].as_str().unwrap().starts_with("## Maestro context"));
    assert_eq!(v["packet"]["needs_you"], 3);
}

#[test]
fn unreadable_asks_are_not_an_empty_list() {
    let s = Store::new(Paths::under("/nonexistent/maestro-test"), TreeSource::WorkingTree);
    let asks = call(&s, "maestro_asks_list", json!({})).unwrap();
    assert_eq!(asks["readable"], false);
    let needs = call(&s, "maestro_needs_list", json!({})).unwrap();
    assert!(needs["data"].is_null());
    let ctx = call(&s, "maestro_context", json!({})).unwrap();
    assert!(ctx["packet"]["needs_you"].is_null());
}

#[test]
fn an_ambiguous_bare_id_is_refused() {
    let tmp = std::env::temp_dir().join(format!("maestro-ambiguous-{}", std::process::id()));
    let asks = tmp.join("workspace/.control/asks");
    std::fs::create_dir_all(&asks).unwrap();
    std::fs::write(
        asks.join("dup.yaml"),
        "asks:\n  - id: a\n    ask: one?\n  - id: a\n    ask: two?\n  - id: a\n    uid: u1\n    ask: three?\n",
    )
    .unwrap();
    let s = Store::new(Paths::under(&tmp), TreeSource::WorkingTree);
    assert!(call(&s, "maestro_ask_get", json!({ "ledger": "dup.yaml", "id": "a" })).is_err());
    let by_uid = call(&s, "maestro_ask_get", json!({ "ledger": "dup.yaml", "id": "u1" })).unwrap();
    assert_eq!(by_uid["ask"]["ask"], "three?");
    let _ = std::fs::remove_dir_all(&tmp);
}

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

#[test]
fn a_long_lived_server_follows_the_revision_as_it_moves() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    let asks = ws.join(".control/asks");
    std::fs::create_dir_all(&asks).unwrap();
    std::fs::write(asks.join("one.yaml"), "asks:\n  - id: a\n    ask: first?\n").unwrap();
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "one"]);

    let mut paths = Paths::under(tmp.path());
    paths.workspace = ws.clone();
    let server = MaestroServer::new(Store::new(paths, TreeSource::GitRev("main".into())));
    let count = |v: serde_json::Value| v["asks"].as_array().unwrap().len();
    assert_eq!(count(server.call("maestro_asks_list", json!({ "state": "all" })).unwrap()), 1);

    std::fs::write(asks.join("two.yaml"), "asks:\n  - id: b\n    ask: second?\n").unwrap();
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "two"]);
    let after = server.call("maestro_asks_list", json!({ "state": "all" })).unwrap();
    assert_eq!(count(after.clone()), 2, "the server read the commit it started on: {after}");
}

#[test]
fn asks_list_reports_total_and_truncation() {
    let s = store();
    let all = call(&s, "maestro_asks_list", json!({ "state": "all", "limit": 1 })).unwrap();
    assert_eq!(all["asks"].as_array().unwrap().len(), 1);
    assert!(all["total"].as_u64().unwrap() > 1, "{all}");
    assert_eq!(all["truncated"], true);
    let open = call(&s, "maestro_asks_list", json!({})).unwrap();
    assert_eq!(open["total"], 3);
    assert_eq!(open["truncated"], false);
}

#[test]
fn an_ask_in_an_unreadable_ledger_is_unreadable_not_missing() {
    let s = store();
    let broken = call(&s, "maestro_ask_get", json!({ "ledger": "broken.yaml", "id": "anything" })).unwrap();
    assert_eq!(broken["state"], "unreadable", "{broken}");
    assert!(!broken["errors"].as_array().unwrap().is_empty());
    // An entry that failed inside a readable ledger makes that ledger's misses unreadable too.
    let partial = call(&s, "maestro_ask_get", json!({ "ledger": "alt-key.yaml", "id": "gone" })).unwrap();
    assert_eq!(partial["state"], "unreadable");
    // A clean ledger's miss is still a plain miss.
    assert!(call(&s, "maestro_ask_get", json!({ "ledger": "demo-arc.yaml", "id": "gone" })).is_err());
}
