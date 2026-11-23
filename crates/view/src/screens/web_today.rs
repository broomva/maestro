//! The read-only Today page. Receipt stages can be supplied here by the answer writer later.
use crate::render::Ctx;
use maestro_core::ask::AskState;
use maestro_core::work::OrchState;
use maestro_store::TreeSource;
use maud::{Markup, html};

#[derive(Clone, Copy)]
struct Stages {
    answered: bool,
    on_main: bool,
}

fn stages(s: Stages) -> Markup {
    html! { div class="today-stages" aria-label="Answer progress" {
        @for (label, known) in [("Answered", s.answered), ("On main", s.on_main), ("Acting", false), ("Acted", false)] {
            span class="today-stage" data-known=(known) { span aria-hidden="true" { "●" } " " (label) @if !known { " · not yet known" } }
        }
    } }
}

fn count<T>(section: &maestro_core::Section<T>, n: Option<usize>) -> String {
    match n {
        None => "Unavailable".into(),
        Some(n) if !section.errors.is_empty() => format!("{n} (partial)"),
        Some(n) => n.to_string(),
    }
}

pub(crate) fn today(cx: &Ctx, id: &str) -> Markup {
    let asks = &cx.reads.asks;
    let all: Vec<_> = asks.data.iter().flatten().flat_map(|lf| lf.ledger.asks.iter()).collect();
    let zone = jiff::tz::TimeZone::system();
    let now = cx.at.to_zoned(zone.clone());
    let date = now.date();
    let mut answers: Vec<_> = asks
        .data
        .iter()
        .flatten()
        .flat_map(|lf| lf.ledger.asks.iter().map(move |a| (lf, a)))
        .filter_map(|(lf, a)| {
            let t = a.answered_at.as_deref().and_then(super::instant)?;
            (a.state() == AskState::Answered && t <= cx.at && t.to_zoned(zone.clone()).date() == date)
                .then_some((lf, a, t))
        })
        .collect();
    answers.sort_by_key(|(_, _, t)| std::cmp::Reverse(*t));
    let undated_answers = all
        .iter()
        .any(|a| a.state() == AskState::Answered && a.answered_at.as_deref().and_then(super::instant).is_none());
    let undated_asks = all.iter().any(|a| a.asked_at.as_deref().and_then(super::instant).is_none());
    let today_count = if undated_answers && asks.data.is_some() {
        format!("{} or more", answers.len())
    } else {
        count(asks, asks.data.as_ref().map(|_| answers.len()))
    };
    let answered_all = all.iter().filter(|a| a.state() == AskState::Answered).count();
    let groups = super::queue(cx, None);
    let waiting: Vec<_> =
        groups.iter().flat_map(|g| &g.rows).filter(|e| e.landing == super::Landing::NotSeen).collect();
    let joined = super::joined(cx, None);
    let reading_main = matches!(&cx.store.tree.source, TreeSource::GitRev(r) if r == "main" || r == "origin/main")
        && cx.store.tree.commit.is_some();
    let work = &cx.reads.work;
    let reviews = work.data.as_ref().map(|w| w.iter().filter(|w| w.front.state == OrchState::Review).count());
    let blocked = work.data.as_ref().map(|w| w.iter().filter(|w| w.front.state == OrchState::Blocked).count());
    let since = cx.at.as_second() - 24 * 60 * 60;
    let mut recent_asks: Vec<_> = all
        .iter()
        .filter_map(|a| a.asked_at.as_deref().and_then(super::instant).map(|t| (*a, t)))
        .filter(|(_, t)| (since..=cx.at.as_second()).contains(&t.as_second()))
        .collect();
    recent_asks.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
    let recent_count = if undated_asks && asks.data.is_some() {
        format!("{} or more", recent_asks.len())
    } else {
        count(asks, asks.data.as_ref().map(|_| recent_asks.len()))
    };
    html! {
      div class="today-page" data-block=(id) data-type="today" {
        div class="today-primary" {
          header class="today-heading" {
            h1 { (now.strftime("%A, %-d %B")) }
            p { "Answered today: " (today_count) ". New asks in the last 24 hours: " (recent_count) "." }
          }
          section class="today-answers" aria-label="Today's answers" {
            @if asks.data.is_none() { p { "Answers are unavailable." } }
            @if asks.data.is_some() && asks.errors.is_empty() && !undated_answers && answers.is_empty() { p { "No answers found today." } }
            @if !asks.errors.is_empty() && asks.data.is_some() { p class="today-notice" { "Answers are partially available." } }
            @if undated_answers { p class="today-notice" { "Some answers have no recorded date. Today's count is a lower bound." } }
            @for (lf, ask, at) in &answers {
              article class="today-answer" {
                time datetime=(at) { (at.to_zoned(zone.clone()).strftime("%H:%M")) }
                div class="today-answer-copy" {
                  strong { (ask.title()) }
                  p { "You chose " (ask.answer_option.as_deref().and_then(|id| ask.options.iter().find(|o| o.id == id).map(|o| o.label.as_str()).or(Some(id))).or(ask.answer.as_deref()).unwrap_or("an answer that is not recorded"))
                    @if let Some(note) = &ask.answer_note { " · note: " (note) }
                  }
                }
                // The read includes the ledger branch overlay. Only an identical answer in
                // the underlying main tree proves this particular answer has landed.
                (stages(Stages { answered: true, on_main: reading_main && cx.main_asks().data.as_ref().is_some_and(|ledgers| {
                    !cx.main_asks().errors.iter().any(|e| maestro_store::views::may_hide(&e.at, &lf.file)) && ledgers.iter().filter(|base| base.file == lf.file).flat_map(|base| &base.ledger.asks).any(|base| base.id == ask.id && base.uid == ask.uid && base.hash() == ask.hash())
                }) }))
              }
            }
          }
          section class="today-overnight" {
            h2 { "Last 24 hours" }
            @if asks.data.is_none() { p { "New asks are unavailable." } }
            @if undated_asks { p class="today-notice" { "Some asks have no recorded date. The last 24 hours may be incomplete." } }
            @if recent_asks.is_empty() && asks.data.is_some() && asks.errors.is_empty() && !undated_asks { p { "No new asks in the last 24 hours." } }
            @for (ask, at) in recent_asks.iter().take(5) {
              div class="today-event" {
                time { (at.to_zoned(zone.clone()).strftime("%H:%M")) }
                span { "Filed: " (ask.title()) }
                // Only an ask still open is one to decide; it links to that ask itself.
                @if ask.state() == AskState::Open { a href="/decisions" { "Decide" } } @else { span class="today-done" { "Answered" } }
              }
            }
          }
        }
        aside class="today-side" {
          section class="today-waiting" {
            h2 {
                @if joined.data.is_none() { "Decision status unavailable" }
                @else if waiting.is_empty() && joined.errors.is_empty() { "No decisions waiting" }
                @else { (count(&joined, Some(waiting.len()))) " decisions waiting" }
            }
            @if joined.data.is_none() { p { "Decisions are unavailable." } }
            @if let Some(needs) = &cx.snap.needs.data {
                @for need in needs.iter().filter(|n| n.kind == maestro_core::need::NeedKind::OwnerStep) {
                    p class="today-waiting-row" { (need.title) }
                }
            }
            @if !cx.snap.needs.errors.is_empty() { p class="today-notice" { "Other holds could not all be read." } }
            @if let Some(landed) = cx.unlanded().data.as_ref() { @if !landed.is_empty() { p { (landed.len()) " answers seen waiting to land." } } }
            @if !cx.unlanded().errors.is_empty() { p class="today-notice" { "Answers waiting to land are partially available." } }
            @for entry in waiting.iter().take(3) { p class="today-waiting-row" { (entry.ask.title()) } }
            a class="today-action" href="/decisions" { @if waiting.is_empty() { "View decisions and holds" } @else { "Decide now" } }
          }
          div class="today-stats" {
            @for (value, label) in [(today_count.clone(), "answered today"), (count(asks, asks.data.as_ref().map(|_| answered_all)), "answered, all time"), (count(work, reviews), "work items in review"), (count(work, blocked), "blocked")] {
              div { strong { (value) } span { (label) } }
            }
          }
        }
      }
    }
}
