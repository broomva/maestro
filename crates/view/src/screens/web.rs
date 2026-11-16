//! The web pages' decision screens (BRO-3783), laid out as the approved design (maestro#20):
//! the queue of open asks as links, the decision, and on a wide screen what the agent tried
//! beside it. One column on a phone, with a pager in place of the queue.
//!
//! Read-only, like every web page: the decision is the same body the App renders, with the
//! same answer bar, and the queue is links between GET pages. Unread is never shown as empty:
//! the calm "nothing needs you" screen appears only when every ask read in full and none is
//! open; any other read falls back to the inbox card, which says what could not be read.

use super::{Entry, Group, Landing, age, joined, kind, queue};
use crate::catalog::DecisionsProps;
use crate::page::Shell;
use crate::render::{Ctx, errors};
use maestro_core::initiative::InitiativeStatus;
use maud::{Markup, html};

/// The header's facts, from this render's reads.
pub(crate) fn shell(cx: &Ctx) -> Shell {
    let counts = cx.snap.asks.data.as_ref();
    Shell {
        tree: cx.snap.tree.clone(),
        open: counts.map(|c| (c.open, c.open_blocking)),
        // The tree read alone: an unread answers branch changes no count of open asks.
        partial: !cx.reads.asks.errors.is_empty(),
        committed: match cx.committed() {
            Ok(Some(t)) => ago(*t, cx.at),
            _ => None,
        },
    }
}

/// A coarse age for the header: minutes under an hour, hours under two days, else days.
pub(crate) fn ago(from: jiff::Timestamp, at: jiff::Timestamp) -> Option<String> {
    match at.as_second() - from.as_second() {
        ..0 => None,
        s @ 0..3_600 => Some(format!("{} min", s / 60)),
        s @ 3_600..172_800 => Some(format!("{} h", s / 3_600)),
        s => Some(format!("{} d", s / 86_400)),
    }
}

/// `/decisions`: the first open ask in queue order, decided in place; the calm screen when the
/// asks read in full and none is open; otherwise the inbox card with its read state.
pub(crate) fn decisions(cx: &Ctx, id: &str, p: &DecisionsProps) -> Markup {
    let section = joined(cx, None);
    let groups = queue(cx, None);
    let ledgers = by_ledger(&groups);
    let first = ledgers.iter().flat_map(|(_, v)| v).find(|e| e.keyed);
    match first {
        Some(e) => {
            let handle = e.ask.handle();
            layout(cx, &groups, Some((e.lf.file.as_str(), handle)), super::decision(cx, id, &e.lf.file, handle))
        }
        // Calm needs a whole read. `data.is_some()` restates it for a section that is unread with
        // no error, which the asks reader never builds (`Section::unavailable` carries one), so no
        // test can tell it apart: it is belt and braces, not a proven guard.
        None if section.data.is_some() && section.errors.is_empty() && groups.is_empty() => calm(cx, id),
        None => super::inbox(cx, id, p),
    }
}

/// `/decisions/<ledger>/<id>`: that ask, with the queue beside it.
pub(crate) fn decide(cx: &Ctx, id: &str, ledger: &str, ask_id: &str) -> Markup {
    let groups = queue(cx, None);
    layout(cx, &groups, Some((ledger, ask_id)), super::decision(cx, id, ledger, ask_id))
}

fn is(e: &Entry, selected: Option<(&str, &str)>) -> bool {
    selected.is_some_and(|(ledger, handle)| e.lf.file == ledger && (e.ask.handle() == handle || e.ask.id == handle))
}

/// A path segment: every byte but the unreserved ones percent-encoded, so a ledger value can
/// add no segment, query or fragment to the link.
fn segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub(crate) fn href(e: &Entry) -> String {
    format!("/decisions/{}/{}", segment(&e.lf.file), segment(e.ask.handle()))
}

/// The open asks by ledger: ledgers in the order of their first ask in the queue (blocking
/// first, then oldest), each ledger's asks in queue order. The column, the pager and the first
/// decision all read this one order.
fn by_ledger<'e, 'a>(groups: &'e [Group<'a>]) -> Vec<(&'e str, Vec<&'e Entry<'a>>)> {
    let mut ledgers: Vec<(&str, Vec<&Entry>)> = Vec::new();
    for e in groups.iter().flat_map(|g| &g.rows) {
        match ledgers.iter_mut().find(|(l, _)| *l == e.lf.file) {
            Some((_, v)) => v.push(e),
            None => ledgers.push((e.lf.file.as_str(), vec![e])),
        }
    }
    ledgers
}

fn layout(cx: &Ctx, groups: &[Group], selected: Option<(&str, &str)>, main: Markup) -> Markup {
    let ledgers = by_ledger(groups);
    let rows: Vec<&Entry> = ledgers.iter().flat_map(|(_, v)| v.iter().copied()).collect();
    let at = rows.iter().position(|e| is(e, selected));
    html! {
        div class="mv-decide" {
            (column(cx, &ledgers, rows.len(), selected))
            div class="mv-decide-main" {
                @if let Some(i) = at { (pager(&rows, i)) }
                (main)
            }
        }
    }
}

