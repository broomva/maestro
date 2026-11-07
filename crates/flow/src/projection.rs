//! The SQLite projection: a cache of what the logs say, rebuilt from them at will.
//!
//! Tables: `meta` (`schema`, `epoch`), `instances` (`id`, `seq`, `last_hash`) and `kind_counts`
//! (`instance`, `kind`, `n`). Writers replace an instance's rows from its verified log after each
//! append, and rebuild everything when the file is missing or from another epoch or schema.
//! Readers open it read-only and only compare; the log wins any disagreement.

use crate::error::Result;
use crate::ids::InstanceId;
use crate::layout::Layout;
use crate::ledger::summaries;
use crate::log::InstanceSummary;
use rusqlite::{Connection, OpenFlags, OptionalExtension as _, params};
use std::collections::BTreeMap;

const SCHEMA_VERSION: &str = "1";
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS instances (id TEXT PRIMARY KEY, seq INTEGER NOT NULL, last_hash TEXT);
CREATE TABLE IF NOT EXISTS kind_counts (
    instance TEXT NOT NULL, kind TEXT NOT NULL, n INTEGER NOT NULL, PRIMARY KEY (instance, kind)
);
";

/// Every row of the projection, sorted: two equal dumps are the same projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionDump {
    pub meta: Vec<(String, String)>,
    pub instances: Vec<(String, i64, Option<String>)>,
    pub kind_counts: Vec<(String, String, i64)>,
}

fn open_rw(layout: &Layout) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        layout.projection(),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

fn open_ro(layout: &Layout) -> Result<Option<Connection>> {
    let path = layout.projection();
    match Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX) {
        Ok(conn) => Ok(Some(conn)),
        Err(_) if std::fs::symlink_metadata(&path).is_err() => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Whether the cache was built for this epoch and schema.
fn current(conn: &Connection, epoch: &str) -> Result<bool> {
    let get = |key: &str| -> Result<Option<String>> {
        Ok(conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    };
    Ok(get("schema")?.as_deref() == Some(SCHEMA_VERSION) && get("epoch")?.as_deref() == Some(epoch))
}

fn write_summary(tx: &rusqlite::Transaction<'_>, s: &InstanceSummary) -> Result<()> {
    tx.execute("DELETE FROM instances WHERE id = ?1", [&s.instance])?;
    tx.execute("DELETE FROM kind_counts WHERE instance = ?1", [&s.instance])?;
    tx.execute(
        "INSERT INTO instances (id, seq, last_hash) VALUES (?1, ?2, ?3)",
        params![s.instance, s.seq as i64, s.last_hash],
    )?;
    for (kind, n) in &s.counts {
        tx.execute(
            "INSERT INTO kind_counts (instance, kind, n) VALUES (?1, ?2, ?3)",
            params![s.instance, kind, *n as i64],
        )?;
    }
    Ok(())
}

fn rebuild_with(conn: &mut Connection, layout: &Layout, epoch: &str) -> Result<()> {
    let all = summaries(layout)?;
    let tx = conn.transaction()?;
    tx.execute_batch("DELETE FROM meta; DELETE FROM instances; DELETE FROM kind_counts;")?;
    tx.execute("INSERT INTO meta (key, value) VALUES ('schema', ?1), ('epoch', ?2)", params![SCHEMA_VERSION, epoch])?;
    for s in &all {
        write_summary(&tx, s)?;
    }
    tx.commit()?;
    Ok(())
}

/// Replaces one instance's rows with its log's summary; a cache that is missing or stale is rebuilt whole.
pub(crate) fn store(layout: &Layout, epoch: &str, summary: &InstanceSummary) -> Result<()> {
    let mut conn = open_rw(layout)?;
    if !current(&conn, epoch)? {
        return rebuild_with(&mut conn, layout, epoch);
    }
    let tx = conn.transaction()?;
    write_summary(&tx, summary)?;
    tx.commit()?;
    Ok(())
}

/// Drops a retired instance's rows.
pub(crate) fn remove(layout: &Layout, epoch: &str, id: &InstanceId) -> Result<()> {
    let mut conn = open_rw(layout)?;
    if !current(&conn, epoch)? {
        return rebuild_with(&mut conn, layout, epoch);
    }
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM instances WHERE id = ?1", [id.as_str()])?;
    tx.execute("DELETE FROM kind_counts WHERE instance = ?1", [id.as_str()])?;
    tx.commit()?;
    Ok(())
}

/// Rebuilds the whole projection from the logs.
pub(crate) fn rebuild(layout: &Layout, epoch: &str) -> Result<()> {
    let mut conn = open_rw(layout)?;
    rebuild_with(&mut conn, layout, epoch)
}

/// What the cache holds for `id`, read-only: `None` when there is no projection file, `Some(None)`
/// when it has no row for `id` or was built for another epoch.
pub(crate) fn read(layout: &Layout, epoch: &str, id: &InstanceId) -> Result<Option<Option<InstanceSummary>>> {
    let Some(conn) = open_ro(layout)? else { return Ok(None) };
    if !current(&conn, epoch)? {
        return Ok(Some(None));
    }
    let row = conn
        .query_row("SELECT seq, last_hash FROM instances WHERE id = ?1", [id.as_str()], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
        })
        .optional()?;
    let Some((seq, last_hash)) = row else { return Ok(Some(None)) };
    let mut stmt = conn.prepare("SELECT kind, n FROM kind_counts WHERE instance = ?1")?;
    let mut counts = BTreeMap::new();
    for row in stmt.query_map([id.as_str()], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (kind, n) = row?;
        counts.insert(kind, n.max(0) as u64);
    }
    Ok(Some(Some(InstanceSummary { instance: id.to_string(), seq: seq.max(0) as u64, last_hash, counts })))
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
    let mut stmt = conn.prepare("SELECT id, seq, last_hash FROM instances ORDER BY id")?;
    for row in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))? {
        instances.push(row?);
    }
    let mut kind_counts = Vec::new();
    let mut stmt = conn.prepare("SELECT instance, kind, n FROM kind_counts ORDER BY instance, kind")?;
    for row in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))? {
        kind_counts.push(row?);
    }
    Ok(Some(ProjectionDump { meta, instances, kind_counts }))
}
