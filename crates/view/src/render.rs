//! Render a validated tree to HTML with maud, every value read from one Store.
//!
//! One render reads each source once ([`Reads`]), so every block on a screen agrees with
//! every other. A section that could not be read renders as unavailable with its source and
//! reason, never as 0 or an empty list; a section that read in part says so.

use crate::catalog::{AskFilter, Block, Drop, Gap, Node, Validated};
use maestro_core::Section;
use maestro_core::ask::AskState;
use maestro_store::views::{self, Reads, Snapshot};
use maestro_store::{LedgerFile, Store};
use maud::{Markup, html};
use serde::Serialize;
use sha2::{Digest, Sha256};

const DEFAULT_ROWS: usize = 8;

/// One render: the fragment the app swaps in, its revision, and what was dropped.
#[derive(Debug, Clone, Serialize)]
pub struct Rendered {
    /// The fragment for `#maestro-root`.
    pub html: String,
    /// sha256 of `html`, hex. Equal revisions mean an identical screen.
    pub revision: String,
    /// The tree the store was read at, e.g. `git:origin/main@abc1234`.
    pub tree: String,
    pub drops: Vec<Drop>,
    /// The spec of what was placed: what the app polls with.
    pub spec: serde_json::Value,
}

struct Ctx<'a> {
    store: &'a Store,
    reads: Reads,
    snap: Snapshot,
}

/// Render `v` from `store`, read now. Each call reads the store anew.
pub fn render(store: &Store, v: &Validated) -> Rendered {
    render_at(store, v, views::now())
}

