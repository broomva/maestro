use maestro_core::ask::AskState;
use maestro_core::loops::{DriftClass, NoticeKind};
use maestro_core::work::WorkState;
use maestro_store::{Paths, Store, TreeSource, views};
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/machine")
}

fn store() -> Store {
    Store::new(Paths::under(fixture()), TreeSource::WorkingTree)
}

#[test]
fn a_broken_ledger_is_reported_not_dropped() {
    let asks = store().asks();
    let ledgers = asks.data.as_ref().unwrap();
    assert_eq!(ledgers.len(), 2, "demo-arc and alt-key parse; broken.yaml does not");
    let at: Vec<_> = asks.errors.iter().map(|e| e.at.as_str()).collect();
    assert_eq!(at, [".control/asks/broken.yaml", ".control/asks/alt-key.yaml#2"]);
    assert!(asks.is_degraded());
}

#[test]
fn map_shaped_exhausted_entries_count_as_rungs() {
    let asks = store().asks();
    let demo = asks.data.unwrap().into_iter().find(|l| l.file == "demo-arc.yaml").unwrap();
    let ask = &demo.ledger.asks[0];
    assert_eq!(ask.rungs().len(), 2);
    assert_eq!(ask.state(), AskState::Open);
}

#[test]
fn needs_are_open_blocking_asks_only() {
    let s = store();
    let needs = views::needs(s.asks().data.as_deref().unwrap(), s.work().data.as_deref().unwrap());
    let keys: Vec<_> = needs.iter().map(|n| n.key.as_str()).collect();
    // The answered blocking asks (one by `resolution`) and the non-blocking idea are not
    // holds; neither is finished review work. Queued work flagged `yours` is an owner step.
    assert_eq!(
        keys,
        ["ask:alt-key.yaml#alt-blocking", "ask:demo-arc.yaml#merge-now", "work:22222222-0000-0000-0000-000000000000"]
    );
}

#[test]
fn work_reads_frontmatter_title_and_last_event() {
    let work = store().work().data.unwrap();
    assert_eq!(work.len(), 3);
    let by = |slug: &str| work.iter().find(|w| w.slug == slug).unwrap();
    let demo = by("demo-task");
    assert_eq!(demo.title, "Demo task title");
    assert_eq!(demo.work_state, WorkState::NeedsYou);
    assert!(demo.finished && !demo.owner_step());
    assert_eq!(demo.last_event_at.as_deref(), Some("2026-10-02T01:00:00Z"));
    assert_eq!(by("quiet-run").work_state, WorkState::Standing);
    let held = by("held-for-you");
    assert_eq!(held.work_state, WorkState::Queued);
    assert!(held.owner_step());
}

#[test]
fn loops_report_only_provable_drift() {
    let s = store();
    let now: jiff::Timestamp = "2026-10-01T06:00:00Z".parse().unwrap();
    let rows = views::loop_rows(s.loops().data.as_deref().unwrap(), s.ticks().data.as_deref(), now);
    let demo = rows.iter().find(|r| r.id == "demo-tick").unwrap();
    assert_eq!(demo.ticks_recorded, 2);
    let classes: Vec<_> = demo.drift.iter().map(|d| d.class).collect();
    assert_eq!(classes, [DriftClass::TickUnclosed], "open tick started 4h ago; limit is 60 min");
    assert!(demo.notices.is_empty());
    let silent = rows.iter().find(|r| r.id == "silent").unwrap();
    assert!(silent.drift.is_empty(), "no substrate observations, so no drift class is claimed");
    let kinds: Vec<_> = silent.notices.iter().map(|n| n.kind).collect();
    assert_eq!(kinds, [NoticeKind::ReviewOverdue, NoticeKind::NoTickReceipts]);

    let early: jiff::Timestamp = "2026-10-01T02:30:00Z".parse().unwrap();
    let rows = views::loop_rows(s.loops().data.as_deref().unwrap(), s.ticks().data.as_deref(), early);
    let demo = rows.iter().find(|r| r.id == "demo-tick").unwrap();
    assert!(demo.drift.is_empty(), "30 min open is under the 60 min limit");
}

#[test]
fn no_tick_drift_is_claimed_when_the_ledger_is_unreadable() {
    let s = store();
    let now: jiff::Timestamp = "2026-10-01T06:00:00Z".parse().unwrap();
    let rows = views::loop_rows(s.loops().data.as_deref().unwrap(), None, now);
    let silent = rows.iter().find(|r| r.id == "silent").unwrap();
    assert!(silent.notices.iter().all(|n| n.kind != NoticeKind::NoTickReceipts));
}

