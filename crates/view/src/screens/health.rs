//! The health rail (design page 10, right): the plans of live initiatives, each loop's heartbeat,
//! the economy and the last hour's tick receipts. Each card stands on its own read and says
//! "unavailable" when that read failed.

use crate::render::{Ctx, card, empty};
use maestro_core::loops::{Desired, NoticeKind};
use maestro_core::plan::PlanGraph;
use maestro_core::tick::{Tick, TickOutcome};
use maestro_core::{Section, SourceError};
use maestro_store::STREAM_TAIL;
use maestro_store::views::LoopRow;
use maud::{Markup, html};
use serde_json::Value;

/// The most ticks a heartbeat strip shows, newest last.
const BEATS: usize = 24;
/// The most receipts the rail lists.
const RECEIPTS: usize = 8;

pub(crate) fn health(cx: &Ctx, id: &str) -> Markup {
    html! { div class="mv-rail" data-block=(id) data-type="health" { (rail(cx)) } }
}

pub(crate) fn rail(cx: &Ctx) -> Markup {
    html! {
        (plan(cx))
        (heartbeats(cx))
        (economy(cx))
        (receipts(cx))
    }
}

/// The plans a live initiative links: a graph whose `ticket` is in the initiative's
/// `links.linear`. Which initiatives are live is a read too, so both must be read.
fn active_plans<'a>(cx: &'a Ctx) -> Section<Vec<&'a (String, PlanGraph)>> {
    let plans = cx.plans();
    let inits = &cx.reads.initiatives;
    let mut errors = plans.errors.clone();
    errors.extend(inits.errors.iter().cloned());
    let data = match (&plans.data, &inits.data) {
        (Some(p), Some(i)) => {
            let linked: Vec<&str> = i
                .iter()
                .filter(|i| i.status.is_live())
                .filter_map(|i| i.extra.get("links")?.get("linear")?.as_array())
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            Some(p.iter().filter(|(_, g)| g.ticket.as_deref().is_some_and(|t| linked.contains(&t))).collect())
        }
        _ => None,
    };
    Section { source: plans.source.clone(), data, errors }
}

fn plan(cx: &Ctx) -> Markup {
    let section = active_plans(cx);
    let list = section.data.clone().unwrap_or_default();
    let body = html! {
        @if list.is_empty() { (super::today::none(!section.errors.is_empty(), "No plan graph is linked to a live initiative.")) } @else {
            @for (file, g) in &list {
                div class="mv-plan" data-plan=(file) {
                    p class="mv-row-title" {
                        span class="bv-code" { (file) }
                        @if let Some(t) = &g.ticket { " · " (t) }
                        " · " (g.nodes.len()) " nodes"
                    }
                    ul class="mv-plan-nodes" {
                        @for n in &g.nodes {
                            li class="mv-node bv-code" data-node=(n.id) title=(n.depends_on.join(", ")) {
                                (n.id) @if let Some(t) = &n.ticket { " " span class="bv-meta" { (t) } }
                            }
                        }
                    }
                }
            }
            p class="mv-note bv-meta" data-tone="warning" {
                "Node state is unavailable: it lives in Linear, which Maestro does not read yet."
            }
        }
    };
    card("health-plan", "plan", "Plan", &section, section.data.as_ref().map(|_| list.len()), body)
}

/// One loop's heartbeat, from its row and the ticks ledger read with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Beat {
    /// Its last tick read is closed and did not fail.
    Ok,
    Drift,
    ReviewOverdue,
    NoReceipts,
    /// Its last tick read ended failed or partial.
    LastFailed,
    /// Desired paused, nothing wrong proven: no tick is expected.
    Paused,
    /// The whole ledger was read and holds no tick for it; its contract names no receipts.
    NoTicks,
    /// The ticks ledger was not read whole and clean, so what it does not show is not known.
    Unknown,
}

impl Beat {
    fn label(self) -> &'static str {
        match self {
            Beat::Ok => "ok",
            Beat::Drift => "drift",
            Beat::ReviewOverdue => "review overdue",
            Beat::NoReceipts => "no receipts",
            Beat::LastFailed => "last tick failed",
            Beat::Paused => "paused",
            Beat::NoTicks => "no ticks recorded",
            Beat::Unknown => "not known",
        }
    }

    fn tone(self) -> &'static str {
        match self {
            Beat::Ok => "success",
            Beat::Drift | Beat::ReviewOverdue | Beat::LastFailed => "warning",
            Beat::NoReceipts => "danger",
            Beat::Paused | Beat::NoTicks | Beat::Unknown => "neutral",
        }
    }
}

