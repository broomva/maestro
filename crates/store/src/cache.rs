//! Bounded in-process caches for immutable git trees and stat-keyed filesystem reads.
//!
//! Git tree entries are keyed by `(repo_root, source_rev, commit_sha)` so a moving ref or a
//! different ref spelling never collides, and immutable commit contents are never re-parsed.
//! Filesystem entries are keyed by full metadata fingerprints (device, inode, size, mtime, ctime),
//! so any edit invalidates the entry on the next read. Transient errors and partial reads are
//! never cached so a momentary failure never pins in a long-lived daemon.

use crate::{LedgerFile, TickTail};
use maestro_core::Section;
use maestro_core::initiative::Initiative;
use maestro_core::loops::LoopSpec;
use maestro_core::plan::PlanGraph;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

const MAX_ENTRIES: usize = 16;

struct RingCache<K, V> {
    entries: Mutex<VecDeque<(K, V)>>,
}

impl<K: PartialEq, V: Clone> RingCache<K, V> {
    fn new() -> Self {
        Self { entries: Mutex::new(VecDeque::with_capacity(MAX_ENTRIES)) }
    }

    fn get_or_insert_if(&self, key: K, compute: impl FnOnce() -> V, should_cache: impl FnOnce(&V) -> bool) -> V {
        if let Ok(mut guard) = self.entries.lock()
            && let Some(pos) = guard.iter().position(|(k, _)| k == &key)
            && let Some(entry) = guard.remove(pos)
        {
            let val = entry.1.clone();
            guard.push_back(entry);
            return val;
        }
        let val = compute();
        if should_cache(&val)
            && let Ok(mut guard) = self.entries.lock()
        {
            if let Some(pos) = guard.iter().position(|(k, _)| k == &key) {
                guard.remove(pos);
            } else if guard.len() >= MAX_ENTRIES {
                guard.pop_front();
            }
            guard.push_back((key, val.clone()));
        }
        val
    }
}

fn section_clean<T>(s: &Section<T>) -> bool {
    s.data.is_some() && s.errors.is_empty()
}

type GitTreeKey = (PathBuf, String, String);
type GitCommitKey = (PathBuf, String);
type GitPathKey = (PathBuf, String, String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileStamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

impl FileStamp {
    pub(crate) fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime: meta.mtime(),
            mtime_nsec: meta.mtime_nsec(),
            ctime: meta.ctime(),
            ctime_nsec: meta.ctime_nsec(),
        }
    }
}

type TickKey = (PathBuf, FileStamp);
type DirAsksKey = (PathBuf, String, Vec<(OsString, FileStamp)>);
type PlansSection = Section<Vec<(String, PlanGraph)>>;

static GIT_LAYOUT: LazyLock<RingCache<GitCommitKey, Result<Vec<String>, String>>> = LazyLock::new(RingCache::new);
static GIT_TREE_PATH: LazyLock<RingCache<GitPathKey, Result<Option<String>, String>>> = LazyLock::new(RingCache::new);
static GIT_COMMITTED_AT: LazyLock<RingCache<GitCommitKey, Result<Option<jiff::Timestamp>, String>>> =
    LazyLock::new(RingCache::new);
static GIT_ASKS: LazyLock<RingCache<GitTreeKey, Section<Vec<LedgerFile>>>> = LazyLock::new(RingCache::new);
static GIT_LOOPS: LazyLock<RingCache<GitTreeKey, Section<Vec<LoopSpec>>>> = LazyLock::new(RingCache::new);
static GIT_INITIATIVES: LazyLock<RingCache<GitTreeKey, Section<Vec<Initiative>>>> = LazyLock::new(RingCache::new);
static GIT_PLANS: LazyLock<RingCache<GitTreeKey, PlansSection>> = LazyLock::new(RingCache::new);
static FS_TICKS: LazyLock<RingCache<TickKey, Section<TickTail>>> = LazyLock::new(RingCache::new);
static FS_ASKS: LazyLock<RingCache<DirAsksKey, Section<Vec<LedgerFile>>>> = LazyLock::new(RingCache::new);

pub(crate) fn git_layout(
    root: &Path,
    commit: &str,
    compute: impl FnOnce() -> Result<Vec<String>, String>,
) -> Result<Vec<String>, String> {
    GIT_LAYOUT.get_or_insert_if((root.to_path_buf(), commit.to_string()), compute, Result::is_ok)
}

pub(crate) fn git_tree_path(
    root: &Path,
    commit: &str,
    name: &str,
    compute: impl FnOnce() -> Result<Option<String>, String>,
) -> Result<Option<String>, String> {
    GIT_TREE_PATH.get_or_insert_if((root.to_path_buf(), commit.to_string(), name.to_string()), compute, Result::is_ok)
}

pub(crate) fn git_committed_at(
    root: &Path,
    commit: &str,
    compute: impl FnOnce() -> Result<Option<jiff::Timestamp>, String>,
) -> Result<Option<jiff::Timestamp>, String> {
    GIT_COMMITTED_AT.get_or_insert_if((root.to_path_buf(), commit.to_string()), compute, Result::is_ok)
}

pub(crate) fn git_asks(
    root: &Path,
    source: &str,
    commit: &str,
    compute: impl FnOnce() -> Section<Vec<LedgerFile>>,
) -> Section<Vec<LedgerFile>> {
    GIT_ASKS.get_or_insert_if((root.to_path_buf(), source.to_string(), commit.to_string()), compute, section_clean)
}

