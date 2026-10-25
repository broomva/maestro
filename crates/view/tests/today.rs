//! The Today screen and the health rail (design page 10) on the fixture machine, plus a plan
//! graph a live initiative links, usage rows, a running work item and the last hour's ticks
//! (`tests/fixtures/today/`).

use maestro_store::{Paths, Store, TreeSource};
use maestro_view::{Rendered, render_at, validate};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/machine")
}

fn extra(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/today").join(name)
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

/// The fixture machine with the Today extras laid in where a real machine keeps them.
fn machine() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    copy_tree(&fixture(), root);
    let plans = root.join("workspace/docs/plans");
    std::fs::create_dir_all(&plans).unwrap();
    std::fs::copy(extra("plan.graph.yaml"), plans.join("2026-10-20-demo.graph.yaml")).unwrap();
    std::fs::copy(extra("unlinked.graph.yaml"), plans.join("2026-10-01-other.graph.yaml")).unwrap();
    std::fs::copy(extra("initiative.yaml"), root.join("workspace/.control/initiatives/today-demo.yaml")).unwrap();
    copy_tree(&extra("work"), &root.join("work"));
    let ticks = root.join("broomva-home/ledger/loops/ticks.jsonl");
    let mut text = std::fs::read_to_string(&ticks).unwrap();
    text.push_str(&std::fs::read_to_string(extra("ticks.jsonl")).unwrap());
    std::fs::write(&ticks, text).unwrap();
    dir
}

fn at() -> jiff::Timestamp {
    "2026-10-23T12:00:00Z".parse().unwrap()
}

fn spec(kind: &str) -> Value {
    json!({ "root": "s", "blocks": { "s": { "type": kind } } })
}

fn render(store: &Store, kind: &str) -> Rendered {
    let v = validate(&spec(kind)).unwrap();
    assert!(v.drops.is_empty(), "{:?}", v.drops);
    render_at(store, &v, at())
}

fn paths(root: &Path, usage: bool) -> Paths {
    let mut p = Paths::under(root.to_path_buf());
    if usage {
        p.usage = Some(extra("usage.json"));
    }
    p
}

fn worktree(root: &Path, usage: bool) -> Store {
    Store::new(paths(root, usage), TreeSource::WorkingTree)
}

fn portable(html: &str, root: &Path) -> String {
    html.replace(&root.display().to_string(), "<fixture>")
        .replace(&extra("usage.json").display().to_string(), "<usage>")
        .replace("><", ">\n<")
}

/// The text of the first element carrying `attr`, tags stripped.
fn text_of(html: &str, attr: &str) -> String {
    let at = html.find(attr).unwrap_or_else(|| panic!("no {attr}"));
    let open = at + html[at..].find('>').unwrap() + 1;
    let mut depth = 0usize;
    let mut out = String::new();
    let mut rest = &html[open..];
    loop {
        let lt = rest.find('<').unwrap();
        out.push_str(&rest[..lt]);
        rest = &rest[lt..];
        if rest.starts_with("</") {
            if depth == 0 {
                return out;
            }
            depth -= 1;
        } else {
            depth += 1;
        }
        rest = &rest[rest.find('>').unwrap() + 1..];
    }
}

