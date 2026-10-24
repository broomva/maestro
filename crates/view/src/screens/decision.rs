//! One decision, full length: stakes first, what silence does, the summary, the options with
//! their real tradeoffs, the sources, what the agent tried first, and where to answer.

use super::rich::{self, Tradeoff};
use super::{Landing, asker, joined, kind, landing};
use crate::render::{Ctx, card, label, scope_errors};
use maestro_core::ask::{Ask, AskClass, AskState, Rung};
use maestro_store::LedgerFile;
use maud::{Markup, html};
use serde_json::Value;

/// A tradeoff axis as the design labels it (`reversibility` is the design's Undo row).
fn axis_label(axis: &str) -> &'static str {
    match axis {
        "cost" => "Cost",
        "risk" => "Risk",
        "time" => "Time",
        _ => "Undo",
    }
}

/// The `decision` block: one ask by ledger and uid or id, as the decision screen.
pub(crate) fn decision(cx: &Ctx, id: &str, ledger: &str, ask_id: &str) -> Markup {
    let section = &cx.reads.asks;
    let file: Option<&LedgerFile> = section.data.iter().flatten().find(|lf| lf.file == ledger);
    // The plugin's namespaces: a handle is a uid, else an id among asks without one.
    let all = || file.into_iter().flat_map(|lf| lf.ledger.asks.iter().map(move |a| (lf, a)));
    let mut matches: Vec<_> = all().filter(|(_, a)| a.uid.as_deref() == Some(ask_id)).collect();
    if matches.is_empty() {
        matches = all().filter(|(_, a)| a.uid.is_none() && a.id == ask_id).collect();
    }
    let handle = format!("{ledger}#{ask_id}");
    let scoped = scope_errors(cx, section, ledger);
    let unread = scoped.errors.iter().any(|e| e.at != section.source);
    let body = match matches.as_slice() {
        [(lf, a)] => body(cx, lf, a, landing(cx, lf, a), false),
        [] if unread => html! {
            p class="mv-note" data-tone="danger" {
                span class="bv-code" { (handle) } " could not be read. This is not the same as missing."
            }
        },
        [] if file.is_none() => {
            html! { p class="mv-note" data-tone="warning" { "No ledger named " span class="bv-code" { (ledger) } "." } }
        }
        [] => match file
            .into_iter()
            .flat_map(|lf| &lf.ledger.asks)
            .find(|a| a.id == ask_id)
            .and_then(|a| a.uid.as_deref())
        {
            // The id is there, but a uid addresses that ask.
            Some(uid) => html! {
                p class="mv-note" data-tone="warning" {
                    span class="bv-code" { (handle) } " is addressed by its uid: " span class="bv-code" { (ledger) "#" (uid) } "."
                }
            },
            None => html! { p class="mv-note" data-tone="warning" { "No ask " span class="bv-code" { (handle) } "." } },
        },
        many => html! {
            p class="mv-note" data-tone="warning" {
                span class="bv-code" { (handle) } " names " (many.len()) " asks, so none is shown: the ledger must give each its own uid or id."
            }
        },
    };
    // The card stands on both reads, scoped to this ledger, so its state explains the outcome.
    card(id, "decision", "Decision", &joined(cx, Some(ledger)), None, body)
}