/// Whether the ticks ledger was read whole and clean: only then does a tick it does not show
/// mean the loop wrote none. The silent-loops count and every heartbeat use this one test.
pub(crate) fn ticks_known(cx: &Ctx) -> bool {
    let t = ticks_read(cx);
    t.data.is_some() && t.errors.is_empty()
}

/// The ticks as read, with a tail that is not the whole ledger named as an error: the ticks
/// before it were not read, so a card built on it says partial, not ok.
pub(crate) fn ticks_read<'a>(cx: &'a Ctx<'_>) -> Section<&'a maestro_store::TickTail> {
    let t = &cx.reads.ticks;
    let mut s = t.as_ref();
    if let Some(d) = &t.data
        && !d.whole()
    {
        s.errors.push(SourceError {
            at: t.source.clone(),
            reason: format!(
                "only the ledger's last {} bytes are read; ticks before byte {} are not",
                STREAM_TAIL, d.from
            ),
        });
    }
    s
}

/// What a loop's row and its ticks prove. A tick seen (drift) or the registry alone (review
/// overdue) proves itself; an absence (no receipts, no ticks) needs the ledger known; "ok" needs
/// a last tick seen that closed and did not fail.
pub(crate) fn beat(row: &LoopRow, known: bool) -> Beat {
    let has = |k: NoticeKind| row.notices.iter().any(|n| n.kind == k);
    if !row.drift.is_empty() {
        Beat::Drift
    } else if known && has(NoticeKind::NoTickReceipts) {
        Beat::NoReceipts
    } else if has(NoticeKind::ReviewOverdue) {
        Beat::ReviewOverdue
    } else if !known {
        Beat::Unknown
    } else if row.desired == Desired::Paused {
        Beat::Paused
    } else {
        match &row.last_tick {
            None => Beat::NoTicks,
            Some(t) if matches!(t.outcome, Some(TickOutcome::Failed | TickOutcome::Partial)) => Beat::LastFailed,
            Some(t) if t.is_closed() => Beat::Ok,
            // Open but not yet unclosed (that would be drift): running now, not proven ok.
            Some(_) => Beat::Unknown,
        }
    }
}

fn outcome(t: &Tick) -> &'static str {
    match (&t.ended_at, t.outcome) {
        (None, _) => "open",
        (Some(_), Some(TickOutcome::Ok)) => "ok",
        (Some(_), Some(TickOutcome::Partial)) => "partial",
        (Some(_), Some(TickOutcome::Failed)) => "failed",
        (Some(_), _) => "ended",
    }
}

fn heartbeats(cx: &Ctx) -> Markup {
    let section = &cx.snap.loops;
    let ticks = ticks_read(cx);
    let known = ticks.data.is_some() && ticks.errors.is_empty();
    let all: &[Tick] = ticks.data.as_ref().map_or(&[], |t| t.ticks.as_slice());
    let rows: Vec<&LoopRow> = section.data.iter().flatten().collect();
    let body = html! {
        @if rows.is_empty() { (super::today::none(!section.errors.is_empty(), "No loops.")) } @else {
            ul class="mv-list" {
                @for r in &rows {
                    @let b = beat(r, known);
                    @let mine: Vec<&Tick> = all.iter().filter(|t| t.loop_id == r.id).collect();
                    li class="mv-row mv-beat" data-loop=(r.id) data-beat=(b.label()) {
                        span class="mv-beat-head" {
                            span class="mv-row-title bv-code" { (r.id) }
                            span class="mv-beat-state" data-tone=(b.tone()) { (b.label()) }
                        }
                        @if !mine.is_empty() {
                            span class="mv-beats" aria-label=(format!("last {} ticks", mine.len().min(BEATS))) {
                                @for t in mine.iter().skip(mine.len().saturating_sub(BEATS)) {
                                    span class="mv-beat-tick" data-outcome=(outcome(t)) title=(t.started_at) {}
                                }
                            }
                        }
                    }
                }
            }
            @if !known {
                p class="mv-note" data-tone="warning" {
                    @if cx.reads.ticks.data.is_none() { "The ticks ledger could not be read" } @else { "The ticks ledger was read in part" }
                    ": a missing tick is not known, so no loop is shown as ok or silent."
                }
            }
        }
    };
    let mut shown = section.as_ref();
    shown.errors.extend(ticks.errors.iter().cloned());
    card("health-loops", "heartbeats", "Loop heartbeats", &shown, section.data.as_ref().map(|_| rows.len()), body)
}

fn pct(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
}

