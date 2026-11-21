//! One decision, full length: stakes first, what silence does, the summary, the options with
//! their real tradeoffs, the sources, what the agent tried first, and where to answer.

use super::rich::{self, Tradeoff};
use super::{Landing, asker, joined, kind, landing};
use crate::render::{Ctx, Owner, card, label, scope_errors};
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
    // The web page takes the answer in place (BRO-4042): the options become choices.
    let answering = answerable(cx, a, state, landing, ambiguous) == Some(&Owner::Ready);
    let rich = rich::of(a);
    let free: Vec<_> = rich.tradeoffs.iter().filter(|r| r.option.is_none()).collect();
    let question = a.ask.as_deref().or(a.question.as_deref()).filter(|q| *q != a.title());
    html! {
        article class="mv-decision" data-ask=(format!("{}#{}", lf.file, a.handle())) data-hash=(a.hash()) data-kind=(kind(a))
            data-ledger=(lf.file) data-handle=(a.handle()) {
            // The kind, whether it blocks, and whether every way out can be undone (the design's chips).
            p class="mv-dchips" {
                span class="mv-dchip" data-kind=(kind(a)) { (kind(a)) }
                @if !a.options.is_empty() && a.options.iter().all(|o| o.reversible == Some(true)) {
                    span class="mv-dchip" data-tone="success" { "Reversible" }
                }
                @if a.blocking { span class="mv-dchip" data-tone="attention" { "Blocking" } }
                @else { span class="mv-dchip" data-tone="warning" { "Not blocking" } }
            }
            p class="mv-dtitle" { (a.title()) }
            @if a.blocking && !a.gates.is_empty() {
                p class="mv-gates bv-meta" { "Blocks " @for (i, g) in a.gates.iter().enumerate() { @if i > 0 { ", " } span class="bv-code" { bdi { (g) } } } }
            }
            @if let Some(s) = &rich.summary { p class="mv-dbody" { (s) } }
            @if let Some(q) = question { p class="mv-dbody" { (q) } }
            // What waiting costs and why it is the owner's, side by side (maestro#20).
            @if a.stakes.is_some() || a.why_you.is_some() {
                div class="mv-pair" {
                    @if let Some(stakes) = &a.stakes { p class="mv-stakes" { span class="mv-pair-label" { "What waiting costs" } " " (stakes) } }
                    @if let Some(why) = &a.why_you { p class="mv-dbody mv-why" { span class="mv-pair-label" { "Why you" } " " bdi { (why) } } }
                }
            }
            @if let Some(details) = &rich.details {
                // Sections, not folds: a poll swap keeps no state inside an opened decision.
                section class="mv-fold" { h3 class="mv-fold-title" { "Details" } p class="mv-pre" { (details) } }
            }
            @if !a.options.is_empty() {
                // "Choose one" only where the owner path is offered, as the answer bar is.
                h3 class="mv-label bv-meta" {
                    @if state == AskState::Open && landing == Landing::NotSeen && !ambiguous { "Choose one" } @else { "Options" }
                }
                ol class="mv-dopts" {
                    @for o in &a.options {
                        li class="mv-dopt" data-option=(o.id) data-recommended=(o.recommended)
                            data-chosen=(a.answer_option.as_deref() == Some(o.id.as_str())) data-answerable=[answering.then_some("true")] {
                            @if answering {
                                // The whole card is the control: a radio, never preselected.
                                input class="mv-opt-input" type="radio" name="mv-answer-option" value=(o.id)
                                    data-label=(o.label) aria-label=(o.label);
                            }
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
            // What silence does is said only while the ask waits on the owner.
            @if state == AskState::Open && landing == Landing::NotSeen {
                @if let Some(d) = &a.default {
                    p class="mv-callout" data-tone="warning" { "If you don't answer: " bdi { (d) } }
                } @else if let Some(why) = &a.no_default_because {
                    p class="mv-callout" data-tone="warning" { "No default: " (label(why)) ". It waits for you." }
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
            // Beside the decision on a wide screen: what the agent tried, where the ask came from,
            // and, where the page takes the answer, exactly what the passkey signs.
            section class="mv-fold mv-tried" {
                h3 class="mv-fold-title" { "What the agent tried" }
                @if a.exhausted.is_empty() {
                    p class="mv-tried-none" { "The ask names nothing it tried first." }
                } @else {
                    ul {
                        @for e in &a.exhausted {
                            @let (rung, what) = tried(e);
                            li { @if let Some(r) = rung { span class="bv-code" { (r.as_str()) } " " } bdi { (what) } }
                        }
                    }
                }
                p class="mv-origin bv-code" {
                    "from " bdi { (asker(lf, a)) } br;
                    bdi { (lf.file) } br;
                    bdi { (a.handle()) }
                    @if let Some(at) = &a.asked_at { br; "asked " bdi { (at) } }
                }
                @if answering {
                    div class="mv-sign" {
                        p class="mv-sign-title" { "What you will sign" }
                        p class="bv-code" { "answer " span data-sign="option" { "the option you choose" } br; "ask " (a.hash()) br; "checked again when it is written" }
                    }
                }
            }
            @if ambiguous {
                p class="mv-callout" data-tone="warning" {
                    span class="bv-code" { bdi { (lf.file) "#" (a.handle()) } } " names more than one ask in its ledger, and Paseo refuses an answer to an ambiguous handle. The ledger must give each its own uid."
                }
            } @else {
                (outcome(cx, lf, a, state, landing))
            }
        }
    }
}

/// Where the answer stands, or where to give it. Phase 1 writes nothing: the plugin is the one writer.
/// Where Paseo's plugin shows an ask and takes its answer: ideas under Initiatives (with
/// develop, park or kill only), every other ask under Decisions.
/// The T3 answer handoff (BRO-3590): each option's exact answer, and links that open `t` with it
/// as a draft. Nothing here answers: the owner sends the draft and the agent there files it.
fn handoff(cx: &Ctx, lf: &LedgerFile, a: &Ask, t: &crate::handoff::AnswerThread) -> Markup {
    // The folder the asks were read from, so the answer lands in the ledger Maestro read.
    let dir = cx.store.rel(maestro_store::ASKS);
    html! {
        div class="mv-control mv-answerbar" data-handoff="t3" {
            p class="mv-answer-cta" {
                "Open at " span class="bv-code" { (cx.snap.tree) } ". To answer, open an option in T3 thread "
                span class="bv-code" { (t.thread()) } ": it arrives as a draft for you to read and send, and the agent there files it."
            }
            @match &dir {
                Err(e) => p class="mv-note" data-tone="warning" { "The asks folder could not be resolved, so no answer can be drafted: " (e) },
                Ok(dir) => ul class="mv-list mv-handoff" {
                    @for o in &a.options {
                        li class="mv-handoff-option" data-option=(o.id) {
                            span class="bv-code" { (o.id) } " " bdi { (o.label) }
                            @if o.recommended { " · " span data-tone="success" { "Recommended" } }
                            @let prompt = crate::handoff::prompt(dir, lf, a, o, t, &cx.snap.tree);
                            @if crate::handoff::fits(&prompt) {
                                " · " a href=(t.app_link(&prompt)) { "Open in T3" }
                                " · " a href=(t.web_link(&prompt)) target="_blank" rel="noopener noreferrer" { "T3 on the web" }
                            } @else {
                                " · " span data-tone="warning" { "Too long for a T3 link: paste the exact answer into the thread." }
                            }
                            details { summary class="bv-meta" { "The exact answer" } pre class="bv-code mv-handoff-prompt" { (prompt) } }
                        }
                    }
                },
            }
            p class="mv-answer-why bv-meta" { "Maestro writes no answer: the ask ledger's own path does." }
        }
    }
}

fn place(a: &Ask) -> &'static str {
    if a.class == Some(AskClass::Idea) { "Paseo · Maestro · Initiatives" } else { "Paseo · Maestro · Decisions" }
}

/// Whether this decision takes the owner's answer on this page: `None` where it does not (the
/// App, an ask that is not open, waiting to land, unknown or ambiguous, or one with no options).
fn answerable<'c>(cx: &'c Ctx, a: &Ask, state: AskState, landing: Landing, ambiguous: bool) -> Option<&'c Owner> {
    let owner = cx.web.as_ref()?;
    (state == AskState::Open && landing == Landing::NotSeen && !ambiguous && !a.options.is_empty()).then_some(owner)
}

/// The answer bar of the web page: a note, the answer button that names the chosen option, and
/// what happens when it is pressed. The page's script signs and sends it; nothing posts without it.
fn answer_bar(a: &Ask, owner: &Owner) -> Markup {
    html! {
        @match owner {
            Owner::Ready => div class="mv-control mv-answerbar mv-answer" data-answer-bar="true" {
                label class="mv-note" {
                    span class="mv-sr" { "Note, sent to the agent with your answer" }
                    textarea class="mv-note-input" name="note" rows="2" maxlength="2000"
                        placeholder="Add a note for the agent (optional)" {}
                }
                div class="mv-answer-row" {
                    button class="mv-go" type="button" disabled data-answer="go" {
                        span class="mv-go-icon" aria-hidden="true" {}
                        span class="mv-go-text" { "Choose an option" }
                    }
                    p class="mv-answer-how" {
                        "Signed with your passkey and written by Maestro now. You can undo for 8 seconds."
                    }
                }
                p class="mv-answer-status" role="status" aria-live="polite" {}
            },
            Owner::Enroll => div class="mv-control mv-answerbar mv-answer" {
                p class="mv-answer-cta" { "Answering here needs your passkey. " a href="/owner" { "Set up your passkey" } " once, on this device." }
            },
            Owner::Off(why) => div class="mv-control mv-answerbar mv-answer" {
                p class="mv-answer-cta" { "Answering is not available on this page: " (why) }
                p class="mv-answer-why bv-meta" { "Find it as " span class="bv-code" { bdi { (a.handle()) } } "." }
            },
        }
    }
}

fn outcome(cx: &Ctx, lf: &LedgerFile, a: &Ask, state: AskState, landing: Landing) -> Markup {
    if let Some(owner) = answerable(cx, a, state, landing, false) {
        return answer_bar(a, owner);
    }
    let given =
        a.answer_option.as_deref().and_then(|id| a.options.iter().find(|o| o.id == id)).map(|o| o.label.as_str());
    html! {
        @if landing == Landing::Unknown {
            // Unknown comes first, whatever the entry says: a sibling that did not read, or a
            // handle two asks share, may make this row not the ask Paseo holds. No bar, no claim.
            p class="mv-callout" data-tone="warning" {
                "Maestro cannot tell for sure whether this ask is answered, or whether Paseo would take an answer to it. "
                (place(a)) " shows it."
            }
        } @else if state == AskState::Open && landing == Landing::Waiting {
            p class="mv-callout" data-tone="success" {
                "Answered. The answer is in effect; it reaches main with the next landing PR."
            }
        } @else if state == AskState::Answered {
            p class="mv-callout" data-tone="success" {
                "Answered"
                @if let Some(x) = given.or(a.answer.as_deref()).or(a.answer_option.as_deref()) { ": " bdi { (x) } }
                @if let Some(by) = &a.answered_by { " · by " bdi { (by) } }
                @if let Some(at) = &a.answered_at { " · " bdi { (at) } }
            }
        } @else if state == AskState::Archived {
            p class="mv-callout" { "Archived" @if let Some(s) = &a.status { ": " bdi { (s) } } "." }
        } @else if let Some(t) = cx.answer_thread.as_ref().filter(|_| a.class != Some(AskClass::Idea) && !a.options.is_empty()) {
            (handoff(cx, lf, a, t))
        } @else {
            div class="mv-control mv-answerbar" {
                textarea class="mv-note-field" rows="1" disabled placeholder="Add a note (optional)" aria-label="Note (answering is not available here yet)" {}
                // The plugin answers ideas under Initiatives, and only with develop, park or kill;
                // the place is named, and no idea option is recommended as an answer here.
                // Maestro does not read every place Paseo keeps answers: the bar never says
                // the ask is unanswered, only where it is open and where to look and answer.
                p class="mv-answer-cta" {
                    "Open at " span class="bv-code" { (cx.snap.tree) } ". "
                    (place(a))
                    " shows whether it has been answered since, and is where to answer it."
                }
                p class="mv-answer-why bv-meta" {
                    "Paseo's Maestro plugin still writes every answer, so this screen only reads. Find it there as "
                    span class="bv-code" { bdi { (a.handle()) } } "."
                }
            }
        }
    }
}
