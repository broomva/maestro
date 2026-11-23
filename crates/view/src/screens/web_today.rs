//! The read-only Today page. A recorded answer does not prove an agent acted on it.
use crate::render::Ctx;
use maestro_core::ask::AskState;
use maestro_core::work::{OrchState, WorkItem};
use maestro_store::TreeSource;
use maud::{Markup, html};

#[derive(Clone, Copy)]
struct Stages {
    answered: bool,
    on_main: bool,
}

fn stages(s: Stages) -> Markup {
    html! { div class="today-stages" aria-label="Answer progress" {
        @for (label, known) in [("Answered", s.answered), ("On main", s.on_main)] {
            span class="today-stage" data-known=(known) { span aria-hidden="true" { "●" } " " (label) @if !known { " · not yet known" } }
        }
        span class="today-outcome" { "Agent outcome not reported" }
    } }
}

fn decision_link(lf: &maestro_store::LedgerFile, ask: &maestro_core::ask::Ask, label: &str) -> Markup {
    let unique = lf.ledger.asks.iter().filter(|a| a.handle() == ask.handle()).count() == 1;
    html! {
        @if unique {
            a href=(format!("/decisions/{}/{}", super::web::segment(&lf.file), super::web::segment(ask.handle()))) { (label) }
        } @else { span class="today-notice" { "Decision link unavailable: shared handle" } }
    }
}

fn work_stat(cx: &Ctx, state: OrchState, label: &str) -> Markup {
    let work = &cx.reads.work;
    let rows: Vec<&WorkItem> = work.data.iter().flatten().filter(|w| w.front.state == state).collect();
    html! {
        details class="today-stat" {
            summary { strong { (count(work, work.data.as_ref().map(|_| rows.len()))) } span { (label) } }
            div class="today-stat-body" {
                @if work.data.is_none() { p { "Work items are unavailable." } }
                @if !work.errors.is_empty() { p class="today-notice" { "This list may be incomplete. See Data sources for read errors." } }
                @if work.data.is_some() && work.errors.is_empty() && rows.is_empty() { p { "No work items in this state." } }
                @for row in rows.iter().take(8) { (crate::render::work_details(row)) }
                @if rows.len() > 8 { p class="today-notice" { "Showing 8 of " (rows.len()) " recorded items." } }
            }
        }
    }
}

fn count<T>(section: &maestro_core::Section<T>, n: Option<usize>) -> String {
    match n {
        None => "Unavailable".into(),
        Some(n) if !section.errors.is_empty() => format!("{n} (partial)"),
        Some(n) => n.to_string(),
    }
}

