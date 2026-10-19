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
    assert_eq!(open["asks"].as_array().unwrap().len(), 2);
    let blocking = call(&s, "maestro_asks_list", json!({ "blocking": true })).unwrap();
    let rows = blocking["asks"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["recommended"], "merge");
    let full = call(&s, "maestro_ask_get", json!({ "ledger": "demo-arc.yaml", "id": "merge-now" })).unwrap();
    assert_eq!(full["hash"], rows[0]["hash"]);
    assert!(call(&s, "maestro_asks_list", json!({ "state": "bogus" })).is_err());
}

#[test]
fn work_state_filter_uses_canonical_labels() {
    let s = store();
    let v = call(&s, "maestro_work_list", json!({ "state": "Needs you" })).unwrap();
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    assert!(call(&s, "maestro_work_list", json!({ "state": "review" })).is_err());
}

#[test]
fn context_returns_markdown_and_packet() {
    let s = store();
    let v = call(&s, "maestro_context", json!({ "project": "demo" })).unwrap();
    assert!(v["markdown"].as_str().unwrap().starts_with("## Maestro context"));
    assert_eq!(v["packet"]["needs_you"], 1);
}
