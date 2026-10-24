//! The Decisions screens (design page 11, screens 1 and 2): the inbox and one decision.
//!
//! Both read the asks the render read once, in one order (the queue), so the inbox's rows and a
//! decision's "1 of 10" agree. Phase 1 answers nothing here: Paseo's Maestro plugin is still
//! the one writer, and these screens say so where the answer would go.

mod decision;
mod inbox;
mod rich;
mod sources;

pub(crate) use decision::decision;
pub(crate) use inbox::inbox;

use crate::catalog::Tab;
use crate::render::Ctx;
use maestro_core::ask::{Ask, AskClass, AskState};
use maestro_store::LedgerFile;
use std::collections::BTreeSet;

/// One open ask in the queue.
pub(crate) struct Entry<'a> {
    pub lf: &'a LedgerFile,
    pub ask: &'a Ask,
    /// Answered in the working tree, not yet on the tree read: waiting to land.
    pub waiting: bool,
}

impl Entry<'_> {
    pub fn tab(&self) -> Tab {
        tab_of(self.ask)
    }

    /// The ask's project, else its ledger's arc, else the ledger's name (the ask format's default).
    pub fn project(&self) -> String {
        self.ask.project.clone().or_else(|| self.lf.ledger.arc.clone()).unwrap_or_else(|| stem(&self.lf.file))
    }

    pub fn asked(&self) -> Option<jiff::Timestamp> {
        self.ask.asked_at.as_deref().or(self.lf.ledger.opened.as_deref()).and_then(instant)
    }

    pub fn handle(&self) -> String {
        format!("{}#{}", self.lf.file, self.ask.handle())
    }
}

pub(crate) fn tab_of(ask: &Ask) -> Tab {
    match ask.class {
        Some(AskClass::Authority) => Tab::Authority,
        Some(AskClass::Idea) => Tab::Ideas,
        _ => Tab::Decisions,
    }
}

/// The chip text: the ask's class as written, `ask` when it names none.
pub(crate) fn kind(ask: &Ask) -> String {
    ask.class.as_ref().map_or_else(|| "ask".into(), crate::render::label)
}

/// Who asked: the asking agent, else the ledger's arc, else the ledger's name. An agent named
/// by a bare uuid is shown by its first 8 characters, as the plugin shows a session.
pub(crate) fn asker(lf: &LedgerFile, ask: &Ask) -> String {
    ask.origin
        .as_ref()
        .and_then(|o| o.agent_id.clone())
        .map(|id| if uuid(&id) { id[..8].to_string() } else { id })
        .or_else(|| lf.ledger.arc.clone())
        .unwrap_or_else(|| stem(&lf.file))
}

fn uuid(id: &str) -> bool {
    id.len() == 36
        && id.char_indices().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() })
}

fn stem(file: &str) -> String {
    file.trim_end_matches(".yaml").trim_end_matches(".yml").to_string()
}

/// One project's open asks, in queue order.
pub(crate) struct Group<'a> {
    pub name: String,
    pub rows: Vec<Entry<'a>>,
}

impl Group<'_> {
    pub fn oldest(&self) -> Option<jiff::Timestamp> {
        self.rows.iter().filter_map(Entry::asked).min()
    }
}