/// Writer receipts are a separate activity feed, never evidence that an agent acted on an ask.
fn deliveries(cx: &Ctx) -> Markup {
    use maestro_store::delivery::Stage;
    let section = cx.store.answer_deliveries();
    html! {
        section class="today-overnight" aria-label="Answer delivery" {
            h2 { "Answer delivery" }
            p class="today-notice" { "Recorded by the answer writer. A written answer or session notification does not prove that an agent started or completed the work." }
            @if section.data.is_none() { p { "Answer delivery receipts are unavailable." } }
            @if !section.errors.is_empty() && section.data.is_some() { p class="today-notice" { "Delivery history is partial. A missing receipt or stage is not proof that nothing happened." } }
            @if section.data.as_ref().is_some_and(Vec::is_empty) && section.errors.is_empty() { p { "No answer delivery receipts recorded." } }
            @for row in section.data.iter().flatten().take(8) {
                article class="today-answer" data-delivery=(row.id) {
                    time datetime=(row.signed_at) { (row.signed_at.parse::<jiff::Timestamp>().map(|t| t.to_zoned(jiff::tz::TimeZone::system()).strftime("%H:%M").to_string()).unwrap_or_else(|_| row.signed_at.clone())) }
                    div class="today-answer-copy" {
                        strong { @match row.stage {
                            Stage::Signed => "Signed, awaiting writer",
                            Stage::Applying => "Write started",
                            Stage::Undone => "Undone before write",
                            Stage::Written => "Recorded written",
                            Stage::Failed => "Write failed",
                        } }
                        p { code { (row.intent.ledger) "#" (row.intent.handle) } }
                        @let matching: Vec<_> = cx.reads.asks.data.iter().flatten().filter(|lf| lf.file == row.intent.ledger).flat_map(|lf| lf.ledger.asks.iter().filter(|a| a.handle() == row.intent.handle).map(move |ask| (lf, ask))).collect();
                        @if matching.len() == 1 && !cx.reads.asks.errors.iter().any(|e| maestro_store::views::may_hide(&e.at, &row.intent.ledger)) { (decision_link(matching[0].0, matching[0].1, "View decision")) }
                        p { @if row.session_notified() { "Session notified" }
                            @else if let Some(woke) = &row.woke { "Recorded delivery result: " (woke) }
                            @else { "Session notification not recorded" }
                        }
                        details class="mv-work-context" {
                            summary { "Receipt details" }
                            div class="mv-work-context-body" {
                                dl {
                                    dt { "Receipt" } dd { code { (row.id) } }
                                    dt { "Signed at" } dd { time datetime=(row.signed_at) { (row.signed_at) } }
                                    @if let Some(commit) = &row.commit { dt { "Writer commit" } dd { code { (commit) } } }
                                    @if let Some(landing) = &row.landing { dt { "Recorded landing result" } dd { (landing) } }
                                    @if let Some(error) = &row.error { dt { "Recorded error" } dd { (error) } }
                                }
                                p { "The writer commit records its write to the ledger branch. It does not establish that the answer has landed on main." }
                            }
                        }
                    }
                }
            }
            @if section.data.as_ref().is_some_and(|rows| rows.len() > 8) { p class="today-notice" { "Showing the 8 most recently signed receipts in the recorded history." } }
            details class="mv-provenance" {
                summary { "Delivery source" }
                div class="mv-provenance-body" {
                    p { code { (section.source) } }
                    @for error in section.errors.iter().take(8) { p class="mv-source-error" { (error.at) ": " (error.reason.chars().take(240).collect::<String>()) @if error.reason.chars().count() > 240 { "… (message shortened)" } } }
                    @if section.errors.len() > 8 { p class="today-notice" { "Showing 8 of " (section.errors.len()) " receipt read errors." } }
                }
            }
        }
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
    let mut history: Vec<_> = asks
        .data
        .iter()
        .flatten()
        .flat_map(|lf| lf.ledger.asks.iter().map(move |a| (lf, a)))
        .filter(|(_, a)| a.state() == AskState::Answered)
        .collect();
    history.sort_by_key(|(_, a)| std::cmp::Reverse(a.answered_at.as_deref().and_then(super::instant)));
    let groups = super::queue(cx, None);
    let waiting: Vec<_> =
        groups.iter().flat_map(|g| &g.rows).filter(|e| e.landing != super::Landing::Waiting).collect();
    let uncertain = waiting.iter().any(|e| e.landing == super::Landing::Unknown);
    let joined = super::joined(cx, None);
    let reading_main = matches!(&cx.store.tree.source, TreeSource::GitRev(r) if r == "main" || r == "origin/main")
        && cx.store.tree.commit.is_some();
    let since = cx.at.as_second() - 24 * 60 * 60;
    let mut recent_asks: Vec<_> = asks
        .data
        .iter()
        .flatten()
        .flat_map(|lf| lf.ledger.asks.iter().map(move |a| (lf, a)))
        .filter_map(|(lf, a)| a.asked_at.as_deref().and_then(super::instant).map(|t| (lf, a, t)))
        .filter(|(_, _, t)| (since..=cx.at.as_second()).contains(&t.as_second()))
        .collect();
    recent_asks.sort_by_key(|(_, _, t)| std::cmp::Reverse(*t));
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
          section class="today-answers" id=(format!("{id}-answers")) aria-label="Today's answers" {
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
            @for (lf, ask, at) in recent_asks.iter().take(5) {
              div class="today-event" {
                time datetime=(at) { (at.to_zoned(zone.clone()).strftime("%H:%M")) }
                span { "Filed: " (ask.title()) }
                // Only an ask still open is one to decide; it links to that ask itself.
                @if ask.state() == AskState::Open { (decision_link(lf, ask, "Decide")) }
                @else { span class="today-done" { @if ask.state() == AskState::Answered { "Answered" } @else { "Archived" } } }
              }
            }
          }
          (deliveries(cx))
        }
        aside class="today-side" {
          section class="today-waiting" {
            h2 {
                @if joined.data.is_none() { "Decision status unavailable" }
                @else if uncertain { "Decision status incomplete" }
                @else if waiting.is_empty() && joined.errors.is_empty() { "No decisions waiting" }
                @else { (count(&joined, Some(waiting.len()))) " decisions waiting" }
            }
            @if joined.data.is_none() { p { "Decisions are unavailable." } }
            @if uncertain { p class="today-notice" { "Some decision states could not be confirmed." } }
            @if let Some(needs) = &cx.snap.needs.data {
                @for need in needs.iter().filter(|n| n.kind == maestro_core::need::NeedKind::OwnerStep) {
                    @let work: Vec<_> = cx.reads.work.data.iter().flatten().filter(|w| need.key == format!("work:{}", w.front.id)).collect();
                    div class="today-waiting-row" {
                        @if work.len() == 1 { (crate::render::work_details(work[0])) }
                        @else { (need.title) p class="today-notice" { "Work details could not be identified uniquely." } }
                    }
                }
            }
            @if !cx.snap.needs.errors.is_empty() { p class="today-notice" { "Other holds could not all be read." } }
            @if let Some(landed) = cx.unlanded().data.as_ref() { @if !landed.is_empty() { p { (landed.len()) " answers seen waiting to land." } } }
            @if !cx.unlanded().errors.is_empty() { p class="today-notice" { "Answers waiting to land are partially available." } }
            @for entry in waiting.iter().take(3) { p class="today-waiting-row" { (entry.ask.title()) } }
            a class="today-action" href="/decisions" { @if waiting.is_empty() { "View decisions and holds" } @else if uncertain { "Review decisions" } @else { "Decide now" } }
          }
          div class="today-stats" {
            a class="today-stat-link" href=(format!("#{id}-answers")) { strong { (today_count) } span { "answered today" } }
            details class="today-stat" {
              summary { strong { (count(asks, asks.data.as_ref().map(|_| answered_all))) } span { "answered, all time" } }
              div class="today-stat-body" {
                @if asks.data.is_none() { p { "Answer history is unavailable." } }
                @if !asks.errors.is_empty() { p class="today-notice" { "Answer history may be incomplete. See Data sources for read errors." } }
                @if asks.data.is_some() && asks.errors.is_empty() && answered_all == 0 { p { "No answered asks recorded." } }
                ul class="mv-list" {
                  @for (lf, ask) in history.iter().take(12) {
                    li { (decision_link(lf, ask, ask.title())) }
                  }
                }
                @if history.len() > 12 { p class="today-notice" { "Showing the 12 most recent of " (history.len()) " recorded answers; undated answers follow dated ones." } }
              }
            }
            (work_stat(cx, OrchState::Review, "work items in review"))
            (work_stat(cx, OrchState::Blocked, "blocked"))
          }
        }
      }
    }
}
