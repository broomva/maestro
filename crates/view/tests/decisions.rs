//! The Decisions screens (design page 11, screens 1 and 2) on the fixture machine plus one
//! ledger with every ask field they show (`tests/fixtures/decisions.yaml`).

use maestro_store::{Paths, Store, TreeSource};
use maestro_view::{Rendered, render_at, validate};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

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

fn at() -> jiff::Timestamp {
    "2026-10-23T12:00:00Z".parse().unwrap()
}

fn spec(block: Value) -> Value {
    json!({ "root": "s", "blocks": { "s": block } })
}

fn inbox() -> Value {
    spec(json!({ "type": "decisions" }))
}

fn detail(ledger: &str, id: &str) -> Value {
    spec(json!({ "type": "decision", "props": { "ledger": ledger, "id": id } }))
}

fn render(store: &Store, spec: &Value) -> Rendered {
    let v = validate(spec).unwrap();
    assert!(v.drops.is_empty(), "{:?}", v.drops);
    render_at(store, &v, at())
}

fn worktree(root: &Path) -> Store {
    Store::new(Paths::under(root.to_path_buf()), TreeSource::WorkingTree)
}

fn portable(html: &str, root: &Path) -> String {
    html.replace(&root.display().to_string(), "<fixture>").replace("><", ">\n<")
}

