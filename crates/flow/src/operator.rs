//! Operator functions. None is a kernel verb and none is exposed to agents: each is a step an
//! operator takes on purpose, under the exclusive lock.
//!
//! - [`recover`] finishes an interrupted genesis or restores a lost lock from its root's own
//!   epoch. It never makes a new epoch, so a deleted lock beside a live root never re-epochs it.
//! - [`create_fresh_ledger`] makes a new ledger, with a new epoch, for a host, after the old root
//!   was archived (moved away). A normal writer never does this.
//! - [`retire`] moves an instance to `retired/<id>`, its log kept as it was.
//! - [`truncate_torn_tail`] cuts a torn last line so appends resume.
//! - [`rebuild_projection`] rebuilds the SQLite cache from the logs.

use crate::error::{FlowError, Result};
use crate::ids::{HostId, InstanceId};
use crate::layout::{EPOCH_FILE, HOST_FILE, INSTANCES, Layout, RETIRED, is_epoch};
use crate::ledger::{
    CacheWrite, Ledger, Options, genesis, random_epoch, refuse_retired, stage_root, sync_dir, write_new,
};
use crate::lock::{self, Mode};
use crate::log;
use crate::projection;
use std::fs::{File, OpenOptions};
use std::io::{Seek as _, SeekFrom, Write as _};
use std::path::Path;

/// What [`recover`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovered {
    /// Moved the staged root of an interrupted genesis into place.
    FinishedGenesis { epoch: String },
    /// Wrote the root's own epoch into a missing or empty lock.
    RestoredLock { epoch: String },
    /// Removed an empty lock that had no root: the tree is empty again.
    ClearedEmptyLock,
}

fn refused(message: impl Into<String>) -> FlowError {
    FlowError::OperatorRefused(message.into())
}

/// The epoch a live root carries, checking its host too.
fn root_epoch(root: &Path) -> Result<String> {
    let read = |name: &str| std::fs::read_to_string(root.join(name)).ok().map(|t| t.trim().to_string());
    let host = read(HOST_FILE).filter(|h| HostId::new(h.as_str()).is_ok());
    match (host, read(EPOCH_FILE).filter(|e| is_epoch(e))) {
        (Some(_), Some(epoch)) => Ok(epoch),
        _ => Err(FlowError::LedgerDamaged { detail: "the root's HOST or EPOCH is missing or unreadable".into() }),
    }
}

/// Overwrites the lock's content in place: the file, and so every holder's lock, stays the same.
fn write_lock(file: &File, lock: &Path, epoch: &str) -> Result<()> {
    let mut f = file;
    let line = format!("{epoch}\n");
    f.seek(SeekFrom::Start(0))
        .and_then(|_| f.write_all(line.as_bytes()))
        .and_then(|()| file.set_len(line.len() as u64))
        .and_then(|()| file.sync_all())
        .map_err(FlowError::io(lock))
}