#[test]
fn fleet_takes_the_highest_numbered_tick() {
    let fleet = store().fleet().data.unwrap();
    assert_eq!(fleet.len(), 1);
    assert_eq!(fleet[0].tick, 12);
    assert_eq!(fleet[0].sessions, 2);
}

#[test]
fn context_scopes_to_the_initiative_of_the_cwd() {
    let s = store();
    let cwd = s.paths.workspace.join("apps/demo/src");
    let packet = views::context(
        &s,
        maestro_core::context::ContextScope { cwd: Some(cwd.display().to_string()), ..Default::default() },
    );
    assert_eq!(packet.scope.project.as_deref(), Some("demo"));
    assert_eq!(packet.initiative.as_ref().map(|i| i.id.as_str()), Some("demo"));
    assert_eq!(packet.needs.len(), 2, "both blocking asks in project demo; the owner step has no project");
    assert_eq!(packet.needs_you, Some(3));
    assert!(!packet.needs_complete, "broken.yaml makes the count a lower bound");
    assert_eq!(packet.open_asks.len(), 1, "the open idea ask");
    assert!(packet.degraded.contains(&"asks".to_string()), "broken.yaml makes asks partial");
    let md = packet.render_markdown(8);
    assert!(md.contains("Ship the demo."));
}

#[test]
fn missing_sources_are_unavailable_not_empty() {
    let s = Store::new(Paths::under("/nonexistent/maestro-test"), TreeSource::WorkingTree);
    let snap = views::snapshot(&s);
    assert!(snap.asks.data.is_none());
    assert!(snap.needs.data.is_none(), "needs from unread asks are unknown, not zero");
    assert!(snap.work.data.is_none());
    assert!(snap.fleet.data.is_none());
    let packet = views::context(&s, Default::default());
    assert_eq!(packet.needs_you, None);
    assert!(packet.render_markdown(8).contains("Needs you: unknown"));
}

#[test]
fn hashes_and_states_match_the_plugin() {
    // Golden values from maestro-paseo `entryHash`/`stateOf` run on these same fixture files.
    let golden = [
        ("demo-arc.yaml", "merge-now", "5b70dab7a7f0afa6", AskState::Open),
        ("demo-arc.yaml", "pick-name", "70b46ca380a91c51", AskState::Open),
        ("demo-arc.yaml", "old-one", "c7b5c96bd01d5123", AskState::Answered),
        ("alt-key.yaml", "alt-blocking", "7e423ddd550cb6e8", AskState::Open),
        ("alt-key.yaml", "resolved-by-resolution", "4e537532577a129c", AskState::Answered),
    ];
    let asks = store().asks().data.unwrap();
    for (ledger, id, hash, state) in golden {
        let ask = asks.iter().find(|l| l.file == ledger).unwrap().ledger.asks.iter().find(|a| a.id == id).unwrap();
        assert_eq!(ask.hash(), hash, "{ledger}#{id}");
        assert_eq!(ask.state(), state, "{ledger}#{id}");
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

#[test]
fn git_revision_reads_and_unlanded_answers() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    let copy = |rel: &str| {
        let from = fixture().join("workspace").join(rel);
        let to = ws.join(rel);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(from, to).unwrap();
    };
    copy(".control/asks/demo-arc.yaml");
    copy(".control/initiatives/demo.yaml");
    // A name git would quote without -z.
    std::fs::copy(fixture().join("workspace/.control/asks/alt-key.yaml"), ws.join(".control/asks/ñ-unicode.yaml"))
        .unwrap();
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "seed"]);

    // Answer an ask in the working tree only.
    let file = ws.join(".control/asks/demo-arc.yaml");
    let text = std::fs::read_to_string(&file).unwrap();
    let answered = text.replacen(
        "    liveness:",
        "    answer: \"Merge now\"\n    answered_at: \"2026-10-03T00:00:00Z\"\n    status: resolved\n    liveness:",
        1,
    );
    std::fs::write(&file, answered).unwrap();

    let mut paths = Paths::under(tmp.path());
    paths.workspace = ws.clone();
    let at_main = Store::new(paths.clone(), TreeSource::GitRev("main".into()));
    let asks = at_main.asks();
    assert!(asks.source.starts_with("git:main@"), "{}", asks.source);
    assert!(asks.data.as_ref().unwrap().iter().any(|l| l.file == "ñ-unicode.yaml"), "non-ASCII names are listed");
    let demo =
        |v: &[maestro_store::LedgerFile]| v.iter().find(|l| l.file == "demo-arc.yaml").unwrap().ledger.asks[0].state();
    assert_eq!(demo(asks.data.as_ref().unwrap()), AskState::Open);
    let waiting = views::unlanded(&at_main, asks.data.as_deref().unwrap()).data.unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].id, "merge-now");

    let local = Store::new(paths, TreeSource::WorkingTree);
    assert_eq!(demo(&local.asks().data.unwrap()), AskState::Answered);
    assert!(views::unlanded(&local, &[]).data.unwrap().is_empty());
}