/// The text between `<tag class="cls"…>` and its close, for one-element checks.
fn tab_count(html: &str, tab: &str) -> usize {
    let at = html.find(&format!(r#"data-tab="{tab}">"#)).unwrap_or_else(|| panic!("no tab {tab}"));
    let rest = &html[at..];
    let open = rest.find(r#"<span class="mv-tab-count">"#).unwrap() + r#"<span class="mv-tab-count">"#.len();
    rest[open..open + rest[open..].find('<').unwrap()].trim_end_matches('+').parse().unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

#[test]
fn the_inbox_renders_from_the_fixture() {
    let m = machine();
    let r = render(&worktree(m.path()), &inbox());
    insta::assert_snapshot!("inbox", portable(&r.html, m.path()));
}

#[test]
fn the_decision_renders_from_the_fixture() {
    let m = machine();
    let r = render(&worktree(m.path()), &detail("decisions.yaml", "fleet-cost-1"));
    insta::assert_snapshot!("decision", portable(&r.html, m.path()));
}

#[test]
fn tabs_count_open_asks_and_add_up_to_all() {
    let m = machine();
    let html = render(&worktree(m.path()), &inbox()).html;
    let (all, d, a, i) = (
        tab_count(&html, "all"),
        tab_count(&html, "decisions"),
        tab_count(&html, "authority"),
        tab_count(&html, "ideas"),
    );
    // Open asks: demo-arc merge-now (decision) + pick-name (idea); alt-key alt-blocking (decision);
    // decisions.yaml context-cost (decision), merge-policy (authority), new-key (credential:
    // a Decisions row), jev-idea (idea). Answered and resolved asks are not rows.
    assert_eq!((all, d, a, i), (7, 4, 1, 2), "{html}");
    assert_eq!(all, d + a + i);
    assert_eq!(html.matches(r#"<li class="mv-irow""#).count(), all);
    // The tab the spec names is the one checked; Decisions by default.
    assert!(html.contains(r#"value="decisions" checked"#), "{html}");
    let ideas = render(&worktree(m.path()), &spec(json!({ "type": "decisions", "props": { "tab": "ideas" } }))).html;
    assert!(ideas.contains(r#"value="ideas" checked"#) && !ideas.contains(r#"value="decisions" checked"#));
    // broken.yaml does not parse: counts are lower bounds and say so.
    assert!(html.contains("at least 7 open"), "{html}");
    for tab in ["all", "decisions", "authority", "ideas"] {
        let at = html.find(&format!(r#"data-tab="{tab}">"#)).unwrap();
        let chip = &html[at..at + html[at..].find("</label>").unwrap()];
        assert!(chip.contains("+</span>"), "{tab} chip is not marked a lower bound: {chip}");
    }
}

#[test]
fn rows_are_grouped_by_project_blocking_first_with_age() {
    let m = machine();
    let html = render(&worktree(m.path()), &inbox()).html;
    let order: Vec<&str> = html
        .match_indices(r#"data-ask=""#)
        .map(|(i, _)| &html[i + 10..][..html[i + 10..].find('"').unwrap()])
        .collect();
    // Every row's article repeats its handle; the row's own comes first.
    let rows: Vec<&str> = order.iter().step_by(2).copied().collect();
    assert_eq!(
        rows,
        [
            // demo: blocks, its ledger opened 10-01; blocking rows first, then the idea.
            "demo-arc.yaml#merge-now",
            "alt-key.yaml#alt-blocking",
            "demo-arc.yaml#pick-name",
            // maestro: blocks, oldest ask 10-12; the blocking key before that older ask.
            "decisions.yaml#new-key",
            "decisions.yaml#merge-policy",
            // fleet: blocks, oldest 10-17.
            "decisions.yaml#fleet-cost-1",
            // research: nothing blocks.
            "decisions.yaml#jev-idea",
        ],
        "{html}"
    );
    assert!(
        html.contains(
            r#"<span class="mv-group-name">fleet</span><span class="mv-group-age" data-tab="all">6 d</span>"#
        ),
        "{html}"
    );
    // A group's age per tab is its oldest ask in that tab: maestro is 11 days old in All (the
    // authority ask of 10-12), 1 day in Decisions (only the key asked 10-22).
    let maestro = r#"<span class="mv-group-age" data-tab="all">11 d</span><span class="mv-group-age" data-tab="decisions">1 d</span><span class="mv-group-age" data-tab="authority">11 d</span>"#;
    assert!(html.contains(maestro), "{html}");
    assert!(html.contains("Recommended: Rotate long orchestrators by handoff"), "{html}");
}

#[test]
fn the_decision_shows_every_field_it_names() {
    let m = machine();
    let html = render(&worktree(m.path()), &detail("decisions.yaml", "fleet-cost-1")).html;
    for shown in [
        "devteam ran out twice this week; context length is the lever.",
        "If you don't answer: wait: nothing changes and spend continues",
        "about 19% of sessions over 400K context",
        "Which context-cost policy should the fleet run?",
        "Why you: It trades quality",
        r#"<span class="mv-badge" data-tone="info">Recommended</span>"#,
        r#"<span class="mv-badge" data-tone="success">Reversible</span>"#,
        r#"<span class="mv-badge">Status quo</span>"#,
        "<dt class=\"bv-meta\">Undo</dt><dd>Turn compaction off</dd>",
        "<dt class=\"bv-meta\">Time</dt><dd>One week</dd>",
        r#"<a href="https://github.com/broomva/workspace/pull/983" target="_blank" rel="noopener noreferrer">Cost-levers study #983</a>"#,
        r#"<a href="https://linear.app/broomva/issue/BRO-3001""#,
        r#"<span class="bv-code">session 2fb204ca</span>"#,
        "What the agent tried first",
        r#"<span class="bv-code">reroute</span>: no standing grant covers it"#,
    ] {
        assert!(html.contains(shown), "missing {shown}\n{html}");
    }
    // A source the plugin drops (a url that is not https) is not shown at all, as in the plugin.
    assert!(!html.contains("javascript:") && !html.contains("not a link"), "{html}");
}

#[test]
fn the_answer_bar_writes_nothing() {
    let m = machine();
    let html = render(&worktree(m.path()), &detail("decisions.yaml", "fleet-cost-1")).html;
    assert!(html.contains("mv-answerbar"), "{html}");
    assert!(html.contains("Answer in Paseo · Maestro · Decisions"), "{html}");
    for writer in ["<form", "<button", "formaction", "<script", " onclick", "data-action"] {
        assert!(!html.contains(writer), "{writer} in {html}");
    }
    assert!(html.contains("<textarea class=\"mv-note-field\" rows=\"1\" disabled"), "{html}");
    // An answered ask shows its answer and no answer bar.
    let done = render(&worktree(m.path()), &detail("decisions.yaml", "done-before")).html;
    assert!(done.contains("Answered: Rotate it · by owner"), "{done}");
    assert!(!done.contains("mv-answerbar") && !done.contains("Choose one"), "{done}");
    // An ask with no default says it waits; a positional count is only for open asks.
    let auth = render(&worktree(m.path()), &detail("decisions.yaml", "merge-policy")).html;
    assert!(auth.contains("No default: irreversible. It waits for you."), "{auth}");
    assert!(auth.contains(r#"data-tone="danger">Not reversible</span>"#), "{auth}");
}

#[test]
fn ledger_text_is_escaped_on_both_screens() {
    let m = machine();
    for s in [inbox(), detail("decisions.yaml", "new-key")] {
        let html = render(&worktree(m.path()), &s).html;
        assert!(html.contains("Paste the &lt;b&gt;deploy&lt;/b&gt; key"), "{html}");
        assert!(!html.contains("<b>deploy"), "{html}");
    }
}

#[test]
fn an_unread_asks_ledger_is_unavailable_never_zero() {
    let empty = tempfile::tempdir().unwrap();
    for s in [inbox(), detail("decisions.yaml", "fleet-cost-1")] {
        let html = render(&worktree(empty.path()), &s).html;
        assert!(html.contains(r#"data-state="unavailable""#), "{html}");
        assert!(html.contains("This is not the same as none."), "{html}");
        assert!(!html.contains("to answer") && !html.contains("mv-tab-count"), "{html}");
    }
    insta::assert_snapshot!(
        "inbox_unavailable",
        portable(&render(&worktree(empty.path()), &inbox()).html, empty.path())
    );
}

/// The decisions ledger committed, then answered in the working tree only.
fn answered_locally() -> (tempfile::TempDir, Store) {
    let m = machine();
    let ws = m.path().join("workspace");
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "seed"]);
    let file = ws.join(".control/asks/decisions.yaml");
    let text = std::fs::read_to_string(&file).unwrap();
    let answered = text.replacen(
        "    gates: [fleet-cost]\n",
        "    gates: [fleet-cost]\n    answer_option: rotate\n    answered_at: \"2026-10-23T11:00:00Z\"\n    status: resolved\n",
        1,
    );
    assert_ne!(text, answered);
    std::fs::write(&file, answered).unwrap();
    let store = Store::new(Paths::under(m.path().to_path_buf()), TreeSource::GitRev("main".into()));
    (m, store)
}

#[test]
fn an_answer_in_the_working_tree_waits_to_land() {
    let (_m, store) = answered_locally();
    let html = render(&store, &inbox()).html;
    // The working tree's broken.yaml is unread there too, so the waiting count is a lower bound,
    // in the sentence and in the banner alike.
    assert!(html.contains("at least 7 open · at least 1 waiting to land"), "{html}");
    assert!(html.contains(r#"<p class="mv-banner-title">at least 1 waiting to land</p>"#), "{html}");
    assert!(html.contains(r#"<span class="mv-irow-rec" data-tone="success">Waiting to land</span>"#), "{html}");
    let d = render(&store, &detail("decisions.yaml", "fleet-cost-1")).html;
    assert!(d.contains("Answered in the working tree; waiting to land on"), "{d}");
    assert!(!d.contains("mv-answerbar"), "{d}");
}

#[test]
fn an_unread_working_tree_is_not_none_waiting() {
    let (m, store) = answered_locally();
    // The local asks directory replaced by a file: the working tree's answers cannot be read.
    let dir = m.path().join("workspace/.control/asks");
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::write(&dir, "not a directory").unwrap();
    let html = render(&store, &inbox()).html;
    assert!(html.contains("Answers waiting to land could not be read. This is not the same as none."), "{html}");
    assert!(html.contains(r#"waiting to land: <span class="mv-unavailable">unavailable</span>"#), "{html}");
    assert!(!html.contains("0 waiting to land"), "{html}");
}

#[test]
fn a_project_prop_keeps_one_project() {
    let m = machine();
    let html =
        render(&worktree(m.path()), &spec(json!({ "type": "decisions", "props": { "project": "maestro" } }))).html;
    assert_eq!(html.matches(r#"<li class="mv-irow""#).count(), 2, "{html}");
    assert_eq!(tab_count(&html, "all"), 2);
}

#[test]
fn an_ask_archived_in_the_working_tree_is_not_waiting_to_land() {
    let m = machine();
    let ws = m.path().join("workspace");
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "seed"]);
    let file = ws.join(".control/asks/decisions.yaml");
    let text = std::fs::read_to_string(&file).unwrap();
    let archived = text.replacen("    gates: [fleet-cost]\n", "    gates: [fleet-cost]\n    status: archived\n", 1);
    assert_ne!(text, archived);
    std::fs::write(&file, archived).unwrap();
    let store = Store::new(Paths::under(m.path().to_path_buf()), TreeSource::GitRev("main".into()));
    let html = render(&store, &inbox()).html;
    assert!(html.contains("at least 0 waiting to land"), "{html}");
    assert!(!html.contains("Waiting to land</span>"), "{html}");
    let d = render(&store, &detail("decisions.yaml", "fleet-cost-1")).html;
    assert!(!d.contains("Answered in the working tree"), "{d}");
}

#[test]
fn two_inboxes_on_one_page_never_share_a_radio_group() {
    let m = machine();
    let two = json!({ "root": "m", "blocks": {
        "m": { "type": "stack", "children": ["a", "b"] },
        "a": { "type": "decisions" },
        "b": { "type": "decisions", "props": { "tab": "ideas" } } } });
    let html = render(&worktree(m.path()), &two).html;
    let attr = |key: &str| -> Vec<String> {
        let pat = format!(" {key}=\"");
        html.match_indices(&pat)
            .map(|(i, _)| {
                let rest = &html[i + pat.len()..];
                rest[..rest.find('"').unwrap()].to_string()
            })
            .collect()
    };
    let names: std::collections::BTreeSet<String> = attr("name").into_iter().collect();
    assert_eq!(names.len(), 2, "{names:?}");
    let ids = attr("id");
    let unique: std::collections::BTreeSet<&String> = ids.iter().collect();
    assert_eq!(ids.len(), unique.len(), "{ids:?}");
}

#[test]
fn a_group_that_holds_nothing_sorts_after_one_that_blocks_even_when_older() {
    let m = machine();
    std::fs::write(
        m.path().join("workspace/.control/asks/old.yaml"),
        "asks:\n  - id: ancient\n    class: idea\n    project: attic\n    asked_at: \"2026-01-01\"\n    headline: \"An old idea\"\n    blocking: false\n    exhausted: [\"read: x\"]\n",
    )
    .unwrap();
    let html = render(&worktree(m.path()), &inbox()).html;
    let attic = html.find(r#"<span class="mv-group-name">attic</span>"#).unwrap();
    let fleet = html.find(r#"<span class="mv-group-name">fleet</span>"#).unwrap();
    assert!(fleet < attic, "a blocking group must come first: {html}");
}

#[test]
fn an_empty_tab_says_so() {
    let m = machine();
    let html =
        render(&worktree(m.path()), &spec(json!({ "type": "decisions", "props": { "project": "research" } }))).html;
    for tab in ["decisions", "authority"] {
        let note = format!(r#"data-tab="{tab}">Nothing to answer here in what could be read.</p>"#);
        assert!(html.contains(&note), "{html}");
    }
    assert!(!html.contains(r#"data-tab="ideas">Nothing"#), "{html}");
}

#[test]
fn a_uuid_agent_is_named_by_its_first_8() {
    let m = machine();
    let html = render(&worktree(m.path()), &detail("decisions.yaml", "jev-idea")).html;
    // Ledger text sits in its own bdi, so a bidi override in it cannot reorder Maestro's words.
    assert!(html.contains(" by <bdi>9f3c2a1b</bdi>"), "{html}");
    let inbox_html = render(&worktree(m.path()), &inbox()).html;
    assert!(inbox_html.contains("<bdi>claude:coordinator</bdi> · <bdi>cost</bdi>"), "{inbox_html}");
    assert!(!html.contains("9f3c2a1b-"), "{html}");
}

#[test]
fn the_decision_on_an_unread_or_ambiguous_ledger_says_so() {
    let m = machine();
    // broken.yaml does not parse: the decision is unread, which is not the same as missing.
    let html = render(&worktree(m.path()), &detail("broken.yaml", "x")).html;
    assert!(html.contains("could not be read. This is not the same as missing."), "{html}");
    // An ask with a uid is addressed by it, and the screen says which.
    let html = render(&worktree(m.path()), &detail("decisions.yaml", "context-cost")).html;
    assert!(html.contains("is addressed by its uid"), "{html}");
    assert!(html.contains("decisions.yaml#fleet-cost-1"), "{html}");
}

#[test]
fn a_partial_working_tree_makes_every_count_a_lower_bound() {
    let m = machine();
    let asks = m.path().join("workspace/.control/asks");
    // A clean tree read: only ledgers that parse whole.
    std::fs::remove_file(asks.join("broken.yaml")).unwrap();
    std::fs::remove_file(asks.join("alt-key.yaml")).unwrap();
    let ws = m.path().join("workspace");
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "seed"]);
    let store = Store::new(Paths::under(m.path().to_path_buf()), TreeSource::GitRev("main".into()));
    let clean = render(&store, &inbox()).html;
    assert!(clean.contains(">6 open · 0 waiting to land<"), "{clean}");
    // A local ledger that does not parse may hold answers: nothing on the screen is exact now.
    std::fs::write(asks.join("local.yaml"), "asks: [unclosed\n").unwrap();
    let html = render(&store, &inbox()).html;
    assert!(html.contains("at least 6 open · at least 0 waiting to land"), "{html}");
    assert!(html.contains(r#"<span class="mv-tab-count">6+</span>"#), "{html}");
}

#[test]
fn an_ambiguous_handle_offers_no_answer_and_no_restore_key() {
    let m = machine();
    std::fs::write(
        m.path().join("workspace/.control/asks/dup.yaml"),
        "asks:\n  - id: same\n    class: decision\n    project: dup\n    headline: \"First\"\n    exhausted: [\"read: x\"]\n  - id: same\n    class: decision\n    project: dup\n    headline: \"Second\"\n    exhausted: [\"read: x\"]\n",
    )
    .unwrap();
    let html = render(&worktree(m.path()), &spec(json!({ "type": "decisions", "props": { "project": "dup" } }))).html;
    assert_eq!(html.matches(r#"data-ambiguous="true""#).count(), 2, "{html}");
    // The bridge restores open rows by `.mv-irow[data-ask]`: an ambiguous row carries none.
    assert!(!html.contains(r#"class="mv-irow" data-tab="decisions" data-ask="#), "{html}");
    assert!(!html.contains("mv-answerbar"), "{html}");
    assert_eq!(html.matches("Paseo refuses an answer to an ambiguous handle").count(), 2, "{html}");
    let d = render(&worktree(m.path()), &detail("dup.yaml", "same")).html;
    assert!(d.contains("names 2 asks, so none is shown"), "{d}");
}
