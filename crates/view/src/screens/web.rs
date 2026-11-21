//! The web pages' decision screens (BRO-3783), laid out as the approved design (maestro#20):
//! the queue of open asks as links, the decision, and on a wide screen what the agent tried
//! beside it. One column on a phone, with a pager in place of the queue.
//!
//! Read-only, like every web page: the decision is the same body the App renders, with the
//! same answer bar, and the queue is links between GET pages. Unread is never shown as empty:
//! the calm "nothing needs you" screen appears only when [`nothing_waits`] holds; any other read
//! falls back to the inbox card, which says what could not be read, with the needs card above it
//! when something else is held for the owner.

use super::{Entry, Group, Landing, age, joined, kind, queue};
use crate::catalog::DecisionsProps;
use crate::page::Shell;
use crate::render::{Ctx, errors};
use maud::{Markup, html};

/// The header's facts, from this render's reads.
pub(crate) fn shell(cx: &Ctx) -> Shell {
    let counts = cx.snap.asks.data.as_ref();
    let needs = &cx.snap.needs;
    Shell {
        tree: cx.snap.tree.clone(),
        open: counts.map(|c| (c.open, c.open_blocking)),
        // The tree read alone: an unread answers branch changes no count of open asks.
        partial: !cx.reads.asks.errors.is_empty(),
        needs: needs.data.as_ref().filter(|_| needs.errors.is_empty()).map(Vec::len),
        calm: nothing_waits(cx),
        committed: match cx.committed() {
            Ok(Some(t)) => ago(*t, cx.at),
            _ => None,
        },
    }
}

/// Whether nothing waits on the owner, as far as a whole read can tell: every ask and every answer
/// source read in full with no ask open, and the needs list (owner holds on work as well as asks)
/// read in full and empty. The header and the calm screen both ask this one question.
pub(crate) fn nothing_waits(cx: &Ctx) -> bool {
    let asks = joined(cx, None);
    let needs = &cx.snap.needs;
    // `data.is_some()` restates the whole read for a section that is unread with no error, which
    // no reader builds (`Section::unavailable` carries one): belt and braces, not a proven guard.
    asks.data.is_some()
        && asks.errors.is_empty()
        && queue(cx, None).is_empty()
        && needs.errors.is_empty()
        && needs.data.as_ref().is_some_and(Vec::is_empty)
}

/// A coarse age for the header: under a minute, minutes under an hour, hours under two days, else days.
pub(crate) fn ago(from: jiff::Timestamp, at: jiff::Timestamp) -> Option<String> {
    match at.as_second() - from.as_second() {
        ..0 => None,
        0..60 => Some("less than a minute".into()),
        s @ 60..3_600 => Some(format!("{} min", s / 60)),
        s @ 3_600..172_800 => Some(format!("{} h", s / 3_600)),
        s => Some(format!("{} d", s / 86_400)),
    }
}

/// `/decisions`: the first open ask in queue order, decided in place; the calm screen when
/// nothing waits; otherwise the inbox card with its read state, under the needs card when the
/// needs list holds something or could not be read.
pub(crate) fn decisions(cx: &Ctx, id: &str, p: &DecisionsProps) -> Markup {
    let groups = queue(cx, None);
    let ledgers = by_ledger(&groups);
    let first = ledgers.iter().flat_map(|(_, v)| v).find(|e| e.keyed);
    match first {
        Some(e) => {
            let handle = e.ask.handle();
            layout(cx, &groups, Some((e.lf.file.as_str(), handle)), super::decision(cx, id, &e.lf.file, handle))
        }
        None if nothing_waits(cx) => calm(cx, id),
        None => {
            let needs = &cx.snap.needs;
            let held = !needs.errors.is_empty() || needs.data.as_ref().is_none_or(|n| !n.is_empty());
            html! {
                div class="mv-stack" {
                    @if held { (crate::render::needs(cx, &format!("{id}-needs"), None, 8)) }
                    (super::inbox(cx, id, p))
                }
            }
        }
    }
}

