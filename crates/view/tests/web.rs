//! The web pages' decision layout (BRO-3783, design approved in maestro#20): the queue as
//! links beside the decision, the header's counts from the same read, and the calm screen only
//! when every ask was read and none is open.

use maestro_store::{Paths, Store, TreeSource};
use maestro_view::page::{self, Shell};
use maestro_view::{Rendered, render_web, validate};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/machine")
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

/// The fixture machine with the decisions ledger beside its own.
fn machine() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    copy_tree(&fixture(), dir.path());
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/decisions.yaml"),
        dir.path().join("workspace/.control/asks/decisions.yaml"),
    )
    .unwrap();
    dir
}

/// The fixture machine whose only ledger holds one answered ask: nothing is open.
fn all_answered(broken: bool) -> tempfile::TempDir {
    let dir = machine();
    let asks = dir.path().join("workspace/.control/asks");
    for entry in std::fs::read_dir(&asks).unwrap() {
        std::fs::remove_file(entry.unwrap().path()).unwrap();
    }
    std::fs::write(
        asks.join("done.yaml"),
        "arc: done\nasks:\n  - id: shipped\n    ask: \"Ship it?\"\n    class: decision\n    gates: [ship]\n    blocking: false\n    preanswerable: true\n    exhausted: [\"read: x\", \"default: y\"]\n    answer: \"yes\"\n    answered_at: \"2026-10-02T00:00:00Z\"\n    status: resolved\n",
    )
    .unwrap();
    if broken {
        std::fs::write(asks.join("broken.yaml"), "not: [valid\n").unwrap();
    }
    dir
}

fn at() -> jiff::Timestamp {
    "2026-10-23T12:00:00Z".parse().unwrap()
}

fn spec(block: Value) -> Value {
    json!({ "root": "s", "blocks": { "s": block } })
}

fn worktree(root: &Path) -> Store {
    Store::new(Paths::under(root.to_path_buf()), TreeSource::WorkingTree)
}

fn web(store: &Store, block: Value) -> (Rendered, Shell) {
    let v = validate(&spec(block)).unwrap();
    assert!(v.drops.is_empty(), "{:?}", v.drops);
    render_web(store, &v, at(), None)
}