fn open_lock_rw(layout: &Layout) -> Result<Option<File>> {
    match OpenOptions::new().read(true).write(true).open(&layout.lock) {
        Ok(f) => Ok(Some(f)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(FlowError::io(&layout.lock)(e)),
    }
}

/// Runs `run` holding `file` exclusive, releasing it however `run` ends.
fn under_lock<T>(file: &File, layout: &Layout, options: &Options, run: impl FnOnce() -> Result<T>) -> Result<T> {
    lock::take(file, Mode::Exclusive, options.lock_wait, &layout.lock)?;
    let out = if lock::same_file(file, &layout.lock)? {
        run()
    } else {
        Err(FlowError::LockLost { lock: layout.lock.clone(), detail: "the lock was replaced".into() })
    };
    let _ = file.unlock();
    out
}

/// Repairs the states a crash or a deleted lock leaves, without ever making a new epoch.
///
/// | State | Recovery |
/// |---|---|
/// | Lock names epoch `e`, no root, staged root for `e` beside it | the stage is moved into place |
/// | Lock missing or empty, live root | the root's own epoch is written into the lock |
/// | Lock empty, no root | the lock is removed, leaving an empty tree |
///
/// Anything else is refused with what was found: a root that moved away stays moved (a new
/// ledger is [`create_fresh_ledger`]), and a lock and root with different epochs are left for
/// the operator to judge.
pub fn recover(root: &Path, options: &Options) -> Result<Recovered> {
    let layout = Layout::new(root)?;
    let root_exists = std::fs::symlink_metadata(&layout.root).is_ok();
    let Some(file) = open_lock_rw(&layout)? else {
        if !root_exists {
            return Err(refused("There is no flows ledger here. A writer's first open creates one."));
        }
        let epoch = root_epoch(&layout.root)?;
        let stage = layout.lock_stage(&epoch);
        let _ = std::fs::remove_file(&stage);
        write_new(&stage, &format!("{epoch}\n"))?;
        let linked = std::fs::hard_link(&stage, &layout.lock);
        let _ = std::fs::remove_file(&stage);
        return match linked {
            Ok(()) => {
                sync_dir(&layout.parent);
                Ok(Recovered::RestoredLock { epoch })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(refused("A lock appeared while recovering. Run the recovery again."))
            }
            Err(e) => Err(FlowError::io(&layout.lock)(e)),
        };
    };
    under_lock(&file, &layout, options, || {
        let content = std::fs::read_to_string(&layout.lock).map_err(FlowError::io(&layout.lock))?;
        let content = content.trim();
        if std::fs::symlink_metadata(&layout.root).is_ok() {
            let epoch = root_epoch(&layout.root)?;
            if content.is_empty() {
                write_lock(&file, &layout.lock, &epoch)?;
                return Ok(Recovered::RestoredLock { epoch });
            }
            if content == epoch {
                return Err(refused("The flows ledger is whole, so there is nothing to recover."));
            }
            return Err(FlowError::EpochMismatch { expected: content.to_string(), found: epoch });
        }
        if content.is_empty() {
            std::fs::remove_file(&layout.lock).map_err(FlowError::io(&layout.lock))?;
            sync_dir(&layout.parent);
            return Ok(Recovered::ClearedEmptyLock);
        }
        let stage = layout.stage(content);
        if is_epoch(content) && std::fs::symlink_metadata(&stage).is_ok() {
            root_epoch(&stage)?;
            std::fs::rename(&stage, &layout.root).map_err(FlowError::io(&layout.root))?;
            sync_dir(&layout.parent);
            return Ok(Recovered::FinishedGenesis { epoch: content.to_string() });
        }
        Err(FlowError::RootMoved { root: layout.root.clone() })
    })
}

/// Makes a new ledger for `host`, with a new epoch, where the old root was archived (moved
/// away). The lock keeps its file and takes the new epoch, so writers that opened the old
/// ledger are refused `epoch_mismatch`. Refused while a root is in place.
pub fn create_fresh_ledger(root: &Path, host: &HostId, options: &Options) -> Result<String> {
    let layout = Layout::new(root)?;
    let in_place = || std::fs::symlink_metadata(&layout.root).is_ok();
    let still_there = || refused("The flows root is still in place. Archive it (move it away) first.");
    if in_place() {
        return Err(still_there());
    }
    let Some(file) = open_lock_rw(&layout)? else {
        return match genesis(&layout, host)? {
            Some(file) => {
                let _ = file.unlock();
                root_epoch(&layout.root)
            }
            None => Err(refused("Another writer created the ledger meanwhile.")),
        };
    };
    under_lock(&file, &layout, options, || {
        if in_place() {
            return Err(still_there());
        }
        let epoch = random_epoch()?;
        let stage = stage_root(&layout, host, &epoch)?;
        // The lock names the new epoch before the root moves in: a crash between the two leaves
        // a genesis that `recover` finishes.
        write_lock(&file, &layout.lock, &epoch)?;
        std::fs::rename(&stage, &layout.root).map_err(FlowError::io(&layout.root))?;
        sync_dir(&layout.parent);
        Ok(epoch)
    })
}

/// Retires instance `id`: under the lock, `instances/<id>` moves to `retired/<id>` with its log
/// as it was. The id is never written again.
pub fn retire(ledger: &mut Ledger, id: &InstanceId) -> Result<CacheWrite> {
    let guard = ledger.lock()?;
    let layout = guard.layout().clone();
    refuse_retired(&layout, id)?;
    let from = layout.instance_dir(id);
    if std::fs::symlink_metadata(&from).is_err() {
        return Err(FlowError::UnknownInstance { instance: id.to_string() });
    }
    let retired = layout.root.join(RETIRED);
    std::fs::create_dir_all(&retired).map_err(FlowError::io(&retired))?;
    std::fs::rename(&from, layout.retired_dir(id)).map_err(FlowError::io(&from))?;
    sync_dir(&retired);
    sync_dir(&layout.root.join(INSTANCES));
    Ok(projection::remove(&layout, guard.epoch(), id).into())
}

/// Cuts a torn last line from instance `id`'s log, back to its last verified line, so appends
/// resume. Returns the bytes removed. Refused when the log verifies or is broken before its tail.
pub fn truncate_torn_tail(ledger: &mut Ledger, id: &InstanceId) -> Result<u64> {
    let guard = ledger.lock()?;
    let layout = guard.layout().clone();
    refuse_retired(&layout, id)?;
    let path = layout.events(id);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(FlowError::UnknownInstance { instance: id.to_string() });
        }
        Err(e) => return Err(FlowError::io(path)(e)),
    };
    match log::verify_prefix(id, &bytes) {
        Ok(_) => Err(refused(format!("Instance {id}'s log verifies to its end, so nothing was truncated."))),
        Err((FlowError::TornTail { .. }, prefix)) => {
            let file = OpenOptions::new().write(true).open(&path).map_err(FlowError::io(&path))?;
            file.set_len(prefix.len).and_then(|()| file.sync_all()).map_err(FlowError::io(&path))?;
            let kept = log::verify(id, &bytes[..prefix.len as usize])?;
            let _ = projection::store(&layout, guard.epoch(), &kept.summary());
            Ok(bytes.len() as u64 - prefix.len)
        }
        Err((e, _)) => Err(e),
    }
}

/// Rebuilds the SQLite projection from the logs.
pub fn rebuild_projection(ledger: &mut Ledger) -> Result<()> {
    ledger.lock()?.rebuild_projection()
}