/// `/decisions/<ledger>/<id>`: that ask, with the queue beside it.
pub(crate) fn decide(cx: &Ctx, id: &str, ledger: &str, ask_id: &str) -> Markup {
    let groups = queue(cx, None);
    layout(cx, &groups, Some((ledger, ask_id)), super::decision(cx, id, ledger, ask_id))
}

/// The one row a selection names: the ask whose handle is exactly `handle` in that ledger, else
/// (a legacy link by id) the one whose id is. Resolved once, so a uid that equals another ask's
/// id never marks two rows current.
fn selected_at(rows: &[&Entry], selected: Option<(&str, &str)>) -> Option<usize> {
    let (ledger, handle) = selected?;
    let here = |e: &&&Entry| e.lf.file == ledger;
    rows.iter()
        .position(|e| here(&e) && e.ask.handle() == handle)
        .or_else(|| rows.iter().position(|e| here(&e) && e.ask.id == handle))
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
    let at = selected_at(&rows, selected);
    let section = joined(cx, None);
    let partial = !section.errors.is_empty() || rows.iter().any(|e| e.landing == Landing::Unknown);
    html! {
        div class="mv-decide" {
            (column(cx, &ledgers, rows.len(), at.map(|i| rows[i])))
            div class="mv-decide-main" {
                // The queue column is hidden on a phone, so the pager carries its read state there.
                @if let Some(i) = at { (pager(&rows, i, partial)) }
                @if section.data.is_none() || partial {
                    div class="mv-pager-read" { (read_state(cx, &section)) }
                }
                (main)
            }
        }
    }
}

/// The queue column: each ledger's open asks, the current one marked.
fn column(cx: &Ctx, ledgers: &[(&str, Vec<&Entry>)], total: usize, current: Option<&Entry>) -> Markup {
    let section = joined(cx, None);
    let partial =
        !section.errors.is_empty() || ledgers.iter().flat_map(|(_, v)| v).any(|e| e.landing == Landing::Unknown);
    let twin = |e: &Entry| ledgers.iter().flat_map(|(_, v)| v).filter(|o| o.ask.title() == e.ask.title()).count() > 1;
    html! {
        aside class="mv-queue" aria-label="Waiting on you" data-state=(if section.data.is_none() { "unavailable" } else if partial { "partial" } else { "ok" }) {
            header class="mv-queue-head" {
                h2 class="mv-kicker" { "Waiting on you" }
                span class="mv-queue-count" {
                    @if section.data.is_none() { "?" } @else { (total) @if partial { "+" } }
                }
            }
            @for (ledger, entries) in ledgers {
                section class="mv-qgroup" {
                    p class="mv-qgroup-name bv-code" { bdi { (ledger.trim_end_matches(".yaml").trim_end_matches(".yml")) } " · " (entries.len()) }
                    ul class="mv-list" {
                        @for e in entries {
                            li class="mv-qrow" data-blocking=(e.ask.blocking) data-landing=(match e.landing { Landing::Waiting => "waiting", Landing::NotSeen => "open", Landing::Unknown => "unknown" }) {
                                @if e.keyed {
                                    // `true`, not `page`: the nav's Decisions tab already marks the page.
                                    a href=(href(e)) aria-current=[current.is_some_and(|c| std::ptr::eq(c, *e)).then_some("true")] { (qrow(cx, e, twin(e))) }
                                } @else {
                                    div { (qrow(cx, e, twin(e))) }
                                }
                            }
                        }
                    }
                }
            }
            footer class="mv-queue-foot bv-meta" { (read_state(cx, &section)) }
        }
    }
}

