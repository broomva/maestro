//! The Flows pages: every flow kernel the ledger holds, its shape, ontology and lineage, and each
//! run as it moves. Rendered from [`crate::flows::Ledger`], which the MCP layer reads from the
//! flow ledger for each request; a page that is live carries `data-live`, the poll interval the
//! owner script re-reads it at.
use crate::flows::{self, CLASSES, EdgeKind, Instance, Kernel, Layout, Ledger, Live, NodeState, RunState, Shape};
use maud::{Markup, html};
use std::collections::BTreeMap;

/// Which Flows page to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowsPage {
    /// Every kernel, and the runs in flight.
    List,
    /// One kernel by flow name.
    Kernel(String),
    /// One run by instance id.
    Run(String),
}

/// How often a page re-reads itself, in milliseconds.
pub const LIVE_RUN_MS: u32 = 2_000;
pub const LIVE_LIST_MS: u32 = 5_000;
pub const IDLE_MS: u32 = 15_000;
/// The most log lines a run page lists.
pub const MAX_STEPS: usize = 300;

/// The page's markup and title; `None` only when the ledger was read in full and holds no such
/// kernel or run. A ledger or instance that was not read still renders, saying so: an unread run
/// is not an absent one.
pub fn flows_page(ledger: &Ledger, page: &FlowsPage, at: jiff::Timestamp) -> Option<(String, String)> {
    let unread_run = |id: &str| ledger.unread.iter().find(|u| u.instance == id);
    let (body, title) = match (page, &ledger.read) {
        (FlowsPage::List, _) => (list(ledger, at), "Flows".to_string()),
        (FlowsPage::Kernel(name), Ok(instances)) => {
            let ks = flows::kernels(instances);
            match ks.iter().find(|k| k.name == *name) {
                Some(k) => (kernel(ledger, k, at), format!("{name} · Flows")),
                None if !ledger.unread.is_empty() => (not_read(ledger, name, false, at), format!("{name} · Flows")),
                None => return None,
            }
        }
        (FlowsPage::Run(id), Ok(instances)) => match instances.iter().find(|i| i.id == *id) {
            Some(i) => {
                // Its nodes link to the kernel page's ontology rows only when that page describes
                // the revision this run pinned (the latest).
                let ks = flows::kernels(instances);
                let latest = ks.iter().find(|k| k.name == i.flow).is_some_and(|k| {
                    let l = k.latest();
                    (l.version, l.ir_hash) == (i.version, i.ir_hash.as_str()) && l.shape.is_some()
                });
                (run(ledger, i, latest, at), format!("{} {} · Flows", i.kernel(), short_id(&i.id)))
            }
            None if unread_run(id).is_some() => (not_read(ledger, id, true, at), format!("{} · Flows", short_id(id))),
            None => return None,
        },
        (_, Err(_)) => (list(ledger, at), "Flows".to_string()),
    };
    Some((html! { div class="mv-screen" { (body) } }.into_string(), title))
}

/// A kernel or run the read could not establish: the run's own reason when it is listed unread,
/// else the unread runs that could hold the kernel.
fn not_read(ledger: &Ledger, what: &str, is_run: bool, at: jiff::Timestamp) -> Markup {
    let own = ledger.unread.iter().find(|u| is_run && u.instance == what);
    html! {
        div class="mf" data-live=(LIVE_LIST_MS) {
            nav class="mf-crumbs bv-meta" aria-label="Breadcrumb" { a href="/flows" { "Flows" } " / " (what) }
            header class="mf-head" {
                h1 { (what) }
                (live_line(ledger.read_at.unwrap_or(at), ledger.read.is_ok()))
            }
            @match own {
                Some(u) => p class="mf-unavailable" role="status" { "This run is in the ledger but its log was not read, so its state is unknown. " (u.reason) },
                None => p class="mf-unavailable" role="status" {
                    "No run that was read belongs to this kernel, and " (plural(ledger.unread.len(), "run was", "runs were")) " not read, so it may be among them."
                },
            }
            (unavailable(ledger, Scope::Uncounted))
            (source(ledger))
        }
    }
}

fn ts(s: &str) -> Option<jiff::Timestamp> {
    s.parse().ok()
}

fn local(s: &str, fmt: &str) -> String {
    ts(s).map_or_else(|| "time unknown".into(), |t| t.to_zoned(jiff::tz::TimeZone::system()).strftime(fmt).to_string())
}

