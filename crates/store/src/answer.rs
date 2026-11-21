//! The owner's answer, written into one ask ledger's text (BRO-4042).
//!
//! The owner chose on 2026-11-20 that Maestro writes the answer itself, once the owner signs it
//! with a passkey, and that the answer takes effect on that signature. This module is the pure
//! half: given a ledger's text, it returns the text with one ask answered, or refuses.
//!
//! It never re-serialises the document. It inserts the answer keys as new lines at the end of
//! the one ask's block, in the ask's own indentation, each value a JSON string (a YAML
//! double-quoted scalar inside the subset [`crate::yaml`] reads). Then it reads the whole result
//! again and refuses unless the result is exactly what was meant:
//! - the ask list has the same length,
//! - every other ask's entry hash is unchanged,
//! - the answered ask is now answered with the chosen option and note.
//!
//! An ask is answered only while open, and only when its entry hash is the one the owner saw
//! and signed: an ask that changed since is refused, so the owner sees the new version.

use maestro_core::ask::{Ask, AskLedger, AskState};

/// How the owner's answer names its ask: by uid, else by an id among asks without a uid
/// (the handle the plugin and every Maestro screen use).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Select<'a> {
    pub handle: &'a str,
}

/// What the owner answered, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer<'a> {
    /// The option's id.
    pub option: &'a str,
    /// The owner's words beside the option, if any.
    pub note: Option<&'a str>,
    /// Who answered: `owner` with how it was proven.
    pub by: &'a str,
    /// RFC 3339, UTC.
    pub at: &'a str,
}

/// Why an answer was not written. Every variant leaves the ledger as it was.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("the ledger does not read: {0}")]
    Unreadable(String),
    #[error("no ask {0} in this ledger")]
    NoSuchAsk(String),
    #[error("{0} names more than one ask in this ledger")]
    Ambiguous(String),
    #[error("the ask changed since you opened it")]
    Changed,
    #[error("the ask is no longer open")]
    NotOpen,
    #[error("the ask has no option {0}")]
    NoSuchOption(String),
    #[error("the ask is written in a form Maestro does not edit: {0}")]
    Unsupported(String),
    #[error("the edit did not read back as meant: {0}")]
    Unverified(String),
}

/// The one ask `select` names, as the store reads it: a uid, else an id among asks without one.
pub fn find<'l>(ledger: &'l AskLedger, select: &Select) -> Result<&'l Ask, Refusal> {
    let by_uid: Vec<&Ask> = ledger.asks.iter().filter(|a| a.uid.as_deref() == Some(select.handle)).collect();
    let found = if by_uid.is_empty() {
        ledger.asks.iter().filter(|a| a.uid.is_none() && a.id == select.handle).collect()
    } else {
        by_uid
    };
    match found.as_slice() {
        [a] => Ok(a),
        [] => Err(Refusal::NoSuchAsk(select.handle.to_string())),
        _ => Err(Refusal::Ambiguous(select.handle.to_string())),
    }
}