fn cells(row: &Tradeoff) -> Markup {
    html! {
        dl class="mv-cells" {
            @for (axis, v) in &row.cells {
                div class="mv-cell" { dt class="bv-meta" { (axis_label(axis)) } dd { (v) } }
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

/// The decision's full body. `landing`: whether its answer waits to land (read for open asks).
/// `ambiguous`: its handle names more than one ask in its ledger, which the owner path refuses.
pub(crate) fn body(cx: &Ctx, lf: &LedgerFile, a: &Ask, landing: Landing, ambiguous: bool) -> Markup {
    let state = a.state();
    let rich = rich::of(a);
    let free: Vec<_> = rich.tradeoffs.iter().filter(|r| r.option.is_none()).collect();
    let question = a.ask.as_deref().or(a.question.as_deref()).filter(|q| *q != a.title());
    html! {
        article class="mv-decision" data-ask=(format!("{}#{}", lf.file, a.handle())) data-hash=(a.hash()) data-kind=(kind(a)) {
            p class="mv-dmeta bv-meta" {
                span class="mv-chip" data-kind=(kind(a)) { (kind(a)) }
                " " (lf.file)
                @if let Some(at) = &a.asked_at { " · asked " (at) }
                " by " bdi { (asker(lf, a)) }
            }
            p class="mv-dtitle" { (a.title()) }
            @if let Some(stakes) = &a.stakes { p class="mv-stakes" { (stakes) } }
            // What silence does is said only while the ask waits on the owner.
            @if state == AskState::Open && landing == Landing::NotWaiting {
                @if let Some(d) = &a.default {
                    p class="mv-callout" data-tone="warning" { "If you don't answer: " (d) }
                } @else if let Some(why) = &a.no_default_because {
                    p class="mv-callout" data-tone="warning" { "No default: " (label(why)) ". It waits for you." }
                }
            }
            @if a.blocking && !a.gates.is_empty() {
                p class="mv-gates bv-meta" { "Blocks " @for (i, g) in a.gates.iter().enumerate() { @if i > 0 { ", " } span class="bv-code" { (g) } } }
            }
            @if let Some(s) = &rich.summary { p class="mv-dbody" { (s) } }
            @if let Some(q) = question { p class="mv-dbody" { (q) } }
            @if let Some(why) = &a.why_you { p class="mv-dbody mv-why" { "Why you: " (why) } }
            @if let Some(details) = &rich.details {
                // Sections, not folds: a poll swap keeps no state inside an opened decision.
                section class="mv-fold" { h3 class="mv-fold-title" { "Details" } p class="mv-pre" { (details) } }
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
                            @for row in rich.tradeoffs.iter().filter(|r| r.option.as_deref() == Some(o.id.as_str())) {
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
                        li { p class="mv-dopt-label" { (row.label.as_deref().unwrap_or("")) } (cells(row)) }
                    }
                }
            }
            @if !rich.sources.is_empty() {
                h3 class="mv-label bv-meta" { "Sources" }
                ul class="mv-sources" {
                    @for s in &rich.sources {
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
                section class="mv-fold mv-tried" {
                    h3 class="mv-fold-title" { "What the agent tried first" }
                    ul {
                        @for e in &a.exhausted {
                            @let (rung, what) = tried(e);
                            li { @if let Some(r) = rung { span class="bv-code" { (r.as_str()) } ": " } (what) }
                        }
                    }
                }
            }
            @if ambiguous {
                p class="mv-callout" data-tone="warning" {
                    span class="bv-code" { (lf.file) "#" (a.handle()) } " names more than one ask in its ledger, and Paseo refuses an answer to an ambiguous handle. The ledger must give each its own uid."
                }
            } @else {
                (outcome(cx, a, state, landing))
            }
        }
    }
}

/// Where the answer stands, or where to give it. Phase 1 writes nothing: the plugin is the one writer.
fn outcome(cx: &Ctx, a: &Ask, state: AskState, landing: Landing) -> Markup {
    let given =
        a.answer_option.as_deref().and_then(|id| a.options.iter().find(|o| o.id == id)).map(|o| o.label.as_str());
    html! {
        @if state == AskState::Open && landing == Landing::Waiting {
            p class="mv-callout" data-tone="success" {
                "Answered in the working tree or on maestro/answers; waiting to land on " span class="bv-code" { (cx.snap.tree) } "."
            }
        } @else if state == AskState::Open && landing == Landing::Unknown {
            // Unknown is never shown as unanswered: no answer bar invites a second answer.
            p class="mv-callout" data-tone="warning" {
                "Maestro cannot tell whether this ask is already answered, or whether Paseo would take an answer to it: part of its ledger did not read, or its handle names two asks. Check Paseo · Maestro · Decisions."
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
                // The plugin answers ideas under Initiatives, and only with develop, park or kill;
                // the place is named, and no idea option is recommended as an answer here.
                p class="mv-answer-cta" {
                    @if a.class == Some(AskClass::Idea) {
                        "To answer: Paseo · Maestro · Initiatives"
                    } @else {
                        "To answer: Paseo · Maestro · Decisions"
                        @if let Some(r) = a.recommended() { span class="bv-meta" { " (recommended: " (r.label) ")" } }
                    }
                }
                p class="mv-answer-why bv-meta" {
                    "Paseo's Maestro plugin still writes every answer, so this screen only reads. Find it there as "
                    span class="bv-code" { (a.handle()) } "."
                }
            }
        }
    }
}
