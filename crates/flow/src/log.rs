//! The event log: one JSON object per line, numbered from 1 and hash-chained.
//!
//! Line `n` carries `seq: n` and `prev_event`, the `sha256:<hex>` of line `n - 1`'s exact bytes
//! (without its newline); line 1 carries `prev_event: null`. A log verifies when every line
//! parses, its `seq` follows the one before and its `prev_event` matches. A last line with no
//! newline (all a crash mid-write can leave) is a torn tail: the log reads as unread and appends
//! wait for an operator truncation. Any whole line that fails is a broken chain.

use crate::error::{FlowError, Result};
use crate::ids::InstanceId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// The largest event line, newline included.
pub const MAX_LINE: u64 = 1 << 20;

/// The largest log an instance may have. A larger file is refused before it is read.
pub const MAX_LOG: u64 = 64 << 20;

/// The most events an instance's log may hold. Replay's work grows with the events, so this
/// bounds a read: room for 100 nodes over 10 iterations with 20 writes each (claims, heartbeats,
/// refusals, receipts), or a 512-node flow whose every node is written about forty times.
pub const MAX_EVENTS: u64 = 20_000;

/// Refuses a line over [`MAX_LINE`], one that would take a log of `log_len` bytes over
/// [`MAX_LOG`], or one more event on a log of `events` that holds [`MAX_EVENTS`] already.
pub(crate) fn fits(line_len: u64, log_len: u64, events: u64) -> Result<()> {
    if events >= MAX_EVENTS {
        return Err(FlowError::TooManyEvents { limit: MAX_EVENTS });
    }
    if line_len > MAX_LINE {
        return Err(FlowError::TooLarge { what: "event line", size: line_len, limit: MAX_LINE });
    }
    let total = log_len.saturating_add(line_len);
    if total > MAX_LOG {
        return Err(FlowError::TooLarge { what: "log", size: total, limit: MAX_LOG });
    }
    Ok(())
}

/// One event as it is stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub prev_event: Option<String>,
    /// When the writer appended it (RFC 3339, UTC).
    pub at: String,
    pub kind: String,
    pub body: serde_json::Value,
}

/// An event to append: the storage layer adds `seq`, `prev_event` and `at`.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    pub kind: String,
    pub body: serde_json::Value,
}

impl NewEvent {
    pub fn new(kind: impl Into<String>, body: serde_json::Value) -> Self {
        Self { kind: kind.into(), body }
    }
}

/// `sha256:<hex>` of a line's bytes.
pub fn line_hash(line: &[u8]) -> String {
    let digest = Sha256::digest(line);
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// A log that verified to its end.
#[derive(Debug, Clone, PartialEq)]
pub struct Log {
    pub instance: InstanceId,
    pub events: Vec<Event>,
    /// The hash of the last line, the next event's `prev_event`.
    pub last_hash: Option<String>,
}

impl Log {
    /// The last event's sequence number; 0 for an empty log.
    pub fn seq(&self) -> u64 {
        self.events.len() as u64
    }

    pub fn summary(&self) -> InstanceSummary {
        let mut counts = BTreeMap::new();
        for e in &self.events {
            *counts.entry(e.kind.clone()).or_insert(0u64) += 1;
        }
        InstanceSummary {
            instance: self.instance.as_str().to_string(),
            seq: self.seq(),
            last_hash: self.last_hash.clone(),
            counts,
        }
    }
}

/// What the projection holds for one instance, derived from its log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceSummary {
    pub instance: String,
    pub seq: u64,
    pub last_hash: Option<String>,
    /// Events by kind.
    pub counts: BTreeMap<String, u64>,
}

/// How far a log verified, when it did not verify to its end.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Prefix {
    pub(crate) seq: u64,
    /// Bytes of the verified lines, newlines included: where an operator truncation cuts.
    pub(crate) len: u64,
}

/// Verifies a whole log. A torn tail is [`FlowError::TornTail`]; earlier damage is [`FlowError::ChainBroken`].
pub fn verify(instance: &InstanceId, bytes: &[u8]) -> Result<Log> {
    verify_prefix(instance, bytes).map_err(|(e, _)| e)
}

/// As [`verify`], with the verified prefix on failure.
///
/// Every line is written whole, newline included, in one write, so a crash can only leave a last
/// line without its newline: that alone is a torn tail. Any whole line that fails, the last one
/// included, is a broken chain, which an operator truncation never "repairs".
pub(crate) fn verify_prefix(instance: &InstanceId, bytes: &[u8]) -> Result<Log, (FlowError, Prefix)> {
    let mut events: Vec<Event> = Vec::new();
    let mut last_hash: Option<String> = None;
    let mut offset = 0usize;
    while offset < bytes.len() {
        let rest = &bytes[offset..];
        let prefix = Prefix { seq: events.len() as u64, len: offset as u64 };
        let Some(end) = rest.iter().position(|b| *b == b'\n') else {
            let detail = "the last line has no newline".to_string();
            return Err((
                FlowError::TornTail { instance: instance.to_string(), verified_seq: prefix.seq, detail },
                prefix,
            ));
        };
        let line = &rest[..end];
        let size = end as u64 + 1;
        if size > MAX_LINE {
            return Err((FlowError::TooLarge { what: "event line", size, limit: MAX_LINE }, prefix));
        }
        if prefix.seq >= MAX_EVENTS {
            return Err((FlowError::TooManyEvents { limit: MAX_EVENTS }, prefix));
        }
        let event = check_line(line, prefix.seq + 1, last_hash.as_deref()).map_err(|detail| {
            (FlowError::ChainBroken { instance: instance.to_string(), line: prefix.seq + 1, detail }, prefix.clone())
        })?;
        last_hash = Some(line_hash(line));
        events.push(event);
        offset += end + 1;
    }
    Ok(Log { instance: instance.clone(), events, last_hash })
}

fn check_line(line: &[u8], want_seq: u64, want_prev: Option<&str>) -> Result<Event, String> {
    let event: Event = serde_json::from_slice(line).map_err(|e| format!("not an event: {e}"))?;
    if event.seq != want_seq {
        return Err(format!("seq {} where {want_seq} was due", event.seq));
    }
    if event.prev_event.as_deref() != want_prev {
        return Err("prev_event does not match the line before".to_string());
    }
    Ok(event)
}

/// The line to append after `log`, newline included.
pub(crate) fn next_line(log: &Log, event: &NewEvent) -> Result<(Event, Vec<u8>)> {
    let stored = Event {
        seq: log.seq() + 1,
        prev_event: log.last_hash.clone(),
        at: jiff::Timestamp::now().to_string(),
        kind: event.kind.clone(),
        body: event.body.clone(),
    };
    let mut line = serde_json::to_vec(&stored).map_err(|e| FlowError::Invalid {
        what: "Event body",
        value: e.to_string(),
        reason: "it does not serialize",
    })?;
    line.push(b'\n');
    Ok((stored, line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_and_a_log_fit_up_to_their_bounds_and_not_a_byte_more() {
        let code = |r: Result<()>| r.map_or_else(|e| e.code().to_string(), |()| "ok".to_string());
        assert_eq!(code(fits(MAX_LINE, 0, 0)), "ok");
        assert_eq!(code(fits(MAX_LINE + 1, 0, 0)), "too_large");
        assert_eq!(code(fits(10, MAX_LOG - 10, 0)), "ok");
        assert_eq!(code(fits(10, MAX_LOG - 9, 0)), "too_large");
        assert_eq!(code(fits(10, 0, MAX_EVENTS - 1)), "ok");
        assert_eq!(code(fits(10, 0, MAX_EVENTS)), "too_many_events");
    }
}
