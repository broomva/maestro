//! One decision, full length: stakes first, what silence does, the summary, the options with
//! their real tradeoffs, the sources, what the agent tried first, and where to answer.

use super::{Entry, asker, kind, queue, sources, tab_of};
use crate::render::{Ctx, card, label, scope_errors};
use maestro_core::ask::{Ask, AskState, Rung};
use maestro_store::LedgerFile;
use maud::{Markup, html};
use serde_json::{Map, Value};

/// The tradeoff cells the ask format names, in the order they are shown.
const CELLS: [(&str, &str); 4] = [("cost", "Cost"), ("risk", "Risk"), ("time", "Time"), ("reversibility", "Undo")];

/// The `decision` block: one ask by ledger and uid or id, as the decision screen.
pub(crate) fn decision(cx: &Ctx, id: &str, ledger: &str, ask_id: &str) -> Markup {
    let section = &cx.reads.asks;
    let file: Option<&LedgerFile> = section.data.iter().flatten().find(|lf| lf.file == ledger);
    let matches: Vec<_> = file
        .into_iter()
        .flat_map(|lf| lf.ledger.asks.iter().map(move |a| (lf, a)))
        .filter(|(_, a)| a.uid.as_deref() == Some(ask_id) || (a.uid.is_none() && a.id == ask_id))
        .collect();
    let handle = format!("{ledger}#{ask_id}");
    let scoped = scope_errors(cx, section, ledger);
    let unread = scoped.errors.iter().any(|e| e.at != section.source);
    let body = match matches.as_slice() {
        [(lf, a)] => {
            let groups = queue(cx, None);
            let tab = tab_of(a);
            let same: Vec<&Entry> = groups.iter().flat_map(|g| &g.rows).filter(|e| e.tab() == tab).collect();
            let at = same.iter().position(|e| e.lf.file == lf.file && e.ask.handle() == a.handle());
            let waiting = at.is_some_and(|i| same[i].waiting);
            body(cx, lf, a, waiting, at.map(|i| (i + 1, same.len())))
        }
        [] if unread => html! {
            p class="mv-note" data-tone="danger" {
                span class="bv-code" { (handle) } " could not be read. This is not the same as missing."
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
    card(id, "decision", "Decision", &scoped, None, body)
}

fn text<'a>(ask: &'a Ask, key: &str) -> Option<&'a str> {
    ask.extra.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// The tradeoff rows the ask carries, as objects.
fn tradeoffs(ask: &Ask) -> Vec<&Map<String, Value>> {
    ask.extra.get("tradeoffs").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_object).collect()
}

fn cells(row: &Map<String, Value>) -> Markup {
    html! {
        dl class="mv-cells" {
            @for (key, name) in CELLS {
                @if let Some(v) = row.get(key).and_then(Value::as_str).filter(|v| !v.is_empty()) {
                    div class="mv-cell" { dt class="bv-meta" { (name) } dd { (v) } }
                }
            }
        }
    }
}

/// One exhausted rung as `(rung, what was tried)`: `"read: did x"` or `{read: did x}`.
fn tried(entry: &Value) -> (Option<Rung>, String) {
    let rung = Rung::of_entry(entry);
    let what = match entry {
        Value::String(s) => match (rung, s.split_once(':')) {
            (Some(_), Some((_, rest))) => rest.trim().to_string(),
            _ => s.clone(),
        },
        Value::Object(map) if map.len() == 1 => match map.values().next() {
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        },
        other => other.to_string(),
    };
    (rung, what)
}

/// The decision's full body. `waiting`: answered in the working tree, not yet landed.
/// `position`: where it sits among its tab's open asks.
pub(crate) fn body(cx: &Ctx, lf: &LedgerFile, a: &Ask, waiting: bool, position: Option<(usize, usize)>) -> Markup {
    let state = a.state();
    let rows = tradeoffs(a);
    let free: Vec<_> = rows.iter().filter(|r| r.get("option").and_then(Value::as_str).is_none()).collect();
    let srcs = sources::of(a.extra.get("sources"));
    let summary = text(a, "summary");
    let question = a.ask.as_deref().or(a.question.as_deref()).filter(|q| *q != a.title());
    html! {
        article class="mv-decision" data-ask=(format!("{}#{}", lf.file, a.handle())) data-hash=(a.hash()) data-kind=(kind(a)) {
            p class="mv-dmeta bv-meta" {
                span class="mv-chip" data-kind=(kind(a)) { (kind(a)) }
                " " (lf.file)
                @if let Some(at) = &a.asked_at { " · asked " (at) }
                " by " (asker(lf, a))
                @if let Some((n, of)) = position { span class="mv-pos" { (n) " of " (of) } }
            }
            p class="mv-dtitle" { (a.title()) }
            @if let Some(stakes) = &a.stakes { p class="mv-stakes" { (stakes) } }
            @if state == AskState::Open {
                @if let Some(d) = &a.default {
                    p class="mv-callout" data-tone="warning" { "If you don't answer: " (d) }
                } @else if let Some(why) = &a.no_default_because {
                    p class="mv-callout" data-tone="warning" { "No default: " (label(why)) ". It waits for you." }
                }
            }
            @if a.blocking && !a.gates.is_empty() {
                p class="mv-gates bv-meta" { "Blocks " @for (i, g) in a.gates.iter().enumerate() { @if i > 0 { ", " } span class="bv-code" { (g) } } }
            }
            @if let Some(s) = summary { p class="mv-dbody" { (s) } }
            @if let Some(q) = question { p class="mv-dbody" { (q) } }
            @if let Some(why) = &a.why_you { p class="mv-dbody mv-why" { "Why you: " (why) } }
            @if let Some(details) = text(a, "details") {
                details class="mv-fold" { summary { "Details" } p class="mv-pre" { (details) } }
            }
            @if !a.options.is_empty() {
                h3 class="mv-label bv-meta" { @if state == AskState::Open { "Choose one" } @else { "Options" } }
                ol class="mv-dopts" {
                    @for o in &a.options {
                        li class="mv-dopt" data-option=(o.id) data-recommended=(o.recommended)
                            data-chosen=(a.answer_option.as_deref() == Some(o.id.as_str())) {
                            p class="mv-dopt-label" { (o.label) }
                            @if let Some(c) = &o.consequence { p class="mv-dopt-text" { (c) } }
                            @if o.recommended || o.reversible.is_some() || o.status_quo {
                                p class="mv-chips" {
                                    @if o.recommended { span class="mv-badge" data-tone="info" { "Recommended" } }
                                    @match o.reversible {
                                        Some(true) => span class="mv-badge" data-tone="success" { "Reversible" },
                                        Some(false) => span class="mv-badge" data-tone="danger" { "Not reversible" },
                                        None => {},
                                    }
                                    @if o.status_quo { span class="mv-badge" { "Status quo" } }
                                }
                            }
                            @for row in rows.iter().filter(|r| r.get("option").and_then(Value::as_str) == Some(o.id.as_str())) {
                                (cells(row))
                            }
                        }
                    }
                }
            }
            @if !free.is_empty() {
                h3 class="mv-label bv-meta" { "Tradeoffs" }
                ul class="mv-free" {
                    @for row in &free {
                        li { p class="mv-dopt-label" { (row.get("label").and_then(Value::as_str).unwrap_or("")) } (cells(row)) }
                    }
                }
            }
            @if !srcs.is_empty() {
                h3 class="mv-label bv-meta" { "Sources" }
                ul class="mv-sources" {
                    @for s in &srcs {
                        li class="mv-source-chip" data-kind=(s.kind) {
                            @match &s.url {
                                Some(url) => a href=(url) target="_blank" rel="noopener noreferrer" { (s.text) },
                                None => span class="bv-code" { (s.text) },
                            }
                        }
                    }
                }
            }
            @if !a.exhausted.is_empty() {
                details class="mv-fold mv-tried" open {
                    summary { "What the agent tried first" }
                    ul {
                        @for e in &a.exhausted {
                            @let (rung, what) = tried(e);
                            li { @if let Some(r) = rung { span class="bv-code" { (r.as_str()) } ": " } (what) }
                        }
                    }
                }
            }
            (outcome(cx, a, state, waiting))
        }
    }
}

/// Where the answer stands, or where to give it. Phase 1 writes nothing: the plugin is the one writer.
fn outcome(cx: &Ctx, a: &Ask, state: AskState, waiting: bool) -> Markup {
    let given =
        a.answer_option.as_deref().and_then(|id| a.options.iter().find(|o| o.id == id)).map(|o| o.label.as_str());
    html! {
        @if waiting {
            p class="mv-callout" data-tone="success" {
                "Answered in the working tree; waiting to land on " span class="bv-code" { (cx.snap.tree) } "."
            }
        } @else if state == AskState::Answered {
            p class="mv-callout" data-tone="success" {
                "Answered"
                @if let Some(x) = given.or(a.answer.as_deref()).or(a.answer_option.as_deref()) { ": " (x) }
                @if let Some(by) = &a.answered_by { " · by " (by) }
                @if let Some(at) = &a.answered_at { " · " (at) }
            }
        } @else if state == AskState::Archived {
            p class="mv-callout" { "Archived" @if let Some(s) = &a.status { ": " (s) } "." }
        } @else {
            div class="mv-control mv-answerbar" {
                textarea class="mv-note-field" rows="1" disabled placeholder="Add a note (optional)" aria-label="Note (answering is not available here yet)" {}
                p class="mv-answer-cta" {
                    "Answer in Paseo · Maestro · Decisions"
                    @if let Some(r) = a.recommended() { span class="bv-meta" { " (recommended: " (r.label) ")" } }
                }
                p class="mv-answer-why bv-meta" {
                    "Paseo's Maestro plugin still writes every answer, so this screen only reads. Find it there as "
                    span class="bv-code" { (a.handle()) } "."
                }
            }
        }
    }
}
