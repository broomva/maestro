//! Derived views: needs, loop health, the snapshot, and context packets.
//!
//! Pure functions over what the readers returned, so every surface (CLI, MCP,
//! hooks) computes the same counts from the same inputs.

use crate::{LedgerFile, Store};
use maestro_core::Section;
use maestro_core::ask::{AskRef, AskState};
use maestro_core::context::{AskBrief, ContextPacket, ContextScope, InitiativeBrief, LoopBrief};
use maestro_core::fleet::FleetSummary;
use maestro_core::initiative::Initiative;
use maestro_core::loops::{Desired, Drift, DriftClass, LoopSpec};
use maestro_core::need::{Need, NeedKind, NeedSource};
use maestro_core::tick::Tick;
use maestro_core::work::{WorkItem, WorkState};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

/// An open tick older than this is reported as unclosed.
pub const UNCLOSED_AFTER_SECS: i64 = 2 * 3_600;

pub fn now() -> jiff::Timestamp {
    jiff::Timestamp::now()
}

/// Holds on the owner: open blocking asks. Work holds need live session state (see below).
pub fn needs(asks: &[LedgerFile], work: &[WorkItem]) -> Vec<Need> {
    let mut out = Vec::new();
    for lf in asks {
        for ask in &lf.ledger.asks {
            if ask.state() != AskState::Open || !ask.blocking {
                continue;
            }
            let r = AskRef { ledger: lf.file.clone(), id: ask.uid.clone().unwrap_or_else(|| ask.id.clone()) };
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
    // Work at the gate is a hold only while its session is paused on a permission
    // prompt (`holdsApproval` in the plugin). That is live session state, not on
    // disk, so phase 0 counts no work holds rather than over-count finished work.
    let _ = work;
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
    pub ticks_recorded: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub drift: Vec<Drift>,
}

fn substrate_label(spec: &LoopSpec) -> String {
    serde_json::to_value(&spec.substrate.kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".into())
}

/// Join the registry with the ticks ledger. Only drift this data can prove is reported.
pub fn loop_rows(specs: &[LoopSpec], ticks: Option<&[Tick]>, now: jiff::Timestamp) -> Vec<LoopRow> {
    let mut by_loop: BTreeMap<&str, Vec<&Tick>> = BTreeMap::new();
    for t in ticks.unwrap_or_default() {
        by_loop.entry(t.loop_id.as_str()).or_default().push(t);
    }
    let today = now.to_zoned(jiff::tz::TimeZone::UTC).date().to_string();
    specs
        .iter()
        .map(|spec| {
            let mine = by_loop.get(spec.id.as_str()).cloned().unwrap_or_default();
            let last = mine.iter().max_by(|a, b| a.started_at.cmp(&b.started_at)).map(|t| (*t).clone());
            let mut drift = Vec::new();
            let active = spec.desired() == Desired::Active;
            if active && spec.review_by.as_str() < today.as_str() {
                drift.push(Drift {
                    class: DriftClass::Expiring,
                    loop_id: spec.id.clone(),
                    detail: format!("review_by {} has passed", spec.review_by),
                });
            }
            if let Some(t) = &last
                && !t.is_closed()
            {
                let age = t.started_at.parse::<jiff::Timestamp>().ok().map(|s| now.as_second() - s.as_second());
                if age.is_some_and(|a| a > UNCLOSED_AFTER_SECS) {
                    drift.push(Drift {
                        class: DriftClass::TickUnclosed,
                        loop_id: spec.id.clone(),
                        detail: format!("tick {} started {} has no end", t.start_id, t.started_at),
                    });
                }
            }
            let produces_ticks =
                spec.contract.as_ref().and_then(|c| c.produces.as_deref()).is_some_and(|p| p.contains("loop.tick"));
            if active && ticks.is_some() && produces_ticks && mine.is_empty() {
                drift.push(Drift {
                    class: DriftClass::TickMissing,
                    loop_id: spec.id.clone(),
                    detail: "contract produces tick receipts but the ledger has none".into(),
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
    /// Open on the read revision but answered in the working tree: waiting to land.
    pub unlanded: usize,
}

/// Asks open at the store's git revision but answered in the workspace working tree.
/// Empty when the store already reads the working tree.
pub fn unlanded(store: &Store, at_rev: &[LedgerFile]) -> Vec<AskRef> {
    if store.tree.source == crate::TreeSource::WorkingTree {
        return Vec::new();
    }
    let local = Store::new(store.paths.clone(), crate::TreeSource::WorkingTree).asks();
    let Some(local) = local.data else { return Vec::new() };
    let answered_here: std::collections::BTreeSet<(String, String)> = local
        .iter()
        .flat_map(|lf| lf.ledger.asks.iter().map(move |a| (lf, a)))
        .filter(|(_, a)| a.state() != AskState::Open)
        .map(|(lf, a)| (lf.file.clone(), a.id.clone()))
        .collect();
    at_rev
        .iter()
        .flat_map(|lf| lf.ledger.asks.iter().map(move |a| (lf, a)))
        .filter(|(lf, a)| a.state() == AskState::Open && answered_here.contains(&(lf.file.clone(), a.id.clone())))
        .map(|(lf, a)| AskRef { ledger: lf.file.clone(), id: a.uid.clone().unwrap_or_else(|| a.id.clone()) })
        .collect()
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

/// Counts by stored state. Canonical labels are per item; a count labelled
/// `Needs you` would claim holds that only live session state can confirm.
pub fn work_counts(work: &[WorkItem]) -> BTreeMap<&'static str, usize> {
    let mut out = BTreeMap::new();
    for item in work {
        *out.entry(item.front.state.as_str()).or_insert(0) += 1;
    }
    out
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

/// Every read in one place. Needs are degraded if either of their sources is.
pub fn snapshot(store: &Store) -> Snapshot {
    let at = now();
    let asks = store.asks();
    let work = store.work();
    let ticks = store.ticks();
    let loops = store.loops();
    let initiatives = store.initiatives();

    let needs = {
        let list = needs(asks.data.as_deref().unwrap_or_default(), work.data.as_deref().unwrap_or_default());
        let mut errors = asks.errors.clone();
        errors.extend(work.errors.clone());
        let source = format!("{} + {}", asks.source, work.source);
        if asks.data.is_none() {
            Section { source, data: None, errors }
        } else {
            Section::partial(source, list, errors)
        }
    };
    let loop_rows = {
        let mut errors = loops.errors.clone();
        errors.extend(ticks.errors.clone());
        let rows = loops.data.as_deref().map(|specs| loop_rows(specs, ticks.data.as_deref(), at));
        Section { source: format!("{} + {}", loops.source, ticks.source), data: rows, errors }
    };
    let live = initiatives.map(|v| v.into_iter().filter(|i| i.status.is_live()).collect());

    Snapshot {
        generated_at: at.to_string(),
        workspace: store.paths.workspace.display().to_string(),
        tree: store.tree.source.to_string(),
        needs,
        asks: asks.as_ref().map(|a| AskCounts { unlanded: unlanded(store, a).len(), ..ask_counts(a) }),
        work: work.as_ref().map(|w| work_counts(w)),
        loops: loop_rows,
        initiatives: live,
        fleet: store.fleet(),
    }
}

/// Infer a project name from a cwd: the repo directory under the workspace (`core/maestro` → `maestro`).
pub fn project_of(cwd: &Path, workspace: &Path) -> Option<String> {
    let rel = cwd.strip_prefix(workspace).ok()?;
    let parts: Vec<_> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    match parts.as_slice() {
        [_, name, ..] => Some(name.clone()),
        _ => None,
    }
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
    let snap = snapshot(store);
    let workspace = &store.paths.workspace;
    let cwd_rel = scope
        .cwd
        .as_deref()
        .and_then(|c| Path::new(c).strip_prefix(workspace).ok())
        .map(|p| p.to_string_lossy().into_owned());
    if scope.project.is_none() {
        scope.project = scope.cwd.as_deref().and_then(|c| project_of(Path::new(c), workspace));
    }
    let initiatives = store.initiatives();
    let init = initiatives.data.as_deref().and_then(|list| initiative_for(list, &scope, cwd_rel.as_deref())).cloned();
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

    let all_needs = snap.needs.data.clone().unwrap_or_default();
    let scoped = scope.project.is_some() || scope.initiative.is_some();
    let needs: Vec<Need> = all_needs
        .iter()
        .filter(|n| {
            !scoped
                || n.project.is_some() && n.project == scope.project
                || n.ask.as_ref().is_some_and(|a| linked_ledgers.contains(&a.ledger))
        })
        .cloned()
        .collect();

    let in_scope = |ledger: &str, project: Option<&String>| {
        scoped && (project.is_some() && project == scope.project.as_ref() || linked_ledgers.iter().any(|l| l == ledger))
    };
    let asks = store.asks();
    let open_asks = asks
        .data
        .iter()
        .flatten()
        .flat_map(|lf| lf.ledger.asks.iter().map(move |a| (lf, a)))
        .filter(|(lf, a)| a.state() == AskState::Open && !a.blocking && in_scope(&lf.file, a.project.as_ref()))
        .map(|(lf, a)| AskBrief {
            key: format!("ask:{}#{}", lf.file, a.uid.as_ref().unwrap_or(&a.id)),
            title: a.title().to_string(),
        })
        .collect();

    let loops = snap
        .loops
        .data
        .iter()
        .flatten()
        .flat_map(|row| row.drift.iter())
        .map(|d| LoopBrief { id: d.loop_id.clone(), note: d.detail.clone() })
        .collect();

    let mut degraded = Vec::new();
    for (name, bad) in [
        ("asks", snap.asks.is_degraded()),
        ("work", snap.work.is_degraded()),
        ("loops", snap.loops.is_degraded()),
        ("initiatives", snap.initiatives.is_degraded()),
        ("fleet", snap.fleet.is_degraded()),
    ] {
        if bad {
            degraded.push(name.to_string());
        }
    }

    ContextPacket {
        generated_at: snap.generated_at,
        scope,
        needs_you: all_needs.len(),
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

/// Count work items by canonical state, for compact human output.
pub fn work_by_state(work: &[WorkItem], state: Option<WorkState>) -> Vec<&WorkItem> {
    work.iter().filter(|w| state.is_none_or(|s| w.work_state == s)).collect()
}
