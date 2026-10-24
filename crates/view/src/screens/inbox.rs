//! The Decisions inbox: open asks in tabs with counts, the answers waiting to land, and the
//! asks grouped by project, oldest first where they hold work. Each row opens in place to the
//! full decision. The tabs are radio inputs and CSS, so they switch with no script: in the
//! MCP App and in the snapshot a host without MCP Apps shows.

use super::{Group, age, asker, decision::body, kind, queue};
use crate::catalog::{DecisionsProps, Tab};
use crate::render::{Ctx, card};
use maud::{Markup, html};
use sha2::{Digest, Sha256};

pub(crate) fn inbox(cx: &Ctx, id: &str, p: &DecisionsProps) -> Markup {
    let section = &cx.reads.asks;
    let groups = queue(cx, p.project.as_deref());
    let rows = || groups.iter().flat_map(|g| &g.rows);
    let count = |tab: Tab| rows().filter(|e| tab == Tab::All || e.tab() == tab).count();
    let waiting = rows().filter(|e| e.waiting).count();
    let to_answer = rows().count() - waiting;
    // A partial read can hide asks: every count is then a lower bound, and says so.
    let floor = if section.errors.is_empty() { "" } else { "at least " };
    let unlanded = cx.unlanded();
    // Radio ids from the block id: unique per block, and always a valid id.
    let key: String = Sha256::digest(id.as_bytes()).iter().take(4).map(|b| format!("{b:02x}")).collect();
    let body = html! {
        p class="mv-sub bv-meta" {
            (floor) (to_answer) " to answer"
            @match &unlanded.data {
                Some(_) => { " · " @if !unlanded.errors.is_empty() { "at least " } (waiting) " waiting to land" },
                None => { " · waiting to land: " span class="mv-unavailable" { "unavailable" } },
            }
        }
        div class="mv-inbox" {
            @for tab in Tab::ALL {
                input class="mv-tab-input" type="radio" name=(format!("mv-tabs-{key}")) id=(format!("mv-tab-{key}-{}", tab.as_str()))
                    value=(tab.as_str()) checked[tab == p.tab];
            }
            nav class="mv-tabs" aria-label="Decision kinds" {
                @for tab in Tab::ALL {
                    label class="mv-tab" for=(format!("mv-tab-{key}-{}", tab.as_str())) data-tab=(tab.as_str()) {
                        (tab.label()) " " span class="mv-tab-count" { (count(tab)) }
                    }
                }
            }
            (banner(cx, waiting))
            @for tab in Tab::ALL {
                @if count(tab) == 0 { p class="mv-tab-empty mv-empty bv-meta" data-tab=(tab.as_str()) { "Nothing to answer here." } }
            }
            div class="mv-groups" { @for g in &groups { (group(cx, g)) } }
        }
    };
    card(id, "decisions", "Decisions", section, None, body)
}

/// Answers given in the working tree that are not on the tree read yet. An unread working tree
/// is said, never shown as none waiting.
fn banner(cx: &Ctx, waiting: usize) -> Markup {
    let unlanded = cx.unlanded();
    html! {
        @if unlanded.data.is_none() {
            p class="mv-banner" data-tone="danger" {
                "Answers waiting to land could not be read. This is not the same as none."
            }
        } @else if waiting > 0 {
            div class="mv-banner" data-tone="info" {
                p class="mv-banner-title" { (waiting) @if waiting == 1 { " answer waiting to land" } @else { " answers waiting to land" } }
                p class="bv-meta" { "answered in the working tree, not yet on " span class="bv-code" { (cx.snap.tree) } }
            }
        }
    }
}

fn group(cx: &Ctx, g: &Group) -> Markup {
    let mut tabs: Vec<&str> = vec![Tab::All.as_str()];
    for r in &g.rows {
        if !tabs.contains(&r.tab().as_str()) {
            tabs.push(r.tab().as_str());
        }
    }
    html! {
        section class="mv-group" data-tabs=(tabs.join(" ")) {
            header class="mv-group-head bv-meta" {
                span class="mv-group-name" { (g.name) }
                @if let Some(a) = g.oldest().and_then(|t| age(t, cx.at)) { span class="mv-group-age" { (a) } }
            }
            ul class="mv-list" {
                @for e in &g.rows {
                    li class="mv-irow" data-tab=(e.tab().as_str()) data-ask=(e.handle()) data-blocking=(e.ask.blocking) {
                        details {
                            summary {
                                span class="mv-irow-meta bv-meta" {
                                    span class="mv-chip" data-kind=(kind(e.ask)) { (kind(e.ask)) }
                                    " " (asker(e.lf, e.ask))
                                    @if let Some(t) = e.ask.topics.first() { " · " (t) }
                                    @if e.ask.blocking { " · " span data-tone="attention" { "blocking" } }
                                }
                                span class="mv-irow-title" { (e.ask.title()) }
                                @if e.waiting {
                                    span class="mv-irow-rec" data-tone="success" { "Waiting to land" }
                                } @else if let Some(r) = e.ask.recommended() {
                                    span class="mv-irow-rec" data-tone="info" { "Recommended: " (r.label) }
                                }
                            }
                            (body(cx, e.lf, e.ask, e.waiting, None))
                        }
                    }
                }
            }
        }
    }
}