fn bar(label: &str, value: Option<f64>) -> Markup {
    html! {
        div class="mv-meter" {
            span class="mv-meter-head" {
                span class="bv-meta" { (label) }
                @match value {
                    Some(v) => span class="mv-meter-value" { (format!("{v:.0}%")) },
                    None => span class="mv-meter-value mv-unavailable" { "not known" },
                }
            }
            span class="mv-meter-track" {
                @if let Some(v) = value { span class="mv-meter-fill" data-high=(v >= 80.0) style=(format!("width: {:.0}%", v.clamp(0.0, 100.0))) {} }
            }
        }
    }
}

fn economy(cx: &Ctx) -> Markup {
    let section = cx.economy();
    let body = match &section.data {
        None => html! {},
        Some(cap) => {
            let empty_map = serde_json::Map::new();
            let accounts = cap.get("accounts").and_then(Value::as_object).unwrap_or(&empty_map);
            let cadence = cap.get("cadence");
            let mode = cadence.and_then(|c| c.get("mode")).and_then(Value::as_str).unwrap_or("not known");
            let held = cadence.and_then(|c| c.get("hold_launches")).and_then(Value::as_bool);
            let why = cadence.and_then(|c| c.get("why")).and_then(Value::as_str);
            html! {
                p class="mv-row-meta bv-meta" { "mode " span class="bv-code" data-mode=(mode) { (mode) } }
                @if accounts.is_empty() { (empty("No account in the usage read.")) }
                @for (name, a) in accounts {
                    div class="mv-account" data-account=(name) {
                        p class="mv-row-title" {
                            (name)
                            @if a.get("active") == Some(&Value::Bool(true)) { " · active" }
                            @if a.get("limited") == Some(&Value::Bool(true)) { " · " span data-tone="danger" { "limited" } }
                        }
                        (bar("5 h", pct(a.get("5h"))))
                        (bar("7 d", pct(a.get("7d"))))
                    }
                }
                @if cap.get("known") == Some(&Value::Bool(false)) {
                    p class="mv-note" data-tone="warning" data-known="false" {
                        "Some account's usage is stale or could not be read: these numbers may be old, and the gate counts it closed."
                    }
                }
                p class="mv-note bv-meta" data-gate=(held.map_or("unknown", |h| if h { "held" } else { "open" })) {
                    @match held {
                        Some(true) => { "Launch gate: held" },
                        Some(false) => { "Launch gate: open" },
                        None => { "Launch gate: not known" },
                    }
                    @if let Some(w) = why { " · " (w) }
                }
            }
        }
    };
    card("health-economy", "economy", "Economy", section, None, body)
}

/// Ticks that started or ended within the hour before the render's clock, newest first.
fn receipts(cx: &Ctx) -> Markup {
    let section = ticks_read(cx);
    let partial = !section.errors.is_empty();
    let from = cx.at.as_second() - 3_600;
    let within =
        |ts: &str| ts.parse::<jiff::Timestamp>().ok().filter(|t| (from..=cx.at.as_second()).contains(&t.as_second()));
    let mut list: Vec<(jiff::Timestamp, &Tick, &'static str)> = Vec::new();
    for t in section.data.iter().flat_map(|d| d.ticks.iter()) {
        if let Some(at) = t.ended_at.as_deref().and_then(within) {
            list.push((at, t, outcome(t)));
        }
        if let Some(at) = within(&t.started_at) {
            list.push((at, t, "started"));
        }
    }
    list.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.loop_id.cmp(&b.1.loop_id)));
    let body = html! {
        @if list.is_empty() { (super::today::none(partial, "No tick receipt in the last hour.")) } @else {
            ul class="mv-list" {
                @for (at, t, what) in list.iter().take(RECEIPTS) {
                    li class="mv-row mv-receipt" data-receipt=(what) {
                        span class="mv-row-meta" {
                            span class="bv-code" { (at.strftime("%H:%M")) } " "
                            span class="mv-row-title" { (t.loop_id) " " (if *what == "started" { "tick started" } else { "tick ended" }) }
                            @if !matches!(*what, "started" | "ended") { " · " (what) }
                        }
                        @if *what != "started" && let Some(s) = &t.summary { p class="mv-row-body" { (s) } }
                    }
                }
            }
            @if list.len() > RECEIPTS { p class="mv-more bv-meta" { (list.len() - RECEIPTS) " more not shown" } }
        }
    };
    // A count only from the whole ledger: from part of it, the hour may hold more.
    let count = (section.data.is_some() && !partial).then_some(list.len());
    card("health-receipts", "receipts", "Receipts, last hour", &section, count, body)
}