/// The open asks, grouped by project, in the order they hold work: groups with a blocking ask
/// first, then by their oldest ask; inside a group blocking first, then oldest. Asks with no
/// readable date sort after dated ones; ties go by ledger and handle, so the order is stable.
pub(crate) fn queue<'a>(cx: &'a Ctx, project: Option<&str>) -> Vec<Group<'a>> {
    let waiting: BTreeSet<(String, String)> =
        cx.unlanded().data.iter().flatten().map(|r| (r.ledger.clone(), r.id.clone())).collect();
    let mut rows: Vec<Entry<'a>> = cx
        .reads
        .asks
        .data
        .iter()
        .flatten()
        .flat_map(|lf| lf.ledger.asks.iter().map(move |ask| (lf, ask)))
        .filter(|(_, ask)| ask.state() == AskState::Open)
        .map(|(lf, ask)| Entry { lf, ask, waiting: waiting.contains(&(lf.file.clone(), ask.handle().to_string())) })
        .filter(|e| project.is_none_or(|p| e.project() == p))
        .collect();
    let undated = jiff::Timestamp::MAX;
    rows.sort_by(|a, b| {
        (!a.ask.blocking, a.asked().unwrap_or(undated), &a.lf.file, a.ask.handle()).cmp(&(
            !b.ask.blocking,
            b.asked().unwrap_or(undated),
            &b.lf.file,
            b.ask.handle(),
        ))
    });
    let mut groups: Vec<Group<'a>> = Vec::new();
    for row in rows {
        let name = row.project();
        match groups.iter_mut().find(|g| g.name == name) {
            Some(g) => g.rows.push(row),
            None => groups.push(Group { name, rows: vec![row] }),
        }
    }
    groups.sort_by(|a, b| {
        let key = |g: &Group| (!g.rows.iter().any(|r| r.ask.blocking), g.oldest().unwrap_or(undated), g.name.clone());
        key(a).cmp(&key(b))
    });
    groups
}

/// A ledger date as an instant: an RFC 3339 timestamp, one without seconds (`2026-10-01T10:00Z`),
/// or a bare date (midnight UTC). Anything else has no age.
pub(crate) fn instant(text: &str) -> Option<jiff::Timestamp> {
    let text = text.trim();
    if let Ok(t) = text.parse::<jiff::Timestamp>() {
        return Some(t);
    }
    let utc = |dt: jiff::civil::DateTime| dt.to_zoned(jiff::tz::TimeZone::UTC).ok().map(|z| z.timestamp());
    if let Some(bare) = text.strip_suffix('Z')
        && let Ok(dt) = bare.parse::<jiff::civil::DateTime>()
    {
        return utc(dt);
    }
    text.parse::<jiff::civil::Date>().ok().and_then(|d| utc(d.to_datetime(jiff::civil::Time::midnight())))
}

/// How long ago, in whole days: `today` under a day, else `N d`. Days, not minutes, so an age
/// changes the screen (and so its revision, which a poll swaps on) at most once a day.
/// A date after the render's clock has no age.
pub(crate) fn age(from: jiff::Timestamp, at: jiff::Timestamp) -> Option<String> {
    let secs = at.as_second() - from.as_second();
    match secs {
        ..0 => None,
        0..86_400 => Some("today".into()),
        _ => Some(format!("{} d", secs / 86_400)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_dates_read_in_every_corpus_form() {
        let t = |s: &str| instant(s).map(|t| t.to_string());
        assert_eq!(t("2026-10-24T03:40:00Z").as_deref(), Some("2026-10-24T03:40:00Z"));
        assert_eq!(t("2026-10-01T10:00Z").as_deref(), Some("2026-10-01T10:00:00Z"));
        assert_eq!(t("2026-10-13").as_deref(), Some("2026-10-13T00:00:00Z"));
        assert_eq!(t("2026-10-13T08:00:00-05:00").as_deref(), Some("2026-10-13T13:00:00Z"));
        assert_eq!(t("last week"), None);
    }

    #[test]
    fn a_uuid_agent_is_shown_short() {
        assert!(uuid("c4a181ad-74df-4338-ba12-af0a94e71efd"));
        assert!(!uuid("claude:2fb204ca"));
        assert!(!uuid("c4a181ad-74df-4338-ba12-af0a94e71efz"));
        assert!(!uuid("c4a181ad074df-4338-ba12-af0a94e71efd"));
    }

    #[test]
    fn ages_are_whole_days() {
        let at: jiff::Timestamp = "2026-10-23T12:00:00Z".parse().unwrap();
        let ago = |s: i64| age(jiff::Timestamp::from_second(at.as_second() - s).unwrap(), at);
        assert_eq!(ago(0).as_deref(), Some("today"));
        assert_eq!(ago(86_399).as_deref(), Some("today"));
        assert_eq!(ago(86_400).as_deref(), Some("1 d"));
        assert_eq!(ago(172_799).as_deref(), Some("1 d"));
        assert_eq!(ago(172_800).as_deref(), Some("2 d"));
        assert_eq!(ago(-1), None);
    }
}