/// Render as of `at` (loop notices depend on the clock); tests pin it.
pub fn render_at(store: &Store, v: &Validated, at: jiff::Timestamp) -> Rendered {
    let reads = Reads::of(store);
    let snap = views::snapshot_of(store, &reads, at);
    let cx = Ctx { store, reads, snap };
    let body = html! {
        div class="mv-screen" {
            (node(&cx, &v.root))
            footer class="mv-foot bv-meta" { "Read from " span class="bv-code" { (cx.snap.tree) } }
        }
    };
    let html = body.into_string();
    let revision = hex(&Sha256::digest(html.as_bytes()));
    Rendered { html, revision, tree: cx.snap.tree.clone(), drops: v.drops.clone(), spec: v.kept.clone() }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn node(cx: &Ctx, n: &Node) -> Markup {
    match &n.block {
        Block::Stack(p) => {
            let gap = match p.gap {
                Gap::Tight => "tight",
                Gap::Normal => "normal",
                Gap::Loose => "loose",
            };
            html! { div class="mv-stack" data-gap=(gap) data-block=(n.id) { @for c in &n.children { (node(cx, c)) } } }
        }
        Block::Snapshot(_) => snapshot(cx, &n.id),
        Block::Needs(p) => needs(cx, &n.id, p.project.as_deref(), rows(p.limit)),
        Block::Asks(p) => asks(cx, &n.id, p),
        Block::Ask(p) => ask(cx, &n.id, &p.ledger, &p.id),
        Block::Work(p) => work(cx, &n.id, p.state, rows(p.limit)),
        Block::Loops(p) => loops(cx, &n.id, p.id.as_deref(), p.drifting),
        Block::Initiatives(p) => initiatives(cx, &n.id, p.all),
        Block::Fleet(_) => fleet(cx, &n.id),
    }
}

fn rows(limit: Option<u8>) -> usize {
    limit.map_or(DEFAULT_ROWS, usize::from)
}

/// A card for one source: its title, a count or "unavailable", the body, and the source line.
fn card<T>(id: &str, kind: &str, title: &str, section: &Section<T>, count: Option<usize>, body: Markup) -> Markup {
    let state = match (&section.data, section.errors.is_empty()) {
        (None, _) => "unavailable",
        (Some(_), false) => "partial",
        (Some(_), true) => "ok",
    };
    html! {
        section class="mv-card bv-card" data-block=(id) data-type=(kind) data-state=(state) {
            header class="mv-head" {
                h2 class="mv-title" { (title) }
                @if section.data.is_none() {
                    span class="mv-count mv-unavailable" { "unavailable" }
                } @else if let Some(n) = count {
                    span class="mv-count" { (n) }
                }
            }
            @if section.data.is_none() {
                p class="mv-note" data-tone="danger" { "Could not be read. This is not the same as none." }
            } @else {
                (body)
            }
            (errors(section))
            p class="mv-source bv-meta" { span class="bv-code" { (section.source) } }
        }
    }
}

fn errors<T>(section: &Section<T>) -> Markup {
    html! {
        @if !section.errors.is_empty() {
            details class="mv-errors" {
                summary data-tone="warning" {
                    @if section.data.is_some() { "Partial: " }
                    (section.errors.len()) @if section.errors.len() == 1 { " source" } @else { " sources" } " could not be read"
                }
                ul { @for e in &section.errors { li { span class="bv-code" { (e.at) } ": " (e.reason) } } }
            }
        }
    }
}

fn empty(text: &str) -> Markup {
    html! { p class="mv-empty bv-meta" { (text) } }
}

fn more(shown: usize, total: usize) -> Markup {
    html! { @if total > shown { p class="mv-more bv-meta" { (total - shown) " more not shown" } } }
}

fn tile(label: &str, value: Option<usize>, tone: &str) -> Markup {
    html! {
        div class="mv-tile" data-tone=(tone) {
            @match value {
                Some(n) => span class="mv-tile-value" { (n) },
                None => span class="mv-tile-value mv-unavailable" { "unavailable" },
            }
            span class="mv-tile-label bv-meta" { (label) }
        }
    }
}

fn snapshot(cx: &Ctx, id: &str) -> Markup {
    let s = &cx.snap;
    let needs = s.needs.data.as_ref().map(Vec::len);
    let open = s.asks.data.as_ref().map(|a| a.open);
    let running = s.work.data.as_ref().map(|w| w.get("Running").copied().unwrap_or(0));
    let stuck = s.work.data.as_ref().map(|w| w.get("Stuck").copied().unwrap_or(0));
    let drifting =
        s.loops.data.as_ref().map(|l| l.iter().filter(|r| !r.drift.is_empty() || !r.notices.is_empty()).count());
    let live = s.initiatives.data.as_ref().map(Vec::len);
    let degraded = [
        s.needs.is_degraded(),
        s.asks.is_degraded(),
        s.work.is_degraded(),
        s.loops.is_degraded(),
        s.initiatives.is_degraded(),
    ]
    .iter()
    .filter(|d| **d)
    .count();
    html! {
        section class="mv-card bv-card" data-block=(id) data-type="snapshot" data-state=(if degraded > 0 { "partial" } else { "ok" }) {
            header class="mv-head" { h2 class="mv-title" { "Snapshot" } }
            div class="mv-tiles" {
                (tile("Needs you", needs, if needs.unwrap_or(0) > 0 { "attention" } else { "neutral" }))
                (tile("Open asks", open, "neutral"))
                (tile("Running", running, "neutral"))
                (tile("Stuck", stuck, if stuck.unwrap_or(0) > 0 { "danger" } else { "neutral" }))
                (tile("Loops drifting", drifting, if drifting.unwrap_or(0) > 0 { "warning" } else { "neutral" }))
                (tile("Live initiatives", live, "neutral"))
            }
            @if degraded > 0 {
                p class="mv-note" data-tone="warning" {
                    (degraded) @if degraded == 1 { " section" } @else { " sections" } " read in part or not at all; counts are lower bounds or unavailable."
                }
            }
        }
    }
}

fn needs(cx: &Ctx, id: &str, project: Option<&str>, limit: usize) -> Markup {
    let section = &cx.snap.needs;
    let list: Vec<_> =
        section.data.iter().flatten().filter(|n| project.is_none() || n.project.as_deref() == project).collect();
    let body = html! {
        @if list.is_empty() { (empty("Nothing needs you.")) } @else {
            ul class="mv-list" {
                @for n in list.iter().take(limit) {
                    li class="mv-row" data-key=(n.key) {
                        span class="mv-row-title" { (n.title) }
                        span class="mv-row-meta bv-meta" {
                            (label(&n.kind))
                            @if let Some(p) = &n.project { " · " (p) }
                            @if let Some(since) = &n.since { " · since " (since) }
                        }
                    }
                }
            }
            (more(list.len().min(limit), list.len()))
        }
    };
    card(id, "needs", "Needs you", section, section.data.as_ref().map(|_| list.len()), body)
}

fn ask_matches(filter: AskFilter, state: AskState) -> bool {
    match filter {
        AskFilter::Open => state == AskState::Open,
        AskFilter::Answered => state == AskState::Answered,
        AskFilter::Archived => state == AskState::Archived,
        AskFilter::All => true,
    }
}

fn asks(cx: &Ctx, id: &str, p: &crate::catalog::AsksProps) -> Markup {
    let section = &cx.reads.asks;
    let list: Vec<_> = section
        .data
        .iter()
        .flatten()
        .flat_map(|lf| lf.ledger.asks.iter().map(move |a| (lf, a)))
        .filter(|(_, a)| ask_matches(p.state, a.state()))
        .filter(|(_, a)| !p.blocking || a.blocking)
        .filter(|(_, a)| p.project.is_none() || a.project == p.project)
        .collect();
    let limit = rows(p.limit);
    let title = match p.state {
        AskFilter::Open => "Open asks",
        AskFilter::Answered => "Answered asks",
        AskFilter::Archived => "Archived asks",
        AskFilter::All => "Asks",
    };
    let body = html! {
        @if list.is_empty() { (empty("No asks match.")) } @else {
            ul class="mv-list" {
                @for (lf, a) in list.iter().take(limit) {
                    li class="mv-row" data-ask=(format!("{}#{}", lf.file, a.handle())) {
                        span class="mv-row-title" { (a.title()) }
                        span class="mv-row-meta bv-meta" {
                            (lf.file)
                            @if a.blocking { " · " span data-tone="attention" { "blocking" } }
                            @if let Some(at) = &a.asked_at { " · " (at) }
                        }
                    }
                }
            }
            (more(list.len().min(limit), list.len()))
        }
    };
    card(id, "asks", title, section, section.data.as_ref().map(|_| list.len()), body)
}

fn ask(cx: &Ctx, id: &str, ledger: &str, ask_id: &str) -> Markup {
    let section = &cx.reads.asks;
    let file: Option<&LedgerFile> = section.data.iter().flatten().find(|lf| lf.file == ledger);
    let matches: Vec<_> = file
        .into_iter()
        .flat_map(|lf| lf.ledger.asks.iter())
        .filter(|a| a.uid.as_deref() == Some(ask_id) || (a.uid.is_none() && a.id == ask_id))
        .collect();
    let handle = format!("{ledger}#{ask_id}");
    let body = match matches.as_slice() {
        [a] => html! {
            div class="mv-ask" data-ask=(handle) data-hash=(a.hash()) {
                p class="mv-ask-title" { (a.title()) }
                p class="mv-row-meta bv-meta" {
                    (label(&a.state()))
                    @if a.blocking { " · " span data-tone="attention" { "blocking" } }
                    @if let Some(p) = &a.project { " · " (p) }
                    @if let Some(at) = &a.asked_at { " · asked " (at) }
                }
                @if let Some(why) = &a.why_you { p class="mv-ask-why" { (why) } }
                @if !a.options.is_empty() {
                    ol class="mv-options" {
                        @for o in &a.options {
                            li class="mv-option" data-option=(o.id) data-recommended=(o.recommended) {
                                span class="mv-option-label" { (o.label) }
                                @if o.recommended { " " span class="mv-badge" data-tone="info" { "recommended" } }
                                @if let Some(c) = &o.consequence { p class="mv-option-consequence bv-meta" { (c) } }
                            }
                        }
                    }
                }
                @if let Some(answer) = a.answer_option.as_ref().or(a.answer.as_ref()) {
                    p class="mv-answer" data-tone="success" { "Answered: " (answer) }
                }
                p class="mv-hash bv-meta" { "entry " span class="bv-code" { (a.hash().get(..12).unwrap_or_default()) } }
            }
        },
        [] if file.is_none() => {
            html! { p class="mv-note" data-tone="warning" { "No ledger named " span class="bv-code" { (ledger) } "." } }
        }
        [] => html! { p class="mv-note" data-tone="warning" { "No ask " span class="bv-code" { (handle) } "." } },
        many => {
            html! { p class="mv-note" data-tone="warning" { span class="bv-code" { (handle) } " names " (many.len()) " asks; address it by uid." } }
        }
    };
    // Errors that may hide the asked-for entry belong on this card; a different ledger's do not.
    let scoped = scope_errors(cx, section, ledger);
    card(id, "ask", "Ask", &scoped, None, body)
}

fn scope_errors<'a>(cx: &Ctx, section: &'a Section<Vec<LedgerFile>>, ledger: &str) -> Section<&'a Vec<LedgerFile>> {
    let mut s = section.as_ref();
    let file_at = cx.store.rel(maestro_store::ASKS).map(|dir| format!("{dir}/{ledger}"));
    s.errors.retain(|e| {
        e.at == section.source
            || file_at.as_ref().is_ok_and(|f| e.at == *f || e.at.starts_with(&format!("{f}#")))
            || file_at.is_err()
    });
    s
}