fn pill(html: &str, name: &str) -> String {
    text_of(html, &format!(r#"data-pill="{name}""#)).trim().to_string()
}

fn git(dir: &Path, env: &[(&str, &str)], args: &[&str]) {
    let ok = Command::new("git")
        .current_dir(dir)
        .envs(env.iter().copied())
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

#[test]
fn today_renders_from_the_fixture() {
    let m = machine();
    let r = render(&worktree(m.path(), true), "today");
    insta::assert_snapshot!("today", portable(&r.html, m.path()));
}

#[test]
fn the_rail_renders_alone() {
    let m = machine();
    let r = render(&worktree(m.path(), true), "health");
    insta::assert_snapshot!("health", portable(&r.html, m.path()));
}

#[test]
fn an_unread_machine_says_unavailable_everywhere() {
    let empty = tempfile::tempdir().unwrap();
    let r = render(&worktree(empty.path(), false), "today");
    for name in ["needs you", "running", "open asks", "silent loops", "landing"] {
        assert_eq!(pill(&r.html, name), "unavailable needs you".replace("needs you", name), "{name}");
    }
    let h = text_of(&r.html, "mv-headline-text");
    assert!(h.contains("could not be read") && h.contains("is not known"), "{h}");
    assert!(!h.contains("Nothing needs you"), "{h}");
    for card in ["plan", "heartbeats", "economy", "receipts", "running", "nodes"] {
        assert!(r.html.contains(&format!(r#"data-type="{card}" data-state="unavailable""#)), "{card}");
    }
    insta::assert_snapshot!("today_unavailable", portable(&r.html, empty.path()));
}

#[test]
fn the_counts_are_the_snapshots() {
    let m = machine();
    let store = worktree(m.path(), true);
    let r = render(&store, "today");
    let snap = maestro_store::views::snapshot_of(&store, &maestro_store::views::Reads::of(&store), at());
    // The pill and the Running now card count the same rows (quiet work is Standing, not Running).
    let rows = r.html.matches(r#"<li class="mv-row" data-work="#).count();
    assert_eq!(rows, 1);
    assert_eq!(pill(&r.html, "running"), "1 running");
    // The fixture's asks read in part (broken.yaml), so both counts say so.
    assert!(!snap.needs.errors.is_empty() && !snap.asks.errors.is_empty());
    assert_eq!(
        pill(&r.html, "needs you"),
        format!("{} needs you, partial read", snap.needs.data.as_ref().unwrap().len())
    );
    assert_eq!(
        pill(&r.html, "open asks"),
        format!("{} open asks, partial read", snap.asks.data.as_ref().unwrap().open)
    );
    assert_eq!(pill(&r.html, "silent loops"), "1 silent loops", "the fixture's `silent` loop has no receipts");
}

#[test]
fn the_headline_names_the_commit_and_its_age() {
    let m = machine();
    let ws = m.path().join("workspace");
    let when = [("GIT_COMMITTER_DATE", "2026-10-23T11:58:00Z"), ("GIT_AUTHOR_DATE", "2026-10-23T11:58:00Z")];
    git(&ws, &[], &["init", "-q", "-b", "main"]);
    git(&ws, &[], &["add", "."]);
    git(&ws, &when, &["commit", "-q", "-m", "seed"]);
    let store = Store::new(paths(m.path(), false), TreeSource::GitRev("main".into()));
    let commit = store.tree.commit.clone().unwrap();
    let r = render(&store, "today");
    let line = text_of(&r.html, "mv-composed");
    assert_eq!(line, format!("Composed by Maestro · main@{} · committed 2 min ago", &commit[..7]));
    assert!(r.html.contains(&format!(r#"data-commit="{commit}""#)));

    // The working tree has no commit: it says so, and claims no age.
    let r = render(&worktree(m.path(), false), "today");
    assert_eq!(text_of(&r.html, "mv-composed"), "Composed by Maestro · the working tree · read now");
}

#[test]
fn a_revision_that_does_not_resolve_has_no_age() {
    let m = machine();
    let ws = m.path().join("workspace");
    git(&ws, &[], &["init", "-q", "-b", "main"]);
    let store = Store::new(paths(m.path(), false), TreeSource::GitRev("nope".into()));
    let r = render(&store, "today");
    assert_eq!(text_of(&r.html, "mv-composed"), "Composed by Maestro · nope, which did not resolve · age not known");
}

#[test]
fn a_bad_ticks_line_makes_silence_and_ok_unknown() {
    let m = machine();
    let ticks = m.path().join("broomva-home/ledger/loops/ticks.jsonl");
    let mut text = std::fs::read_to_string(&ticks).unwrap();
    text.push_str("not json\n");
    std::fs::write(&ticks, text).unwrap();
    let r = render(&worktree(m.path(), false), "today");
    assert_eq!(pill(&r.html, "silent loops"), "unavailable silent loops");
    // Proven states stay; nothing is called ok while a line may hide a failed tick.
    assert!(!r.html.contains(r#"data-beat="ok""#));
    assert!(r.html.contains(r#"data-loop="demo-tick" data-beat="not known""#));
}

#[test]
fn heartbeats_say_ok_drift_and_no_receipts() {
    let m = machine();
    let r = render(&worktree(m.path(), false), "health");
    assert!(r.html.contains(r#"data-loop="demo-tick" data-beat="ok""#));
    assert!(r.html.contains(r#"data-loop="silent" data-beat="no receipts""#));
    // demo-tick's strip: the fixture's three ticks, oldest first; the old open one is "open".
    let strip = &r.html[r.html.find(r#"data-loop="demo-tick""#).unwrap()..];
    let strip = &strip[..strip.find("</li>").unwrap()];
    let outcomes: Vec<&str> = strip
        .match_indices(r#"data-outcome=""#)
        .map(|(i, _)| &strip[i + 14..i + 14 + strip[i + 14..].find('"').unwrap()])
        .collect();
    assert_eq!(outcomes, ["ok", "open", "partial", "ok"]);
}

#[test]
fn receipts_are_the_last_hour_only() {
    let m = machine();
    let r = render(&worktree(m.path(), false), "health");
    let card = &r.html[r.html.find(r#"data-type="receipts""#).unwrap()..];
    let card = &card[..card.find("</ul>").unwrap()];
    // 11:40 ended ok, 11:30 started, 11:05 ended partial; the 10:59 start is over an hour old.
    let times: Vec<&str> =
        card.match_indices(r#"<span class="bv-code">"#).map(|(i, s)| &card[i + s.len()..i + s.len() + 5]).collect();
    assert_eq!(times, ["11:40", "11:30", "11:05"]);
    assert!(card.contains(r#"<span class="mv-count">3</span>"#));
}

#[test]
fn only_plans_a_live_initiative_links_are_shown() {
    let m = machine();
    let r = render(&worktree(m.path(), false), "health");
    assert!(r.html.contains(r#"data-plan="2026-10-20-demo.graph.yaml""#));
    assert!(!r.html.contains("2026-10-01-other.graph.yaml"));
    assert!(r.html.contains("Node state is unavailable"));

    // Parked, the initiative links no live plan.
    let init = m.path().join("workspace/.control/initiatives/today-demo.yaml");
    let text = std::fs::read_to_string(&init).unwrap().replace("status: active", "status: parked");
    std::fs::write(&init, text).unwrap();
    let r = render(&worktree(m.path(), false), "health");
    assert!(!r.html.contains("data-plan="));

    // Initiatives unread: which plan is live is not known, so the card is unavailable.
    std::fs::remove_dir_all(m.path().join("workspace/.control/initiatives")).unwrap();
    let r = render(&worktree(m.path(), false), "health");
    assert!(r.html.contains(r#"data-type="plan" data-state="unavailable""#));
}

#[test]
fn the_economy_is_read_from_the_usage_file() {
    let m = machine();
    let r = render(&worktree(m.path(), false), "health");
    assert!(r.html.contains(r#"data-type="economy" data-state="unavailable""#));
    assert!(r.html.contains("no usage file is set"));

    let r = render(&worktree(m.path(), true), "health");
    assert!(r.html.contains(r#"data-type="economy" data-state="ok""#));
    assert!(r.html.contains(r#"data-account="main""#) && r.html.contains(r#"data-account="spare""#));
    assert!(r.html.contains("82%") && r.html.contains("64%"));
    assert!(r.html.contains(r#"data-gate="held""#) || r.html.contains(r#"data-gate="open""#));

    // A usage file that is not a list is unread, not empty.
    let bad = m.path().join("usage.json");
    std::fs::write(&bad, "{}").unwrap();
    let mut p = Paths::under(m.path().to_path_buf());
    p.usage = Some(bad);
    let r = render(&Store::new(p, TreeSource::WorkingTree), "health");
    assert!(r.html.contains(r#"data-type="economy" data-state="unavailable""#));
    assert!(r.html.contains("not a list of usage rows"));
}

#[test]
fn running_work_shows_only_the_chips_it_carries() {
    let m = machine();
    let r = render(&worktree(m.path(), false), "today");
    let row = &r.html[r.html.find(r#"data-work="today-run""#).unwrap()..];
    let row = &row[..row.find("</li>").unwrap()];
    assert!(row.contains("frontier · opus · high"), "{row}");
    assert!(row.contains("claude · mac-studio"), "{row}");
    assert!(row.contains("budget 48%"), "{row}");
    // An item with none of those fields shows none of them.
    let bare = m.path().join("work/bare-run");
    std::fs::create_dir_all(&bare).unwrap();
    std::fs::write(
        bare.join("_work.md"),
        "---\nid: 55555555-0000-0000-0000-000000000000\nkind: task\nstate: running\ngate: auto\nupdated: 2026-10-23\n---\n\n# Bare\n",
    )
    .unwrap();
    let r = render(&worktree(m.path(), false), "today");
    let row = &r.html[r.html.find(r#"data-work="bare-run""#).unwrap()..];
    let row = &row[..row.find("</li>").unwrap()];
    assert!(!row.contains("mv-chip") && !row.contains("budget") && !row.contains(" · "), "{row}");
}

#[test]
fn the_snapshot_tiles_count_by_presentation_state() {
    // The fixture holds two stored `running` items: today-run (Running) and quiet-run (Standing).
    let m = machine();
    let r = render(&worktree(m.path(), false), "snapshot");
    let label = r.html.find("mv-tile-label bv-meta\">Running").unwrap();
    let value = r.html[..label].rfind(r#"<span class="mv-tile-value">"#).unwrap();
    assert_eq!(&r.html[value..label], r#"<span class="mv-tile-value">1</span><span class=""#);
}

/// A loop spec in the scratch workspace, and ticks appended to its ledger.
fn add_loop(root: &Path, id: &str, extra: &str) {
    let spec = format!(
        "id: {id}\ntitle: {id}\npurpose: test\nowner: coordinator\nscope: broomva\nsubstrate: {{kind: launchd, ref: x}}\ncadence: hourly\nreview_by: '2099-01-01'\n{extra}"
    );
    std::fs::write(root.join(format!("workspace/.broomva/loops/{id}.yaml")), spec).unwrap();
}

fn add_ticks(root: &Path, lines: &str) {
    let ticks = root.join("broomva-home/ledger/loops/ticks.jsonl");
    let mut text = std::fs::read_to_string(&ticks).unwrap();
    text.push_str(lines);
    std::fs::write(&ticks, text).unwrap();
}

fn tick(id: &str, loop_id: &str, ts: &str, end: Option<(&str, &str)>) -> String {
    let start = format!(
        r#"{{"actor":"a","cause":null,"data":{{}},"id":"{id}","prev":null,"refs":{{}},"stream":"loops/ticks","subject":"loop:{loop_id}","ts":"{ts}","type":"loop.tick.started","v":1}}"#
    );
    match end {
        None => format!("{start}\n"),
        Some((at, outcome)) => format!(
            "{start}\n{{\"actor\":\"a\",\"cause\":\"{id}\",\"data\":{{\"outcome\":\"{outcome}\"}},\"id\":\"{id}e\",\"prev\":null,\"refs\":{{}},\"stream\":\"loops/ticks\",\"subject\":\"loop:{loop_id}\",\"ts\":\"{at}\",\"type\":\"loop.tick.ended\",\"v\":1}}\n"
        ),
    }
}

fn beat_of(html: &str, loop_id: &str) -> String {
    let at = html.find(&format!(r#"data-loop="{loop_id}" data-beat=""#)).unwrap_or_else(|| panic!("no loop {loop_id}"));
    let rest = &html[at + format!(r#"data-loop="{loop_id}" data-beat=""#).len()..];
    rest[..rest.find('"').unwrap()].to_string()
}

#[test]
fn ok_needs_a_closed_tick_and_absence_needs_the_whole_ledger() {
    let m = machine();
    add_loop(m.path(), "never", "");
    add_loop(m.path(), "resting", "desired: paused\n");
    add_loop(m.path(), "drifty", "");
    add_loop(m.path(), "failing", "");
    add_ticks(
        m.path(),
        &(tick("D1", "drifty", "2026-10-23T10:00:00Z", None)
            + &tick("F1", "failing", "2026-10-23T11:00:00Z", Some(("2026-10-23T11:10:00Z", "failed")))),
    );
    let r = render(&worktree(m.path(), false), "health");
    assert_eq!(beat_of(&r.html, "demo-tick"), "ok");
    assert_eq!(beat_of(&r.html, "never"), "no ticks recorded", "no tick read is not ok");
    assert_eq!(beat_of(&r.html, "resting"), "paused");
    assert_eq!(beat_of(&r.html, "drifty"), "drift", "an open tick past its limit");
    assert_eq!(beat_of(&r.html, "failing"), "last tick failed");
    assert_eq!(beat_of(&r.html, "silent"), "no receipts");
}

#[test]
fn a_ledger_read_in_part_proves_no_absence() {
    // A bad line may be the receipt the silent loop is missing.
    let m = machine();
    add_ticks(m.path(), "{\"subject\":\"loop:silent\", truncated\n");
    let r = render(&worktree(m.path(), false), "today");
    assert_eq!(pill(&r.html, "silent loops"), "unavailable silent loops");
    // Its review date has passed (the registry alone proves that), but not "no receipts".
    assert_eq!(beat_of(&r.html, "silent"), "review overdue");
    assert_eq!(beat_of(&r.html, "demo-tick"), "not known");
    assert!(r.html.contains("The ticks ledger was read in part"));

    // A ledger past the tail is read in part too, every line valid.
    let m = machine();
    let filler: String = (0..9_000)
        .map(|i| tick(&format!("P{i}"), "filler", "2026-10-22T00:00:00Z", Some(("2026-10-22T00:01:00Z", "ok"))))
        .collect();
    assert!(filler.len() > maestro_store::STREAM_TAIL);
    let ticks = m.path().join("broomva-home/ledger/loops/ticks.jsonl");
    let text = std::fs::read_to_string(&ticks).unwrap();
    std::fs::write(&ticks, filler + &text).unwrap();
    let r = render(&worktree(m.path(), false), "today");
    assert_eq!(pill(&r.html, "silent loops"), "unavailable silent loops");
    assert_eq!(beat_of(&r.html, "silent"), "review overdue");
    assert_eq!(beat_of(&r.html, "demo-tick"), "not known");

    // Unread: it says so.
    let m = machine();
    std::fs::remove_file(m.path().join("broomva-home/ledger/loops/ticks.jsonl")).unwrap();
    let r = render(&worktree(m.path(), false), "health");
    assert!(r.html.contains("The ticks ledger could not be read"));
    assert!(!r.html.contains(r#"data-beat="ok""#));
}

#[test]
fn a_receipt_after_the_clock_is_not_in_the_last_hour() {
    let m = machine();
    add_ticks(m.path(), &tick("Z1", "demo-tick", "2026-10-23T12:30:00Z", None));
    let r = render(&worktree(m.path(), false), "health");
    let card = &r.html[r.html.find(r#"data-type="receipts""#).unwrap()..];
    assert!(!card[..card.find("</section>").unwrap()].contains("12:30"));
}

#[test]
fn usage_rows_core_refuses_are_unavailable() {
    let m = machine();
    let bad = m.path().join("usage.json");
    std::fs::write(&bad, r#"[{"email": "main@x.io", "fiveHourUtil": "lots", "isActive": true}]"#).unwrap();
    let mut p = Paths::under(m.path().to_path_buf());
    p.usage = Some(bad);
    let r = render(&Store::new(p, TreeSource::WorkingTree), "health");
    assert!(r.html.contains(r#"data-type="economy" data-state="unavailable""#), "{}", r.html);

    // A stale active account is read, and the card says its numbers may be old.
    let stale = m.path().join("stale.json");
    std::fs::write(&stale, r#"[{"email": "main@x.io", "fiveHourUtil": null, "sevenDayUtil": null, "isActive": true, "isRateLimited": false, "stale": true}]"#).unwrap();
    let mut p = Paths::under(m.path().to_path_buf());
    p.usage = Some(stale);
    let r = render(&Store::new(p, TreeSource::WorkingTree), "health");
    assert!(r.html.contains(r#"data-known="false""#), "{}", r.html);
}

#[test]
fn the_top_decision_is_not_one_already_answered() {
    let m = machine();
    let ws = m.path().join("workspace");
    git(&ws, &[], &["init", "-q", "-b", "main"]);
    git(&ws, &[], &["add", "."]);
    git(&ws, &[], &["commit", "-q", "-m", "seed"]);
    let store = Store::new(paths(m.path(), false), TreeSource::GitRev("main".into()));
    let r = render(&store, "today");
    assert!(text_of(&r.html, "mv-top").contains("Merge the demo PR now"));

    // Answered in the working tree: it waits to land, so the next open ask is the top.
    let file = ws.join(".control/asks/demo-arc.yaml");
    let text = std::fs::read_to_string(&file).unwrap();
    let answered = text.replacen(
        "    liveness: {kind: pr-open, ref: \"demo/repo#1\"}\n",
        "    liveness: {kind: pr-open, ref: \"demo/repo#1\"}\n    answer_option: merge\n    answered_at: \"2026-10-23T11:00:00Z\"\n    status: resolved\n",
        1,
    );
    assert_ne!(text, answered);
    std::fs::write(&file, answered).unwrap();
    let r = render(&store, "today");
    let top = text_of(&r.html, "mv-top");
    assert!(!top.contains("Merge the demo PR now"), "{top}");
}

#[test]
fn partial_reads_say_so_and_claim_no_none() {
    // Every ask ledger and the owner step unreadable, the running item too.
    let m = machine();
    for f in std::fs::read_dir(m.path().join("workspace/.control/asks")).unwrap() {
        std::fs::write(f.unwrap().path(), "not: [valid").unwrap();
    }
    std::fs::write(m.path().join("work/held-for-you/_work.md"), "---\nnot: [valid\n---\n").unwrap();
    std::fs::write(m.path().join("work/today-run/_work.md"), "---\nnot: [valid\n---\n").unwrap();
    let r = render(&worktree(m.path(), false), "today");
    let h = text_of(&r.html, "mv-headline-text");
    assert!(!h.contains("Nothing needs you") && !h.contains("No work is running"), "{h}");
    assert!(h.contains("found needing you in the part that was read"), "{h}");
    assert!(h.contains("running in the part that was read"), "{h}");
    assert!(r.html.contains(r#"data-pill="open asks" data-partial="true""#));
    assert_eq!(pill(&r.html, "open asks"), "0 open asks, partial read");
    for card in ["top-decision", "running"] {
        let at = r.html.find(&format!(r#"data-type="{card}""#)).unwrap();
        let body = &r.html[at..at + r.html[at..].find("</section>").unwrap()];
        assert!(body.contains("None in the part that was read."), "{card}: {body}");
    }

    // The live initiative unreadable: the plan it links may be the one missing.
    let m = machine();
    std::fs::write(m.path().join("workspace/.control/initiatives/today-demo.yaml"), "not: [valid").unwrap();
    let r = render(&worktree(m.path(), false), "health");
    let at = r.html.find(r#"data-type="plan""#).unwrap();
    assert!(r.html[at..].starts_with(r#"data-type="plan" data-state="partial""#));
    assert!(r.html[at..at + r.html[at..].find("</section>").unwrap()].contains("None in the part that was read."));
}

#[test]
fn a_cut_tail_is_a_partial_read_on_every_tick_card() {
    let m = machine();
    let filler: String = (0..9_000)
        .map(|i| tick(&format!("P{i}"), "filler", "2026-10-22T00:00:00Z", Some(("2026-10-22T00:01:00Z", "ok"))))
        .collect();
    let ticks = m.path().join("broomva-home/ledger/loops/ticks.jsonl");
    // The hour's only receipts sit before the tail.
    let buried = tick("B1", "demo-tick", "2026-10-23T11:50:00Z", Some(("2026-10-23T11:51:00Z", "failed")));
    let text = std::fs::read_to_string(&ticks).unwrap();
    let text: String = text.lines().filter(|l| !l.contains("2026-10-23")).map(|l| format!("{l}\n")).collect();
    std::fs::write(&ticks, buried + &text + &filler).unwrap();
    let r = render(&worktree(m.path(), false), "health");
    assert!(r.html.contains(r#"data-type="heartbeats" data-state="partial""#));
    assert!(r.html.contains(r#"data-type="receipts" data-state="partial""#));
    let at = r.html.find(r#"data-type="receipts""#).unwrap();
    let card = &r.html[at..at + r.html[at..].find("</section>").unwrap()];
    assert!(card.contains("None in the part that was read."), "{card}");
    assert!(!card.contains("mv-count"), "no count from part of the ledger: {card}");
    assert!(card.contains("only the ledger's last"), "{card}");
    assert_eq!(beat_of(&r.html, "demo-tick"), "not known");
}
