//! The Decisions inbox: open asks in tabs with counts, the answers waiting to land, and the
//! asks grouped by project, oldest first where they hold work. Each row opens in place to the
//! full decision. The tabs are radio inputs and CSS, so they switch with no script: in the
//! MCP App and in the snapshot a host without MCP Apps shows.
//!
//! Every count comes from one of two reads, and each read that came back in part marks every
//! count taken from it as a lower bound: the asks (open, per tab) and the working tree (waiting).

use super::{Entry, Group, age, asker, decision::body, kind, queue};
use crate::catalog::{DecisionsProps, Tab};
use crate::render::{Ctx, card};
use maud::{Markup, html};

/// A count in a sentence: `at least 7` when its read was partial.
fn at_least(n: usize, partial: bool) -> String {
    if partial { format!("at least {n}") } else { n.to_string() }
}

/// A count on a chip: `7+` when its read was partial.
fn plus(n: usize, partial: bool) -> String {
    if partial { format!("{n}+") } else { n.to_string() }
}

pub(crate) fn inbox(cx: &Ctx, id: &str, p: &DecisionsProps) -> Markup {
    let section = &cx.reads.asks;
    let groups = queue(cx, p.project.as_deref());
    let rows = || groups.iter().flat_map(|g| &g.rows);
    let count = |tab: Tab| rows().filter(|e| in_tab(e, tab)).count();
    let partial = !section.errors.is_empty();
    let unlanded = cx.unlanded();
    let waiting = rows().filter(|e| e.waiting).count();
    let waiting_partial = !unlanded.errors.is_empty();
    // Radio names and ids unique in the document, so two inboxes never share a group.
    let key = cx.next_id();
    let body = html! {
        p class="mv-sub bv-meta" {
            (at_least(count(Tab::All), partial)) " open"
            @match &unlanded.data {
                Some(_) => { " · " (at_least(waiting, waiting_partial)) " waiting to land" },
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
                        (tab.label()) " " span class="mv-tab-count" { (plus(count(tab), partial)) }
                    }
                }
            }
            (banner(cx, waiting, waiting_partial))
            @for tab in Tab::ALL {
                @if count(tab) == 0 {
                    p class="mv-tab-empty mv-empty bv-meta" data-tab=(tab.as_str()) {
                        @if partial { "Nothing to answer here in what could be read." } @else { "Nothing to answer here." }
                    }
                }
            }
            div class="mv-groups" { @for g in &groups { (group(cx, g)) } }
        }
    };
    card(id, "decisions", "Decisions", section, None, body)
}

fn in_tab(e: &Entry, tab: Tab) -> bool {
    tab == Tab::All || e.tab() == tab
}

/// Answers given in the working tree that are not on the tree read yet. An unread working tree
/// is said, never shown as none waiting.
fn banner(cx: &Ctx, waiting: usize, partial: bool) -> Markup {
    html! {
        @if cx.unlanded().data.is_none() {
            p class="mv-banner" data-tone="danger" {
                "Answers waiting to land could not be read. This is not the same as none."
            }
        } @else if waiting > 0 {
            div class="mv-banner" data-tone="info" {
                p class="mv-banner-title" { (at_least(waiting, partial)) " waiting to land" }
                p class="bv-meta" { "answered in the working tree, not yet on " span class="bv-code" { (cx.snap.tree) } }
            }
        }
    }
}

fn group(cx: &Ctx, g: &Group) -> Markup {
    let tabs: Vec<Tab> = Tab::ALL.into_iter().filter(|t| g.rows.iter().any(|e| in_tab(e, *t))).collect();
    // The age of the oldest ask the tab shows, one per tab, so a hidden row never ages the group.
    let oldest = |tab: Tab| g.rows.iter().filter(|e| in_tab(e, tab)).filter_map(Entry::asked).min();
    html! {
        section class="mv-group" data-tabs=(tabs.iter().map(|t| t.as_str()).collect::<Vec<_>>().join(" ")) {
            header class="mv-group-head bv-meta" {
                span class="mv-group-name" { (g.name) }
                @for tab in &tabs {
                    @if let Some(a) = oldest(*tab).and_then(|t| age(t, cx.at)) {
                        span class="mv-group-age" data-tab=(tab.as_str()) { (a) }
                    }
                }
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
