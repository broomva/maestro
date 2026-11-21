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
    let mut answers: Vec<_> = all
        .iter()
        .filter_map(|a| {
            let t = a.answered_at.as_deref().and_then(super::instant)?;
            (t <= cx.at && t.to_zoned(zone.clone()).date() == date).then_some((*a, t))
        })
        .collect();
    answers.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
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
    let on_main = matches!(&cx.store.tree.source, TreeSource::GitRev(r) if r == "main" || r == "origin/main")
        && cx.store.tree.commit.is_some();
    let work = &cx.reads.work;
    let reviews = work.data.as_ref().map(|w| w.iter().filter(|w| w.front.state == OrchState::Review).count());
    let blocked = work.data.as_ref().map(|w| w.iter().filter(|w| w.front.state == OrchState::Blocked).count());
    let since = cx.at.as_second() - 24 * 60 * 60;
    let recent_asks: Vec<_> = all
        .iter()
        .filter_map(|a| a.asked_at.as_deref().and_then(super::instant).map(|t| (*a, t)))
        .filter(|(_, t)| (since..=cx.at.as_second()).contains(&t.as_second()))
        .collect();
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
            @if (!asks.errors.is_empty() || undated_answers) && asks.data.is_some() { p class="today-notice" { "Answers are partially available." } }
            @for (ask, at) in &answers {
              article class="today-answer" {
                time { (at.to_zoned(zone.clone()).strftime("%H:%M")) }
                div class="today-answer-copy" {
                  strong { (ask.title()) }
                  p { "You chose " (ask.answer_option.as_deref().and_then(|id| ask.options.iter().find(|o| o.id == id).map(|o| o.label.as_str()).or(Some(id))).or(ask.answer.as_deref()).unwrap_or("an answer that is not recorded"))
                    @if let Some(note) = &ask.answer_note { " · note: " (note) }
                  }
                }
                (stages(Stages { answered: true, on_main }))
              }
            }
          }
          section class="today-overnight" {
            h2 { "Last 24 hours" }
            @if asks.data.is_none() { p { "New asks are unavailable." } }
            @for (ask, at) in recent_asks.iter().rev().take(5) {
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
            h2 { (count(&joined, joined.data.as_ref().map(|_| waiting.len()))) " asks may wait on you" }
            @if joined.data.is_none() { p { "Decisions are unavailable." } }
            @if let Some(landed) = cx.unlanded().data.as_ref() { @if !landed.is_empty() { p { (landed.len()) " answers seen waiting to land." } } }
            @if !cx.unlanded().errors.is_empty() { p class="today-notice" { "Answers waiting to land are partially available." } }
            @for entry in waiting.iter().take(3) { p class="today-waiting-row" { (entry.ask.title()) } }
            a class="today-action" href="/decisions" { "Decide now" }
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