/// `source` with the ask `select` names answered, if it is open, its entry hash is `hash`, and
/// `answer.option` is one of its options. `rel` names the ledger in errors.
pub fn answered(source: &str, rel: &str, select: &Select, hash: &str, answer: &Answer) -> Result<String, Refusal> {
    let before = crate::ask_ledger_of(rel, source).map_err(Refusal::Unreadable)?;
    let ask = find(&before, select)?;
    if ask.hash() != hash {
        return Err(Refusal::Changed);
    }
    if ask.state() != AskState::Open {
        return Err(Refusal::NotOpen);
    }
    let option = ask
        .options
        .iter()
        .find(|o| o.id == answer.option)
        .ok_or_else(|| Refusal::NoSuchOption(answer.option.to_string()))?;
    let note = answer.note.map(str::trim).filter(|n| !n.is_empty());
    // `answer` keeps the full text, so a reader that knows only `answer` loses nothing
    // (the validator of record's rule for `answer_note`, BRO-3146).
    let text = match note {
        Some(n) => format!("{}. {n}", option.label),
        None => option.label.clone(),
    };
    let mut keys: Vec<(&str, &str)> = vec![("answer", &text), ("answer_option", &option.id)];
    if let Some(n) = note {
        keys.push(("answer_note", n));
    }
    keys.push(("answered_at", answer.at));
    keys.push(("answered_by", answer.by));

    let eol = if source.contains("\r\n") { "\r\n" } else { "\n" };
    let items = items(source)?;
    // The item that holds the ask: the one whose edit makes exactly that ask answered. An item
    // that does not mention the handle cannot hold it, so only those that do are tried.
    let mut last = None;
    for item in items.iter().filter(|i| source[i.start..i.end].contains(select.handle)) {
        let edited = match edit(source, item, &keys, eol) {
            Ok(e) => e,
            Err(e) => {
                // A block that holds the ask and refuses the edit says why; it outranks the
                // read-back of a block that merely mentions the handle.
                last = Some(e);
                continue;
            }
        };
        match verify(&edited, rel, &before, select, &option.id, note) {
            Ok(()) => return Ok(edited),
            Err(e) => {
                if !matches!(last, Some(Refusal::Unsupported(_))) {
                    last = Some(e);
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| Refusal::Unsupported("no block holds the ask".into())))
}

/// `source` with `keys` written into `item`: a key the item already holds empty (`null`, `~`,
/// `""`, `''` or nothing) has that line replaced; every other key is a new line after the item's
/// last line. A key the item holds with a value refuses.
fn edit(source: &str, item: &Item, keys: &[(&str, &str)], eol: &str) -> Result<String, Refusal> {
    let pad = " ".repeat(item.indent);
    let first = line_start(source, item.start);
    let mut out = String::with_capacity(source.len() + 256);
    out.push_str(&source[..first]);
    let mut written: Vec<&str> = Vec::new();
    let mut at = first;
    let mut appended = false;
    let lines: Vec<&str> = source[first..item.end].split_inclusive('\n').collect();
    for (i, line) in lines.iter().enumerate() {
        if at == item.insert && !appended {
            appended = true;
            for (k, v) in keys.iter().filter(|(k, _)| !written.contains(k)) {
                out.push_str(&format!("{pad}{k}: {}{eol}", json(v)));
            }
        }
        // The item's own keys sit at its indentation; the first line also carries the `- `.
        let body =
            if i == 0 { &source[item.start..at + line.len()] } else { line.strip_prefix(pad.as_str()).unwrap_or("") };
        let own = i == 0 || (line.starts_with(pad.as_str()) && !body.starts_with(' '));
        let hit =
            own.then(|| keys.iter().find(|(k, _)| body.strip_prefix(k).is_some_and(|r| r.starts_with(':')))).flatten();
        match hit {
            Some((k, v)) => {
                let value = body[k.len() + 1..].trim();
                let nested = lines
                    .get(i + 1)
                    .is_some_and(|n| n.starts_with(&format!("{pad} ")) || n.starts_with(&format!("{pad}-")));
                if !matches!(value, "" | "null" | "~" | "\"\"" | "''") || (value.is_empty() && nested) {
                    return Err(Refusal::Unsupported(format!("the ask already holds `{k}`")));
                }
                let lead = &line[..line.len() - body.len().min(line.len())];
                out.push_str(&format!("{lead}{k}: {}{eol}", json(v)));
                written.push(k);
            }
            None => out.push_str(line),
        }
        at += line.len();
    }
    if !appended {
        for (k, v) in keys.iter().filter(|(k, _)| !written.contains(k)) {
            out.push_str(&format!("{pad}{k}: {}{eol}", json(v)));
        }
    }
    out.push_str(&source[item.end..]);
    Ok(out)
}

/// A JSON string, which is a YAML double-quoted scalar with the same value.
fn json(v: &str) -> String {
    serde_json::Value::from(v).to_string()
}

/// The edited text read back: same asks, every other hash unchanged, the one ask answered as meant.
fn verify(
    edited: &str,
    rel: &str,
    before: &AskLedger,
    select: &Select,
    option: &str,
    note: Option<&str>,
) -> Result<(), Refusal> {
    let after = crate::ask_ledger_of(rel, edited).map_err(Refusal::Unverified)?;
    if after.asks.len() != before.asks.len() {
        return Err(Refusal::Unverified("the number of asks changed".into()));
    }
    let target = find(before, select)?;
    let mut answered = 0;
    for (b, a) in before.asks.iter().zip(&after.asks) {
        if std::ptr::eq(b, target) {
            let ok = a.id == b.id
                && a.uid == b.uid
                && a.state() == AskState::Answered
                && a.answer_option.as_deref() == Some(option)
                && a.answer_note.as_deref() == note;
            if !ok {
                return Err(Refusal::Unverified(format!("{} did not read back answered", b.handle())));
            }
            answered += 1;
        } else if a.hash() != b.hash() {
            return Err(Refusal::Unverified(format!("{} changed", b.handle())));
        }
    }
    if answered == 1 { Ok(()) } else { Err(Refusal::Unverified("the ask was not found again".into())) }
}

/// One block-mapping item of a top-level `asks` or `open_asks` sequence: its byte span, the
/// column its keys sit at, and where new keys go (after its last line that is not blank or a
/// comment).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Item {
    start: usize,
    end: usize,
    indent: usize,
    insert: usize,
}

fn items(source: &str) -> Result<Vec<Item>, Refusal> {
    use yaml_rust2::parser::{Event, Parser};
    let mut parser = Parser::new_from_str(source);
    // Open collections: (is a mapping, its next node is a key).
    let mut stack: Vec<(bool, bool)> = Vec::new();
    // The top-level key whose value is being read.
    let mut top_key: Option<String> = None;
    // The depth of an open `asks` or `open_asks` sequence.
    let mut in_asks: Option<usize> = None;
    // Each item: the byte index of its first key, and that key's column; flow items are None.
    let mut starts: Vec<Option<(usize, usize)>> = Vec::new();
    let mut ends: Vec<usize> = Vec::new();
    // An item's mapping has started and its first key is not read yet: the key marks the item.
    let mut pending = false;
    loop {
        let (event, mark) = parser.next_token().map_err(|e| Refusal::Unreadable(e.to_string()))?;
        let depth = stack.len();
        match event {
            Event::StreamEnd => break,
            Event::Scalar(v, ..) => {
                if std::mem::take(&mut pending) {
                    let at = char_to_byte(source, mark.index());
                    // A flow mapping's first key follows its `{`: such an item is not edited.
                    let flow = source[..at].trim_end_matches([' ']).ends_with('{');
                    if let Some(last) = starts.last_mut() {
                        *last = (!flow).then_some((at, mark.col()));
                    }
                }
                if let Some(top) = stack.last_mut().filter(|t| t.0) {
                    if top.1 && depth == 1 {
                        top_key = Some(v);
                    }
                    top.1 = !top.1;
                }
            }
            Event::Alias(..) => {
                if let Some(top) = stack.last_mut().filter(|t| t.0) {
                    top.1 = !top.1;
                }
            }
            Event::MappingStart(..) | Event::SequenceStart(..) => {
                let is_map = matches!(event, Event::MappingStart(..));
                if in_asks == Some(depth) {
                    starts.push(None);
                    pending = is_map;
                }
                if !is_map && depth == 1 && matches!(top_key.as_deref(), Some("asks" | "open_asks")) {
                    in_asks = Some(depth + 1);
                }
                stack.push((is_map, true));
            }
            Event::MappingEnd | Event::SequenceEnd => {
                stack.pop();
                if in_asks == Some(stack.len() + 1) && matches!(event, Event::SequenceEnd) {
                    in_asks = None;
                    ends.push(starts.len());
                    ends.push(char_to_byte(source, mark.index()));
                }
                if let Some(top) = stack.last_mut().filter(|t| t.0) {
                    top.1 = !top.1;
                }
            }
            _ => {}
        }
    }
    // Where each sequence ends, as (items before its end, byte index).
    let seq_ends: Vec<(usize, usize)> = ends.chunks(2).map(|c| (c[0], c[1])).collect();
    let mut out = Vec::new();
    for (i, start) in starts.iter().enumerate() {
        let Some((start, col)) = *start else { continue };
        // An item ends where the next item of its sequence begins, or where the sequence ends.
        let (count, seq_end) = seq_ends.iter().copied().find(|(n, _)| *n > i).unwrap_or((starts.len(), source.len()));
        let end = match starts.get(i + 1).filter(|_| i + 1 < count) {
            Some(Some((next, _))) => line_start(source, *next),
            _ => line_start(source, seq_end).max(start),
        };
        let end = if end <= start { source.len() } else { end };
        out.push(Item { start, end, indent: col, insert: after_last_content(source, start, end) });
    }
    Ok(out)
}

/// yaml-rust2 marks positions in characters; the text is sliced in bytes.
fn char_to_byte(source: &str, chars: usize) -> usize {
    source.char_indices().nth(chars).map_or(source.len(), |(b, _)| b)
}

fn line_start(source: &str, at: usize) -> usize {
    source[..at].rfind('\n').map_or(0, |i| i + 1)
}

/// The byte just past the last line in `start..end` that holds more than blanks or a comment.
fn after_last_content(source: &str, start: usize, end: usize) -> usize {
    let mut at = line_start(source, start);
    let mut insert = at;
    for line in source[at..end].split_inclusive('\n') {
        let t = line.trim();
        at += line.len();
        if !t.is_empty() && !t.starts_with('#') {
            insert = at;
        }
    }
    // A last line with no line break of its own (the item ends the file) gets one first.
    insert
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEDGER: &str = "arc: t\nasks:\n  - id: a\n    ask: \"First?\"\n    class: decision\n    options:\n      - id: yes\n        label: \"Yes\"\n      - id: no\n        label: \"No\"\n    details: |\n      two\n      lines\n\n  # a comment before b\n  - id: b\n    ask: \"Second?\"\n    options:\n      - {id: go, label: Go}\n";

    fn hash_of(text: &str, handle: &str) -> String {
        let l = crate::ask_ledger_of("t.yaml", text).unwrap();
        find(&l, &Select { handle }).unwrap().hash()
    }

    fn answer<'a>(option: &'a str, note: Option<&'a str>) -> Answer<'a> {
        Answer { option, note, by: "owner (passkey)", at: "2026-11-20T23:00:00Z" }
    }

    #[test]
    fn writes_only_the_answer_keys_into_the_one_ask() {
        let h = hash_of(LEDGER, "a");
        let out =
            answered(LEDGER, "t.yaml", &Select { handle: "a" }, &h, &answer("no", Some("after the soak"))).unwrap();
        let l = crate::ask_ledger_of("t.yaml", &out).unwrap();
        let a = find(&l, &Select { handle: "a" }).unwrap();
        assert_eq!(a.state(), AskState::Answered);
        assert_eq!(a.answer_option.as_deref(), Some("no"));
        assert_eq!(a.answer_note.as_deref(), Some("after the soak"));
        assert_eq!(a.answer.as_deref(), Some("No. after the soak"));
        assert_eq!(find(&l, &Select { handle: "b" }).unwrap().hash(), hash_of(LEDGER, "b"));
        // Every byte of the original survives, in order: only lines were added.
        let mut rest = out.as_str();
        for line in LEDGER.split_inclusive('\n') {
            let at = rest.find(line).expect(line);
            rest = &rest[at + line.len()..];
        }
        assert!(out.contains("    answer_option: \"no\"\n    answer_note: \"after the soak\"\n"));
        // The block scalar keeps its value.
        assert!(out.contains("      lines\n    answer:"));
    }

    #[test]
    fn answers_the_last_ask_in_the_file() {
        let h = hash_of(LEDGER, "b");
        let out = answered(LEDGER, "t.yaml", &Select { handle: "b" }, &h, &answer("go", None)).unwrap();
        assert!(out.ends_with("    answered_by: \"owner (passkey)\"\n"), "{out}");
        assert!(!out.contains("answer_note"));
    }

    #[test]
    fn refuses_a_changed_or_closed_ask_or_an_unknown_option() {
        let h = hash_of(LEDGER, "a");
        let s = Select { handle: "a" };
        assert_eq!(answered(LEDGER, "t.yaml", &s, "stale", &answer("no", None)), Err(Refusal::Changed));
        assert_eq!(
            answered(LEDGER, "t.yaml", &s, &h, &answer("maybe", None)),
            Err(Refusal::NoSuchOption("maybe".into()))
        );
        let once = answered(LEDGER, "t.yaml", &s, &h, &answer("no", None)).unwrap();
        let h2 = hash_of(&once, "a");
        assert_eq!(answered(&once, "t.yaml", &s, &h2, &answer("yes", None)), Err(Refusal::NotOpen));
        assert_eq!(
            answered(LEDGER, "t.yaml", &Select { handle: "zz" }, &h, &answer("no", None)),
            Err(Refusal::NoSuchAsk("zz".into()))
        );
    }

    #[test]
    fn a_note_with_quotes_and_unicode_reads_back_exactly() {
        let h = hash_of(LEDGER, "a");
        let note = "wake it \"after\" the Cuenca soak: ñ, 😀\nsecond line";
        let out = answered(LEDGER, "t.yaml", &Select { handle: "a" }, &h, &answer("yes", Some(note))).unwrap();
        let l = crate::ask_ledger_of("t.yaml", &out).unwrap();
        assert_eq!(find(&l, &Select { handle: "a" }).unwrap().answer_note.as_deref(), Some(note));
    }

    #[test]
    fn an_empty_answer_key_is_filled_in_place_and_a_set_one_refuses() {
        let with_null =
            LEDGER.replace("    class: decision\n", "    class: decision\n    answered_at: null\n    answer_note: ~\n");
        let h = hash_of(&with_null, "a");
        let out = answered(&with_null, "t.yaml", &Select { handle: "a" }, &h, &answer("yes", Some("n"))).unwrap();
        assert_eq!(out.matches("answered_at:").count(), 1, "{out}");
        assert!(
            out.contains("    class: decision\n    answered_at: \"2026-11-20T23:00:00Z\"\n    answer_note: \"n\"\n")
        );
        let with_by = LEDGER.replace("    class: decision\n", "    class: decision\n    answered_by: someone\n");
        let h = hash_of(&with_by, "a");
        let r = answered(&with_by, "t.yaml", &Select { handle: "a" }, &h, &answer("yes", None));
        assert!(matches!(r, Err(Refusal::Unsupported(_))), "{r:?}");
    }

    #[test]
    fn the_read_back_refuses_an_edit_that_touches_another_ask() {
        let before = crate::ask_ledger_of("t.yaml", LEDGER).unwrap();
        // The right ask answered, and another ask changed too.
        let h = hash_of(LEDGER, "a");
        let good = answered(LEDGER, "t.yaml", &Select { handle: "a" }, &h, &answer("no", None)).unwrap();
        let bad = good.replace("ask: \"Second?\"", "ask: \"Second, changed?\"");
        assert!(verify(&good, "t.yaml", &before, &Select { handle: "a" }, "no", None).is_ok());
        assert!(matches!(
            verify(&bad, "t.yaml", &before, &Select { handle: "a" }, "no", None),
            Err(Refusal::Unverified(_))
        ));
        // Answered with another option than meant.
        assert!(verify(&good, "t.yaml", &before, &Select { handle: "a" }, "yes", None).is_err());
    }

    #[test]
    fn crlf_ledgers_stay_crlf() {
        let crlf = LEDGER.replace('\n', "\r\n");
        let h = hash_of(&crlf, "b");
        let out = answered(&crlf, "t.yaml", &Select { handle: "b" }, &h, &answer("go", None)).unwrap();
        assert!(!out.replace("\r\n", "").contains('\n'));
    }
}

#[cfg(test)]
mod real_ledgers {
    /// `MAESTRO_ANSWER_DRY_RUN=<asks dir>`: answer every open ask of every ledger there in memory.
    #[test]
    fn every_open_ask_in_a_real_folder_takes_an_answer() {
        let Ok(dir) = std::env::var("MAESTRO_ANSWER_DRY_RUN") else { return };
        let (mut ok, mut refused) = (0, Vec::new());
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if !p.extension().is_some_and(|x| x == "yaml" || x == "yml") {
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap();
            let l = match crate::ask_ledger_of("x", &text) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("unread {}: {e}", p.display());
                    continue;
                }
            };
            eprintln!(
                "{} open {}",
                p.display(),
                l.asks.iter().filter(|a| a.state() == maestro_core::ask::AskState::Open).count()
            );
            for a in l.asks.iter().filter(|a| a.state() == maestro_core::ask::AskState::Open && !a.options.is_empty()) {
                let r = super::answered(
                    &text,
                    "x",
                    &super::Select { handle: a.handle() },
                    &a.hash(),
                    &super::Answer {
                        option: &a.options[0].id,
                        note: Some("n"),
                        by: "owner",
                        at: "2026-11-21T00:00:00Z",
                    },
                );
                match r {
                    Ok(_) => ok += 1,
                    Err(e) => refused.push(format!("{}#{}: {e}", p.display(), a.handle())),
                }
            }
        }
        eprintln!("answered {ok}; refused {}:\n{}", refused.len(), refused.join("\n"));
        assert!(refused.is_empty());
    }
}