/// How long before `at`: `12 s`, `4 min`, `3 h`, `2 d`.
pub(crate) fn ago(s: &str, at: jiff::Timestamp) -> String {
    let Some(t) = ts(s) else { return "at an unknown time".into() };
    let secs = at.as_second() - t.as_second();
    match secs {
        ..0 => "ahead of this clock".into(),
        0..60 => format!("{secs} s ago"),
        60..3_600 => format!("{} min ago", secs / 60),
        3_600..86_400 => format!("{} h ago", secs / 3_600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

fn short_hash(h: &str) -> String {
    h.strip_prefix("sha256:").unwrap_or(h).chars().take(10).collect()
}

fn short_id(id: &str) -> String {
    let rest = id.strip_prefix("fi_").unwrap_or(id);
    format!("fi_{}", rest.chars().take(8).collect::<String>())
}

fn kernel_href(name: &str) -> String {
    format!("/flows/kernel/{}", crate::screens::web::segment(name))
}

fn run_href(id: &str) -> String {
    format!("/flows/run/{}", crate::screens::web::segment(id))
}

fn chip(s: RunState) -> Markup {
    html! { span class="mf-chip" data-state=(s.name()) { (s.label()) } }
}

/// `3 runs`, `1 run`.
fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The live line: the poll interval and the clock the ledger was read at (a shared walk may be a
/// second older than the page).
/// The page says only when the ledger was read (or tried); the live script marks it live and puts
/// its notes beside the time, so a page without the script never claims to update.
fn live_line(at: jiff::Timestamp, read: bool) -> Markup {
    let local = at.to_zoned(jiff::tz::TimeZone::system()).strftime("%H:%M:%S").to_string();
    html! {
        p class="mf-live bv-meta" data-live-status {
            span class="mf-live-dot" aria-hidden="true" {}
            span { @if read { "Read at " } @else { "Not read, tried at " } time datetime=(at) { (local) } }
            span data-live-note {}
        }
    }
}

fn progress(i: &Instance) -> Markup {
    html! {
        @match i.progress() {
            Some((done, total)) => span class="mf-progress" title=(format!("{done} of {total} nodes done")) {
                meter min="0" max=(total.max(1)) value=(done) {}
                span class="bv-meta" { (done) "/" (total) }
            },
            None => span class="mf-progress bv-meta" { "progress unknown" },
        }
    }
}

/// Where a run is, in a phrase: its running, waiting and ready nodes.
fn whereabouts(i: &Instance) -> String {
    let Some(at) = i.frontier() else { return "Not replayed, so where it is is unknown".into() };
    let verdicts = i.live.as_ref().map(|l| l.verdicts.as_slice()).unwrap_or_default();
    if at.is_empty() && !verdicts.is_empty() {
        return format!("Waiting on the until verdict of repeat {}", verdicts.join(", "));
    }
    if at.is_empty() {
        return match i.state() {
            RunState::Done => "Finished".into(),
            RunState::Stuck => "Stopped on a failure".into(),
            RunState::Retired => "Retired".into(),
            _ => "Nothing to do now".into(),
        };
    }
    at.iter().map(|(id, s)| format!("{id} ({})", s.label().to_lowercase())).collect::<Vec<_>>().join(", ")
}

/// The runs a page's statements stand on, so its unknown-runs notice speaks of exactly the runs its
/// counts cover: the whole ledger (the list), one kernel's runs (its page), or none (a page that
/// states no count).
enum Scope<'a> {
    Ledger,
    Kernel(&'a [&'a Instance]),
    Uncounted,
}

fn unavailable(ledger: &Ledger, scope: Scope) -> Markup {
    let all: Vec<&Instance> = ledger.read.as_deref().unwrap_or_default().iter().collect();
    // The read runs the page's counts cover, and whether the runs listed but not read are among them.
    let (runs, listed_counted): (&[&Instance], bool) = match &scope {
        Scope::Ledger | Scope::Uncounted => (&all, true),
        Scope::Kernel(runs) => (runs, false),
    };
    let tally = flows::Tally::of(runs.iter().copied(), if listed_counted { ledger.unread.len() } else { 0 });
    html! {
        @if let Err(why) = &ledger.read {
            p class="mf-unavailable" role="status" {
                "The flow ledger was not read, so no flow is shown. This is not the same as no flows. "
                (why)
            }
        }
        @if ledger.read.is_ok() && tally.unknown > 0 {
            details class="mf-unavailable" id="mf-unread" {
                summary {
                    @if matches!(scope, Scope::Uncounted) {
                        (plural(tally.unknown, "run in this ledger was", "runs in this ledger were")) " not read or not replayed, so "
                        @if tally.unknown == 1 { "its state is" } @else { "their states are" } " unknown."
                    } @else {
                        (tally.unknown_note())
                    }
                }
                ul {
                    @if listed_counted { @for u in &ledger.unread { li { code { (u.instance) } " · not read · " (u.reason) } } }
                    @for i in runs {
                        @if let Err(why) = &i.live { li { code { (i.id) } " · not replayed · " (why) } }
                    }
                }
            }
        }
        @if !listed_counted && !ledger.unread.is_empty() {
            p class="mf-unavailable" role="status" {
                (plural(ledger.unread.len(), "run was", "runs were")) " listed but not read, so which kernel each belongs to is unknown; any may belong here, and the counts above do not include them."
            }
        }
    }
}

fn source(ledger: &Ledger) -> Markup {
    html! {
        details class="mf-source" id="mf-source" {
            summary { "Data source" }
            p { "The flow ledger at " code { (ledger.root) } @if let Some(h) = &ledger.host { ", bound to host " code { (h) } } "." }
            p {
                "Each instance's kernel is the IR its Started event pinned, and its state is the kernel's own replay of its log at the read's clock, as maestro_flow_status reports it. "
                "Instances moved to retired/ are not listed."
            }
        }
    }
}

fn list(ledger: &Ledger, at: jiff::Timestamp) -> Markup {
    let instances = ledger.read.as_deref().unwrap_or_default();
    let ks = flows::kernels(instances);
    // A run whose state is unknown is listed with the open ones: it may be moving.
    let mut open: Vec<&Instance> =
        instances.iter().filter(|i| i.state().open() || i.state() == RunState::Unread).collect();
    open.sort_by(|a, b| (a.state(), &b.last_at).cmp(&(b.state(), &a.last_at)));
    let hour = |i: &&Instance| ts(&i.last_at).is_some_and(|t| at.as_second() - t.as_second() < 3_600);
    let (active, idle): (Vec<&Instance>, Vec<&Instance>) =
        open.iter().partition(|i| i.running() || matches!(i.state(), RunState::Asking | RunState::Unread) || hour(i));
    let tally = ledger.tally().unwrap_or_default();
    let every = if tally.running > 0 { LIVE_RUN_MS } else { LIVE_LIST_MS };
    html! {
        div class="mf" data-live=(every) {
            header class="mf-head" {
                h1 { "Flows" }
                @if ledger.read.is_ok() {
                    p class="mf-lead" {
                        (tally.line()) ", in " @if !ledger.unread.is_empty() { "at least " } (plural(ks.len(), "kernel", "kernels")) "."
                    }
                }
                (live_line(ledger.read_at.unwrap_or(at), ledger.read.is_ok()))
            }
            (unavailable(ledger, Scope::Ledger))
            @if ledger.read.as_ref().is_ok_and(Vec::is_empty) && ledger.unread.is_empty() {
                p class="mf-empty" { "No run is listed in this ledger. Runs retired out of it are not shown." }
            }
            @if ledger.read.is_ok() {
                section class="mf-section" aria-labelledby="mf-inflight" {
                    h2 id="mf-inflight" { "In flight" }
                    @if active.is_empty() && tally.may_say_none() {
                        p class="bv-meta" { "No open run is running or moved in the last hour." }
                    } @else if active.is_empty() {
                        p class="bv-meta" { "No open run that was read is running or moved in the last hour; " (plural(tally.unknown, "run's state is", "runs' states are")) " unknown." }
                    } @else {
                        ul class="mf-runs" { @for i in &active { (run_row(i, at)) } }
                    }
                    @if !idle.is_empty() {
                        details id="mf-idle" {
                            summary { (plural(idle.len(), "open run", "open runs")) " idle for over an hour" }
                            ul class="mf-runs" { @for i in &idle { (run_row(i, at)) } }
                        }
                    }
                }
                section class="mf-section" aria-labelledby="mf-kernels" {
                    h2 id="mf-kernels" { "Kernels" }
                    div class="mf-cards" { @for k in &ks { (card(k, at)) } }
                }
            }
            (source(ledger))
        }
    }
}

fn run_row(i: &Instance, at: jiff::Timestamp) -> Markup {
    html! {
        li class="mf-run" {
            a href=(run_href(&i.id)) {
                span class="mf-run-main" {
                    (chip(i.state()))
                    strong { (i.kernel()) }
                    span class="bv-code bv-meta" { (short_id(&i.id)) }
                }
                span class="mf-run-at" { (whereabouts(i)) }
                span class="mf-run-meta bv-meta" { (progress(i)) " · moved " (ago(&i.last_at, at)) }
            }
        }
    }
}

fn composition(shape: &Shape) -> Vec<(&'static str, usize)> {
    CLASSES
        .iter()
        .map(|(c, _)| (*c, shape.nodes.iter().filter(|n| n.class == *c).count()))
        .filter(|(_, n)| *n > 0)
        .collect()
}

/// Each version of a kernel with the IR hashes its runs pinned, oldest first.
fn versions<'a>(k: &Kernel<'a>) -> Vec<(u32, Vec<&'a str>)> {
    let mut out: Vec<(u32, Vec<&str>)> = Vec::new();
    for r in &k.revisions {
        match out.iter_mut().find(|(v, _)| *v == r.version) {
            Some((_, hashes)) => hashes.push(r.ir_hash),
            None => out.push((r.version, vec![r.ir_hash])),
        }
    }
    out
}

/// A kernel's card states no count: a run listed but not read may belong to any kernel, so only
/// the kernel's page, which says so, counts its runs.
fn card(k: &Kernel, at: jiff::Timestamp) -> Markup {
    let latest = k.latest();
    html! {
        a class="mf-card" href=(kernel_href(&k.name)) {
            span class="mf-card-head" {
                strong { (k.name) }
                span class="mf-versions" {
                    @for (v, hashes) in versions(k) {
                        span class="mf-version" title=(hashes.join("\n")) {
                            "@" (v)
                        }
                    }
                }
            }
            @match latest.shape {
                Some(shape) => {
                    span class="mf-thumb" { (graph(shape, None, &BTreeMap::new(), true, None)) }
                    span class="mf-classes" {
                        @for (c, n) in composition(shape) {
                            span class="mf-class" data-class=(c) { (n) " " (c) }
                        }
                    }
                },
                None => span class="mf-card-meta bv-meta" { "Shape unverified: no run of its latest revision replayed" },
            }
            span class="mf-card-meta bv-meta" { "Last moved " (ago(k.last_at(), at)) }
        }
    }
}

fn kernel(ledger: &Ledger, k: &Kernel, at: jiff::Timestamp) -> Markup {
    let latest = k.latest();
    let runs = k.runs();
    let tally = flows::Tally::of(runs.iter().copied(), 0);
    let every = if tally.running > 0 { LIVE_RUN_MS } else { LIVE_LIST_MS };
    html! {
        div class="mf" data-live=(every) {
            nav class="mf-crumbs bv-meta" aria-label="Breadcrumb" { a href="/flows" { "Flows" } " / " (k.name) }
            header class="mf-head" {
                h1 { (k.name) span class="mf-at" { "@" (latest.version) } }
                p class="mf-lead" {
                    @match latest.shape {
                        Some(shape) => { (plural(shape.nodes.len(), "node", "nodes")) ", " (plural(shape.repeats.len(), "repeat", "repeats")) ", IR version " (shape.ir_version) ". " },
                        None => "Shape unverified. ",
                    }
                    (tally.line()) ". "
                    span class="bv-code" title=(latest.ir_hash) { "ir " (short_hash(latest.ir_hash)) }
                }
                (live_line(ledger.read_at.unwrap_or(at), ledger.read.is_ok()))
            }
            (unavailable(ledger, Scope::Kernel(&runs)))
            section class="mf-section" aria-labelledby="mf-shape" {
                h2 id="mf-shape" { "Shape" }
                @match latest.shape {
                    Some(shape) => {
                        @let heat = flows::Heat::of(latest.runs.iter().copied());
                        p class="bv-meta" { "Read left to right: a node runs once everything to its left that it links to is done. " (heat.caption()) }
                        div class="mf-graph" { (graph(shape, None, &heat.at, false, Some(""))) }
                        (legend_classes())
                    },
                    None => p class="mf-unavailable" { (UNVERIFIED) },
                }
            }
            section class="mf-section" aria-labelledby="mf-ontology" {
                h2 id="mf-ontology" { "Ontology" }
                @match latest.shape {
                    Some(shape) => (ontology(shape)),
                    None => p class="mf-unavailable" { (UNVERIFIED) },
                }
            }
            section class="mf-section" aria-labelledby="mf-lineage" {
                h2 id="mf-lineage" { "Lineage" }
                p class="bv-meta" { "Every revision of this kernel that a run which was read pinned, oldest first. A run keeps the revision it started with." }
                ol class="mf-lineage" {
                    @for (n, r) in k.revisions.iter().enumerate() {
                        li {
                            p {
                                strong { (k.name) "@" (r.version) }
                                " " span class="bv-code" title=(r.ir_hash) { "ir " (short_hash(r.ir_hash)) }
                                span class="bv-meta" {
                                    " · " (plural(r.runs.len(), "run", "runs")) " · first " (local(r.first_at(), "%b %d, %H:%M"))
                                    " · last " (ago(r.last_at(), at))
                                }
                            }
                            @if n == 0 {
                                p class="bv-meta" { "The first revision a run that was read pinned." }
                            } @else {
                                @match (k.revisions[n - 1].shape, r.shape) {
                                    (Some(before), Some(after)) => {
                                        @let changes = flows::diff(before, after);
                                        @if changes.is_empty() {
                                            p class="bv-meta" { "Same nodes and settings as the revision before; the IR differs in a field this page does not show." }
                                        } @else {
                                            ul class="mf-changes" { @for c in &changes { li { (c) } } }
                                        }
                                    },
                                    _ => p class="bv-meta" { "Not compared: no run of one of the two revisions replayed, so its shape is unverified." },
                                }
                            }
                            @if let (true, Some(shape)) = (n + 1 < k.revisions.len(), r.shape) {
                                details id=(format!("mf-rev-{n}")) {
                                    summary { "Shape of this revision" }
                                    div class="mf-graph" { (graph(shape, None, &BTreeMap::new(), false, None)) }
                                }
                            }
                        }
                    }
                }
            }
            section class="mf-section" aria-labelledby="mf-all-runs" {
                h2 id="mf-all-runs" { "Runs" }
                ul class="mf-runs" { @for i in &runs { (run_row(i, at)) } }
            }
            (source(ledger))
        }
    }
}

/// What a page shows in place of a shape the kernel has not accepted.
const UNVERIFIED: &str =
    "No run of this revision replayed, so the kernel has not accepted its pinned IR and its shape is not shown.";

fn legend_classes() -> Markup {
    html! {
        dl class="mf-glossary" {
            @for (c, what) in CLASSES {
                div { dt { span class="mf-class" data-class=(c) { (c) } } dd { (what) } }
            }
        }
    }
}

fn ontology(shape: &Shape) -> Markup {
    // In the order the flow runs: by column, then row, as the shape draws them.
    let layout = flows::layout(shape);
    let mut nodes: Vec<&flows::Node> = shape.nodes.iter().collect();
    nodes.sort_by_key(|n| layout.place(&n.id).unwrap_or((usize::MAX, usize::MAX)));
    html! {
        @if !shape.params.is_empty() || shape.workdir.is_some() {
            h3 { "Params" }
            ul class="mf-params" {
                @for (name, required, ty) in &shape.params {
                    li {
                        code { (name) }
                        span class="bv-meta" { @if *required { " required" } @else { " optional" } @if let Some(t) = ty { " · " (t) } }
                    }
                }
                @if let Some(w) = &shape.workdir { li { "Runs in " code { (w) } } }
            }
        }
        h3 { "Nodes" }
        div class="mf-table-wrap" {
            table class="mf-table" {
                thead { tr { th { "Node" } th { "Class" } th { "Needs" } th { "Performer" } th { "Limits" } } }
                tbody {
                    @for n in &nodes {
                        tr id=(format!("node-{}", n.id)) {
                            td { code { (n.id) } }
                            td { span class="mf-class" data-class=(n.class) { (n.class) } }
                            td {
                                @for (i, need) in n.needs.iter().enumerate() {
                                    @if i > 0 { ", " }
                                    @match need.kind {
                                        flows::NeedKind::Node => code { (need.on) },
                                        flows::NeedKind::Repeat => { "repeat " code { (need.on) } " finished" },
                                        flows::NeedKind::Exhausted => { "repeat " code { (need.on) } " ran out" },
                                    }
                                }
                                @if let Some((on, case)) = &n.switch { @if !n.needs.is_empty() { "; " } "only when " code { (on) } " took " code { (case) } }
                                @if n.needs.is_empty() && n.switch.is_none() { span class="bv-meta" { "Starts the flow" } }
                            }
                            td {
                                @if let Some(p) = &n.performer { (p) }
                                @if let Some(s) = &n.skill { br; span class="bv-meta" { "skill " (s) } }
                                @for (k, v) in &n.routing { br; span class="bv-meta" { (k) ": " (v) } }
                                @if n.performer.is_none() && n.skill.is_none() { span class="bv-meta" { @if matches!(n.class.as_str(), "pure" | "observe") { "The kernel" } @else if n.class == "human" { "Whoever answers its ask" } @else { "Not named" } } }
                            }
                            td class="bv-meta" {
                                @if let Some(r) = n.retry { (plural(r as usize, "attempt", "attempts")) br; }
                                @if let Some(c) = n.cancel_after { "ends after " (c) " s" br; }
                                @if let Some((ext, hb)) = &n.wait { "waits on " (ext) ", heartbeat " (hb) " s" br; }
                                @if !n.facts.is_empty() {
                                    details id=(format!("facts-{}", n.id)) {
                                        summary { "Definition" }
                                        dl class="mf-facts" { @for (k, v) in &n.facts { dt { (k) } dd { code { (v) } } } }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        @if !shape.repeats.is_empty() {
            h3 { "Repeats" }
            ul class="mf-params" {
                @for r in &shape.repeats {
                    li {
                        code { (r.id) } " runs " (r.body.join(", ")) " up to " (r.max) " times"
                        @if let Some(u) = &r.until { ", until " code { (u) } }
                        @if let Some(e) = &r.on_exhausted { ", then " code { (e) } " when it runs out" }
                    }
                }
            }
        }
    }
}

fn run(ledger: &Ledger, i: &Instance, latest: bool, at: jiff::Timestamp) -> Markup {
    // A run page has no ontology table: its nodes link to their rows on the kernel's page, which
    // describes only the latest revision.
    let linked = latest.then(|| kernel_href(&i.flow));
    let state = i.state();
    let every = if state.open() { LIVE_RUN_MS } else { IDLE_MS };
    html! {
        div class="mf" data-live=(every) {
            nav class="mf-crumbs bv-meta" aria-label="Breadcrumb" {
                a href="/flows" { "Flows" } " / " a href=(kernel_href(&i.flow)) { (i.flow) } " / " (short_id(&i.id))
            }
            header class="mf-head" {
                h1 { (i.kernel()) " " span class="mf-at bv-code" { (short_id(&i.id)) } }
                p class="mf-lead" {
                    (chip(state)) " "
                    "Started " (local(&i.started_at, "%b %d, %H:%M:%S")) @if let Some(s) = &i.starter { " by " span class="bv-code" { (s) } }
                    ". Last event " (ago(&i.last_at, at)) ", event " (i.seq) "."
                }
                (live_line(ledger.read_at.unwrap_or(at), ledger.read.is_ok()))
            }
            (unavailable(ledger, Scope::Uncounted))
            section class="mf-section mf-where" aria-labelledby="mf-where" {
                h2 id="mf-where" { "Where it is" }
                @match &i.live {
                    Err(why) => p class="mf-unavailable" { "The kernel could not replay this run, so its position is unknown. " (why) },
                    Ok(live) => (whereabouts_panel(i, live)),
                }
            }
            section class="mf-section" aria-labelledby="mf-run-shape" {
                h2 id="mf-run-shape" { "Shape" }
                // The shape this run pinned is shown only once its replay accepted that IR.
                @match &i.live {
                    Ok(live) => {
                        div class="mf-graph" { (graph(&i.shape, Some(live), &BTreeMap::new(), false, linked.as_deref())) }
                        (legend_states())
                    },
                    Err(_) => p class="mf-unavailable" { "This run did not replay, so the kernel has not accepted the IR it pinned and its shape is not shown." },
                }
            }
            section class="mf-section" aria-labelledby="mf-activity" {
                h2 id="mf-activity" { "Activity" }
                p class="bv-meta" {
                    "The log as written. "
                    @if i.live.is_ok() { "An event replay set aside is marked; it changed nothing." }
                    @else { "Replay did not run, so no event below is known to have been accepted." }
                }
                @if i.steps.len() > MAX_STEPS { p class="bv-meta" { "The latest " (MAX_STEPS) " of " (i.steps.len()) " events." } }
                ol class="mf-steps" reversed {
                    @for s in i.steps.iter().rev().take(MAX_STEPS) {
                        li data-kind=(s.kind) data-rejected=[s.rejected.is_some().then_some("true")] {
                            time datetime=(s.at) title=(local(&s.at, "%b %d, %H:%M:%S %Z")) { (local(&s.at, "%H:%M:%S")) }
                            span class="mf-step-kind" { (s.kind) }
                            @if let Some(subject) = &s.subject { code { (subject) } }
                            span class="mf-step-note" {
                                @if let Some(why) = &s.rejected { "Set aside by replay: " (why) ". " }
                                (s.note)
                            }
                        }
                    }
                }
            }
            section class="mf-section" aria-labelledby="mf-run-lineage" {
                h2 id="mf-run-lineage" { "Lineage" }
                ul class="mf-params" {
                    li { "Kernel " a href=(kernel_href(&i.flow)) { (i.kernel()) } " " span class="bv-code" title=(i.ir_hash) { "ir " (short_hash(&i.ir_hash)) } }
                    li { "Started by " @match &i.starter { Some(s) => span class="bv-code" { (s) }, None => span class="bv-meta" { "a session not read from its Started event" } } }
                    @if let Ok(live) = &i.live {
                        li {
                            "Owned now by "
                            @match &live.owner { Some(o) => span class="bv-code" { (o) }, None => span class="bv-meta" { "no session replay records" } }
                            span class="bv-meta" { " (replay's owner: the starter, or the last adoption it accepted)" }
                        }
                        @for p in &live.proposals { li { "Proposal: " (p) } }
                        @if let Some(ask) = &live.retired { li { "Retired on the approval of ask " code { (ask) } } }
                        @if live.rejected > 0 { li { (live.rejected) " events set aside by replay" } }
                        @if live.stops_let_through > 0 { li { (live.stops_let_through) " stops let through with work left" } }
                    }
                }
                @if !i.params.is_empty() {
                    h3 { "Params" }
                    dl class="mf-facts" { @for (k, v) in &i.params { dt { (k) } dd { code { (v) } } } }
                }
            }
            (source(ledger))
        }
    }
}

fn whereabouts_panel(i: &Instance, live: &Live) -> Markup {
    let (at, done, total) = (live.frontier(), live.done(), i.shape.nodes.len());
    html! {
        div class="mf-where-grid" {
            div class="mf-where-now" {
                p class="mf-where-title" { (whereabouts(i)) }
                @let skipped = live.nodes.values().filter(|n| n.state == NodeState::Blocked).count();
                p {
                    meter class="mf-meter" min="0" max=(total.max(1)) value=(done) {}
                    span class="bv-meta" { " " (done) " of " (plural(total, "node", "nodes")) " done" @if live.terminal && skipped > 0 { ", " (skipped) " not reached" } }
                }
                @if !at.is_empty() {
                    ul class="mf-frontier" {
                        @for (id, s) in &at {
                            @let n = &live.nodes[*id];
                            li data-state=(s.name()) {
                                span class="mf-state-dot" aria-hidden="true" {}
                                code { (id) } " " (s.label())
                                @if !n.iteration.is_empty() { span class="bv-meta" { " · iteration " (n.iteration) } }
                                @if let Some(c) = &n.claim { br; span class="bv-meta" { (c) } }
                                @for u in &n.unmet { br; span class="bv-meta" { (u) } }
                            }
                        }
                    }
                }
            }
            div {
                @if !live.work.is_empty() {
                    h3 { "Its owner session can do now" }
                    ul class="mf-params" { @for w in &live.work { li { (w) } } }
                }
                @if !live.waiting.is_empty() {
                    h3 { "Waiting on" }
                    ul class="mf-params" {
                        @for w in &live.waiting {
                            li data-ask=[w.ask.is_some().then_some("true")] {
                                (w.text)
                                @if let Some(href) = w.ask.as_ref().and_then(|(ledger, id)| crate::decision_path(ledger, id)) {
                                    " · " a href=(href) { "see it in Decisions" }
                                }
                            }
                        }
                    }
                }
                @if !live.repeats.is_empty() {
                    h3 { "Repeats" }
                    ul class="mf-params" { @for (id, s, n) in &live.repeats { li { code { (id) } " " (s) ", iteration " (n) } } }
                }
                @if live.work.is_empty() && live.waiting.is_empty() && !live.terminal {
                    p class="bv-meta" { "No work is enabled and nothing is awaited at this clock." }
                }
            }
        }
    }
}

fn legend_states() -> Markup {
    let states = [
        NodeState::Running,
        NodeState::Waiting,
        NodeState::Ready,
        NodeState::Stale,
        NodeState::Done,
        NodeState::Failed,
        NodeState::Exhausted,
        NodeState::Blocked,
    ];
    html! {
        ul class="mf-legend" aria-label="Node states" {
            @for s in states { li data-state=(s.name()) { span class="mf-state-dot" aria-hidden="true" {} (s.label()) } }
        }
    }
}

struct Geometry {
    w: f64,
    h: f64,
    col_gap: f64,
    row_gap: f64,
    pad: f64,
    top: f64,
}

const FULL: Geometry = Geometry { w: 172.0, h: 54.0, col_gap: 56.0, row_gap: 22.0, pad: 12.0, top: 22.0 };
const THUMB: Geometry = Geometry { w: 26.0, h: 12.0, col_gap: 12.0, row_gap: 6.0, pad: 2.0, top: 2.0 };

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max - 1).collect::<String>()) }
}

/// A shape as an SVG: nodes by class (and, for a run, by state), dependencies as curves, repeats
/// as frames. `heat` puts a count on nodes; `thumb` draws the small card version without text.
/// `link` is the page whose ontology table holds each node's row (`""` for this page); `None`
/// draws nodes without links, for a shape no table on the site describes.
fn graph(
    shape: &Shape,
    live: Option<&Live>,
    heat: &BTreeMap<String, usize>,
    thumb: bool,
    link: Option<&str>,
) -> Markup {
    let g = if thumb { &THUMB } else { &FULL };
    let layout: Layout = flows::layout(shape);
    let cols = layout.columns.len().max(1) as f64;
    let rows = layout.rows().max(1) as f64;
    let width = g.pad * 2.0 + cols * g.w + (cols - 1.0) * g.col_gap;
    let height = g.top + g.pad * 2.0 + rows * g.h + (rows - 1.0) * g.row_gap;
    let xy = |id: &str| {
        layout
            .place(id)
            .map(|(c, r)| (g.pad + c as f64 * (g.w + g.col_gap), g.top + g.pad + r as f64 * (g.h + g.row_gap)))
    };
    let class_of = |id: &str| shape.nodes.iter().find(|n| n.id == id).map_or("pure", |n| n.class.as_str());
    let state_of = |id: &str| live.and_then(|l| l.nodes.get(id)).map(|n| n.state);
    html! {
        svg class=(if thumb { "mf-svg mf-svg-thumb" } else { "mf-svg" })
            viewBox=(format!("0 0 {width} {height}")) width=(width) height=(height)
            role="img" aria-label=(format!("{} nodes in {} steps", shape.nodes.len(), layout.columns.len())) {
            @for r in &shape.repeats {
                @let boxes: Vec<(f64, f64)> = r.body.iter().filter_map(|b| xy(b)).collect();
                @if !boxes.is_empty() {
                    @let x0 = boxes.iter().map(|b| b.0).fold(f64::MAX, f64::min) - 8.0;
                    @let y0 = boxes.iter().map(|b| b.1).fold(f64::MAX, f64::min) - if thumb { 2.0 } else { 18.0 };
                    @let x1 = boxes.iter().map(|b| b.0).fold(f64::MIN, f64::max) + g.w + 8.0;
                    @let y1 = boxes.iter().map(|b| b.1).fold(f64::MIN, f64::max) + g.h + 8.0;
                    g class="mf-repeat" {
                        rect x=(x0) y=(y0) width=(x1 - x0) height=(y1 - y0) rx="12" {}
                        @if !thumb {
                            @let iter = live.and_then(|l| l.repeats.iter().find(|x| x.0 == r.id)).map(|x| format!(" · iteration {} {}", x.2, x.1)).unwrap_or_default();
                            text x=(x0 + 10.0) y=(y0 + 13.0) { "repeat " (r.id) " · up to " (r.max) (iter) }
                        }
                    }
                }
            }
            @for e in &layout.edges {
                @if let (Some((x1, y1)), Some((x2, y2))) = (xy(&e.from), xy(&e.to)) {
                    @let (sx, sy, tx, ty) = (x1 + g.w, y1 + g.h / 2.0, x2, y2 + g.h / 2.0);
                    @let mx = (sx + tx) / 2.0;
                    @let lit = state_of(&e.from) == Some(NodeState::Done);
                    path class="mf-edge" data-kind=(e.kind.name()) data-lit=[lit.then_some("true")]
                        d=(format!("M{sx} {sy} C{mx} {sy}, {mx} {ty}, {tx} {ty}")) {}
                    @if !thumb {
                        circle class="mf-edge-end" data-kind=(e.kind.name()) cx=(tx) cy=(ty) r="3" {}
                        @if let Some(label) = &e.label {
                            @if e.kind != EdgeKind::Needs {
                                text class="mf-edge-label" x=(tx - 6.0) y=(ty - 6.0) text-anchor="end" { (clip(label, 16)) }
                            }
                        }
                    }
                }
            }
            @for n in &shape.nodes {
                @if let Some((x, y)) = xy(&n.id) {
                    @let state = state_of(&n.id);
                    @let class = class_of(&n.id);
                    @let unmet = live.and_then(|l| l.nodes.get(&n.id)).map(|s| s.unmet.join("; ")).unwrap_or_default();
                    @let title = match state { Some(s) => format!("{} · {} · {}{}", n.id, n.class, s.label(), if unmet.is_empty() { String::new() } else { format!(" · {unmet}") }), None => format!("{} · {}", n.id, n.class) };
                    @if thumb {
                        rect class="mf-box" data-class=(class) x=(x) y=(y) width=(g.w) height=(g.h) rx="3" {}
                    } @else {
                        a href=[link.map(|base| format!("{base}#node-{}", n.id))] class="mf-node" data-class=(class) data-state=[state.map(NodeState::name)] {
                            title { (title) }
                            rect class="mf-box" x=(x) y=(y) width=(g.w) height=(g.h) rx="10" {}
                            rect class="mf-stripe" x=(x) y=(y + 8.0) width="4" height=(g.h - 16.0) rx="2" {}
                            text class="mf-node-id" x=(x + 14.0) y=(y + 23.0) { (clip(&n.id, 20)) }
                            text class="mf-node-meta" x=(x + 14.0) y=(y + 41.0) {
                                (n.class) @if let Some(s) = state { " · " (s.label().to_lowercase()) }
                            }
                            @if let Some(count) = heat.get(&n.id) {
                                circle class="mf-heat" cx=(x + g.w - 4.0) cy=(y + 4.0) r="11" {}
                                text class="mf-heat-n" x=(x + g.w - 4.0) y=(y + 8.0) text-anchor="middle" { (count) }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flows::{Need, NeedKind, Node, NodeLive, Step, Wait};

    fn shape() -> Shape {
        let node = |id: &str, class: &str, needs: &[&str]| Node {
            id: id.into(),
            class: class.into(),
            needs: needs.iter().map(|n| Need { on: n.to_string(), kind: NeedKind::Node }).collect(),
            ..Node::default()
        };
        Shape {
            ir_version: 4,
            nodes: vec![node("plan", "work", &[]), node("review", "human", &["plan"])],
            ..Shape::default()
        }
    }

    fn instance(live: Result<Live, String>) -> Instance {
        Instance {
            id: "fi_0123456789abcdef".into(),
            flow: "ship".into(),
            version: 2,
            ir_hash: "sha256:abcdef0123456789".into(),
            started_at: "2026-11-26T10:00:00Z".into(),
            last_at: "2026-11-26T10:05:00Z".into(),
            seq: 3,
            starter: Some("t3:env/thread".into()),
            params: vec![("repo".into(), "/tmp/r".into())],
            shape: shape(),
            live,
            steps: vec![Step { seq: 1, at: "2026-11-26T10:00:00Z".into(), kind: "Started".into(), ..Step::default() }],
        }
    }

    fn running() -> Live {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            "plan".to_string(),
            NodeLive { state: NodeState::Running, claim: Some("Claimed by codex".into()), ..NodeLive::default() },
        );
        nodes.insert("review".to_string(), NodeLive { state: NodeState::Blocked, ..NodeLive::default() });
        Live { nodes, work: vec!["plan is running".into()], ..Live::default() }
    }

    fn at() -> jiff::Timestamp {
        "2026-11-26T10:05:30Z".parse().unwrap()
    }

    fn ledger(read: Result<Vec<Instance>, String>) -> Ledger {
        Ledger { root: "/w/.broomva/flows".into(), host: Some("mac".into()), read, unread: vec![], read_at: None }
    }

    #[test]
    fn an_unread_ledger_says_so_and_never_shows_no_flows() {
        let (html, _) = flows_page(&ledger(Err("no_ledger: nothing here".into())), &FlowsPage::List, at()).unwrap();
        assert!(html.contains("The flow ledger was not read"), "{html}");
        assert!(html.contains("no_ledger: nothing here"));
        assert!(!html.contains("No run is listed"), "unread is not empty");
        assert!(html.contains("Not read, tried at"), "a failed read is not said read: {html}");
        assert!(!html.contains("0 kernels"), "no count over an unread ledger");
        assert!(ledger(Err("x".into())).tally().is_none(), "an unread ledger has no tally to state");
        let (empty, _) = flows_page(&ledger(Ok(vec![])), &FlowsPage::List, at()).unwrap();
        assert!(empty.contains("No run is listed in this ledger."));
    }

    #[test]
    fn the_list_shows_a_running_run_and_its_kernel_and_polls_fast() {
        let (html, title) = flows_page(&ledger(Ok(vec![instance(Ok(running()))])), &FlowsPage::List, at()).unwrap();
        assert_eq!(title, "Flows");
        assert!(html.contains(&format!(r#"data-live="{LIVE_RUN_MS}""#)), "a running run polls fast");
        assert!(html.contains("1 run, 1 running, 1 open, in 1 kernel."), "{html}");
        assert!(html.contains(r#"href="/flows/run/fi_0123456789abcdef""#));
        assert!(html.contains(r#"href="/flows/kernel/ship""#));
        assert!(html.contains("plan (running)"), "where the run is: {html}");
    }

    #[test]
    fn a_run_page_marks_the_running_node_and_names_its_claim() {
        let l = ledger(Ok(vec![instance(Ok(running()))]));
        let (html, title) = flows_page(&l, &FlowsPage::Run("fi_0123456789abcdef".into()), at()).unwrap();
        assert!(title.starts_with("ship@2 fi_01234567"), "{title}");
        assert!(html.contains(r#"data-class="work" data-state="running""#), "{html}");
        assert!(html.contains("Claimed by codex"));
        assert!(html.contains(r#"<span class="mf-chip" data-state="running">Running</span>"#));
        assert!(flows_page(&l, &FlowsPage::Run("fi_nope".into()), at()).is_none(), "an unknown run is no page");
    }

    #[test]
    fn a_run_that_did_not_replay_says_its_position_is_unknown() {
        let l = ledger(Ok(vec![instance(Err("torn_tail".into()))]));
        let (html, _) = flows_page(&l, &FlowsPage::Run("fi_0123456789abcdef".into()), at()).unwrap();
        assert!(html.contains("its position is unknown"), "{html}");
        assert!(html.contains(">Unknown</span>"));
    }

    #[test]
    fn an_ask_wait_links_to_decisions_and_a_kernel_page_shows_ontology_and_lineage() {
        let mut live = running();
        live.nodes.get_mut("plan").unwrap().state = NodeState::Done;
        live.nodes.get_mut("review").unwrap().state = NodeState::Waiting;
        live.waiting.push(Wait {
            ask: Some(("arc.yaml".into(), "a1".into())),
            text: "review: waits on ask a1 in arc.yaml".into(),
        });
        let mut older = instance(Ok(Live::default()));
        older.id = "fi_ffff".into();
        older.version = 1;
        older.ir_hash = "sha256:111".into();
        older.started_at = "2026-11-20T00:00:00Z".into();
        older.shape.nodes.pop();
        let l = ledger(Ok(vec![instance(Ok(live)), older]));
        let (run, _) = flows_page(&l, &FlowsPage::Run("fi_0123456789abcdef".into()), at()).unwrap();
        assert!(run.contains(r#"<span class="mf-chip" data-state="asking">Waiting on an ask</span>"#), "{run}");
        assert!(run.contains(r#"href="/decisions/arc.yaml/a1""#), "the wait links to the ask in Decisions: {run}");
        let (k, _) = flows_page(&l, &FlowsPage::Kernel("ship".into()), at()).unwrap();
        assert!(k.contains(r#"id="node-review""#), "the ontology table has a row per node");
        assert!(k.contains("Added review"), "the lineage diffs @1 against @2: {k}");
        assert!(k.contains("ship@1") && k.contains("ship@2"));
        assert!(flows_page(&l, &FlowsPage::Kernel("nope".into()), at()).is_none());
        // The kernel page's ontology describes @2 only, so a run of @1 links nowhere it would mislead.
        let (old_run, _) = flows_page(&l, &FlowsPage::Run("fi_ffff".into()), at()).unwrap();
        assert!(!old_run.contains("/flows/kernel/ship#node-"), "{old_run}");
        assert!(run.contains("/flows/kernel/ship#node-plan"), "the latest revision's run links its rows");
    }

    #[test]
    fn an_unread_run_or_kernel_renders_unread_and_only_a_full_read_says_absent() {
        let mut l = ledger(Ok(vec![]));
        l.unread.push(crate::flows::Unread { instance: "fi_torn".into(), reason: "torn_tail: a torn log".into() });
        let (run, _) = flows_page(&l, &FlowsPage::Run("fi_torn".into()), at()).expect("an unread run is a page");
        assert!(run.contains("its log was not read") && run.contains("torn_tail: a torn log"), "{run}");
        let (k, _) =
            flows_page(&l, &FlowsPage::Kernel("ship".into()), at()).expect("a kernel may be among unread runs");
        assert!(k.contains("1 run was not read") && !k.contains("its log was not read"), "a kernel is not a run: {k}");
        let (named, _) = flows_page(&l, &FlowsPage::Kernel("fi_torn".into()), at()).unwrap();
        assert!(!named.contains("its log was not read"), "a kernel named like a run is still a kernel");
        assert!(flows_page(&l, &FlowsPage::Run("fi_other".into()), at()).is_none(), "a run listed nowhere is absent");
        let full = ledger(Ok(vec![]));
        assert!(
            flows_page(&full, &FlowsPage::Kernel("ship".into()), at()).is_none(),
            "a full read establishes absence"
        );
    }

    #[test]
    fn run_page_nodes_link_to_the_kernels_ontology_rows() {
        let l = ledger(Ok(vec![instance(Ok(running()))]));
        let (html, _) = flows_page(&l, &FlowsPage::Run("fi_0123456789abcdef".into()), at()).unwrap();
        assert!(html.contains(r##"href="/flows/kernel/ship#node-plan""##), "{html}");
        let (k, _) = flows_page(&l, &FlowsPage::Kernel("ship".into()), at()).unwrap();
        assert!(k.contains(r##"href="#node-plan""##) && k.contains(r#"id="node-plan""#));
    }

    #[test]
    fn a_run_that_did_not_replay_is_listed_and_counted_as_unknown_never_as_nothing() {
        let l = ledger(Ok(vec![instance(Err("torn_tail: a torn log".into()))]));
        let (html, _) = flows_page(&l, &FlowsPage::List, at()).unwrap();
        assert!(!html.contains("No open run is running"), "{html}");
        assert!(html.contains("1 unknown"), "{html}");
        assert!(
            html.contains("progress unknown") && !html.contains("0/2"),
            "an unreplayed run's progress is unknown: {html}"
        );
        assert!(
            html.contains(r#"<span class="mf-chip" data-state="unread">Unknown</span>"#),
            "the run is listed: {html}"
        );
        assert!(html.contains("not replayed · torn_tail: a torn log"), "{html}");
        assert_eq!(l.tally().map(|t| t.unknown), Some(1));
        // A run listed but never read has no row to show: the empty list still says it is unknown.
        let mut listed = ledger(Ok(vec![]));
        listed.unread.push(crate::flows::Unread { instance: "fi_torn".into(), reason: "torn_tail".into() });
        let (html, _) = flows_page(&listed, &FlowsPage::List, at()).unwrap();
        assert!(!html.contains("No open run is running"), "{html}");
        assert!(html.contains("1 run's state is unknown"), "{html}");
    }

    /// The accounting conformance: on a mixed ledger (known runs in every state, a run that did not
    /// replay, a run listed but not read) the list, each kernel's card and each kernel's page say
    /// the same counts, the cards sum to the list, and no unknown run is counted open or dropped.
    #[test]
    fn every_view_tells_one_tally_of_a_mixed_ledger() {
        let mk = |id: &str, flow: &str, live: Result<Live, String>| {
            let mut i = instance(live);
            i.id = id.into();
            i.flow = flow.into();
            i
        };
        let state = |s: NodeState, terminal: bool| {
            let mut l = running();
            l.nodes.get_mut("plan").unwrap().state = s;
            l.terminal = terminal;
            Ok(l)
        };
        let mut owner = state(NodeState::Done, false).unwrap();
        owner.nodes.get_mut("review").unwrap().state = NodeState::Waiting;
        owner.waiting.push(crate::flows::Wait { ask: Some(("arc.yaml".into(), "a1".into())), text: "answer".into() });
        let runs = vec![
            mk("fi_run", "ship", Ok(running())),
            mk("fi_you", "ship", Ok(owner)),
            mk("fi_lost", "ship", Err("torn_tail".into())),
            mk("fi_done", "audit", state(NodeState::Done, true)),
            mk("fi_stuck", "audit", state(NodeState::Failed, true)),
            mk("fi_retired", "audit", {
                let mut l = state(NodeState::Done, true).unwrap();
                l.retired = Some("a9".into());
                Ok(l)
            }),
        ];
        let mut l = ledger(Ok(runs));
        l.unread.push(crate::flows::Unread { instance: "fi_gone".into(), reason: "torn_tail".into() });

        let all = l.tally().expect("a ledger read in full has a tally");
        assert_eq!(
            (all.runs, all.running, all.open, all.done, all.stuck, all.unknown),
            (7, 1, 2, 1, 1, 2),
            "an unknown run is never open, and the listed one is counted"
        );
        assert!(!all.may_say_none());
        let (list, _) = flows_page(&l, &FlowsPage::List, at()).unwrap();
        assert!(list.contains(&format!("{}, in at least 2 kernels.", all.line())), "{list}");
        assert!(list.contains(&all.unknown_note()), "the notice is the tally's own words: {list}");
        assert!(list.contains("<code>fi_gone</code> · not read"), "the list's notice names the listed run: {list}");
        assert!(!list.contains("leaves them out"), "{list}");
        let (run_page, _) = flows_page(&l, &FlowsPage::Run("fi_run".into()), at()).unwrap();
        assert!(!run_page.contains("the counts"), "a page with no counts claims none: {run_page}");
        assert!(run_page.contains("2 runs in this ledger were not read or not replayed"), "{run_page}");
        assert_eq!(
            all.open + all.done + all.stuck + all.retired + all.unknown,
            all.runs,
            "every run is in exactly one bucket"
        );
        assert!(all.line().contains("1 retired"), "{}", all.line());

        let instances = l.read.as_deref().unwrap();
        let mut summed = crate::flows::Tally::default();
        for k in crate::flows::kernels(instances) {
            let t = crate::flows::Tally::of(k.runs(), 0);
            assert!(!list.contains(&format!("{} · last", t.line())), "a card states no count: {list}");
            let (page, _) = flows_page(&l, &FlowsPage::Kernel(k.name.clone()), at()).unwrap();
            assert!(page.contains(&format!(". {}. ", t.line())), "the {} page says the card's tally: {page}", k.name);
            // Its notice speaks of its own runs: their unknown ones, and the listed runs apart.
            assert_eq!(
                page.contains(&t.unknown_note()),
                t.unknown > 0,
                "the {} page's notice is its own tally's: {page}",
                k.name
            );
            assert!(
                !page.contains(&all.unknown_note()),
                "the {} page carries the whole ledger's notice: {page}",
                k.name
            );
            assert!(page.contains("1 run was listed but not read"), "{page}");
            assert!(
                !page.contains("<code>fi_gone</code> · not read"),
                "a run the counts exclude is not listed as theirs: {page}"
            );
            summed.runs += t.runs;
            summed.open += t.open;
            summed.running += t.running;
            summed.done += t.done;
            summed.stuck += t.stuck;
            summed.unknown += t.unknown;
            summed.retired += t.retired;
        }
        let listed = l.unread.len();
        assert_eq!(
            (
                summed.runs + listed,
                summed.open,
                summed.running,
                summed.done,
                summed.stuck,
                summed.retired,
                summed.unknown + listed
            ),
            (all.runs, all.open, all.running, all.done, all.stuck, all.retired, all.unknown),
            "the cards sum to the list, which adds the runs no kernel can claim"
        );
    }

    #[test]
    fn a_replayed_run_shows_its_progress_and_an_unreplayed_one_says_it_is_unknown() {
        let l = ledger(Ok(vec![instance(Ok(running()))]));
        let (html, _) = flows_page(&l, &FlowsPage::List, at()).unwrap();
        assert!(html.contains("0/2") && !html.contains("progress unknown"), "the control, a known zero: {html}");
        let lost = ledger(Ok(vec![instance(Err("torn_tail".into()))]));
        let (list, _) = flows_page(&lost, &FlowsPage::List, at()).unwrap();
        assert!(list.contains("Not replayed, so where it is is unknown"), "{list}");
        let (k, _) = flows_page(&lost, &FlowsPage::Kernel("ship".into()), at()).unwrap();
        assert!(k.contains("progress unknown") && !k.contains("0/2"), "{k}");
    }

    /// The node counts on a kernel's shape carry their completeness: a run that did not replay is
    /// no node's, and the caption says how many such runs no node counts.
    #[test]
    fn node_counts_say_how_many_runs_they_cannot_place() {
        let mut lost = instance(Err("torn_tail".into()));
        lost.id = "fi_lost".into();
        let l = ledger(Ok(vec![instance(Ok(running())), lost]));
        let (k, _) = flows_page(&l, &FlowsPage::Kernel("ship".into()), at()).unwrap();
        assert!(k.contains(r#"class="mf-heat-n" x="#), "the running run is counted on its node: {k}");
        assert!(
            k.contains("1 more run of it is at no node (it did not replay, or waits on a repeat's verdict), so no node counts it."),
            "{k}"
        );
        let only = ledger(Ok(vec![instance(Ok(running()))]));
        let (k, _) = flows_page(&only, &FlowsPage::Kernel("ship".into()), at()).unwrap();
        assert!(
            k.contains("The count on a node is how many open runs of this revision are there now.")
                && !k.contains("did not replay"),
            "{k}"
        );
        let heat = crate::flows::Heat::of(l.read.as_deref().unwrap());
        assert_eq!((heat.at.get("plan"), heat.unplaced), (Some(&1), 1));
    }

    #[test]
    fn a_revision_no_run_replayed_has_no_shape_and_is_not_compared() {
        let l = ledger(Ok(vec![instance(Err("ir_hash_mismatch".into()))]));
        let (k, _) = flows_page(&l, &FlowsPage::Kernel("ship".into()), at()).unwrap();
        assert!(k.contains(UNVERIFIED) && !k.contains(r#"class="mf-svg""#), "an unaccepted IR draws nothing: {k}");
        assert!(!k.contains(r#"id="node-plan""#), "and has no ontology rows: {k}");
        let (list, _) = flows_page(&l, &FlowsPage::List, at()).unwrap();
        assert!(list.contains("Shape unverified") && !list.contains("mf-svg-thumb"), "{list}");
        let mut good = instance(Ok(running()));
        good.id = "fi_good".into();
        good.version = 3;
        good.ir_hash = "sha256:333".into();
        let two = ledger(Ok(vec![instance(Err("ir_hash_mismatch".into())), good]));
        let (k, _) = flows_page(&two, &FlowsPage::Kernel("ship".into()), at()).unwrap();
        assert!(k.contains("Not compared: no run of one of the two revisions replayed"), "{k}");
    }

    #[test]
    fn a_run_waiting_on_a_verdict_says_so_and_heat_counts_it_unplaced() {
        let mut live = running();
        for n in live.nodes.values_mut() {
            n.state = NodeState::Done;
        }
        live.nodes.get_mut("review").unwrap().state = NodeState::Blocked;
        live.verdicts = vec!["draft".into()];
        let l = ledger(Ok(vec![instance(Ok(live))]));
        let (html, _) = flows_page(&l, &FlowsPage::List, at()).unwrap();
        assert!(html.contains("Waiting on the until verdict of repeat draft"), "{html}");
        assert!(!html.contains("Nothing to do now"), "{html}");
        let heat = crate::flows::Heat::of(l.read.as_deref().unwrap());
        assert_eq!((heat.at.len(), heat.unplaced), (0, 1), "an open run at no node is unplaced, not dropped");
    }

    #[test]
    fn an_event_replay_set_aside_is_marked_and_a_run_that_did_not_replay_says_none_is_known() {
        let mut i = instance(Ok(running()));
        i.steps.push(Step {
            seq: 2,
            at: "2026-11-26T10:01:00Z".into(),
            kind: "Adopted".into(),
            rejected: Some("no approved proposal".into()),
            ..Step::default()
        });
        let (run, _) = flows_page(&ledger(Ok(vec![i])), &FlowsPage::Run("fi_0123456789abcdef".into()), at()).unwrap();
        assert!(run.contains(r#"data-kind="Adopted" data-rejected="true""#), "{run}");
        assert!(run.contains("Set aside by replay: no approved proposal."), "{run}");
        let lost = ledger(Ok(vec![instance(Err("torn".into()))]));
        let (lost, _) = flows_page(&lost, &FlowsPage::Run("fi_0123456789abcdef".into()), at()).unwrap();
        assert!(lost.contains("Replay did not run, so no event below is known to have been accepted."), "{lost}");
    }

    #[test]
    fn a_run_that_did_not_replay_draws_no_shape_and_links_nowhere() {
        let l = ledger(Ok(vec![instance(Err("ir_hash_mismatch".into()))]));
        let (run, _) = flows_page(&l, &FlowsPage::Run("fi_0123456789abcdef".into()), at()).unwrap();
        assert!(!run.contains(r#"class="mf-svg""#), "an unaccepted IR is not drawn: {run}");
        assert!(!run.contains("/flows/kernel/ship#node-"), "nor linked to rows the kernel page does not show: {run}");
        assert!(run.contains("its shape is not shown"), "{run}");
    }

    #[test]
    fn a_run_links_its_nodes_only_to_a_drawn_latest_revision_of_its_own() {
        // A run that did not replay claims the same hash at a higher version: it becomes the
        // latest revision, with no shape, so a good run of the older version links to no rows.
        let good = instance(Ok(running()));
        let mut forged = instance(Err("ir_hash_mismatch".into()));
        forged.id = "fi_forged".into();
        forged.version = 9;
        let l = ledger(Ok(vec![good, forged]));
        let (run, _) = flows_page(&l, &FlowsPage::Run("fi_0123456789abcdef".into()), at()).unwrap();
        assert!(run.contains(r#"class="mf-svg""#) && !run.contains("/flows/kernel/ship#node-"), "{run}");
        let (k, _) = flows_page(&l, &FlowsPage::Kernel("ship".into()), at()).unwrap();
        assert!(k.contains(UNVERIFIED), "the kernel page draws no rows to link to: {k}");
    }

    #[test]
    fn ages_read_in_the_largest_whole_unit() {
        let at = at();
        assert_eq!(ago("2026-11-26T10:05:00Z", at), "30 s ago");
        assert_eq!(ago("2026-11-26T09:05:30Z", at), "1 h ago");
        assert_eq!(ago("2026-11-24T10:05:30Z", at), "2 d ago");
        assert_eq!(ago("2026-11-26T10:06:00Z", at), "ahead of this clock");
        assert_eq!(ago("soon", at), "at an unknown time");
    }
}
