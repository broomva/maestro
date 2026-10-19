use maestro_core::ask::AskState;
use maestro_core::loops::DriftClass;
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
    assert_eq!(ledgers.len(), 1);
    assert_eq!(asks.errors.len(), 1);
    assert!(asks.errors[0].at.ends_with("broken.yaml"));
    assert!(asks.is_degraded());
}

#[test]
fn map_shaped_exhausted_entries_count_as_rungs() {
    let asks = store().asks();
    let ask = &asks.data.unwrap()[0].ledger.asks[0];
    assert_eq!(ask.rungs().len(), 2);
    assert_eq!(ask.state(), AskState::Open);
}

#[test]
fn needs_are_open_blocking_asks_only() {
    let s = store();
    let needs = views::needs(s.asks().data.as_deref().unwrap(), s.work().data.as_deref().unwrap());
    let keys: Vec<_> = needs.iter().map(|n| n.key.as_str()).collect();
    // The answered blocking ask and the non-blocking idea are not holds; neither is
    // review work, whose permission prompt is live session state.
    assert_eq!(keys, ["ask:demo-arc.yaml#merge-now"]);
}

#[test]
fn work_reads_frontmatter_title_and_last_event() {
    let work = store().work().data.unwrap();
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].title, "Demo task title");
    assert_eq!(work[0].work_state, WorkState::NeedsYou);
    assert_eq!(work[0].last_event_at.as_deref(), Some("2026-10-02T01:00:00Z"));
}

#[test]
fn loops_report_only_provable_drift() {
    let s = store();
    let now: jiff::Timestamp = "2026-10-01T06:00:00Z".parse().unwrap();
    let rows = views::loop_rows(s.loops().data.as_deref().unwrap(), s.ticks().data.as_deref(), now);
    let demo = rows.iter().find(|r| r.id == "demo-tick").unwrap();
    assert_eq!(demo.ticks_recorded, 2);
    let classes: Vec<_> = demo.drift.iter().map(|d| d.class).collect();
    assert_eq!(classes, [DriftClass::TickUnclosed], "open tick started 4h ago");
    let silent = rows.iter().find(|r| r.id == "silent").unwrap();
    let classes: Vec<_> = silent.drift.iter().map(|d| d.class).collect();
    assert_eq!(classes, [DriftClass::Expiring, DriftClass::TickMissing]);
}

#[test]
fn no_tick_drift_is_claimed_when_the_ledger_is_unreadable() {
    let s = store();
    let now: jiff::Timestamp = "2026-10-01T06:00:00Z".parse().unwrap();
    let rows = views::loop_rows(s.loops().data.as_deref().unwrap(), None, now);
    let silent = rows.iter().find(|r| r.id == "silent").unwrap();
    assert!(silent.drift.iter().all(|d| d.class != DriftClass::TickMissing));
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
    assert_eq!(packet.needs.len(), 1);
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
    assert!(asks.source.starts_with("git:main:"));
    assert_eq!(asks.data.as_ref().unwrap()[0].ledger.asks[0].state(), AskState::Open);
    let waiting = views::unlanded(&at_main, asks.data.as_deref().unwrap());
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].id, "merge-now");

    let local = Store::new(paths, TreeSource::WorkingTree);
    assert_eq!(local.asks().data.unwrap()[0].ledger.asks[0].state(), AskState::Answered);
    assert!(views::unlanded(&local, &[]).is_empty());
}
