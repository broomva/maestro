//! Rendering reads every value from the Store it is given, shows an unread source as
//! unavailable, and produces stable HTML (maud snapshots).

use maestro_store::{Paths, Store, TreeSource};
use maestro_view::{render_at, validate};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/machine")
}

fn store_at(root: &Path) -> Store {
    Store::new(Paths::under(root.to_path_buf()), TreeSource::WorkingTree)
}

fn at() -> jiff::Timestamp {
    "2026-10-23T12:00:00Z".parse().unwrap()
}

fn every_block() -> Value {
    json!({
        "root": "main",
        "blocks": {
            "main": { "type": "stack", "children": ["snap", "needs", "asks", "ask", "work", "loops", "init", "fleet"] },
            "snap": { "type": "snapshot" },
            "needs": { "type": "needs" },
            "asks": { "type": "asks", "props": { "state": "all" } },
            "ask": { "type": "ask", "props": { "ledger": "demo-arc.yaml", "id": "merge-now" } },
            "work": { "type": "work" },
            "loops": { "type": "loops" },
            "init": { "type": "initiatives", "props": { "all": true } },
            "fleet": { "type": "fleet" },
        }
    })
}

/// The fixture's absolute path appears in sources; snapshots name it `<fixture>`, and break
/// between tags so a diff reads line by line.
fn portable(html: &str, root: &Path) -> String {
    html.replace(&root.display().to_string(), "<fixture>").replace("><", ">\n<")
}

#[test]
fn every_block_renders_from_the_fixture() {
    let root = fixture();
    let v = validate(&every_block()).unwrap();
    let r = render_at(&store_at(&root), &v, at());
    assert!(r.drops.is_empty());
    assert_eq!(r.revision.len(), 64);
    insta::assert_snapshot!("every_block", portable(&r.html, &root));
}

#[test]
fn values_come_from_the_ledger_not_the_spec() {
    let root = fixture();
    let v = validate(&every_block()).unwrap();
    let r = render_at(&store_at(&root), &v, at());
    // The fixture's ask and its recommended option, as written in demo-arc.yaml.
    assert!(r.html.contains("Merge the demo PR now, or hold for review?"), "{}", r.html);
    assert!(r.html.contains(r#"data-option="merge" data-recommended="true""#), "{}", r.html);
    // broken.yaml does not parse: the asks card says it read in part.
    assert!(r.html.contains(r#"data-type="asks" data-state="partial""#), "{}", r.html);
}

#[test]
fn an_unreadable_source_renders_unavailable_never_zero() {
    let empty = tempfile::tempdir().unwrap();
    let v = validate(&every_block()).unwrap();
    let r = render_at(&store_at(empty.path()), &v, at());
    for kind in ["needs", "asks", "work", "loops", "initiatives", "fleet"] {
        let card = format!(r#"data-type="{kind}" data-state="unavailable""#);
        assert!(r.html.contains(&card), "{kind}: {}", r.html);
    }
    // No count of zero stands in for a source that was not read.
    assert!(!r.html.contains(r#"<span class="mv-count">0</span>"#), "{}", r.html);
    assert!(!r.html.contains(r#"<span class="mv-tile-value">0</span>"#), "{}", r.html);
    assert!(r.html.contains("This is not the same as none."));
    insta::assert_snapshot!("unavailable", portable(&r.html, empty.path()));
}

#[test]
fn each_render_reads_the_store_anew() {
    let dir = tempfile::tempdir().unwrap();
    copy_tree(&fixture(), dir.path());
    let v = validate(&every_block()).unwrap();
    let before = render_at(&store_at(dir.path()), &v, at());
    let ledger = dir.path().join("workspace/.control/asks/demo-arc.yaml");
    let text = std::fs::read_to_string(&ledger).unwrap();
    let edited = text.replace("Merge the demo PR now, or hold for review?", "Merge it after the second review?");
    assert_ne!(text, edited);
    std::fs::write(&ledger, edited).unwrap();
    let after = render_at(&store_at(dir.path()), &v, at());
    assert!(after.html.contains("Merge it after the second review?"));
    assert!(!after.html.contains("Merge the demo PR now"));
    assert_ne!(before.revision, after.revision);
    // The same store read twice is the same screen.
    let again = render_at(&store_at(dir.path()), &v, at());
    assert_eq!(after.revision, again.revision);
}

#[test]
fn ledger_text_is_escaped() {
    let dir = tempfile::tempdir().unwrap();
    copy_tree(&fixture(), dir.path());
    let ledger = dir.path().join("workspace/.control/asks/demo-arc.yaml");
    let text = std::fs::read_to_string(&ledger).unwrap();
    std::fs::write(&ledger, text.replace("label: Merge now", "label: \"<script>alert(1)</script>\"")).unwrap();
    let v = validate(&every_block()).unwrap();
    let r = render_at(&store_at(dir.path()), &v, at());
    assert!(!r.html.contains("<script>"), "{}", r.html);
    assert!(r.html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"), "{}", r.html);
}

#[test]
fn an_ask_that_is_not_there_says_so() {
    let root = fixture();
    let v = validate(&json!({
        "root": "m",
        "blocks": {
            "m": { "type": "stack", "children": ["a", "b"] },
            "a": { "type": "ask", "props": { "ledger": "demo-arc.yaml", "id": "no-such" } },
            "b": { "type": "ask", "props": { "ledger": "nope.yaml", "id": "merge-now" } },
        }
    }))
    .unwrap();
    let r = render_at(&store_at(&root), &v, at());
    assert!(r.html.contains("No ask <span class=\"bv-code\">demo-arc.yaml#no-such</span>"), "{}", r.html);
    assert!(r.html.contains("No ledger named <span class=\"bv-code\">nope.yaml</span>"), "{}", r.html);
    // broken.yaml's parse error is not this ask's: the card is not marked partial for it.
    assert!(!r.html.contains(r#"data-type="ask" data-state="partial""#), "{}", r.html);
    insta::assert_snapshot!("ask_not_found", portable(&r.html, &root));
}

#[test]
fn an_unparsed_ledger_marks_its_own_ask_card() {
    let root = fixture();
    let v = validate(&json!({
        "root": "a",
        "blocks": { "a": { "type": "ask", "props": { "ledger": "broken.yaml", "id": "anything" } } }
    }))
    .unwrap();
    let r = render_at(&store_at(&root), &v, at());
    assert!(r.html.contains(r#"data-type="ask" data-state="partial""#), "{}", r.html);
}

#[test]
fn lists_respect_their_limit_and_say_what_was_cut() {
    let root = fixture();
    let v = validate(&json!({
        "root": "a",
        "blocks": { "a": { "type": "asks", "props": { "state": "all", "limit": 1 } } }
    }))
    .unwrap();
    let r = render_at(&store_at(&root), &v, at());
    assert_eq!(r.html.matches(r#"<li class="mv-row""#).count(), 1, "{}", r.html);
    assert!(r.html.contains(" more not shown"), "{}", r.html);
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
