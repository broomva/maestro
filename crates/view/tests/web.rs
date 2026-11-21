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
    // The fixture holds one work item for the owner; with it, something still needs you.
    std::fs::remove_dir_all(dir.path().join("work/held-for-you")).unwrap();
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
            (href, tag.contains(r#"aria-current="true""#))
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
    // The fixture holds a broken ledger: the pager's count is a lower bound, and the phone's
    // read note (the column is hidden there) names what could not be read.
    assert!(r.html.contains(&format!(r#"2 of {}+"#, links.len())), "{}", r.html);
    assert!(r.html.contains(r#"<div class="mv-pager-read">"#) && r.html.contains("could not be read"), "{}", r.html);
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
    assert!(shell.calm && shell.state() == "calm");
    // It says no ask is open, not that every ask has an answer: an archived ask has none.
    assert!(r.html.contains("is open, and nothing else is held for you"), "{}", r.html);
    assert!(!r.html.contains("has an answer"), "{}", r.html);

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

/// The calm screen's stat for `label`: its `data-state` and its value.
fn stat<'h>(html: &'h str, label: &str) -> (&'h str, &'h str) {
    let end = html
        .find(&format!(r#"<span class="mv-stat-label bv-meta">{label}</span>"#))
        .unwrap_or_else(|| panic!("no stat {label}: {html}"));
    let start = html[..end].rfind(r#"<div class="mv-stat" data-state=""#).unwrap()
        + r#"<div class="mv-stat" data-state=""#.len();
    let state = &html[start..start + html[start..].find('"').unwrap()];
    let v = html[start..end].find(r#"class="mv-stat-value"#).unwrap() + start;
    let v = v + html[v..].find('>').unwrap() + 1;
    (state, &html[v..v + html[v..].find('<').unwrap()])
}

#[test]
fn an_unread_work_root_is_not_nothing_needs_you() {
    // Every ask answered, and the work root unreadable: the needs list reads as empty with an
    // error. Its error alone keeps the calm claim off, and the header's hold count unknown.
    let m = all_answered(false);
    let work = m.path().join("work");
    std::fs::remove_dir_all(&work).unwrap();
    std::fs::write(&work, "not a directory").unwrap();
    let (r, shell) = web(&worktree(m.path()), json!({ "type": "decisions" }));
    assert!(!r.html.contains("mv-calm"), "{}", r.html);
    assert!(r.html.contains(r#"data-type="needs""#), "the needs card says what could not be read: {}", r.html);
    assert_eq!((shell.calm, shell.needs), (false, None), "{shell:?}");
    assert_eq!(shell.summary(), "No decision open; not everything that could need you was read");
    // The needs card does not read an empty list from a failed read as an all-clear.
    assert!(r.html.contains("Nothing needs you in what could be read."), "{}", r.html);
    assert!(!r.html.contains(">Nothing needs you.<"), "{}", r.html);
}

/// One open, undated ask in YAML, with an optional uid.
fn open_ask(id: &str, uid: Option<&str>, title: &str) -> String {
    let uid = uid.map(|u| format!("    uid: {u}\n")).unwrap_or_default();
    format!(
        "  - id: {id}\n{uid}    ask: \"{title}\"\n    class: decision\n    gates: [g]\n    blocking: false\n    preanswerable: true\n    exhausted: [\"read: x\", \"default: y\"]\n"
    )
}

#[test]
fn a_uid_equal_to_another_asks_id_marks_one_row_current() {
    // `a1`'s legacy id is `z`, and `z` is the other ask's uid: a link to `z` names only that one.
    let m = all_answered(false);
    let asks = m.path().join("workspace/.control/asks");
    std::fs::write(
        asks.join("clash.yaml"),
        format!("asks:\n{}{}", open_ask("z", Some("a1"), "Legacy"), open_ask("b0", Some("z"), "By uid")),
    )
    .unwrap();
    let (r, _) =
        web(&worktree(m.path()), json!({ "type": "decision", "props": { "ledger": "clash.yaml", "id": "z" } }));
    let current: Vec<_> = queue_links(&r.html).into_iter().filter(|(_, c)| *c).collect();
    assert_eq!(current, vec![("/decisions/clash.yaml/z".to_string(), true)], "{}", r.html);
}

#[test]
fn a_legacy_link_by_id_still_marks_the_asks_row() {
    // The ask is addressed by its uid `u1`; an old link names it by its id `old`. With no handle
    // `old` in the ledger, the id names the row: it is current, and the pager places it.
    let m = all_answered(false);
    let asks = m.path().join("workspace/.control/asks");
    std::fs::write(
        asks.join("legacy.yaml"),
        format!("asks:\n{}{}", open_ask("first", None, "First"), open_ask("old", Some("u1"), "Renamed")),
    )
    .unwrap();
    let (r, _) =
        web(&worktree(m.path()), json!({ "type": "decision", "props": { "ledger": "legacy.yaml", "id": "old" } }));
    let current: Vec<_> = queue_links(&r.html).into_iter().filter(|(_, c)| *c).collect();
    assert_eq!(current, vec![("/decisions/legacy.yaml/u1".to_string(), true)], "{}", r.html);
    assert!(r.html.contains("2 of 2"), "{}", r.html);
}

#[test]
fn the_pager_steps_over_rows_it_cannot_link() {
    // Undated asks sort by ledger: `0a`, the two that share `same` in `1dup`, then `2c`.
    let m = all_answered(false);
    let asks = m.path().join("workspace/.control/asks");
    std::fs::write(asks.join("0a.yaml"), format!("asks:\n{}", open_ask("x", None, "First"))).unwrap();
    std::fs::write(
        asks.join("1dup.yaml"),
        format!("asks:\n{}{}", open_ask("same", None, "A"), open_ask("same", None, "B")),
    )
    .unwrap();
    std::fs::write(asks.join("2c.yaml"), format!("asks:\n{}", open_ask("y", None, "Last"))).unwrap();
    let store = worktree(m.path());
    let (a, _) = web(&store, json!({ "type": "decision", "props": { "ledger": "0a.yaml", "id": "x" } }));
    assert!(a.html.contains(r#"href="/decisions/2c.yaml/y" rel="next""#), "{}", a.html);
    assert!(a.html.contains("1 of 4"), "the position counts every row: {}", a.html);
    let (c, _) = web(&store, json!({ "type": "decision", "props": { "ledger": "2c.yaml", "id": "y" } }));
    assert!(c.html.contains(r#"href="/decisions/0a.yaml/x" rel="prev""#), "{}", c.html);
}

#[test]
fn calm_counts_say_when_their_read_was_partial_or_failed() {
    let m = all_answered(false);
    let (r, _) = web(&worktree(m.path()), json!({ "type": "decisions" }));
    assert!(r.html.contains("mv-calm"), "{}", r.html);
    assert_eq!(stat(&r.html, "loops").0, "ok", "{}", r.html);
    assert!(!r.html.contains("mv-calm-note"), "{}", r.html);

    // The ticks ledger unread (drift comes from it) and one initiative that does not parse.
    std::fs::remove_file(m.path().join("broomva-home/ledger/loops/ticks.jsonl")).unwrap();
    std::fs::write(m.path().join("workspace/.control/initiatives/broken.yaml"), "not: [valid\n").unwrap();
    let (r, _) = web(&worktree(m.path()), json!({ "type": "decisions" }));
    assert!(r.html.contains("mv-calm"), "the calm screen stands on asks and holds alone: {}", r.html);
    for label in ["loops", "loops drifting", "initiatives live"] {
        let (state, value) = stat(&r.html, label);
        assert_eq!(state, "partial", "{label}: {}", r.html);
        assert!(value.starts_with("at least "), "{label} is a lower bound: {value}");
    }
    assert!(r.html.contains("Some counts read in part: they are lower bounds."), "{}", r.html);

    // The loop registry and the initiatives folder unreadable: unavailable, never zero.
    let loops = m.path().join("workspace/.broomva/loops");
    std::fs::remove_dir_all(&loops).unwrap();
    std::fs::write(&loops, "not a directory").unwrap();
    let initiatives = m.path().join("workspace/.control/initiatives");
    std::fs::remove_dir_all(&initiatives).unwrap();
    std::fs::write(&initiatives, "not a directory").unwrap();
    let (r, _) = web(&worktree(m.path()), json!({ "type": "decisions" }));
    for label in ["loops", "loops drifting", "initiatives live"] {
        assert_eq!(stat(&r.html, label), ("unavailable", "unavailable"), "{label}: {}", r.html);
    }
}

#[test]
fn the_current_queue_row_is_styled_by_the_mark_it_carries() {
    // The row carries aria-current="true" (the nav tab carries "page"); the stylesheet must
    // highlight what the markup marks, or the current decision is not shown as current.
    let m = machine();
    let (r, shell) = web(&worktree(m.path()), json!({ "type": "decisions" }));
    assert!(r.html.contains(r#"aria-current="true">"#), "{}", r.html);
    let doc = page::web(&r.html, "Decisions", "/decisions", &shell);
    assert!(doc.contains(r#".mv-qrow > a[aria-current="true"] {"#), "the current row's style");
    assert!(!doc.contains(r#".mv-qrow > a[aria-current="page"]"#), "a selector the markup never matches");
}

#[test]
fn work_held_for_you_is_not_nothing_needs_you() {
    // Every ask answered, but the fixture's work item is held for the owner (an owner step).
    let held = all_answered(false);
    copy_tree(&fixture().join("work/held-for-you"), &held.path().join("work/held-for-you"));
    let store = worktree(held.path());
    let (r, shell) = web(&store, json!({ "type": "decisions" }));
    assert!(!r.html.contains("mv-calm"), "{}", r.html);
    assert!(r.html.contains(r#"data-type="needs""#), "the needs card names the hold: {}", r.html);
    assert!(!shell.calm);
    assert_eq!(shell.needs, Some(1));
    assert_eq!(shell.summary(), "No decision open, 1 other hold for you");
    assert_eq!(shell.state(), "held");
    // Today's page under the same header: the header never says nothing needs you.
    let (_, today) = web(&store, json!({ "type": "today" }));
    assert_eq!(today.summary(), "No decision open, 1 other hold for you");
}

#[test]
fn an_unread_answers_read_is_not_nothing_needs_you() {
    // Every ask answered and committed; the tree reads whole at `main`, but the working tree's
    // answers cannot be read (its asks folder is a file). Calm needs every source Maestro reads.
    let m = all_answered(false);
    let ws = m.path().join("workspace");
    for args in [&["init", "-q", "-b", "main"][..], &["add", "."], &["commit", "-q", "-m", "seed"]] {
        let ok = std::process::Command::new("git")
            .current_dir(&ws)
            .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }
    let store = Store::new(Paths::under(m.path().to_path_buf()), TreeSource::GitRev("main".into()));
    let (r, shell) = web(&store, json!({ "type": "decisions" }));
    assert!(r.html.contains("mv-calm") && shell.calm, "whole, before the break: {}", r.html);
    let dir = ws.join(".control/asks");
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::write(&dir, "not a directory").unwrap();
    let (r, shell) = web(&store, json!({ "type": "decisions" }));
    assert!(!r.html.contains("mv-calm"), "{}", r.html);
    assert!(!shell.calm && shell.summary() != "Nothing needs you", "{shell:?}");
}

#[test]
fn an_unread_queue_is_a_question_mark_never_zero() {
    let empty = tempfile::tempdir().unwrap();
    let (r, _) =
        web(&worktree(empty.path()), json!({ "type": "decision", "props": { "ledger": "a.yaml", "id": "x" } }));
    assert!(
        r.html.contains(r#"<aside class="mv-queue" aria-label="Waiting on you" data-state="unavailable">"#),
        "{}",
        r.html
    );
    assert!(r.html.contains(r#"<span class="mv-queue-count">?</span>"#), "{}", r.html);
    assert!(r.html.contains("The asks could not be read. This is not the same as none."), "{}", r.html);
}

#[test]
fn an_ask_whose_handle_is_shared_is_never_opened_or_linked() {
    // `0dup.yaml` sorts first among undated asks, and both its asks share the handle `same`.
    let m = all_answered(false);
    let asks = m.path().join("workspace/.control/asks");
    let ask = |id: &str, title: &str| {
        format!(
            "  - id: {id}\n    ask: \"{title}\"\n    class: decision\n    gates: [g]\n    blocking: false\n    preanswerable: true\n    exhausted: [\"read: x\", \"default: y\"]\n"
        )
    };
    std::fs::write(asks.join("0dup.yaml"), format!("asks:\n{}{}", ask("same", "First"), ask("same", "Second")))
        .unwrap();
    std::fs::write(asks.join("a.yaml"), format!("asks:\n{}", ask("x", "The one to open"))).unwrap();
    let store = worktree(m.path());
    let (r, _) = web(&store, json!({ "type": "decisions" }));
    assert!(
        r.html.contains(r#"<article class="mv-decision" data-ask="a.yaml#x""#),
        "the first keyed ask opens: {}",
        r.html
    );
    let (d, _) = web(&store, json!({ "type": "decision", "props": { "ledger": "a.yaml", "id": "x" } }));
    for html in [&r.html, &d.html] {
        assert!(!html.contains("/decisions/0dup.yaml/same"), "a shared handle is never a link: {html}");
        assert!(html.contains("shares its handle"), "{html}");
    }
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
        needs: Some(3),
        calm: false,
        committed: Some("2 h".into()),
    };
    assert_eq!(shell.tree_short(), "main 0123456");
    assert_eq!(shell.summary(), "3 decisions for you, none blocking");
    assert_eq!(Shell { open: Some((1, 1)), ..shell.clone() }.summary(), "1 decision for you, 1 blocking");
    assert_eq!(Shell { tree: "worktree".into(), ..shell.clone() }.tree_short(), "worktree");
    // The commit is after the last `@`, cut by characters; an unresolved one is said.
    let short = |tree: &str| Shell { tree: tree.into(), ..shell.clone() }.tree_short();
    assert_eq!(short("git:origin/main@unresolved"), "main (commit unresolved)");
    assert_eq!(short("git:HEAD@{1}@0123456789ab"), "HEAD@{1} 0123456");
    assert_eq!(short("git:main@ééééééééé"), "main ééééééé");
    // A partial read is a lower bound on the badge and in the summary; calm wins only when set.
    let partial = Shell { partial: true, ..shell.clone() };
    assert_eq!((partial.badge(), partial.state()), ("3+".to_string(), "partial"));
    assert_eq!(
        Shell { open: Some((0, 0)), needs: Some(0), calm: true, ..shell.clone() }.summary(),
        "Nothing needs you"
    );
    assert_eq!(
        Shell { open: Some((0, 0)), needs: None, ..shell.clone() }.summary(),
        "No decision open; not everything that could need you was read"
    );
    let doc = page::web("<p>x</p>", "Today", "/today", &shell);
    assert!(doc.contains(r#"<a class="mv-tab" href="/today" aria-current="page">Today</a>"#), "{doc}");
    assert!(doc.contains(r#"<span class="mv-nav-count">3</span>"#), "{doc}");
    assert!(doc.contains("main 0123456</span> · committed 2 h ago"), "{doc}");
    assert!(
        !doc.contains("<script") && !doc.to_ascii_lowercase().contains("url("),
        "the web page runs and fetches nothing"
    );
}

#[test]
fn today_shows_answer_progress_and_unavailable_sources() {
    let m = all_answered(false);
    let ledger = m.path().join("workspace/.control/asks/done.yaml");
    let text = std::fs::read_to_string(&ledger).unwrap().replace("2026-10-02T00:00:00Z", "2026-10-23T10:00:00Z");
    std::fs::write(ledger, text).unwrap();
    let (r, _) = web(&worktree(m.path()), json!({ "type": "today" }));
    assert!(r.html.contains("Ship it?"), "{}", r.html);
    assert!(r.html.contains("You chose yes"), "{}", r.html);
    assert!(r.html.contains("On main · not yet known"), "{}", r.html);
    assert!(r.html.contains("Acted · not yet known"), "{}", r.html);
    assert!(r.html.contains("href=\"/decisions\""), "{}", r.html);
    let empty = tempfile::tempdir().unwrap();
    let (r, _) = web(&worktree(empty.path()), json!({ "type": "today" }));
    assert!(r.html.contains("Answers are unavailable"), "{}", r.html);
    assert!(!r.html.contains("No answers found today"), "{}", r.html);
}