pub(crate) fn git_loops(
    root: &Path,
    source: &str,
    commit: &str,
    compute: impl FnOnce() -> Section<Vec<LoopSpec>>,
) -> Section<Vec<LoopSpec>> {
    GIT_LOOPS.get_or_insert_if((root.to_path_buf(), source.to_string(), commit.to_string()), compute, section_clean)
}

pub(crate) fn git_initiatives(
    root: &Path,
    source: &str,
    commit: &str,
    compute: impl FnOnce() -> Section<Vec<Initiative>>,
) -> Section<Vec<Initiative>> {
    GIT_INITIATIVES.get_or_insert_if(
        (root.to_path_buf(), source.to_string(), commit.to_string()),
        compute,
        section_clean,
    )
}

pub(crate) fn git_plans(
    root: &Path,
    source: &str,
    commit: &str,
    compute: impl FnOnce() -> Section<Vec<(String, PlanGraph)>>,
) -> Section<Vec<(String, PlanGraph)>> {
    GIT_PLANS.get_or_insert_if((root.to_path_buf(), source.to_string(), commit.to_string()), compute, section_clean)
}

pub(crate) fn fs_ticks(path: &Path, compute: impl FnOnce() -> Section<TickTail>) -> Section<TickTail> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return compute();
    };
    if !meta.file_type().is_file() {
        return compute();
    }
    FS_TICKS.get_or_insert_if((path.to_path_buf(), FileStamp::of(&meta)), compute, section_clean)
}

/// Fingerprint a working-tree asks directory when every matching entry is a regular file.
/// Returns `None` on any non-regular entry or read error so the uncached error path runs verbatim.
fn fingerprint_asks_dir(dir: &Path) -> Option<Vec<(OsString, FileStamp)>> {
    let meta = std::fs::symlink_metadata(dir).ok()?;
    if !meta.file_type().is_dir() {
        return None;
    }
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name_str = name.to_str()?;
        if !name_str.ends_with(".yaml") && !name_str.ends_with(".yml") {
            continue;
        }
        let emeta = std::fs::symlink_metadata(entry.path()).ok()?;
        if !emeta.file_type().is_file() {
            return None;
        }
        entries.push((name, FileStamp::of(&emeta)));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Some(entries)
}

pub(crate) fn fs_asks(
    dir: &Path,
    label: &str,
    compute: impl FnOnce() -> Section<Vec<LedgerFile>>,
) -> Section<Vec<LedgerFile>> {
    let Some(fp) = fingerprint_asks_dir(dir) else {
        return compute();
    };
    FS_ASKS.get_or_insert_if((dir.to_path_buf(), label.to_string(), fp), compute, section_clean)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn ring_cache_hits_and_evicts_oldest() {
        let cache = RingCache::new();
        let calls = AtomicUsize::new(0);
        let a = cache.get_or_insert_if(1usize, || calls.fetch_add(1, Ordering::SeqCst) + 1, |_| true);
        let b = cache.get_or_insert_if(1usize, || calls.fetch_add(1, Ordering::SeqCst) + 1, |_| true);
        assert_eq!((a, b, calls.load(Ordering::SeqCst)), (1, 1, 1));
        for i in 2..=(MAX_ENTRIES + 2) {
            cache.get_or_insert_if(i, || i, |_| true);
        }
        let c = cache.get_or_insert_if(1usize, || calls.fetch_add(1, Ordering::SeqCst) + 1, |_| true);
        assert_eq!(c, 2, "entry 1 should have been evicted after MAX_ENTRIES inserts");
    }

    #[test]
    fn transient_errors_and_unavailable_sections_are_never_cached() {
        let res_cache: RingCache<usize, Result<usize, String>> = RingCache::new();
        let calls = AtomicUsize::new(0);
        let first = res_cache.get_or_insert_if(
            1,
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n == 0 { Err("transient".into()) } else { Ok(42) }
            },
            Result::is_ok,
        );
        assert!(first.is_err());
        let second = res_cache.get_or_insert_if(1, || Ok(42), Result::is_ok);
        assert_eq!(second, Ok(42), "transient Err must not pin in cache");

        let sec_cache: RingCache<usize, Section<usize>> = RingCache::new();
        let unavail = sec_cache.get_or_insert_if(1, || Section::unavailable("s", "boom"), section_clean);
        assert!(unavail.data.is_none());
        let partial = sec_cache.get_or_insert_if(
            1,
            || Section {
                source: "s".into(),
                data: Some(5),
                errors: vec![maestro_core::SourceError { at: "p".into(), reason: "partial".into() }],
            },
            section_clean,
        );
        assert_eq!(partial.data, Some(5));
        let recovered = sec_cache.get_or_insert_if(1, || Section::ok("s", 7), section_clean);
        assert_eq!(recovered.data, Some(7), "unavailable or partial Section must not pin in cache");
    }

    #[test]
    fn fs_ticks_invalidates_on_file_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ticks.jsonl");
        std::fs::write(&path, "a\n").unwrap();
        let calls = AtomicUsize::new(0);
        let _ = fs_ticks(&path, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Section::ok("t", TickTail::default())
        });
        let _ = fs_ticks(&path, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Section::ok("t", TickTail::default())
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        std::fs::write(&path, "a\nb\n").unwrap();
        let _ = fs_ticks(&path, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Section::ok("t", TickTail::default())
        });
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