/// What the queue stands on: answered counts, answers not yet landed, and every read error.
/// The column shows it; on a phone, where the column is hidden, the pager's note does.
fn read_state(cx: &Ctx, section: &maestro_core::Section<()>) -> Markup {
    let counts = cx.snap.asks.data.as_ref();
    html! {
        @match counts {
            Some(c) => {
                p { (c.answered) " answered across " (c.ledgers) @if c.ledgers == 1 { " ledger" } @else { " ledgers" } ". Answered asks live in Today." }
                // Reading the working tree itself, nothing lands onto it: no count is made.
                @if cx.store.tree.source != maestro_store::TreeSource::WorkingTree {
                    @match c.unlanded {
                        Some(0) => {},
                        Some(n) => p data-tone="success" { (n) " answered and in effect, waiting to land on main." },
                        None => p data-tone="warning" { "Answers not yet landed could not all be read. This is not the same as none." },
                    }
                }
            },
            None => p class="mv-unavailable" { "The asks could not be read. This is not the same as none." },
        }
        (errors(section))
    }
}

fn qrow(cx: &Ctx, e: &Entry, twin: bool) -> Markup {
    html! {
        span class="mv-qrow-title" { (e.ask.title()) }
        // Two asks with one title are told apart by their handles.
        @if twin { span class="mv-qrow-handle bv-code" { bdi { (e.ask.handle()) } } }
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
fn pager(rows: &[&Entry], i: usize, partial: bool) -> Markup {
    // The nearest row each way that can be linked: a row whose handle is shared has no link.
    let link = |e: &&Entry| e.keyed.then(|| href(e));
    let prev = rows[..i].iter().rev().find_map(link);
    let next = rows[i + 1..].iter().find_map(link);
    html! {
        nav class="mv-pager" aria-label="Queue" {
            span class="mv-pager-at bv-code" { (i + 1) " of " (rows.len()) @if partial { "+" } " · " bdi { (rows[i].project()) } }
            span class="mv-pager-links" {
                @if let Some(h) = prev { a class="mv-pager-link" href=(h) rel="prev" { "Previous" } }
                @if let Some(h) = next { a class="mv-pager-link" href=(h) rel="next" { "Next" } }
            }
        }
    }
}

/// Nothing waits ([`nothing_waits`]). Each count says when its own read was partial or failed.
fn calm(cx: &Ctx, id: &str) -> Markup {
    let s = &cx.snap;
    let whole = |errors: &[maestro_core::SourceError]| errors.is_empty();
    let answered = s.asks.data.as_ref().map(|c| (c.answered, true));
    let loops = s.loops.data.as_ref().map(|l| (l.len(), whole(&s.loops.errors)));
    // Drift comes from the ticks ledger: a loops read with any error may hide some.
    let drifting =
        s.loops.data.as_ref().map(|l| (l.iter().filter(|r| !r.drift.is_empty()).count(), whole(&s.loops.errors)));
    // The snapshot's initiatives are the live ones already (active or blocked on the owner).
    let live = s.initiatives.data.as_ref().map(|i| (i.len(), whole(&s.initiatives.errors)));
    let number = |v: (usize, bool)| if v.1 { v.0.to_string() } else { format!("at least {}", v.0) };
    let stat = |value: Option<(usize, bool)>, label: Markup| {
        html! {
            div class="mv-stat" data-state=(match value { None => "unavailable", Some((_, false)) => "partial", Some(_) => "ok" }) {
                @match value {
                    Some(v) => span class="mv-stat-value" { (number(v)) },
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
                "No ask on " span class="bv-code" { (s.tree) } " is open, and nothing else is held for you. The next decision appears here when an agent files one."
            }
            div class="mv-stats" {
                (stat(answered, html! { "answered" }))
                (stat(loops, html! { "loops" }))
                (stat(drifting, html! { "loops drifting" }))
                (stat(live, html! { "initiatives live" }))
            }
            @if !(whole(&s.loops.errors) && whole(&s.initiatives.errors)) {
                p class="mv-calm-note bv-meta" data-tone="warning" { "Some counts read in part: they are lower bounds." }
            }
        }
    }
}