#[test]
fn a_directory_named_like_a_ledger_is_not_read_in_git_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(ws.join(".control/asks/folder.yaml")).unwrap();
    std::fs::write(ws.join(".control/asks/folder.yaml/inner.txt"), "x").unwrap();
    std::fs::write(ws.join(".control/asks/real.yaml"), "asks:\n  - id: a\n").unwrap();
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "seed"]);
    let mut paths = Paths::under(tmp.path());
    paths.workspace = ws;
    let asks = Store::new(paths, TreeSource::GitRev("main".into())).asks();
    let files: Vec<_> = asks.data.as_ref().unwrap().iter().map(|l| l.file.as_str()).collect();
    assert_eq!(files, ["real.yaml"]);
    assert!(asks.errors.is_empty(), "{:?}", asks.errors);
}

#[test]
fn an_unknown_revision_is_unreadable_not_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(ws.join(".control/asks")).unwrap();
    std::fs::write(ws.join(".control/asks/real.yaml"), "asks:\n  - id: a\n").unwrap();
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "seed"]);
    let mut paths = Paths::under(tmp.path());
    paths.workspace = ws;
    let s = Store::new(paths, TreeSource::GitRev("no-such-ref".into()));
    assert!(s.asks().data.is_none());
    assert!(s.preauth().data.is_none());
    assert!(views::snapshot(&s).needs.data.is_none(), "needs are unknown, not zero");
}

#[test]
fn unlanded_reports_an_unreadable_working_tree_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(ws.join(".control/asks")).unwrap();
    std::fs::write(ws.join(".control/asks/a.yaml"), "asks:\n  - id: a\n").unwrap();
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "seed"]);
    std::fs::write(ws.join(".control/asks/a.yaml"), "asks: [unclosed\n").unwrap();
    let mut paths = Paths::under(tmp.path());
    paths.workspace = ws;
    let s = Store::new(paths, TreeSource::GitRev("main".into()));
    let at_main = s.asks();
    let waiting = views::unlanded(&s, at_main.data.as_deref().unwrap());
    assert_eq!(waiting.errors.len(), 1, "{:?}", waiting.errors);
    assert!(waiting.errors[0].at.starts_with("worktree:"), "{:?}", waiting.errors);
    let snap = views::snapshot(&s);
    assert!(
        snap.asks.errors.iter().any(|e| e.at.starts_with("worktree:")),
        "the snapshot shows the count is a lower bound"
    );
}

#[test]
fn project_is_the_repo_not_the_second_path_part() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join(".git")).unwrap();
    std::fs::create_dir_all(ws.join("core/maestro/.git")).unwrap();
    std::fs::create_dir_all(ws.join("core/maestro/crates/x")).unwrap();
    std::fs::create_dir_all(ws.join("research/notes")).unwrap();
    assert_eq!(views::project_of(&ws.join("core/maestro/crates/x"), &ws).as_deref(), Some("maestro"));
    assert_eq!(views::project_of(&ws.join("core/maestro"), &ws).as_deref(), Some("maestro"));
    assert_eq!(views::project_of(&ws.join("research/notes"), &ws), None, "a folder of the workspace repo");
    assert_eq!(views::project_of(&ws, &ws), None);
    assert_eq!(views::project_of(Path::new("/elsewhere"), &ws), None);
    assert_eq!(
        views::project_of(&ws.join("apps/absent/src"), &ws).as_deref(),
        Some("absent"),
        "not on disk: layout rule"
    );
}
