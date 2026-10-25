//! The Today screen (design page 10): a headline composed from the render's one snapshot, its
//! counts, the top decision, work running now and nodes, beside the health rail.
//!
//! Every count here is the snapshot's, so the headline, the pills and the cards agree. A count
//! whose section was not read says "unavailable" in words, in the headline as in the pills.

use super::{Landing, age, asker, kind, queue};
use crate::render::{Ctx, card, empty};
use maestro_core::Section;
use maestro_core::loops::NoticeKind;
use maestro_core::work::{WorkItem, WorkState};
use maestro_store::TreeSource;
use maud::{Markup, html};

/// The counts the headline and the pills are made of, all from one snapshot.
pub(crate) struct Counts {
    pub needs: Option<usize>,
    pub running: Option<usize>,
    pub open: Option<usize>,
    pub silent: Option<usize>,
    pub landing: Option<usize>,
}

impl Counts {
    pub fn of(cx: &Ctx) -> Self {
        let s = &cx.snap;
        Counts {
            needs: s.needs.data.as_ref().map(Vec::len),
            running: running_count(cx),
            open: s.asks.data.as_ref().map(|a| a.open),
            silent: silent_loops(cx),
            landing: s.asks.data.as_ref().and_then(|a| a.unlanded),
        }
    }
}

/// Work in the Running state, from the work read every card uses. Not the snapshot's work
/// counts: those are keyed by the stored state (`running`), which counts quiet work too.
pub(crate) fn running_count(cx: &Ctx) -> Option<usize> {
    cx.reads.work.data.as_deref().map(|w| maestro_store::views::work_by_state(w, Some(WorkState::Running)).len())
}

/// Loops the ticks ledger, read whole, proves silent: their contract names tick receipts and the
/// ledger holds none. A partial or unread ledger proves nothing, so the count is then not known.
fn silent_loops(cx: &Ctx) -> Option<usize> {
    let rows = cx.snap.loops.data.as_ref()?;
    let whole = cx.reads.ticks.data.as_ref().is_some_and(maestro_store::TickTail::whole);
    (whole && cx.reads.ticks.errors.is_empty())
        .then(|| rows.iter().filter(|r| r.notices.iter().any(|n| n.kind == NoticeKind::NoTickReceipts)).count())
}

pub(crate) fn today(cx: &Ctx, id: &str) -> Markup {
    let counts = Counts::of(cx);
    html! {
        div class="mv-today" data-block=(id) data-type="today" {
            div class="mv-today-main" {
                (headline(cx, &counts))
                (top_decision(cx))
                (running(cx))
                (nodes())
            }
            aside class="mv-rail" { (super::health::rail(cx)) }
        }
    }
}

/// "Composed by Maestro · origin/main@abc1234 · 2 min ago", then one sentence per count.
fn headline(cx: &Ctx, c: &Counts) -> Markup {
    html! {
        header class="mv-headline" {
            p class="mv-composed bv-meta" data-commit=[cx.store.tree.commit.as_deref()] {
                "Composed by Maestro · " span class="bv-code" { (read_from(cx)) } " · " (read_age(cx))
            }
            h1 class="mv-headline-text" { (sentence(c)) }
            div class="mv-pills" {
                (pill("needs you", c.needs, "attention"))
                (pill("running", c.running, "success"))
                (pill("open asks", c.open, "info"))
                (pill("silent loops", c.silent, "danger"))
                (pill("landing", c.landing, "info"))
            }
        }
    }
}

/// The tree read: `origin/main@abc1234`, `worktree`, or the revision that did not resolve.
fn read_from(cx: &Ctx) -> String {
    match (&cx.store.tree.source, &cx.store.tree.commit) {
        (TreeSource::GitRev(rev), Some(commit)) => format!("{rev}@{}", commit.get(..7).unwrap_or(commit)),
        (TreeSource::GitRev(rev), None) => format!("{rev}, which did not resolve"),
        (TreeSource::WorkingTree, _) => "the working tree".into(),
    }
}

/// How old the commit read is. The working tree has no commit, so it is "read now".
fn read_age(cx: &Ctx) -> String {
    match cx.committed() {
        Ok(Some(t)) => ago(*t, cx.at).unwrap_or_else(|| "committed after this read's clock".into()),
        Ok(None) => "read now".into(),
        Err(_) => "age not known".into(),
    }
}

/// Minutes under an hour, hours under a day, then days.
pub(crate) fn ago(from: jiff::Timestamp, at: jiff::Timestamp) -> Option<String> {
    let secs = at.as_second() - from.as_second();
    match secs {
        ..0 => None,
        0..60 => Some("under a minute ago".into()),
        60..3_600 => Some(format!("{} min ago", secs / 60)),
        3_600..86_400 => Some(format!("{} h ago", secs / 3_600)),
        _ => Some(format!("{} d ago", secs / 86_400)),
    }
}

fn sentence(c: &Counts) -> String {
    let mut parts = Vec::new();
    parts.push(match c.needs {
        Some(0) => "Nothing needs you.".to_string(),
        Some(1) => "One thing needs you.".to_string(),
        Some(n) => format!("{n} things need you."),
        None => "What needs you could not be read.".to_string(),
    });
    parts.push(match c.running {
        Some(0) => "No work is running.".to_string(),
        Some(1) => "One piece of work is running.".to_string(),
        Some(n) => format!("{n} pieces of work are running."),
        None => "Running work could not be read.".to_string(),
    });
    parts.push(match c.silent {
        Some(0) => "Every loop that names tick receipts has some.".to_string(),
        Some(1) => "One loop has no tick receipts.".to_string(),
        Some(n) => format!("{n} loops have no tick receipts."),
        None => "Whether a loop has gone silent is not known.".to_string(),
    });
    parts.join(" ")
}