fn work(cx: &Ctx, id: &str, state: Option<maestro_core::work::WorkState>, limit: usize) -> Markup {
    let section = &cx.reads.work;
    let list = views::work_by_state(section.data.as_deref().unwrap_or_default(), state);
    let title = state.map_or("Work", |s| s.label());
    let body = html! {
        @if list.is_empty() { (empty("No work in this state.")) } @else {
            ul class="mv-list" {
                @for w in list.iter().take(limit) {
                    li class="mv-row" data-work=(w.slug) {
                        span class="mv-row-title" { (w.title) }
                        span class="mv-row-meta bv-meta" {
                            span class="mv-state" data-state=(w.work_state.label()) { (w.work_state.label()) }
                            @if let Some(i) = &w.filed_under { " · " (i) }
                            @if let Some(at) = &w.last_event_at { " · " (at) }
                        }
                    }
                }
            }
            (more(list.len().min(limit), list.len()))
        }
    };
    card(id, "work", title, section, section.data.as_ref().map(|_| list.len()), body)
}

fn loops(cx: &Ctx, id: &str, only: Option<&str>, drifting: bool) -> Markup {
    let section = &cx.snap.loops;
    let list: Vec<_> = section
        .data
        .iter()
        .flatten()
        .filter(|r| only.is_none_or(|o| r.id == o))
        .filter(|r| !drifting || !r.drift.is_empty() || !r.notices.is_empty())
        .collect();
    let body = html! {
        @if list.is_empty() { (empty(if drifting { "No loop is drifting." } else { "No loops." })) } @else {
            ul class="mv-list" {
                @for r in &list {
                    li class="mv-row" data-loop=(r.id) {
                        span class="mv-row-title" { (r.title) }
                        span class="mv-row-meta bv-meta" {
                            (r.cadence) " · " (r.substrate)
                            @match &r.last_tick {
                                Some(t) => { " · last tick " (t.started_at) },
                                None => { " · no tick recorded" },
                            }
                        }
                        @for d in &r.drift { span class="mv-badge" data-tone="warning" { (label(d)) } }
                        @for n in &r.notices { span class="mv-badge" data-tone="info" { (label(n)) } }
                    }
                }
            }
        }
    };
    card(id, "loops", "Loops", section, section.data.as_ref().map(|_| list.len()), body)
}

