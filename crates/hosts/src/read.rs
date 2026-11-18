//! One rule for every read that discovers a registry or enumerates its entries (BRO-3846): a path
//! that is not there (`NotFound`, or a parent that is a file, `NotADirectory`) is [`Read::Absent`]; one that reads is [`Read::Present`]; any other error
//! (permissions, an I/O error, a directory entry that does not read) is [`Read::Unreadable`],
//! never absence. Identity treats `Unreadable` as a registry it cannot read (fail closed); the
//! owner's permission listings report it as an unread source (fail open, with a signal).
//!
//! Registry discovery and enumeration in this crate go through these functions only, never a raw
//! `is_dir()`, `is_file()`, `exists()` or a `read_dir` whose entry errors are dropped, which turn
//! an error into "not there". `crates/hosts/tests/read_guard.rs` holds them to it.

use std::fs::Metadata;
use std::io;
use std::path::{Path, PathBuf};

/// What a read found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read<T> {
    /// Not there: `ErrorKind::NotFound`, or `NotADirectory` (a parent of the path is a file).
    Absent,
    Present(T),
    /// Any other error, with why: never absence.
    Unreadable(String),
}

impl<T> Read<T> {
    /// `NotFound` and `NotADirectory` are absent, `Ok` present, any other error unreadable.
    pub fn from_io(result: io::Result<T>, what: &Path) -> Self {
        match result {
            Ok(v) => Read::Present(v),
            Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => Read::Absent,
            Err(e) => Read::Unreadable(format!("{} does not read: {e}", what.display())),
        }
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Read<U> {
        match self {
            Read::Absent => Read::Absent,
            Read::Present(v) => Read::Present(f(v)),
            Read::Unreadable(why) => Read::Unreadable(why),
        }
    }
}

/// The metadata of `path` (following links).
pub fn metadata(path: &Path) -> Read<Metadata> {
    Read::from_io(std::fs::metadata(path), path)
}

/// Whether `path` is a directory: absent when it is not there, `Present(false)` when it is
/// something else, unreadable when it cannot be told.
pub fn is_dir(path: &Path) -> Read<bool> {
    metadata(path).map(|m| m.is_dir())
}

/// Whether `path` is a regular file, by the same rule.
pub fn is_file(path: &Path) -> Read<bool> {
    metadata(path).map(|m| m.is_file())
}

/// The text of `path`.
pub fn read_text(path: &Path) -> Read<String> {
    Read::from_io(std::fs::read_to_string(path), path)
}

/// The entries of directory `path`, each its path, or why it did not read: a directory entry
/// whose read fails is an unreadable entry, never dropped. Sorted, so a listing is stable.
pub fn dir_entries(path: &Path) -> Read<Vec<Read<PathBuf>>> {
    match std::fs::read_dir(path) {
        Ok(entries) => Read::Present(entries_of(entries.map(|e| e.map(|e| e.path())), path)),
        Err(e) => Read::from_io(Err(e), path),
    }
}

/// [`dir_entries`]' rule over any iterator of entry results (tests inject failing entries).
pub fn entries_of(entries: impl Iterator<Item = io::Result<PathBuf>>, dir: &Path) -> Vec<Read<PathBuf>> {
    let mut out: Vec<Read<PathBuf>> = entries
        .map(|e| match e {
            Ok(p) => Read::Present(p),
            Err(e) => Read::Unreadable(format!("an entry of {} does not read: {e}", dir.display())),
        })
        .collect();
    out.sort_by(|a, b| match (a, b) {
        (Read::Present(x), Read::Present(y)) => x.cmp(y),
        (Read::Present(_), _) => std::cmp::Ordering::Less,
        (_, Read::Present(_)) => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_is_absent_and_any_other_error_is_unreadable() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(read_text(&tmp.path().join("none")), Read::Absent);
        assert!(matches!(is_dir(tmp.path()), Read::Present(true)));
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        assert!(matches!(Read::<()>::from_io(Err(denied), tmp.path()), Read::Unreadable(_)));
        let entries = entries_of(
            [Ok(PathBuf::from("/b")), Err(io::Error::other("bad sector")), Ok(PathBuf::from("/a"))].into_iter(),
            tmp.path(),
        );
        assert_eq!(entries[..2], [Read::Present(PathBuf::from("/a")), Read::Present(PathBuf::from("/b"))]);
        assert!(matches!(&entries[2], Read::Unreadable(why) if why.contains("bad sector")));
    }
}
