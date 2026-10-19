//! A read result that keeps "could not read" distinct from "nothing there".
//!
//! An unread ask ledger is not a resolved ask. Every collection Maestro reports
//! carries the source it came from and every per-file error, so a consumer can
//! tell an empty healthy list from a blind one.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceError {
    /// File, ref or command that failed.
    pub at: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section<T> {
    /// Where the data was read from, e.g. `git:origin/main:.control/asks`.
    pub source: String,
    /// `None` when the source itself could not be read.
    pub data: Option<T>,
    /// Per-item failures; `data` may still hold the items that did parse.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<SourceError>,
}

impl<T> Section<T> {
    pub fn ok(source: impl Into<String>, data: T) -> Self {
        Self { source: source.into(), data: Some(data), errors: Vec::new() }
    }

    pub fn partial(source: impl Into<String>, data: T, errors: Vec<SourceError>) -> Self {
        Self { source: source.into(), data: Some(data), errors }
    }

    pub fn unavailable(source: impl Into<String>, reason: impl Into<String>) -> Self {
        let source = source.into();
        Self { errors: vec![SourceError { at: source.clone(), reason: reason.into() }], source, data: None }
    }

    /// True when anything failed to read. A blind section must not be shown as healthy.
    pub fn is_degraded(&self) -> bool {
        self.data.is_none() || !self.errors.is_empty()
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Section<U> {
        Section { source: self.source, data: self.data.map(f), errors: self.errors }
    }

    pub fn as_ref(&self) -> Section<&T> {
        Section { source: self.source.clone(), data: self.data.as_ref(), errors: self.errors.clone() }
    }
}