/// The rows of the queue column, in order: (href, whether it is the current one).
fn queue_links(html: &str) -> Vec<(String, bool)> {
    let start = html.find(r#"<aside class="mv-queue""#).expect("a queue column");
    let column = &html[start..start + html[start..].find("</aside>").unwrap()];
    column
        .match_indices("<a href=\"")
        .map(|(i, m)| {
            let rest = &column[i + m.len()..];
            let href = rest[..rest.find('"').unwrap()].to_string();
            let tag = &rest[..rest.find('>').unwrap()];
            (href, tag.contains(r#"aria-current="page""#))
        })
        .collect()
}

#[test]
fn decisions_opens_the_first_ask_in_queue_order_beside_the_queue() {
    let m = machine();
    let store = worktree(m.path());
    let (r, shell) = web(&store, json!({ "type": "decisions" }));
    assert!(r.html.contains(r#"<div class="mv-decide">"#), "{}", r.html);
    let links = queue_links(&r.html);
    assert!(links.len() >= 2, "{links:?}");
    assert!(links[0].1 && links.iter().skip(1).all(|(_, current)| !current), "only the first is current: {links:?}");
    // The decision shown is the first row's: its article names the same ledger and handle.
    let (ledger, handle) = links[0].0.trim_start_matches("/decisions/").split_once('/').unwrap();
    assert!(r.html.contains(&format!(r#"<article class="mv-decision" data-ask="{ledger}#{handle}""#)), "{}", r.html);
    // The header counts from the same read as the queue: every open ask is a row.
    let (open, _) = shell.open.expect("the asks were read");
    assert!(shell.partial, "the fixture holds a broken ledger");
    assert!(r.html.contains(&format!(r#"<span class="mv-queue-count">{open}+</span>"#)), "{}", r.html);
    assert_eq!(shell.summary(), format!("at least {open} decisions for you, {} blocking", shell.open.unwrap().1));
    assert_eq!(shell.badge(), format!("{open}+"));
}

#[test]
fn one_decision_marks_its_row_and_offers_the_neighbours_on_a_phone() {
    let m = machine();
    let store = worktree(m.path());
    let (first, _) = web(&store, json!({ "type": "decisions" }));
    let links = queue_links(&first.html);
    let (ledger, handle) = links[1].0.trim_start_matches("/decisions/").split_once('/').unwrap();
    let (r, _) = web(&store, json!({ "type": "decision", "props": { "ledger": ledger, "id": handle } }));
    let now = queue_links(&r.html);
    assert_eq!(now.iter().filter(|(_, c)| *c).count(), 1, "{now:?}");
    assert!(now[1].1, "the second row is current: {now:?}");
    assert!(r.html.contains(&format!(r#"2 of {}"#, links.len())), "{}", r.html);
    assert!(r.html.contains(&format!(r#"href="{}" rel="prev""#, links[0].0)), "{}", r.html);
}

#[test]
fn nothing_needs_you_only_when_every_ask_was_read() {
    let calm = all_answered(false);
    let (r, shell) = web(&worktree(calm.path()), json!({ "type": "decisions" }));
    assert!(r.html.contains(r#"class="mv-calm""#), "{}", r.html);
    assert!(r.html.contains("Nothing needs <em>you</em>"), "{}", r.html);
    assert_eq!((shell.open, shell.partial), (Some((0, 0)), false));
    assert_eq!(shell.summary(), "Nothing needs you");

    // A ledger that did not read may hold an open ask: no calm screen, the inbox says so.
    let partial = all_answered(true);
    let (r, shell) = web(&worktree(partial.path()), json!({ "type": "decisions" }));
    assert!(!r.html.contains("mv-calm"), "{}", r.html);
    assert!(r.html.contains(r#"data-type="decisions" data-state="partial""#), "{}", r.html);
    assert!(shell.partial);
    assert_eq!(shell.summary(), "at least 0 decisions for you, none seen blocking");

    // Nothing read at all: unavailable, never calm, never zero.
    let empty = tempfile::tempdir().unwrap();
    let (r, shell) = web(&worktree(empty.path()), json!({ "type": "decisions" }));
    assert!(!r.html.contains("mv-calm"), "{}", r.html);
    assert!(r.html.contains(r#"data-state="unavailable""#), "{}", r.html);
    assert_eq!(shell.open, None);
    assert_eq!(shell.summary(), "Decisions could not be read");
    assert_eq!(shell.badge(), "?");
}

#[test]
fn a_queue_link_carries_its_ledger_and_handle_as_single_segments() {
    let m = machine();
    let asks = m.path().join("workspace/.control/asks");
    std::fs::write(
        asks.join("odd name.yaml"),
        "arc: odd\nasks:\n  - id: \"a/b?c#d\"\n    ask: \"Odd handle?\"\n    class: decision\n    gates: [odd]\n    blocking: true\n    preanswerable: true\n    exhausted: [\"read: x\", \"default: y\"]\n",
    )
    .unwrap();
    let (r, _) = web(&worktree(m.path()), json!({ "type": "decisions" }));
    let links = queue_links(&r.html);
    assert!(links.iter().any(|(h, _)| h == "/decisions/odd%20name.yaml/a%2Fb%3Fc%23d"), "{links:?}");
    for (h, _) in &links {
        assert_eq!(h.matches('/').count(), 3, "{h}");
        assert!(!h.contains('?') && !h.contains('#'), "{h}");
    }
}

#[test]
fn the_shell_names_the_tree_short_and_marks_the_current_page() {
    let shell = Shell {
        tree: "git:origin/main@0123456789ab".into(),
        open: Some((3, 0)),
        partial: false,
        committed: Some("2 h".into()),
    };
    assert_eq!(shell.tree_short(), "main 0123456");
    assert_eq!(shell.summary(), "3 decisions for you, none blocking");
    assert_eq!(Shell { open: Some((1, 1)), ..shell.clone() }.summary(), "1 decision for you, 1 blocking");
    assert_eq!(Shell { tree: "worktree".into(), ..shell.clone() }.tree_short(), "worktree");
    let doc = page::web("<p>x</p>", "Today", "/today", &shell);
    assert!(doc.contains(r#"<a class="mv-tab" href="/today" aria-current="page">Today</a>"#), "{doc}");
    assert!(doc.contains(r#"<span class="mv-nav-count">3</span>"#), "{doc}");
    assert!(doc.contains("main 0123456</span> · committed 2 h ago"), "{doc}");
    assert!(
        !doc.contains("<script") && !doc.to_ascii_lowercase().contains("url("),
        "the web page runs and fetches nothing"
    );
}
