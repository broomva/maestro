//! Derived views: needs, loop health, the snapshot, and context packets.
//!
//! Pure functions over what the readers returned, so every surface (CLI, MCP,
//! hooks) computes the same counts from the same inputs.

use crate::{LedgerFile, Store, TickTail};
use maestro_core::ask::{AskRef, AskState};
use maestro_core::context::{AskBrief, ContextPacket, ContextScope, InitiativeBrief, LoopBrief};
use maestro_core::fleet::FleetSummary;
use maestro_core::initiative::Initiative;
use maestro_core::loops::{Desired, Drift, DriftClass, LoopSpec, Notice, NoticeKind, TICK_CLOSE_SECS};
use maestro_core::need::{Need, NeedKind, NeedSource};
use maestro_core::tick::Tick;
use maestro_core::work::{WorkItem, WorkState};
use maestro_core::{Section, SourceError};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub fn now() -> jiff::Timestamp {
    jiff::Timestamp::now()
}

/// Holds on the owner from on-disk data: open blocking asks, and queued work only
/// the owner may start (`onlyYouStart`). Work paused on a permission prompt is a
/// hold too, but that prompt is live surface state and joins with surface adapters.
pub fn needs(asks: &[LedgerFile], work: &[WorkItem]) -> Vec<Need> {
    let mut out = Vec::new();
    for lf in asks {
        for ask in &lf.ledger.asks {
            if ask.state() != AskState::Open || !ask.blocking {
                continue;
            }
            let r = AskRef { ledger: lf.file.clone(), id: ask.handle().to_string() };
            out.push(Need {
                kind: NeedKind::Ask,
                source: NeedSource::Asks,
                key: format!("ask:{r}"),
                title: ask.title().to_string(),
                since: ask.asked_at.clone().or_else(|| lf.ledger.opened.clone()),
                ask: Some(r),
                project: ask.project.clone(),
                unblocks: ask.gates.clone(),
            });
        }
    }
    for item in work.iter().filter(|w| w.owner_step()) {
        out.push(Need {
            kind: NeedKind::OwnerStep,
            source: NeedSource::Work,
            key: format!("work:{}", item.front.id),
            title: item.title.clone(),
            since: item.flag.as_ref().and_then(|f| f.since.clone()).or_else(|| item.last_event_at.clone()),
            ask: None,
            project: None,
            unblocks: vec![],
        });
    }
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct LoopRow {
    pub id: String,
    pub title: String,
    pub substrate: String,
    pub cadence: String,
    pub desired: Desired,
    pub owner: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tick: Option<Tick>,
    /// Ticks started inside the ledger's tail ([`crate::STREAM_TAIL`]), not the whole history.
    pub ticks_recorded: usize,
    /// Plugin drift classes, with the plugin's meaning. Only those the ticks ledger proves.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub drift: Vec<Drift>,
    /// Maestro's own signals from the registry and ledger; not drift classes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<Notice>,
}

fn substrate_label(spec: &LoopSpec) -> String {
    serde_json::to_value(&spec.substrate.kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".into())
}

fn instant(ts: &str) -> Option<jiff::Timestamp> {
    ts.parse::<jiff::Timestamp>().ok()
}

fn secs(ts: &str) -> Option<i64> {
    ts.parse::<jiff::Timestamp>().ok().map(|t| t.as_second())
}

fn median(values: &mut [i64]) -> i64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let mid = values.len() / 2;
    if values.len() % 2 == 1 { values[mid] } else { (values[mid - 1] + values[mid]) / 2 }
}

/// The plugin's unclosed limit: max(`TICK_CLOSE_SECS`, 2 x the row's median closed tick length).
pub fn unclosed_limit(ticks: &[&Tick]) -> i64 {
    let mut lengths: Vec<i64> =
        ticks.iter().filter_map(|t| Some(secs(t.ended_at.as_deref()?)? - secs(&t.started_at)?)).collect();
    TICK_CLOSE_SECS.max(2 * median(&mut lengths))
}

/// Join the registry with the ticks ledger. `ticks` is `None` when the ledger is unread:
/// then nothing about ticks is claimed. A tail that is not the whole ledger claims no absence.
pub fn loop_rows(specs: &[LoopSpec], ticks: Option<&TickTail>, now: jiff::Timestamp) -> Vec<LoopRow> {
    let mut by_loop: BTreeMap<&str, Vec<&Tick>> = BTreeMap::new();
    for t in ticks.map(|t| t.ticks.as_slice()).unwrap_or_default() {
        by_loop.entry(t.loop_id.as_str()).or_default().push(t);
    }
    let today = now.to_zoned(jiff::tz::TimeZone::UTC).date().to_string();
    specs
        .iter()
        .map(|spec| {
            let mine = by_loop.get(spec.id.as_str()).cloned().unwrap_or_default();
            // By instant, not text: offsets and precisions differ across writers. Unparseable sorts lowest.
            let last = mine.iter().max_by_key(|t| (instant(&t.started_at), t.started_at.clone())).map(|t| (*t).clone());
            let active = spec.desired() == Desired::Active;
            let mut drift = Vec::new();
            let mut notices = Vec::new();

            if let Some(t) = &last
                && !t.is_closed()
            {
                let limit = unclosed_limit(&mine);
                if secs(&t.started_at).is_some_and(|s| now.as_second() - s > limit) {
                    drift.push(Drift {
                        class: DriftClass::TickUnclosed,
                        loop_id: spec.id.clone(),
                        detail: format!("the tick started at {} has not ended after {} min", t.started_at, limit / 60),
                    });
                }
            }
            // A date sorts as text; a non-date review_by is left alone rather than guessed at.
            let review_is_date = spec.review_by.len() == 10 && spec.review_by.parse::<jiff::civil::Date>().is_ok();
            if active && review_is_date && spec.review_by < today {
                notices.push(Notice {
                    kind: NoticeKind::ReviewOverdue,
                    loop_id: spec.id.clone(),
                    detail: format!("review_by {} has passed", spec.review_by),
                });
            }
            let names_receipts = spec
                .contract
                .as_ref()
                .and_then(|c| c.produces.as_deref())
                .is_some_and(|p| p.contains("loop.tick.started") || p.contains("loop.tick.ended"));
            if active && ticks.is_some_and(TickTail::whole) && names_receipts && mine.is_empty() {
                notices.push(Notice {
                    kind: NoticeKind::NoTickReceipts,
                    loop_id: spec.id.clone(),
                    detail: "the contract names tick receipts and the ledger holds none for this loop".into(),
                });
            }
            LoopRow {
                id: spec.id.clone(),
                title: spec.title.clone(),
                substrate: substrate_label(spec),
                cadence: spec.cadence.clone(),
                desired: spec.desired(),
                owner: spec.owner.clone(),
                last_tick: last,
                ticks_recorded: mine.len(),
                drift,
                notices,
            }
        })
        .collect()
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AskCounts {
    pub ledgers: usize,
    pub open: usize,
    pub open_blocking: usize,
    pub answered: usize,
    pub archived: usize,
    /// Open on the read revision but answered in the working tree or on `maestro/answers`:
    /// waiting to land. `None` when those could not be read (unread is not none).
    pub unlanded: Option<usize>,
}

/// The branch Paseo's Maestro plugin lands owner answers on before their batch PR merges
/// (spec row R2). An answer there is waiting to land as much as one in the working tree.
pub const ANSWERS_BRANCH: &str = "maestro/answers";

/// Answered asks by ledger and the plugin's key: `u:<uid>`, else `i:<id>` for an ask without a
/// uid (`answers-landing.ts`), so a uid and a uid-less id that share their text never match.
type Answered = BTreeSet<(String, String)>;

fn key(ask: &maestro_core::ask::Ask) -> String {
    match &ask.uid {
        Some(uid) => format!("u:{uid}"),
        None => format!("i:{}", ask.id),
    }
}

fn answered_in(asks: &[LedgerFile]) -> Answered {
    asks.iter()
        .flat_map(|lf| lf.ledger.asks.iter().map(move |a| (lf, a)))
        // Answered, not archived: an ask archived there is not an answer waiting to land.
        .filter(|(_, a)| a.state() == AskState::Answered)
        .map(|(lf, a)| (lf.file.clone(), key(a)))
        .collect()
}

/// Answers on [`ANSWERS_BRANCH`]. A workspace without the branch holds none there; a branch
/// that exists but does not read at all is `None` with its errors: its answers are unknown.
/// Its errors are labelled with the branch, so they never read as the tree read's.
fn answered_on_branch(store: &Store) -> (Option<Answered>, Vec<SourceError>) {
    let branch = Store::new(store.paths.clone(), crate::TreeSource::GitRev(format!("refs/heads/{ANSWERS_BRANCH}")));
    // `Tree::resolved` says this when the revision names nothing: the branch is not there.
    if branch.tree.unresolved.as_deref().is_some_and(|r| r.starts_with("no commit is named")) {
        return (Some(Answered::new()), Vec::new());
    }
    let read = branch.asks();
    let mut errors: Vec<SourceError> = read
        .errors
        .iter()
        .map(|e| SourceError {
            at: if e.at.starts_with("git:") { e.at.clone() } else { format!("{ANSWERS_BRANCH}:{}", e.at) },
            reason: e.reason.clone(),
        })
        .collect();
    match read.data {
        Some(asks) => (Some(answered_in(&asks)), errors),
        None => {
            if errors.is_empty() {
                errors.push(SourceError { at: read.source, reason: "unread".into() });
            }
            (None, errors)
        }
    }
}

/// Asks open at the store's git revision but answered in the workspace working tree or on
/// [`ANSWERS_BRANCH`]. Empty when the store already reads the working tree. The section's
/// errors are those two reads' failures: an unreadable ledger there hides answers waiting to
/// land, so the count is then a lower bound, and `data` is `None` when either read failed whole.
pub fn unlanded(store: &Store, at_rev: &[LedgerFile]) -> Section<Vec<AskRef>> {
    if store.tree.source == crate::TreeSource::WorkingTree {
        return Section::ok("worktree", Vec::new());
    }
    // `None` once a read failed whole: what waits to land is then unknown, never none.
    let (mut answered, mut errors) = answered_on_branch(store);
    let waiting = |answered: &Answered| -> Vec<AskRef> {
        at_rev
            .iter()
            .flat_map(|lf| lf.ledger.asks.iter().map(move |a| (lf, a)))
            .filter(|(lf, a)| a.state() == AskState::Open && answered.contains(&(lf.file.clone(), key(a))))
            .map(|(lf, a)| AskRef { ledger: lf.file.clone(), id: a.handle().to_string() })
            .collect()
    };
    // No local asks directory holds no local answers: nothing waits there, and nothing is unread.
    // Anything else in its place (a file, a dangling link) is unread, and the read reports it.
    let local = Store::new(store.paths.clone(), crate::TreeSource::WorkingTree);
    let (source, local) = match local.rel(crate::ASKS) {
        Err(reason) => {
            let at = local.tree.label(crate::ASKS);
            errors.push(SourceError { at: at.clone(), reason });
            (at, None)
        }
        Ok(rel) => {
            let local_dir = store.paths.workspace.join(&rel);
            if std::fs::symlink_metadata(&local_dir).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
                (format!("worktree:{rel}"), Some(Answered::new()))
            } else {
                let read = local.asks();
                errors.extend(read.errors.iter().map(|e| SourceError {
                    at: if e.at.starts_with("worktree:") { e.at.clone() } else { format!("worktree:{}", e.at) },
                    reason: e.reason.clone(),
                }));
                (read.source, read.data.map(|d| answered_in(&d)))
            }
        }
    };
    match (&mut answered, local) {
        (Some(all), Some(here)) => all.extend(here),
        _ => answered = None,
    }
    match answered {
        Some(all) => Section::partial(source, waiting(&all), errors),
        None => Section { source, data: None, errors },
    }
}

pub fn ask_counts(asks: &[LedgerFile]) -> AskCounts {
    let mut c = AskCounts { ledgers: asks.len(), ..Default::default() };
    for ask in asks.iter().flat_map(|l| &l.ledger.asks) {
        match ask.state() {
            AskState::Open => {
                c.open += 1;
                c.open_blocking += usize::from(ask.blocking);
            }
            AskState::Answered => c.answered += 1,
            AskState::Archived => c.archived += 1,
        }
    }
    c
}

/// Counts by stored state. A count labelled `Needs you` would claim holds that only
/// the needs list can make.
pub fn work_counts(work: &[WorkItem]) -> BTreeMap<&'static str, usize> {
    let mut out = BTreeMap::new();
    for item in work {
        *out.entry(item.front.state.as_str()).or_insert(0) += 1;
    }
    out
}

