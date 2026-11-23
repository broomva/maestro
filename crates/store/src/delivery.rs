//! Read the answer writer's own receipts without inferring what an agent did afterwards.
use crate::Store;
use maestro_core::{Section, SourceError};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};

const TAIL_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Deserialize)]
pub struct Intent {
    pub verb: String,
    pub ledger: String,
    pub handle: String,
    pub hash: String,
    pub option: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Signed,
    Applying,
    Undone,
    Written,
    Failed,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Delivery {
    pub id: String,
    pub intent: Intent,
    pub stage: Stage,
    pub signed_at: String,
    pub commit: Option<String>,
    pub woke: Option<String>,
    pub landing: Option<String>,
    pub error: Option<String>,
}

impl Delivery {
    /// The writer records this exact phrase only after T3 accepts `thread send`.
    /// It proves delivery to a session, never that work started or finished.
    pub fn session_notified(&self) -> bool {
        self.stage == Stage::Written
            && self.woke.as_deref().and_then(|w| w.strip_prefix("told T3 thread ")).is_some_and(|thread| {
                !thread.is_empty() && !thread.chars().any(|c| c.is_whitespace() || c.is_control())
            })
    }
}

impl Store {
    /// Latest recorded state per receipt id in a bounded tail of the configured owner directory.
    /// Missing files and malformed rows stay visible as unavailable or partial reads.
    pub fn answer_deliveries(&self) -> Section<Vec<Delivery>> {
        let path = self.paths.owner_dir.join("receipts.jsonl");
        let mut source = format!("fs:{}", path.display());
        let read = || -> Result<(Vec<u8>, u64), String> {
            if !std::fs::metadata(&path).map_err(|e| e.to_string())?.is_file() {
                return Err("the receipt source is not a regular file".into());
            }
            let mut file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            let len = file.metadata().map_err(|e| e.to_string())?.len();
            let from = len.saturating_sub(TAIL_BYTES);
            file.seek(SeekFrom::Start(from)).map_err(|e| e.to_string())?;
            let mut bytes = Vec::new();
            file.take(len - from).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            if bytes.len() as u64 != len - from {
                return Err("the receipt file changed while being read".into());
            }
            Ok((bytes, from))
        };
        let (bytes, from) = match read() {
            Ok(read) => read,
            Err(e) => return Section::unavailable(source, e),
        };
        // The first suffix record may start before the byte bound. Discard it whole, never
        // parse its fragment or replace malformed UTF-8 inside a complete retained record.
        let skip = if from > 0 { bytes.iter().position(|b| *b == b'\n').map_or(bytes.len(), |n| n + 1) } else { 0 };
        if from > 0 {
            source.push_str(&format!(" from byte {} (256 KiB tail)", from + skip as u64));
        }
        let text = match std::str::from_utf8(&bytes[skip..]) {
            Ok(text) => text,
            Err(e) => return Section::unavailable(source, format!("receipt tail is not valid UTF-8: {e}")),
        };
        let mut errors = Vec::new();
        if from > 0 {
            errors.push(SourceError { at: source.clone(), reason: "Earlier receipt history was not read.".into() });
        }
        let mut latest = BTreeMap::<String, (usize, Delivery)>::new();
        for (n, line) in text.lines().enumerate().filter(|(_, line)| !line.trim().is_empty()) {
            let parsed = serde_json::from_str::<Delivery>(line).and_then(|receipt| {
                let valid_id = receipt.id.len() == 16
                    && receipt.id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
                if !valid_id
                    || receipt.intent.verb != "ask.answer"
                    || receipt.signed_at.parse::<jiff::Timestamp>().is_err()
                {
                    return Err(<serde_json::Error as serde::de::Error>::custom(
                        "invalid receipt id, verb or signed_at timestamp",
                    ));
                }
                Ok(receipt)
            });
            match parsed {
                Ok(receipt) => {
                    latest.insert(receipt.id.clone(), (n, receipt));
                }
                Err(e) => {
                    // A later damaged state for a known id invalidates its older valid state.
                    // When its identity cannot be read, the whole section remains partial.
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(line)
                        && let Some(id) = value.get("id").and_then(|v| v.as_str())
                    {
                        latest.remove(id);
                    }
                    errors.push(SourceError { at: format!("{source}:{}", n + 1), reason: e.to_string() });
                }
            }
        }
        let mut rows: Vec<_> = latest.into_values().collect();
        rows.sort_by_key(|(line, row)| {
            (std::cmp::Reverse(row.signed_at.parse::<jiff::Timestamp>().ok()), std::cmp::Reverse(*line))
        });
        Section::partial(source, rows.into_iter().map(|(_, row)| row).collect(), errors)
    }
}