fn pill(label: &str, value: Option<usize>, tone: &str) -> Markup {
    html! {
        span class="mv-pill" data-tone=(if value.unwrap_or(0) > 0 { tone } else { "neutral" }) data-pill=(label) {
            @match value {
                Some(n) => span class="mv-pill-value" { (n) },
                None => span class="mv-pill-value mv-unavailable" { "unavailable" },
            }
            " " (label)
        }
    }
}

/// The first ask in queue order (blocking first, then oldest), as the inbox orders it.
fn top_decision(cx: &Ctx) -> Markup {
    let section = super::joined(cx, None);
    let groups = queue(cx, None);
    let top = groups.first().and_then(|g| g.rows.first());
    let body = match top {
        None => empty("No open ask."),
        Some(e) => html! {
            div class="mv-top" data-ask=(e.handle()) {
                p class="mv-row-meta bv-meta" {
                    span class="mv-badge" data-tone="warning" { (kind(e.ask)) }
                    " " (e.project()) " · " span class="bv-code" { (e.lf.file) }
                    @if e.ask.blocking { " · " span data-tone="attention" { "blocking" } }
                }
                p class="mv-ask-title" { (e.ask.title()) }
                @if let Some(why) = &e.ask.why_you { p class="mv-ask-why" { (why) } }
                p class="mv-row-meta bv-meta" {
                    @if let Some(a) = e.asked().and_then(|t| age(t, cx.at)) { "asked " (a) @if a != "today" { " ago" } " by " (asker(e.lf, e.ask)) } @else { "asked by " (asker(e.lf, e.ask)) }
                    @if e.landing == Landing::Waiting { " · " span data-tone="info" { "answered, waiting to land" } }
                }
                p class="mv-note bv-meta" { "Answer it in Paseo's Maestro plugin. Maestro answers nothing in this phase." }
            }
        },
    };
    card("today-decision", "top-decision", "Top decision", &section, None, body)
}

/// One work item's chips: tier, model and thinking, harness and node, budget, from its
/// frontmatter. A field the item does not carry is not shown, never guessed.
fn chips(w: &WorkItem) -> (Vec<String>, Vec<String>, Option<String>) {
    let text =
        |k: &str| w.front.extra.get(k).and_then(|v| v.as_str().map(str::to_string).or_else(|| Some(v.to_string())));
    let routing: Vec<String> = ["tier", "model", "thinking"].iter().filter_map(|k| text(k)).collect();
    let place: Vec<String> =
        [w.front.provider.clone().or_else(|| text("harness")), text("node")].into_iter().flatten().collect();
    (routing, place, text("budget"))
}

fn running(cx: &Ctx) -> Markup {
    let section = &cx.reads.work;
    let list: Vec<&WorkItem> =
        maestro_store::views::work_by_state(section.data.as_deref().unwrap_or_default(), Some(WorkState::Running));
    let body = html! {
        @if list.is_empty() { (empty("No work is running.")) } @else {
            ul class="mv-list" {
                @for w in &list {
                    @let (routing, place, budget) = chips(w);
                    li class="mv-row" data-work=(w.slug) {
                        span class="mv-row-title" { (w.title) }
                        span class="mv-row-meta bv-meta" {
                            @if !routing.is_empty() { span class="mv-chip bv-code" { (routing.join(" · ")) } " " }
                            @if !place.is_empty() { (place.join(" · ")) }
                            @if let Some(b) = &budget { " · budget " (b) }
                            @if let Some(at) = &w.last_event_at { " · last event " (at) }
                        }
                    }
                }
            }
        }
    };
    card("today-running", "running", "Running now", section, section.data.as_ref().map(|_| list.len()), body)
}

/// Nodes need a census Maestro does not read yet (host adapters), so the card says so.
fn nodes() -> Markup {
    let section: Section<()> =
        Section::unavailable("nodes", "Maestro reads no node census yet; host adapters bring one (BRO-3276)");
    card("today-nodes", "nodes", "Nodes", &section, None, html! {})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_name_minutes_hours_then_days() {
        let at: jiff::Timestamp = "2026-10-23T12:00:00Z".parse().unwrap();
        let ago_s = |s: i64| ago(jiff::Timestamp::from_second(at.as_second() - s).unwrap(), at);
        assert_eq!(ago_s(59).as_deref(), Some("under a minute ago"));
        assert_eq!(ago_s(60).as_deref(), Some("1 min ago"));
        assert_eq!(ago_s(3_599).as_deref(), Some("59 min ago"));
        assert_eq!(ago_s(3_600).as_deref(), Some("1 h ago"));
        assert_eq!(ago_s(86_400).as_deref(), Some("1 d ago"));
        assert_eq!(ago_s(-1), None);
    }

    #[test]
    fn an_unread_count_is_said_in_words() {
        let c = Counts { needs: None, running: Some(0), open: None, silent: None, landing: None };
        let s = sentence(&c);
        assert!(s.contains("could not be read"), "{s}");
        assert!(s.contains("is not known"), "{s}");
        assert!(!s.contains("Nothing needs you"), "{s}");
    }
}