/// Every source read once. Snapshot and context both derive from one of these.
pub struct Reads {
    pub asks: Section<Vec<LedgerFile>>,
    pub work: Section<Vec<WorkItem>>,
    pub ticks: Section<TickTail>,
    pub loops: Section<Vec<LoopSpec>>,
    pub initiatives: Section<Vec<Initiative>>,
    pub fleet: Section<Vec<FleetSummary>>,
}

impl Reads {
    pub fn of(store: &Store) -> Self {
        Self {
            asks: store.asks(),
            work: store.work(),
            ticks: store.ticks(),
            loops: store.loops(),
            initiatives: store.initiatives(),
            fleet: store.fleet(),
        }
    }

    /// Needs are unknown when the asks are unread, and a lower bound when anything they read is partial.
    pub fn needs(&self) -> Section<Vec<Need>> {
        let mut errors = self.asks.errors.clone();
        errors.extend(self.work.errors.clone());
        let source = format!("{} + {}", self.asks.source, self.work.source);
        match &self.asks.data {
            None => Section { source, data: None, errors },
            Some(asks) => {
                if self.work.data.is_none() && errors.is_empty() {
                    errors.push(maestro_core::SourceError { at: self.work.source.clone(), reason: "unread".into() });
                }
                Section::partial(source, needs(asks, self.work.data.as_deref().unwrap_or_default()), errors)
            }
        }
    }

