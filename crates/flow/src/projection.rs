//! The SQLite projection: a cache of what the logs say, rebuilt from them at will.
//!
//! Tables: `meta` (`epoch`), `instances` (`id`, `state`, `seq`, `last_hash`, `stat`)
//! and `kind_counts` (`instance`, `kind`, `n`). Every live instance has a row, whatever its log
//! holds: `state` is `ok`, `torn_tail`, `chain_broken` or `unreadable`, and `seq`, `last_hash`
//! and the counts describe the verified prefix (nothing, when unreadable). `stat` is the log
//! file's length, modification and change times and inode.
//!
//! Each row is a function of its log file alone ([`analyze`]), so the incremental path and a
//! rebuild agree on any tree: after every write, [`sync`] re-analyzes each instance whose `stat`
//! changed and drops the rows of instances no longer live (retired). A cache missing, or built
//! for another epoch, is rebuilt whole. Readers open it read-only and only compare; the
//! log wins any disagreement.

use crate::at::Dir;
use crate::error::{FlowError, Result};
use crate::ids::InstanceId;
use crate::layout::{EVENTS, Layout, PROJECTION};
use crate::ledger::{instances, list_instances, read_log};
use crate::lock::{self, FileId};
use crate::log::{self, InstanceSummary};
use rusqlite::{Connection, OpenFlags, OptionalExtension as _, params};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS instances (
    id TEXT PRIMARY KEY, state TEXT NOT NULL, seq INTEGER NOT NULL, last_hash TEXT, stat TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS kind_counts (
    instance TEXT NOT NULL, kind TEXT NOT NULL, n INTEGER NOT NULL, PRIMARY KEY (instance, kind)
);
";

/// Every row of the projection, sorted: two equal dumps are the same projection. For tests.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionDump {
    pub meta: Vec<(String, String)>,
    /// `(id, state, seq, last_hash, stat)`.
    pub instances: Vec<(String, String, i64, Option<String>, String)>,
    pub kind_counts: Vec<(String, String, i64)>,
}

/// One instance's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) state: String,
    pub(crate) summary: InstanceSummary,
    pub(crate) stat: String,
}

/// The log file's stat, as the row records it: what changes when the file does.
fn stat_key(root: &Dir, id: &InstanceId) -> String {
    match instances(root).and_then(|d| d.dir(id.as_str())) {
        Ok(Some(dir)) => dir.stat_key(EVENTS),
        _ => "missing".to_string(),
    }
}

/// The row for `id`, from its log file alone.
pub(crate) fn analyze(root: &Dir, id: &InstanceId) -> Row {
    let stat = stat_key(root, id);
    let empty = || InstanceSummary { instance: id.to_string(), seq: 0, last_hash: None, counts: BTreeMap::new() };
    let (state, summary) = match read_log(root, id) {
        Ok(None) => ("ok".to_string(), empty()),
        Ok(Some(bytes)) => match log::verify_prefix(id, &bytes) {
            Ok(log) => ("ok".to_string(), log.summary()),
            Err((e, prefix)) if matches!(e.code(), "torn_tail" | "chain_broken") => {
                let kept = log::verify(id, &bytes[..prefix.len as usize]).map(|l| l.summary());
                (e.code().to_string(), kept.unwrap_or_else(|_| empty()))
            }
            Err(_) => ("unreadable".to_string(), empty()),
        },
        Err(_) => ("unreadable".to_string(), empty()),
    };
    Row { state, summary, stat }
}

/// The cache's path, with the links above the root resolved (the workspace itself may sit behind
/// one): `SQLITE_OPEN_NOFOLLOW` refuses a link anywhere in the path it is given, so it then
/// refuses only a link at `projection.sqlite` itself. SQLite opens the journal beside it
/// without following a link of its own accord (see the projection link test).
fn db_path(layout: &Layout) -> Result<PathBuf> {
    Ok(std::fs::canonicalize(&layout.root).map_err(FlowError::io(&layout.root))?.join(PROJECTION))
}

