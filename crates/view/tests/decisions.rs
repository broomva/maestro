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
            r#"<span class="mv-group-name"><bdi>fleet</bdi></span><span class="mv-group-age" data-tab="all">6 d</span>"#
        ),
        "{html}"
    );
    // A group's age per tab is its oldest ask in that tab: maestro is 11 days old in All (the
    // authority ask of 10-12), 1 day in Decisions (only the key asked 10-22).
    let maestro = r#"<span class="mv-group-age" data-tab="all">11 d</span><span class="mv-group-age" data-tab="decisions">1 d</span><span class="mv-group-age" data-tab="authority">11 d</span>"#;
    assert!(html.contains(maestro), "{html}");
    assert!(html.contains("Recommended: <bdi>Rotate long orchestrators by handoff</bdi>"), "{html}");
}

#[test]
fn the_decision_shows_every_field_it_names() {
    let m = machine();
    let html = render(&worktree(m.path()), &detail("decisions.yaml", "fleet-cost-1")).html;
    for shown in [
        "devteam ran out twice this week; context length is the lever.",
        "If you don't answer: <bdi>wait: nothing changes and spend continues",
        "about 19% of sessions over 400K context",
        "Which context-cost policy should the fleet run?",
        "Why you: <bdi>It trades quality",
        r#"<span class="mv-badge" data-tone="info">Recommended</span>"#,
        r#"<span class="mv-badge" data-tone="success">Reversible</span>"#,
        r#"<span class="mv-badge">Status quo</span>"#,
        "<dt class=\"bv-meta\">Undo</dt><dd>Turn compaction off</dd>",
        "<dt class=\"bv-meta\">Time</dt><dd>One week</dd>",
        r#"<a href="https://github.com/broomva/workspace/pull/983" target="_blank" rel="noopener noreferrer">Cost-levers study #983</a>"#,
        r#"<a href="https://linear.app/broomva/issue/BRO-3001""#,
        r#"<span class="bv-code">session 2fb204ca</span>"#,
        "What the agent tried first",
        r#"<span class="bv-code">reroute</span>: <bdi>no standing grant covers it"#,
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
    assert!(html.contains("shows whether it has been answered since, and is where to answer it."), "{html}");
    for writer in ["<form", "<button", "formaction", "<script", " onclick", "data-action"] {
        assert!(!html.contains(writer), "{writer} in {html}");
    }
    assert!(html.contains("<textarea class=\"mv-note-field\" rows=\"1\" disabled"), "{html}");
    // An answered ask shows its answer and no answer bar.
    let done = render(&worktree(m.path()), &detail("decisions.yaml", "done-before")).html;
    assert!(done.contains("Answered: <bdi>Rotate it</bdi> · by <bdi>owner</bdi>"), "{done}");
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
        assert!(!html.contains("No ledger named"), "an unread tree is not a missing ledger: {html}");
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
    assert!(html.contains("at least 7 open · at least 1 answered, not yet landed"), "{html}");
    assert!(
        html.contains(
            r#"<p class="mv-banner-title">at least 1 answered in this checkout or on maestro/answers (this clone's, or origin's as last fetched), not yet on"#
        ),
        "{html}"
    );
    assert!(html.contains(r#"<span class="mv-irow-rec" data-tone="success">Waiting to land</span>"#), "{html}");
    let d = render(&store, &detail("decisions.yaml", "fleet-cost-1")).html;
    assert!(
        d.contains(
            "Answered in this checkout or on maestro/answers (this clone's, or origin's as last fetched); not yet on"
        ),
        "{d}"
    );
    // Answered, if not landed: what silence would do no longer applies.
    assert!(!d.contains("If you don't answer"), "{d}");
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
    assert!(
        html.contains("Answers given and not yet landed could not be read. This is not the same as none."),
        "{html}"
    );
    assert!(html.contains(r#"answered, not yet landed: <span class="mv-unavailable">unavailable</span>"#), "{html}");
    assert!(!html.contains("0 answered, not yet landed"), "{html}");
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
    assert!(html.contains("at least 0 answered, not yet landed"), "{html}");
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
    let attic = html.find(r#"<span class="mv-group-name"><bdi>attic</bdi></span>"#).unwrap();
    let fleet = html.find(r#"<span class="mv-group-name"><bdi>fleet</bdi></span>"#).unwrap();
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
    assert!(clean.contains(">6 open · 0 answered, not yet landed<"), "{clean}");
    // A local ledger that does not parse may hold answers: nothing on the screen is exact now.
    std::fs::write(asks.join("local.yaml"), "asks: [unclosed\n").unwrap();
    let html = render(&store, &inbox()).html;
    assert!(html.contains("at least 6 open · at least 0 answered, not yet landed"), "{html}");
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

/// The fixture machine in git mode at a clean revision (only ledgers that parse whole),
/// with `local` applied to the working tree afterwards.
fn clean_at_main(local: impl FnOnce(&Path)) -> (tempfile::TempDir, Store) {
    let m = machine();
    let asks = m.path().join("workspace/.control/asks");
    std::fs::remove_file(asks.join("broken.yaml")).unwrap();
    std::fs::remove_file(asks.join("alt-key.yaml")).unwrap();
    let ws = m.path().join("workspace");
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "seed"]);
    local(&asks);
    let store = Store::new(Paths::under(m.path().to_path_buf()), TreeSource::GitRev("main".into()));
    (m, store)
}

#[test]
fn an_unread_working_tree_never_invites_an_answer() {
    let (_m, store) = clean_at_main(|asks| {
        std::fs::remove_dir_all(asks).unwrap();
        std::fs::write(asks, "not a directory").unwrap();
    });
    let html = render(&store, &inbox()).html;
    // The card names the working tree's failure, so every "at least" has its reason.
    assert!(html.contains(r#"data-type="decisions" data-state="partial""#), "{html}");
    assert!(html.contains("worktree:"), "{html}");
    assert!(!html.contains("mv-answerbar") && !html.contains("Recommended: "), "{html}");
    assert_eq!(html.matches("Not offered here").count(), 6, "{html}");
    let d = render(&store, &detail("decisions.yaml", "fleet-cost-1")).html;
    assert!(d.contains(r#"data-type="decision" data-state="partial""#), "{d}");
    assert!(d.contains("Maestro cannot tell for sure whether this ask is answered"), "{d}");
    assert!(!d.contains("mv-answerbar"), "{d}");
}

#[test]
fn one_unread_working_tree_ledger_is_unknown_and_the_rest_are_not() {
    let (_m, store) = clean_at_main(|asks| {
        std::fs::write(asks.join("decisions.yaml"), "asks: [unclosed\n").unwrap();
    });
    let html = render(&store, &inbox()).html;
    assert!(html.contains(r#"data-type="decisions" data-state="partial""#), "{html}");
    assert_eq!(html.matches("Not offered here").count(), 4, "decisions.yaml's open asks: {html}");
    // demo-arc.yaml read cleanly in both trees: its rows still offer the owner path.
    let demo = &html[html.find(r#"data-ask="demo-arc.yaml#merge-now""#).unwrap()..];
    let demo = &demo[..demo[1..].find(r#"<li class="mv-irow""#).map_or(demo.len(), |n| n + 1)];
    assert!(demo.contains("Recommended: <bdi>Merge now</bdi>") && demo.contains("mv-answerbar"), "{demo}");
    let d = render(&store, &detail("decisions.yaml", "fleet-cost-1")).html;
    assert!(d.contains(r#"data-state="partial""#) && !d.contains("mv-answerbar"), "{d}");
    // The other ledger's decision stands on reads that cover it: no error, the owner path offered.
    let other = render(&store, &detail("demo-arc.yaml", "merge-now")).html;
    assert!(other.contains(r#"data-type="decision" data-state="ok""#), "{other}");
    assert!(other.contains("mv-answerbar"), "{other}");
}

#[test]
fn the_row_recommends_the_recommended_option_not_the_first() {
    let m = machine();
    let html = render(&worktree(m.path()), &inbox()).html;
    assert!(html.contains("Recommended: <bdi>Paste it now</bdi>"), "{html}");
    assert!(!html.contains("Recommended: Later"), "{html}");
    // An idea is answered under Initiatives, and only with develop, park or kill: no option of
    // it is put forward as the answer here.
    assert!(!html.contains("Recommended: Develop it"), "{html}");
    let idea = render(&worktree(m.path()), &detail("decisions.yaml", "jev-idea")).html;
    assert!(idea.contains("Paseo · Maestro · Initiatives shows whether") && !idea.contains("(recommended:"), "{idea}");
}

#[test]
fn a_default_is_said_only_while_the_ask_is_open() {
    let m = machine();
    let done = render(&worktree(m.path()), &detail("decisions.yaml", "done-before")).html;
    assert!(!done.contains("If you don't answer"), "{done}");
}

#[test]
fn uids_and_ids_are_separate_namespaces_as_in_the_plugin() {
    let m = machine();
    std::fs::write(
        m.path().join("workspace/.control/asks/ns.yaml"),
        "asks:\n  - id: px\n    uid: shared\n    project: ns\n    headline: \"By uid\"\n    exhausted: [\"read: x\"]\n  - id: shared\n    project: ns\n    headline: \"By id\"\n    exhausted: [\"read: x\"]\n  - id: px\n    project: ns\n    headline: \"Plain px\"\n    exhausted: [\"read: x\"]\n",
    )
    .unwrap();
    let html = render(&worktree(m.path()), &spec(json!({ "type": "decisions", "props": { "project": "ns" } }))).html;
    // The plugin answers all three: a uid and an id among uid-less asks never collide (`px` is
    // one ask's id and the other's uid-less id), so each offers the owner path.
    assert!(!html.contains("data-ambiguous"), "{html}");
    assert_eq!(html.matches("mv-answerbar").count(), 3, "{html}");
    // Two share the text `shared`, so neither keys a row for the bridge; `px` alone does.
    assert!(!html.contains(r#"data-tab="decisions" data-ask="ns.yaml#shared""#), "{html}");
    assert!(html.contains(r#"class="mv-irow" data-tab="decisions" data-ask="ns.yaml#px""#), "{html}");
    // A handle is a uid first: `shared` addresses the ask whose uid it is.
    let d = render(&worktree(m.path()), &detail("ns.yaml", "shared")).html;
    assert!(d.contains("By uid") && !d.contains("By id"), "{d}");
}

#[test]
fn a_waiting_answer_is_placed_by_the_plugins_key_not_its_text() {
    // `shared` is one ask's uid and another's uid-less id. The one with the uid is answered in
    // the working tree only. The plugin keys answers `u:shared` and `i:shared`, so only it waits.
    let m = machine();
    let asks = m.path().join("workspace/.control/asks");
    std::fs::remove_file(asks.join("broken.yaml")).unwrap();
    std::fs::remove_file(asks.join("alt-key.yaml")).unwrap();
    let open = "asks:\n  - id: px\n    uid: shared\n    project: ns\n    headline: \"By uid\"\n    exhausted: [\"read: x\"]\n  - id: shared\n    project: ns\n    headline: \"By id\"\n    exhausted: [\"read: x\"]\n";
    std::fs::write(asks.join("ns.yaml"), open).unwrap();
    let ws = m.path().join("workspace");
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "seed"]);
    let answered = open.replacen("    headline: \"By uid\"\n", "    headline: \"By uid\"\n    answer: \"yes\"\n", 1);
    assert_ne!(open, answered);
    std::fs::write(asks.join("ns.yaml"), answered).unwrap();
    let store = Store::new(Paths::under(m.path().to_path_buf()), TreeSource::GitRev("main".into()));
    let html = render(&store, &spec(json!({ "type": "decisions", "props": { "project": "ns" } }))).html;
    assert!(
        html.contains(r#"mv-irow-title">By uid</span><span class="mv-irow-rec" data-tone="success">Waiting to land"#),
        "{html}"
    );
    assert!(!html.contains(r#"mv-irow-title">By id</span><span class="mv-irow-rec" data-tone="success">"#), "{html}");
    // One project's open count stands alone; the checkout's count is the banner's, and it says so.
    assert!(html.contains(">2 open<"), "{html}");
    assert!(
        html.contains(r#"<p class="mv-banner-title">1 answered in this checkout or on maestro/answers (this clone's, or origin's as last fetched), not yet on"#),
        "{html}"
    );
    assert!(html.contains("Counted for the whole checkout, not this project alone."), "{html}");
    // The uid-less ask is open and nothing answers it: the owner path is offered for it alone.
    assert_eq!(html.matches("mv-answerbar").count(), 1, "{html}");
    assert!(!html.contains("Not offered here"), "{html}");
}

#[test]
fn an_entry_that_did_not_read_withholds_its_ledgers_owner_path() {
    // The second entry has a uid but no id: it does not read, yet the plugin counts its uid and
    // refuses `u1` as ambiguous. Nothing in that ledger is offered as answerable.
    let m = machine();
    std::fs::write(
        m.path().join("workspace/.control/asks/amb.yaml"),
        "asks:\n  - id: a\n    uid: u1\n    project: amb\n    headline: \"Pick A?\"\n    exhausted: [\"read: x\"]\n    options:\n      - { id: x, label: Do X, recommended: true }\n  - uid: u1\n    headline: \"same uid, no id\"\n",
    )
    .unwrap();
    let html = render(&worktree(m.path()), &spec(json!({ "type": "decisions", "props": { "project": "amb" } }))).html;
    assert!(!html.contains("mv-answerbar") && !html.contains("Recommended: Do X"), "{html}");
    assert!(html.contains("Not offered here"), "{html}");
    let d = render(&worktree(m.path()), &detail("amb.yaml", "u1")).html;
    assert!(!d.contains("mv-answerbar") && d.contains(r#"data-state="partial""#), "{d}");
}

#[test]
fn an_answer_waiting_in_another_ledger_is_not_this_ones() {
    // twin.yaml holds an ask with merge-now's id; only twin's is answered in the working tree.
    let m = machine();
    let asks = m.path().join("workspace/.control/asks");
    std::fs::remove_file(asks.join("broken.yaml")).unwrap();
    std::fs::remove_file(asks.join("alt-key.yaml")).unwrap();
    let twin = "asks:\n  - id: merge-now\n    project: twin\n    headline: \"Twin\"\n    exhausted: [\"read: x\"]\n";
    std::fs::write(asks.join("twin.yaml"), twin).unwrap();
    let ws = m.path().join("workspace");
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "seed"]);
    std::fs::write(asks.join("twin.yaml"), twin.replace("    headline", "    answer: \"done\"\n    headline")).unwrap();
    let store = Store::new(Paths::under(m.path().to_path_buf()), TreeSource::GitRev("main".into()));
    let d = render(&store, &detail("demo-arc.yaml", "merge-now")).html;
    assert!(!d.contains("not yet on") && d.contains("mv-answerbar"), "{d}");
    let t = render(&store, &detail("twin.yaml", "merge-now")).html;
    assert!(t.contains("not yet on"), "{t}");
}

#[test]
fn an_answer_on_the_answers_branch_waits_to_land() {
    // Paseo's plugin lands an answer on maestro/answers before its batch PR merges (spec R2).
    let m = machine();
    let ws = m.path().join("workspace");
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "seed"]);
    git(&ws, &["checkout", "-q", "-b", "maestro/answers"]);
    let file = ws.join(".control/asks/decisions.yaml");
    let text = std::fs::read_to_string(&file).unwrap();
    let answered = text.replacen(
        "    gates: [fleet-cost]\n",
        "    gates: [fleet-cost]\n    answer_option: rotate\n    answered_at: \"2026-10-23T11:00:00Z\"\n",
        1,
    );
    assert_ne!(text, answered);
    std::fs::write(&file, answered).unwrap();
    git(&ws, &["commit", "-q", "-am", "answer"]);
    // The working tree is back on main: only the branch holds the answer.
    git(&ws, &["checkout", "-q", "main"]);
    let store = Store::new(Paths::under(m.path().to_path_buf()), TreeSource::GitRev("main".into()));
    let d = render(&store, &detail("decisions.yaml", "fleet-cost-1")).html;
    assert!(d.contains("not yet on") && !d.contains("mv-answerbar"), "{d}");
    let html = render(&store, &inbox()).html;
    assert!(html.contains("or on maestro/answers"), "{html}");
}

#[test]
fn an_archived_ask_is_said_archived_and_offers_nothing() {
    let m = machine();
    std::fs::write(
        m.path().join("workspace/.control/asks/arch.yaml"),
        "asks:\n  - id: gone\n    headline: \"Gone\"\n    status: archived\n    exhausted: [\"read: x\"]\n    options:\n      - { id: x, label: X, recommended: true }\n",
    )
    .unwrap();
    let d = render(&worktree(m.path()), &detail("arch.yaml", "gone")).html;
    assert!(d.contains("Archived: <bdi>archived</bdi>.") && !d.contains("mv-answerbar"), "{d}");
}

/// A git workspace at `main` with a `maestro/answers` branch made by `on_branch` (run on the
/// branch, then committed); the working tree is left on `main`.
fn with_answers_branch(on_branch: impl FnOnce(&Path)) -> (tempfile::TempDir, Store) {
    let m = machine();
    let ws = m.path().join("workspace");
    let asks = ws.join(".control/asks");
    std::fs::remove_file(asks.join("broken.yaml")).unwrap();
    std::fs::remove_file(asks.join("alt-key.yaml")).unwrap();
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "seed"]);
    git(&ws, &["checkout", "-q", "-b", "maestro/answers"]);
    on_branch(&asks);
    git(&ws, &["add", "-A"]);
    git(&ws, &["commit", "-q", "-m", "branch"]);
    git(&ws, &["checkout", "-q", "main"]);
    let store = Store::new(Paths::under(m.path().to_path_buf()), TreeSource::GitRev("main".into()));
    (m, store)
}

#[test]
fn an_answers_branch_that_does_not_read_is_unavailable_never_none() {
    let (m, store) = with_answers_branch(|asks| {
        std::fs::remove_dir_all(asks).unwrap();
        std::fs::write(asks, "not a directory").unwrap();
    });
    // No local asks directory either: the branch was the only place answers could wait.
    std::fs::remove_dir_all(m.path().join("workspace/.control/asks")).unwrap();
    let html = render(&store, &inbox()).html;
    assert!(html.contains(r#"answered, not yet landed: <span class="mv-unavailable">unavailable</span>"#), "{html}");
    assert!(
        html.contains("Answers given and not yet landed could not be read. This is not the same as none."),
        "{html}"
    );
    assert!(!html.contains("mv-answerbar"), "{html}");
    let d = render(&store, &detail("decisions.yaml", "fleet-cost-1")).html;
    assert!(d.contains(r#"data-state="partial""#), "{d}");
    assert!(d.contains("Maestro cannot tell for sure whether this ask is answered"), "{d}");
    assert!(!d.contains("mv-answerbar"), "{d}");
}

#[test]
fn a_broken_ledger_on_the_answers_branch_makes_only_its_asks_unknown() {
    let (_m, store) = with_answers_branch(|asks| {
        std::fs::write(asks.join("decisions.yaml"), "asks: [unclosed\n").unwrap();
    });
    let html = render(&store, &inbox()).html;
    assert_eq!(html.matches("Not offered here").count(), 4, "decisions.yaml's open asks: {html}");
    let demo = render(&store, &detail("demo-arc.yaml", "merge-now")).html;
    assert!(demo.contains("mv-answerbar"), "{demo}");
    // The branch's error is labelled with the branch, never as the tree read's own.
    assert!(html.contains(r#"<span class="bv-code">maestro/answers:"#), "{html}");
}

#[test]
fn an_answer_given_as_text_is_shown_as_written() {
    // demo-arc.yaml's old-one carries `answer: "yes"` and no option.
    let m = machine();
    let d = render(&worktree(m.path()), &detail("demo-arc.yaml", "old-one")).html;
    assert!(d.contains("Answered: <bdi>yes</bdi>"), "{d}");
}

#[test]
fn no_screen_claims_an_ask_is_unanswered() {
    // Maestro reads this checkout and maestro/answers; Paseo may hold answers in other
    // checkouts. So the bar names where the ask is open and where to look, never "answer it",
    // the banner names its sources and what it does not read, and no total is claimed.
    let (_m, store) = clean_at_main(|_| {});
    let d = render(&store, &detail("decisions.yaml", "fleet-cost-1")).html;
    assert!(d.contains("Open at <span class=\"bv-code\">git:main@"), "{d}");
    assert!(d.contains("shows whether it has been answered since"), "{d}");
    assert!(!d.contains("(recommended:") && !d.contains("To answer"), "{d}");
    let html = render(&store, &inbox()).html;
    assert!(html.contains("Paseo may hold answers in other checkouts, which this screen does not read."), "{html}");
    assert!(
        html.contains(
            "0 answered in this checkout or on maestro/answers (this clone's, or origin's as last fetched), not yet on"
        ),
        "{html}"
    );
    assert!(!html.contains("waiting to land<"), "{html}");
    // Reading the working tree itself, no landing count is made and the branch is said unread.
    let m = machine();
    let wt = render(&worktree(m.path()), &inbox()).html;
    assert!(wt.contains("Answers on maestro/answers are not read here."), "{wt}");
    assert!(!wt.contains("answered, not yet landed"), "{wt}");
}

#[test]
fn an_answer_counts_only_for_its_own_namespace() {
    // A (uid `shared`) is answered on main already; B (uid-less id `shared`) is open. With no
    // change anywhere, nothing waits to land: B is not taken for A's answer.
    let open_b = "asks:\n  - id: px\n    uid: shared\n    project: ns\n    headline: \"A\"\n    answered_at: \"2026-10-20T00:00:00Z\"\n    answer: \"done\"\n    exhausted: [\"read: x\"]\n  - id: shared\n    project: ns\n    headline: \"B\"\n    exhausted: [\"read: x\"]\n";
    let m = machine();
    let asks = m.path().join("workspace/.control/asks");
    std::fs::remove_file(asks.join("broken.yaml")).unwrap();
    std::fs::remove_file(asks.join("alt-key.yaml")).unwrap();
    std::fs::write(asks.join("ns.yaml"), open_b).unwrap();
    let ws = m.path().join("workspace");
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "seed"]);
    let store = Store::new(Paths::under(m.path().to_path_buf()), TreeSource::GitRev("main".into()));
    let unlanded = maestro_store::views::unlanded(&store, store.asks().data.as_deref().unwrap());
    assert_eq!(unlanded.data.unwrap().len(), 0);
}

#[test]
fn a_decision_beside_an_entry_that_did_not_read_claims_no_state() {
    let m = machine();
    std::fs::write(
        m.path().join("workspace/.control/asks/sib.yaml"),
        "asks:\n  - id: u1\n    headline: \"Done?\"\n    answered_at: \"2026-10-20T00:00:00Z\"\n    answer: \"yes\"\n    exhausted: [\"read: x\"]\n  - uid: u1\n    headline: \"no id\"\n",
    )
    .unwrap();
    let d = render(&worktree(m.path()), &detail("sib.yaml", "u1")).html;
    assert!(d.contains("Maestro cannot tell for sure"), "{d}");
    assert!(!d.contains("Answered: ") && !d.contains("mv-answerbar"), "{d}");
}

#[test]
fn an_answers_branch_that_does_not_resolve_is_unread_never_absent() {
    let (m, _) = with_answers_branch(|asks| std::fs::write(asks.join("more.yaml"), "asks: []\n").unwrap());
    // The ref is there, but names an object the repo does not hold.
    let git_dir = m.path().join("workspace/.git");
    std::fs::write(git_dir.join("refs/heads/maestro/answers"), format!("{}\n", "1".repeat(40))).unwrap();
    let store = Store::new(Paths::under(m.path().to_path_buf()), TreeSource::GitRev("main".into()));
    let snap = maestro_store::views::snapshot(&store);
    let asks = snap.asks;
    assert_eq!(asks.data.unwrap().unlanded, None);
    assert!(asks.errors.iter().any(|e| e.at.starts_with("maestro/answers:")), "{:?}", asks.errors);
    let html = render(&store, &inbox()).html;
    assert!(html.contains(r#"answered, not yet landed: <span class="mv-unavailable">unavailable</span>"#), "{html}");
}

#[test]
fn a_broken_branch_ref_over_a_packed_one_is_unread_never_absent() {
    let (m, _) = with_answers_branch(|asks| std::fs::write(asks.join("more.yaml"), "asks: []\n").unwrap());
    let ws = m.path().join("workspace");
    git(&ws, &["pack-refs", "--all"]);
    // A crashed write leaves an empty loose ref; git ignores it, and the packed one stands under it.
    std::fs::create_dir_all(ws.join(".git/refs/heads/maestro")).unwrap();
    std::fs::write(ws.join(".git/refs/heads/maestro/answers"), "").unwrap();
    let store = Store::new(Paths::under(m.path().to_path_buf()), TreeSource::GitRev("main".into()));
    let asks = maestro_store::views::snapshot(&store).asks;
    assert_eq!(asks.data.unwrap().unlanded, None);
    assert!(asks.errors.iter().any(|e| e.at.starts_with("maestro/answers:")), "{:?}", asks.errors);
}

#[test]
fn asks_sharing_a_key_are_unknown_and_their_one_answer_waits_once() {
    // Two uid-less asks named `dup`: the plugin's `ambiguity()` refuses an answer to either, and
    // its `unlandedAnswers` counts the one answer once.
    let m = machine();
    let asks = m.path().join("workspace/.control/asks");
    std::fs::remove_file(asks.join("broken.yaml")).unwrap();
    std::fs::remove_file(asks.join("alt-key.yaml")).unwrap();
    let open = "asks:\n  - id: dup\n    project: dd\n    headline: \"First\"\n    exhausted: [\"read: x\"]\n    options:\n      - { id: a, label: A, recommended: true }\n  - id: dup\n    project: dd\n    headline: \"Second\"\n    exhausted: [\"read: x\"]\n";
    std::fs::write(asks.join("dd.yaml"), open).unwrap();
    let ws = m.path().join("workspace");
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "seed"]);
    let answered = open.replacen("    headline: \"First\"\n", "    headline: \"First\"\n    answer: \"yes\"\n", 1);
    assert_ne!(open, answered);
    std::fs::write(asks.join("dd.yaml"), answered).unwrap();
    let store = Store::new(Paths::under(m.path().to_path_buf()), TreeSource::GitRev("main".into()));
    let html = render(&store, &spec(json!({ "type": "decisions", "props": { "project": "dd" } }))).html;
    assert_eq!(html.matches("Not offered here").count(), 2, "{html}");
    assert!(!html.contains("Waiting to land</span>") && !html.contains("Recommended:"), "{html}");
    assert!(!html.contains("mv-answerbar") && !html.contains("Choose one"), "{html}");
    // No read failed: the checkout's count is the plugin's batch count, exact, though which of the
    // two asks the answer waits for is not known.
    assert!(html.contains(">at least 2 open<"), "{html}");
    assert!(
        html.contains(r#"<p class="mv-banner-title">1 answered in this checkout or on maestro/answers (this clone's, or origin's as last fetched), not yet on"#),
        "{html}"
    );
    assert_eq!(maestro_store::views::snapshot(&store).asks.data.unwrap().unlanded, Some(1));
}

#[test]
fn a_key_whose_first_ask_is_closed_at_the_tree_does_not_wait() {
    // The plugin's `unlandedAnswers` reads main by key, first entry wins: the first `dup` is
    // answered at main, so an answer to `dup` elsewhere is not waiting, though the second is open.
    let m = machine();
    let asks = m.path().join("workspace/.control/asks");
    std::fs::remove_file(asks.join("broken.yaml")).unwrap();
    std::fs::remove_file(asks.join("alt-key.yaml")).unwrap();
    let at_main = "asks:\n  - id: dup\n    headline: \"First\"\n    answer: \"done\"\n    exhausted: [\"read: x\"]\n  - id: dup\n    headline: \"Second\"\n    exhausted: [\"read: x\"]\n";
    std::fs::write(asks.join("dd.yaml"), at_main).unwrap();
    let ws = m.path().join("workspace");
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "seed"]);
    let both = at_main.replacen("    headline: \"Second\"\n", "    headline: \"Second\"\n    answer: \"yes\"\n", 1);
    assert_ne!(both, at_main);
    std::fs::write(asks.join("dd.yaml"), both).unwrap();
    let store = Store::new(Paths::under(m.path().to_path_buf()), TreeSource::GitRev("main".into()));
    assert_eq!(maestro_store::views::snapshot(&store).asks.data.unwrap().unlanded, Some(0));
}

#[test]
fn an_ask_closed_on_the_answers_branch_waits_to_land_as_the_plugin_counts_it() {
    // The plugin's `unlandedAnswers` takes any entry answered or otherwise closed on the branch.
    let (_m, store) = with_answers_branch(|asks| {
        let file = asks.join("decisions.yaml");
        let text = std::fs::read_to_string(&file).unwrap();
        let archived = text.replacen("    gates: [fleet-cost]\n", "    gates: [fleet-cost]\n    status: archived\n", 1);
        assert_ne!(archived, text);
        std::fs::write(&file, archived).unwrap();
    });
    let snap = maestro_store::views::snapshot(&store);
    assert_eq!(snap.asks.data.unwrap().unlanded, Some(1));
    let d = render(&store, &detail("decisions.yaml", "fleet-cost-1")).html;
    assert!(!d.contains("mv-answerbar"), "{d}");
}

#[test]
fn the_working_tree_makes_no_waiting_count() {
    let m = machine();
    let snap = maestro_store::views::snapshot(&worktree(m.path()));
    assert_eq!(snap.asks.data.unwrap().unlanded, None, "nothing lands onto the working tree: no count, not 0");
}

#[test]
fn an_unread_waiting_count_is_none_never_zero() {
    let (_m, store) = with_answers_branch(|asks| {
        std::fs::remove_dir_all(asks).unwrap();
        std::fs::write(asks, "not a directory").unwrap();
    });
    let snap = maestro_store::views::snapshot(&store);
    assert_eq!(snap.asks.data.unwrap().unlanded, None);
}

#[test]
fn a_branch_answer_waits_even_with_no_local_asks_directory() {
    let (m, store) = with_answers_branch(|asks| {
        let file = asks.join("decisions.yaml");
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(
            &file,
            text.replacen(
                "    gates: [fleet-cost]\n",
                "    gates: [fleet-cost]\n    answered_at: \"2026-10-23T11:00:00Z\"\n",
                1,
            ),
        )
        .unwrap();
    });
    std::fs::remove_dir_all(m.path().join("workspace/.control/asks")).unwrap();
    let html = render(&store, &inbox()).html;
    assert!(html.contains("Waiting to land</span>"), "{html}");
}

#[test]
fn gates_are_said_only_for_a_blocking_ask() {
    let m = machine();
    std::fs::write(
        m.path().join("workspace/.control/asks/g.yaml"),
        "asks:\n  - id: g\n    headline: \"G\"\n    blocking: false\n    gates: [lane-x]\n    exhausted: [\"read: x\"]\n",
    )
    .unwrap();
    let d = render(&worktree(m.path()), &detail("g.yaml", "g")).html;
    assert!(!d.contains("lane-x"), "{d}");
}

#[test]
fn an_answer_to_an_ask_the_tree_does_not_hold_is_counted_for_the_checkout_never_a_project_zero() {
    // Filed and answered in the working tree only: no row holds it, so no project can count it.
    let (_m, store) = clean_at_main(|asks| {
        std::fs::write(
            asks.join("new.yaml"),
            "asks:\n  - id: b\n    project: proj\n    headline: \"New\"\n    answer: \"yes\"\n    answered_at: \"2026-10-03T00:00:00Z\"\n    status: resolved\n",
        )
        .unwrap();
    });
    assert_eq!(maestro_store::views::snapshot(&store).asks.data.unwrap().unlanded, Some(1));
    let all = render(&store, &inbox()).html;
    assert!(all.contains(" open · 1 answered, not yet landed<"), "{all}");
    assert!(!all.contains("Counted for the whole checkout"), "{all}");
    for project in ["proj", "maestro"] {
        let html = render(&store, &spec(json!({ "type": "decisions", "props": { "project": project } }))).html;
        assert!(!html.contains("answered, not yet landed"), "{html}");
        assert!(
            html.contains(
                r#"<p class="mv-banner-title">1 answered in this checkout or on maestro/answers (this clone's, or origin's as last fetched), not yet on"#
            ),
            "{html}"
        );
        assert!(html.contains("Counted for the whole checkout, not this project alone."), "{html}");
    }
}