    pub fn loop_rows(&self, now: jiff::Timestamp) -> Section<Vec<LoopRow>> {
        let mut errors = self.loops.errors.clone();
        errors.extend(self.ticks.errors.clone());
        let rows = self.loops.data.as_deref().map(|specs| loop_rows(specs, self.ticks.data.as_ref(), now));
        Section { source: format!("{} + {}", self.loops.source, self.ticks.source), data: rows, errors }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub generated_at: String,
    pub workspace: String,
    pub tree: String,
    pub needs: Section<Vec<Need>>,
    pub asks: Section<AskCounts>,
    pub work: Section<BTreeMap<&'static str, usize>>,
    pub loops: Section<Vec<LoopRow>>,
    pub initiatives: Section<Vec<Initiative>>,
    pub fleet: Section<Vec<FleetSummary>>,
}

/// Ask counts, with the working tree's read errors joined in: they make the unlanded count a lower bound.
fn asks_with_unlanded(store: &Store, asks: &Section<Vec<LedgerFile>>) -> Section<AskCounts> {
    let mut section = asks.as_ref().map(|a| {
        let waiting = unlanded(store, a);
        (AskCounts { unlanded: waiting.data.as_ref().map(Vec::len), ..ask_counts(a) }, waiting.errors)
    });
    let local_errors = section.data.as_mut().map(|(_, e)| std::mem::take(e)).unwrap_or_default();
    section.errors.extend(local_errors);
    section.map(|(counts, _)| counts)
}

pub fn snapshot(store: &Store) -> Snapshot {
    snapshot_of(store, &Reads::of(store), now())
}

pub fn snapshot_of(store: &Store, reads: &Reads, at: jiff::Timestamp) -> Snapshot {
    Snapshot {
        generated_at: at.to_string(),
        workspace: store.paths.workspace.display().to_string(),
        tree: store.tree.label("").trim_end_matches(':').to_string(),
        needs: reads.needs(),
        asks: asks_with_unlanded(store, &reads.asks),
        work: reads.work.as_ref().map(|w| work_counts(w)),
        loops: reads.loop_rows(at),
        initiatives: reads.initiatives.as_ref().map(|v| v.iter().filter(|i| i.status.is_live()).cloned().collect()),
        fleet: reads.fleet.clone(),
    }
}

/// Infer a project name from a cwd: the outermost git repository holding it below the
/// workspace (`core/maestro` → `maestro`). Outermost, so a worktree checked out inside
/// its repo (`core/life/.claude/worktrees/feat-x`) still names `life`. The workspace's own
/// directories (`research/notes`) and its own worktrees (`.claude/worktrees/x`) name no
/// project, and neither does a cwd that is not on this disk: its repo cannot be known.
pub fn project_of(cwd: &Path, workspace: &Path) -> Option<String> {
    let cwd = lexical(cwd);
    let workspace = lexical(workspace);
    cwd.strip_prefix(&workspace).ok()?;
    let workspace_git = workspace.join(".git");
    cwd.ancestors()
        .take_while(|d| *d != workspace)
        .filter(|d| match std::fs::read_to_string(d.join(".git")) {
            // A `.git` file is a worktree: it is the workspace's own when its gitdir lies in the workspace's `.git`.
            Ok(link) => !link
                .strip_prefix("gitdir:")
                .map(|g| lexical(&d.join(g.trim())))
                .is_some_and(|g| g.starts_with(&workspace_git)),
            Err(_) => d.join(".git").is_dir(),
        })
        .last()
        .and_then(|repo| repo.file_name())
        .map(|n| n.to_string_lossy().into_owned())
}

/// A path with `.` and `..` resolved by the text alone, so `core/maestro/../life` is `core/life`.
pub(crate) fn lexical(p: &Path) -> std::path::PathBuf {
    let mut out = std::path::PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn initiative_for<'a>(
    initiatives: &'a [Initiative],
    scope: &ContextScope,
    cwd_rel: Option<&str>,
) -> Option<&'a Initiative> {
    if let Some(id) = scope.initiative.as_deref() {
        return initiatives.iter().find(|i| i.id == id);
    }
    let live = || initiatives.iter().filter(|i| i.status.is_live());
    if let Some(rel) = cwd_rel {
        let by_repo = live().find(|i| {
            i.extra.get("links").and_then(|l| l.get("repos")).and_then(|r| r.as_array()).is_some_and(|repos| {
                repos.iter().filter_map(|r| r.as_str()).any(|r| rel == r || rel.starts_with(&format!("{r}/")))
            })
        });
        if by_repo.is_some() {
            return by_repo;
        }
    }
    scope.project.as_deref().and_then(|p| live().find(|i| i.id == p))
}

