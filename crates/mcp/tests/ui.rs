//! The generative-UI tools: catalog, render and the App's poll, and the App resource.

use maestro_mcp::{MaestroServer, TOOLS, Ui, call};
use maestro_store::{Paths, Store, TreeSource};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/machine")
}

fn store_at(root: &Path) -> Store {
    Store::new(Paths::under(root.to_path_buf()), TreeSource::WorkingTree)
}

fn spec() -> Value {
    json!({
        "root": "m",
        "blocks": {
            "m": { "type": "stack", "children": ["a", "x"] },
            "a": { "type": "ask", "props": { "ledger": "demo-arc.yaml", "id": "merge-now" } },
            "x": { "type": "chart", "props": { "values": [1, 2, 3] } },
        }
    })
}

#[test]
fn the_render_tool_opens_the_app_and_the_poll_is_app_only() {
    let tools = MaestroServer::tools();
    let meta = |name: &str| {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        serde_json::to_value(&tool.meta).unwrap()
    };
    let render = meta("maestro_ui_render");
    assert_eq!(render["ui"]["resourceUri"], maestro_view::page::APP_URI);
    assert_eq!(render["ui"]["visibility"], json!(["model", "app"]));
    assert_eq!(render["ui/resourceUri"], maestro_view::page::APP_URI);
    assert_eq!(meta("maestro_ui_poll")["ui"]["visibility"], json!(["app"]));
    for spec in TOOLS {
        let m = meta(spec.name);
        assert_eq!(m.is_null(), spec.ui == Ui::None, "{}", spec.name);
    }
}

#[test]
fn render_reports_every_drop_and_shows_only_ledger_values() {
    let s = store_at(&fixture_root());
    let r = call(&s, "maestro_ui_render", spec()).unwrap();
    assert_eq!(r["drops"].as_array().unwrap().len(), 1);
    assert_eq!(r["drops"][0]["block"], "x");
    assert_eq!(r["drops"][0]["type"], "chart");
    let text = r["text"].as_str().unwrap();
    assert!(text.contains("1 dropped:\n- `x`: unknown block type `chart`"), "{text}");
    let html = r["html"].as_str().unwrap();
    assert!(html.contains("Merge the demo PR now, or hold for review?"));
    assert!(!html.contains("chart"));
    assert_eq!(r["spec"]["blocks"]["m"]["children"], json!(["a"]));
}

#[test]
fn a_snapshot_is_a_standalone_document_in_the_text() {
    let s = store_at(&fixture_root());
    let mut args = spec();
    args["format"] = json!("snapshot");
    args["theme"] = json!("dark");
    let r = call(&s, "maestro_ui_render", args).unwrap();
    let doc = r["text"].as_str().unwrap();
    assert!(doc.starts_with("<!DOCTYPE html>"));
    assert!(doc.contains(r#"data-theme="dark""#));
    assert!(doc.contains(r["html"].as_str().unwrap()));
    assert!(!doc.contains("<script"));
    for bad in [json!({ "format": "pdf" }), json!({ "theme": "sepia" })] {
        let mut args = spec();
        args.as_object_mut().unwrap().extend(bad.as_object().unwrap().clone());
        assert!(call(&s, "maestro_ui_render", args).is_err());
    }
}

#[test]
fn every_call_reads_a_fresh_store_and_the_poll_follows_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    copy_tree(&fixture_root(), dir.path());
    let server = MaestroServer::new(store_at(dir.path()));
    let first = server.call("maestro_ui_render", spec()).unwrap();
    let held = json!({ "spec": first["spec"], "revision": first["revision"] });

    let same = server.call("maestro_ui_poll", held.clone()).unwrap();
    assert_eq!(same["changed"], false);
    assert!(same.get("html").is_none());
    assert_eq!(same["revision"], first["revision"]);

    let ledger = dir.path().join("workspace/.control/asks/demo-arc.yaml");
    let text = std::fs::read_to_string(&ledger).unwrap();
    let edited = text.replace("label: Merge now", "label: Merge after review");
    assert_ne!(text, edited);
    std::fs::write(&ledger, edited).unwrap();

    let moved = server.call("maestro_ui_poll", held).unwrap();
    assert_eq!(moved["changed"], true);
    assert_ne!(moved["revision"], first["revision"]);
    assert!(moved["html"].as_str().unwrap().contains("Merge after review"));
    // The kept spec dropped nothing; a poll with it records no drop.
    assert_eq!(moved["drops"], json!([]));
}

#[test]
fn the_app_resource_is_the_mcp_apps_page() {
    assert!(maestro_mcp::RESOURCES.iter().any(|(uri, _, _)| *uri == maestro_view::page::APP_URI));
    let page = maestro_view::page::app();
    assert!(page.contains(maestro_view::page::BRIDGE_JS));
    assert_eq!(maestro_view::page::APP_MIME, "text/html;profile=mcp-app");
}

#[test]
fn the_catalog_tool_returns_the_catalog() {
    let s = store_at(&fixture_root());
    assert_eq!(call(&s, "maestro_ui_catalog", json!({})).unwrap(), maestro_view::catalog());
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
            copy_tree(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}