/// The serde name of a drift class or notice, which is its label everywhere else too.
fn label<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s.replace('_', " "),
        Ok(serde_json::Value::Object(o)) => {
            o.get("kind").or_else(|| o.get("class")).and_then(|k| k.as_str()).unwrap_or("notice").replace('_', " ")
        }
        _ => "notice".into(),
    }
}

fn initiatives(cx: &Ctx, id: &str, all: bool) -> Markup {
    let section = &cx.reads.initiatives;
    let list: Vec<_> = section.data.iter().flatten().filter(|i| all || i.status.is_live()).collect();
    let body = html! {
        @if list.is_empty() { (empty("No live initiatives.")) } @else {
            ul class="mv-list" {
                @for i in &list {
                    li class="mv-row" data-initiative=(i.id) {
                        span class="mv-row-title" { (i.title) }
                        span class="mv-row-meta bv-meta" { (label(&i.status)) }
                        @if !i.next_action.is_empty() { p class="mv-row-body" { "Next: " (i.next_action) } }
                    }
                }
            }
        }
    };
    card(id, "initiatives", "Initiatives", section, section.data.as_ref().map(|_| list.len()), body)
}

fn fleet(cx: &Ctx, id: &str) -> Markup {
    let section = &cx.reads.fleet;
    let list: Vec<_> = section.data.iter().flatten().collect();
    let body = html! {
        @if list.is_empty() { (empty("No fleet report.")) } @else {
            ul class="mv-list" {
                @for f in &list {
                    li class="mv-row" data-scope=(f.scope) {
                        span class="mv-row-title" { (f.scope) }
                        span class="mv-row-meta bv-meta" {
                            "tick " (f.tick) " · " (f.ts) " · " (f.sessions) " sessions · " (f.drift) " drift"
                            @if f.dry_run == Some(true) { " · dry run" }
                        }
                    }
                }
            }
        }
    };
    card(id, "fleet", "Fleet", section, section.data.as_ref().map(|_| list.len()), body)
}