/// Build the packet injected into a session scoped to `scope`.
pub fn context(store: &Store, mut scope: ContextScope) -> ContextPacket {
    let at = now();
    let reads = Reads::of(store);
    let snap = snapshot_of(store, &reads, at);
    let workspace = &store.paths.workspace;
    let cwd_rel = scope
        .cwd
        .as_deref()
        .and_then(|c| lexical(Path::new(c)).strip_prefix(lexical(workspace)).ok().map(Path::to_path_buf))
        .map(|p| p.to_string_lossy().into_owned());
    if scope.project.is_none() {
        scope.project = scope.cwd.as_deref().and_then(|c| project_of(Path::new(c), workspace));
    }
    let init =
        reads.initiatives.data.as_deref().and_then(|list| initiative_for(list, &scope, cwd_rel.as_deref())).cloned();
    if scope.initiative.is_none() {
        scope.initiative = init.as_ref().map(|i| i.id.clone());
    }
    let linked_ledgers: Vec<String> = init
        .as_ref()
        .and_then(|i| i.extra.get("links")?.get("asks")?.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();

    let scoped = scope.project.is_some() || scope.initiative.is_some();
    let in_scope = |ledger: Option<&str>, project: Option<&String>| {
        !scoped
            || project.is_some() && project == scope.project.as_ref()
            || ledger.is_some_and(|l| linked_ledgers.iter().any(|x| x == l))
    };
    let all_needs = snap.needs.data.clone();
    let needs: Vec<Need> = all_needs
        .iter()
        .flatten()
        .filter(|n| in_scope(n.ask.as_ref().map(|a| a.ledger.as_str()), n.project.as_ref()))
        .cloned()
        .collect();
    let open_asks = if scoped {
        reads
            .asks
            .data
            .iter()
            .flatten()
            .flat_map(|lf| lf.ledger.asks.iter().map(move |a| (lf, a)))
            .filter(|(lf, a)| {
                a.state() == AskState::Open && !a.blocking && in_scope(Some(&lf.file), a.project.as_ref())
            })
            .map(|(lf, a)| AskBrief { key: format!("ask:{}#{}", lf.file, a.handle()), title: a.title().to_string() })
            .collect()
    } else {
        Vec::new()
    };

    let loops = snap
        .loops
        .data
        .iter()
        .flatten()
        .flat_map(|row| {
            row.drift
                .iter()
                .map(|d| LoopBrief { id: d.loop_id.clone(), note: d.detail.clone() })
                .chain(row.notices.iter().map(|n| LoopBrief { id: n.loop_id.clone(), note: n.detail.clone() }))
        })
        .collect();

    let mut degraded = Vec::new();
    for (name, bad) in [
        ("asks", snap.asks.is_degraded()),
        ("work", snap.work.is_degraded()),
        ("loops", snap.loops.is_degraded()),
        ("initiatives", reads.initiatives.is_degraded()),
        ("fleet", snap.fleet.is_degraded()),
    ] {
        if bad {
            degraded.push(name.to_string());
        }
    }

    ContextPacket {
        generated_at: snap.generated_at,
        scope,
        needs_you: all_needs.as_ref().map(Vec::len),
        needs_complete: !snap.needs.is_degraded(),
        needs,
        open_asks,
        initiative: init.map(|i| InitiativeBrief {
            id: i.id,
            title: i.title,
            status: serde_json::to_value(i.status)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default(),
            next_action: i.next_action,
            driver: i.driver.into(),
        }),
        loops,
        degraded,
    }
}

/// Work items with a canonical state, or all of them.
pub fn work_by_state(work: &[WorkItem], state: Option<WorkState>) -> Vec<&WorkItem> {
    work.iter().filter(|w| state.is_none_or(|s| w.work_state == s)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use maestro_core::loops::LoopSpec;

    fn tick(started_at: &str) -> Tick {
        Tick {
            loop_id: "l".into(),
            actor: "a".into(),
            started_at: started_at.into(),
            start_id: started_at.into(),
            ended_at: Some(started_at.into()),
            outcome: None,
            summary: None,
            tiers: vec![],
            calls: None,
        }
    }

    #[test]
    fn the_latest_tick_is_the_latest_instant_not_the_largest_text() {
        let spec: LoopSpec = serde_yaml::from_str(
            "id: l\ntitle: L\npurpose: p\nowner: o\nscope: broomva\nsubstrate: {kind: t3-schedule}\ncadence: x\nreview_by: '2099-01-01'\n",
        )
        .unwrap();
        // 09:00-05:00 is 14:00Z, the latest instant, though it is the smallest text.
        let ticks = [tick("2026-10-19T13:30:00Z"), tick("2026-10-19T09:00:00-05:00"), tick("2026-10-19T12:00:00Z")];
        let rows = loop_rows(std::slice::from_ref(&spec), Some(&whole(&ticks)), now());
        assert_eq!(rows[0].last_tick.as_ref().unwrap().started_at, "2026-10-19T09:00:00-05:00");
        // 16:00+02:00 is 14:00Z: the largest text, and the earlier instant.
        let ticks = [tick("2026-10-19T16:00:00+02:00"), tick("2026-10-19T14:30:00Z")];
        let rows = loop_rows(std::slice::from_ref(&spec), Some(&whole(&ticks)), now());
        assert_eq!(rows[0].last_tick.as_ref().unwrap().started_at, "2026-10-19T14:30:00Z");
    }

    /// The plugin's `TICK_UNCLOSED` "waits 2x the median tick when that is longer" (loops-model.test.ts).
    #[test]
    fn an_unclosed_tick_waits_twice_the_median_tick_when_that_is_longer() {
        let spec: LoopSpec = serde_yaml::from_str(
            "id: l\ntitle: L\npurpose: p\nowner: o\nscope: broomva\nsubstrate: {kind: t3-schedule}\ncadence: x\nreview_by: '2099-01-01'\n",
        )
        .unwrap();
        let now: jiff::Timestamp = "2026-10-19T20:00:00Z".parse().unwrap();
        let at = |mins: i64| (now - jiff::SignedDuration::from_mins(mins)).to_string();
        let closed = |start: i64| Tick { ended_at: Some(at(start - 50)), ..tick(&at(start)) };
        let open = |start: i64| Tick { ended_at: None, ..tick(&at(start)) };
        let unclosed = |ticks: &[Tick]| {
            loop_rows(std::slice::from_ref(&spec), Some(&whole(ticks)), now)[0]
                .drift
                .iter()
                .any(|d| d.class == DriftClass::TickUnclosed)
        };
        // Two 50-minute ticks: the limit is 100 minutes, past the 60-minute floor.
        assert!(!unclosed(&[closed(600), closed(480), open(61)]));
        assert!(unclosed(&[closed(600), closed(480), open(101)]));
        // With no closed tick the floor holds.
        assert!(unclosed(&[open(61)]));
    }

    fn whole(ticks: &[Tick]) -> TickTail {
        TickTail { ticks: ticks.to_vec(), from: 0 }
    }

    /// No receipts in a cut tail is not "none in the ledger": the ticks before it are unread.
    #[test]
    fn a_cut_tail_claims_no_missing_receipts() {
        let spec: LoopSpec = serde_yaml::from_str(
            "id: l\ntitle: L\npurpose: p\nowner: o\nscope: broomva\nsubstrate: {kind: t3-schedule}\ncadence: x\nreview_by: '2099-01-01'\ncontract: {produces: a loop.tick.started per fire}\n",
        )
        .unwrap();
        let notices = |ticks: TickTail| loop_rows(std::slice::from_ref(&spec), Some(&ticks), now())[0].notices.clone();
        assert_eq!(notices(TickTail::default())[0].kind, NoticeKind::NoTickReceipts);
        assert!(notices(TickTail { ticks: vec![], from: 1 }).is_empty());
    }
}