/// The queue column: each ledger's open asks, the current one marked.
fn column(cx: &Ctx, ledgers: &[(&str, Vec<&Entry>)], total: usize, selected: Option<(&str, &str)>) -> Markup {
    let section = joined(cx, None);
    let partial =
        !section.errors.is_empty() || ledgers.iter().flat_map(|(_, v)| v).any(|e| e.landing == Landing::Unknown);
    let counts = cx.snap.asks.data.as_ref();
    html! {
        aside class="mv-queue" aria-label="Waiting on you" data-state=(if section.data.is_none() { "unavailable" } else if partial { "partial" } else { "ok" }) {
            header class="mv-queue-head" {
                h2 class="mv-kicker" { "Waiting on you" }
                span class="mv-queue-count" { (total) @if partial { "+" } }
            }
            @for (ledger, entries) in ledgers {
                section class="mv-qgroup" {
                    p class="mv-qgroup-name bv-code" { bdi { (ledger.trim_end_matches(".yaml").trim_end_matches(".yml")) } " · " (entries.len()) }
                    ul class="mv-list" {
                        @for e in entries {
                            li class="mv-qrow" data-blocking=(e.ask.blocking) data-landing=(match e.landing { Landing::Waiting => "waiting", Landing::NotSeen => "open", Landing::Unknown => "unknown" }) {
                                @if e.keyed {
                                    a href=(href(e)) aria-current=[is(e, selected).then_some("page")] { (qrow(cx, e)) }
                                } @else {
                                    div { (qrow(cx, e)) }
                                }
                            }
                        }
                    }
                }
            }
            footer class="mv-queue-foot bv-meta" {
                @match counts {
                    Some(c) => {
                        p { (c.answered) " answered across " (c.ledgers) " ledgers. Answered asks live in Today." }
                        // Reading the working tree itself, nothing lands onto it: no count is made.
                        @if cx.store.tree.source != maestro_store::TreeSource::WorkingTree {
                            @match c.unlanded {
                                Some(0) => {},
                                Some(n) => p data-tone="success" { (n) " answered, not yet on " span class="bv-code" { (cx.snap.tree) } "." },
                                None => p data-tone="warning" { "Answers not yet landed could not all be read. This is not the same as none." },
                            }
                        }
                    },
                    None => p class="mv-unavailable" { "The asks could not be read. This is not the same as none." },
                }
                (errors(&section))
            }
        }
    }
}

fn qrow(cx: &Ctx, e: &Entry) -> Markup {
    html! {
        span class="mv-qrow-title" { (e.ask.title()) }
        span class="mv-qrow-meta bv-code" {
            bdi { (e.project()) } " · " (kind(e.ask))
            @if let Some(a) = e.asked().and_then(|t| age(t, cx.at)) { " · " (a) }
            @if e.ask.blocking { " · " span data-tone="attention" { "blocking" } }
            @match e.landing {
                Landing::Waiting => { " · " span data-tone="success" { "waiting to land" } },
                Landing::Unknown => { " · " span data-tone="warning" { "state not known" } },
                Landing::NotSeen => {},
            }
            @if !e.keyed { " · " span data-tone="warning" { "shares its handle" } }
        }
    }
}

/// The phone's place in the queue: `1 of 3 · project`, with the previous and next asks.
fn pager(rows: &[&Entry], i: usize) -> Markup {
    let link = |j: usize| rows.get(j).filter(|e| e.keyed).map(|e| href(e));
    html! {
        nav class="mv-pager" aria-label="Queue" {
            span class="mv-pager-at bv-code" { (i + 1) " of " (rows.len()) " · " bdi { (rows[i].project()) } }
            span class="mv-pager-links" {
                @if let Some(h) = i.checked_sub(1).and_then(link) { a class="mv-pager-link" href=(h) rel="prev" { "Previous" } }
                @if let Some(h) = link(i + 1) { a class="mv-pager-link" href=(h) rel="next" { "Next" } }
            }
        }
    }
}

/// Every ask read in full and none open.
fn calm(cx: &Ctx, id: &str) -> Markup {
    let s = &cx.snap;
    let answered = s.asks.data.as_ref().map(|c| c.answered);
    let loops = s.loops.data.as_ref().map(Vec::len);
    let drifting = s.loops.data.as_ref().map(|l| l.iter().filter(|r| !r.drift.is_empty()).count());
    let live = s.initiatives.data.as_ref().map(|i| i.iter().filter(|i| i.status == InitiativeStatus::Active).count());
    let stat = |value: Option<usize>, label: Markup| {
        html! {
            div class="mv-stat" {
                @match value {
                    Some(n) => span class="mv-stat-value" { (n) },
                    None => span class="mv-stat-value mv-unavailable" { "unavailable" },
                }
                span class="mv-stat-label bv-meta" { (label) }
            }
        }
    };
    html! {
        section class="mv-calm" data-block=(id) data-type="decisions" data-state="ok" {
            p class="mv-kicker" { span class="mv-dot" data-tone="success" aria-hidden="true" {} "All clear" }
            h1 class="mv-calm-title" { "Nothing needs " em { "you" } }
            p class="mv-calm-sub" {
                "Every ask on " span class="bv-code" { (s.tree) } " has an answer. The next decision appears here when an agent files one."
            }
            div class="mv-stats" {
                (stat(answered, html! { "answered" }))
                (stat(loops, html! { "loops" @if let Some(d) = drifting { ", " (d) " drifting" } }))
                (stat(live, html! { "initiatives live" }))
            }
        }
    }
}