/// Opens the cache for writing, only while the root's path still names the directory `root_id`
/// (the one the writer holds): SQLite needs a path, and a root moved away must never have its
/// rows written into whatever ledger now sits at that path.
fn open_rw(layout: &Layout, root_id: FileId) -> Result<Connection> {
    if lock::root_id(&layout.root)? != root_id {
        return Err(FlowError::Projection(
            "the root's path no longer names this ledger, so the cache was not written".into(),
        ));
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    Ok(Connection::open_with_flags(db_path(layout)?, flags | OpenFlags::SQLITE_OPEN_NOFOLLOW)?)
}

fn open_ro(layout: &Layout) -> Result<Option<Connection>> {
    let path = db_path(layout)?;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    match Connection::open_with_flags(&path, flags | OpenFlags::SQLITE_OPEN_NOFOLLOW) {
        Ok(conn) => Ok(Some(conn)),
        Err(_) if std::fs::symlink_metadata(&path).is_err() => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Whether the cache was built for this epoch.
fn current(conn: &Connection, epoch: &str) -> Result<bool> {
    let get = |key: &str| -> Result<Option<String>> {
        Ok(conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    };
    let has_meta: bool =
        conn.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'meta'", [], |r| r.get(0))?;
    Ok(has_meta && get("epoch")?.as_deref() == Some(epoch))
}

fn write_row(tx: &rusqlite::Transaction<'_>, row: &Row) -> Result<()> {
    let s = &row.summary;
    delete_row(tx, &s.instance)?;
    tx.execute(
        "INSERT INTO instances (id, state, seq, last_hash, stat) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![s.instance, row.state, s.seq as i64, s.last_hash, row.stat],
    )?;
    for (kind, n) in &s.counts {
        tx.execute(
            "INSERT INTO kind_counts (instance, kind, n) VALUES (?1, ?2, ?3)",
            params![s.instance, kind, *n as i64],
        )?;
    }
    Ok(())
}

fn delete_row(tx: &rusqlite::Transaction<'_>, id: &str) -> Result<()> {
    tx.execute("DELETE FROM instances WHERE id = ?1", [id])?;
    tx.execute("DELETE FROM kind_counts WHERE instance = ?1", [id])?;
    Ok(())
}

fn rebuild_with(conn: &mut Connection, root: &Dir, epoch: &str) -> Result<()> {
    let ids = list_instances(root)?;
    let tx = conn.transaction()?;
    tx.execute_batch("DROP TABLE IF EXISTS meta; DROP TABLE IF EXISTS instances; DROP TABLE IF EXISTS kind_counts;")?;
    tx.execute_batch(SCHEMA)?;
    tx.execute("INSERT INTO meta (key, value) VALUES ('epoch', ?1)", params![epoch])?;
    for id in &ids {
        write_row(&tx, &analyze(root, id))?;
    }
    tx.commit()?;
    Ok(())
}

/// Brings the cache up to the log tree after a write: rows whose log changed are re-analyzed and
/// rows of instances no longer live are dropped. A cache missing or stale is rebuilt whole.
pub(crate) fn sync(root: &Dir, layout: &Layout, epoch: &str, root_id: FileId) -> Result<()> {
    let mut conn = open_rw(layout, root_id)?;
    pause!("projection_open");
    // SQLite opened the cache by path after the check in `open_rw`, and makes its journal by path
    // when it writes: a root moved meanwhile fails the write, which is reported as a skipped
    // cache. Moved after the journal exists, the write lands in that root's own cache. The log
    // wins either way.
    if current(&conn, epoch)? { sync_with(&mut conn, root) } else { rebuild_with(&mut conn, root, epoch) }
}

fn sync_with(conn: &mut Connection, root: &Dir) -> Result<()> {
    let live = list_instances(root)?;
    let mut cached = BTreeMap::new();
    {
        let mut stmt = conn.prepare("SELECT id, stat FROM instances")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (id, stat) = row?;
            cached.insert(id, stat);
        }
    }
    let tx = conn.transaction()?;
    let mut seen = BTreeSet::new();
    for id in &live {
        seen.insert(id.as_str().to_string());
        if cached.get(id.as_str()) != Some(&stat_key(root, id)) {
            write_row(&tx, &analyze(root, id))?;
        }
    }
    for id in cached.keys().filter(|id| !seen.contains(*id)) {
        delete_row(&tx, id)?;
    }
    tx.commit()?;
    Ok(())
}

/// Rebuilds the whole projection from the logs.
pub(crate) fn rebuild(root: &Dir, layout: &Layout, epoch: &str, root_id: FileId) -> Result<()> {
    let mut conn = open_rw(layout, root_id)?;
    rebuild_with(&mut conn, root, epoch)
}

/// What the cache holds for `id`, read-only: `None` when there is no projection file, `Some(None)`
/// when it has no row for `id`. A row from another epoch's cache is compared like any other; the
/// log wins either way.
pub(crate) fn read(layout: &Layout, id: &InstanceId) -> Result<Option<Option<Row>>> {
    let Some(conn) = open_ro(layout)? else { return Ok(None) };
    let row = conn
        .query_row("SELECT state, seq, last_hash, stat FROM instances WHERE id = ?1", [id.as_str()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, String>(3)?))
        })
        .optional()?;
    let Some((state, seq, last_hash, stat)) = row else { return Ok(Some(None)) };
    let mut stmt = conn.prepare("SELECT kind, n FROM kind_counts WHERE instance = ?1")?;
    let mut counts = BTreeMap::new();
    for row in stmt.query_map([id.as_str()], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (kind, n) = row?;
        counts.insert(kind, n.max(0) as u64);
    }
    let summary = InstanceSummary { instance: id.to_string(), seq: seq.max(0) as u64, last_hash, counts };
    Ok(Some(Some(Row { state, summary, stat })))
}

/// Every row, read-only; `None` when there is no projection file.
pub(crate) fn dump(layout: &Layout) -> Result<Option<ProjectionDump>> {
    let Some(conn) = open_ro(layout)? else { return Ok(None) };
    let mut meta = Vec::new();
    let mut stmt = conn.prepare("SELECT key, value FROM meta ORDER BY key")?;
    for row in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
        meta.push(row?);
    }
    let mut instances = Vec::new();
    let mut stmt = conn.prepare("SELECT id, state, seq, last_hash, stat FROM instances ORDER BY id")?;
    for row in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))? {
        instances.push(row?);
    }
    let mut kind_counts = Vec::new();
    let mut stmt = conn.prepare("SELECT instance, kind, n FROM kind_counts ORDER BY instance, kind")?;
    for row in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))? {
        kind_counts.push(row?);
    }
    Ok(Some(ProjectionDump { meta, instances, kind_counts }))
}
