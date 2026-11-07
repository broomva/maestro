//! Operator functions. None is a kernel verb and none is exposed to agents: each is a step an
//! operator takes on purpose, under the exclusive lock.
//!
//! - [`recover`] finishes an interrupted genesis, clears the stages of one that crashed before
//!   linking its lock, or restores a lost lock from its root's own epoch. It never makes a new
//!   epoch, so a deleted lock beside a live root never re-epochs it.
//! - [`archive`] moves the root and its lock away together and leaves a marker, so no writer
//!   starts a new ledger there by itself.
//! - [`create_fresh_ledger`] makes a new ledger, with a new epoch, for a host, after the old root
//!   was archived. A normal writer never does this.
//! - [`retire`] moves an instance to `retired/<id>`, its log kept as it was.
//! - [`truncate_torn_tail`] cuts a torn last line so appends resume.
//! - [`rebuild_projection`] rebuilds the SQLite cache from the logs.

use crate::error::{FlowError, Result};
use crate::hooks;
use crate::ids::{HostId, InstanceId};
use crate::layout::{self, EPOCH_FILE, HOST_FILE, INSTANCES, Layout, RETIRED, State, is_epoch, real_dir};
use crate::ledger::{
    CacheWrite, Ledger, Options, genesis, open_lock, random_epoch, read_log, refuse_retired, stage_root, sync_dir,
    write_new,
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
    /// Removed the stages of geneses that crashed before linking a lock: the tree is empty again.
    ClearedOrphanStages { count: usize },
}

fn refused(message: impl Into<String>) -> FlowError {
    FlowError::OperatorRefused(message.into())
}

/// The epoch a live root carries, checking its host too.
fn root_epoch(root: &Path) -> Result<String> {
    let read = |name: &str| layout::read_small(&root.join(name));
    let host = read(HOST_FILE)?.filter(|h| HostId::new(h.as_str()).is_ok());
    match (host, read(EPOCH_FILE)?.filter(|e| is_epoch(e))) {
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
    match layout::nofollow(OpenOptions::new().read(true).write(true)).open(&layout.lock) {
        Ok(f) => Ok(Some(f)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(layout::open_error(&layout.lock, e)),
    }
}

/// Runs `run` holding `file` exclusive, releasing it however `run` ends.
fn under_lock<T>(file: &File, layout: &Layout, options: &Options, run: impl FnOnce() -> Result<T>) -> Result<T> {
    hooks::pause("lock_opened");
    lock::take(file, Mode::Exclusive, options.lock_wait, &layout.lock)?;
    let out = if lock::same_file(file, &layout.lock)? {
        run()
    } else {
        Err(FlowError::LockLost {
            lock: layout.lock.clone(),
            detail: "the lock file was replaced while the operator opened it".into(),
        })
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
/// | No lock, no root, stages of geneses that crashed before linking | the stages are removed |
///
/// Restoring a missing lock takes the root directory's own lock first, so it waits for (or
/// refuses `busy` against) any writer still holding a guard through the deleted lock file.
/// Anything else is refused with what was found: a root that moved away stays moved (a new
/// ledger is [`create_fresh_ledger`]), and a lock and root with different epochs are left for
/// the operator to judge.
pub fn recover(root: &Path, options: &Options) -> Result<Recovered> {
    let layout = Layout::new(root)?;
    let Some(file) = open_lock_rw(&layout)? else {
        if !layout::exists(&layout.root)? {
            if layout::exists(&layout.archived_marker())? {
                return Err(refused(
                    "The ledger here was archived. A new one is made with `operator::create_fresh_ledger`.",
                ));
            }
            let orphans = layout::orphan_stages(&layout)?;
            if orphans.is_empty() {
                return Err(refused("There is no flows ledger here. A writer's first open creates one."));
            }
            for stage in &orphans {
                std::fs::remove_dir_all(stage).map_err(FlowError::io(stage))?;
                let epoch = stage.file_name().and_then(|n| n.to_str()).and_then(|n| n.rsplit('-').next());
                if let Some(epoch) = epoch {
                    let _ = std::fs::remove_file(layout.lock_stage(epoch));
                }
            }
            sync_dir(&layout.parent);
            return Ok(Recovered::ClearedOrphanStages { count: orphans.len() });
        }
        let epoch = root_epoch(&layout.root)?;
        // A writer still holding a guard holds the root directory's lock: wait for it.
        let held_root = lock::lock_root(&layout.root, None, Mode::Exclusive, options.lock_wait)?;
        let stage = layout.lock_stage(&epoch);
        let _ = std::fs::remove_file(&stage);
        write_new(&stage, &format!("{epoch}\n"))?;
        let linked = std::fs::hard_link(&stage, &layout.lock);
        let _ = std::fs::remove_file(&stage);
        let _ = held_root.unlock();
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
        let content = layout::read_small(&layout.lock)?.unwrap_or_default();
        if layout::exists(&layout.root)? {
            let epoch = root_epoch(&layout.root)?;
            if content.is_empty() {
                write_lock(&file, &layout.lock, &epoch)?;
                return Ok(Recovered::RestoredLock { epoch });
            }
            if content == epoch {
                return Err(refused("The flows ledger is whole, so there is nothing to recover."));
            }
            return Err(FlowError::EpochMismatch { expected: content, found: epoch });
        }
        if content.is_empty() {
            std::fs::remove_file(&layout.lock).map_err(FlowError::io(&layout.lock))?;
            sync_dir(&layout.parent);
            return Ok(Recovered::ClearedEmptyLock);
        }
        let stage = layout.stage(&content);
        if is_epoch(&content) && std::fs::symlink_metadata(&stage).is_ok() {
            root_epoch(&stage)?;
            std::fs::rename(&stage, &layout.root).map_err(FlowError::io(&layout.root))?;
            let _ = std::fs::remove_file(layout.lock_stage(&content));
            sync_dir(&layout.parent);
            return Ok(Recovered::FinishedGenesis { epoch: content });
        }
        Err(FlowError::RootMoved { root: layout.root.clone() })
    })
}

/// Archives the ledger: under its lock, the root moves to `dest/<name>` and the lock to
/// `dest/<name>.lock`, and a marker `<name>.archived` is left beside where the root was. Every
/// writer, waiting or new, then refuses `root_moved` and creates nothing, until an operator makes
/// a fresh ledger there with [`create_fresh_ledger`]. `dest` must not exist.
pub fn archive(root: &Path, dest: &Path, options: &Options) -> Result<()> {
    let layout = Layout::new(root)?;
    if layout::exists(dest)? {
        return Err(refused(format!("{} already exists, so nothing was archived.", dest.display())));
    }
    let Some(file) = open_lock(&layout)? else {
        return Err(FlowError::NoLedger { root: layout.root.clone() });
    };
    lock::take(&file, Mode::Exclusive, options.lock_wait, &layout.lock)?;
    let done = (|| {
        if !lock::same_file(&file, &layout.lock)? {
            return Err(FlowError::LockLost { lock: layout.lock.clone(), detail: "the lock file was replaced".into() });
        }
        let State::Live { epoch, .. } = layout::inspect(&layout)? else {
            return Err(FlowError::RootMoved { root: layout.root.clone() });
        };
        let root_dir = lock::lock_root(&layout.root, None, Mode::Exclusive, options.lock_wait)?;
        let marker = layout.archived_marker();
        let pending = marker.with_extension("archived-pending");
        let _ = std::fs::remove_file(&pending);
        write_new(&pending, &format!("{}\n{epoch}\n", dest.display()))?;
        std::fs::rename(&pending, &marker).map_err(FlowError::io(&marker))?;
        std::fs::create_dir(dest).map_err(FlowError::io(dest))?;
        let moved_root = dest.join(layout.name());
        std::fs::rename(&layout.root, &moved_root).map_err(FlowError::io(&layout.root))?;
        std::fs::rename(&layout.lock, dest.join(format!("{}.lock", layout.name())))
            .map_err(FlowError::io(&layout.lock))?;
        sync_dir(dest);
        sync_dir(&layout.parent);
        let _ = root_dir.unlock();
        Ok(())
    })();
    let _ = file.unlock();
    done
}

/// Makes a new ledger for `host`, with a new epoch, where the old root was archived (moved
/// away). Writers that opened the old ledger are refused `epoch_mismatch`. Refused while a root
/// is in place. Removes the archive marker once the new root is in place.
pub fn create_fresh_ledger(root: &Path, host: &HostId, options: &Options) -> Result<String> {
    let layout = Layout::new(root)?;
    let in_place = || layout::exists(&layout.root);
    let still_there = || refused("The flows root is still in place. Archive it first.");
    if in_place()? {
        return Err(still_there());
    }
    hooks::pause("fresh_checked");
    let clear_marker = || {
        let _ = std::fs::remove_file(layout.archived_marker());
        sync_dir(&layout.parent);
    };
    let Some(file) = open_lock_rw(&layout)? else {
        return match genesis(&layout, host)? {
            Some(file) => {
                let _ = file.unlock();
                clear_marker();
                root_epoch(&layout.root)
            }
            None => Err(refused("Another writer created the ledger meanwhile.")),
        };
    };
    under_lock(&file, &layout, options, || {
        if in_place()? {
            return Err(still_there());
        }
        let epoch = random_epoch()?;
        let stage = stage_root(&layout, host, &epoch)?;
        // The lock names the new epoch before the root moves in: a crash between the two leaves
        // a genesis that `recover` finishes.
        write_lock(&file, &layout.lock, &epoch)?;
        std::fs::rename(&stage, &layout.root).map_err(FlowError::io(&layout.root))?;
        clear_marker();
        Ok(epoch)
    })
}

/// Retires instance `id`: under the lock, `instances/<id>` moves to `retired/<id>` with its log
/// as it was. The id is never written again.
pub fn retire(ledger: &mut Ledger, id: &InstanceId) -> Result<CacheWrite> {
    let guard = ledger.lock()?;
    guard.verify()?;
    let layout = guard.layout().clone();
    refuse_retired(&layout, id)?;
    let from = layout.instance_dir(id);
    if !real_dir(&from)? {
        return Err(FlowError::UnknownInstance { instance: id.to_string() });
    }
    let retired = layout.root.join(RETIRED);
    if !real_dir(&retired)? {
        return Err(FlowError::LedgerDamaged { detail: "retired/ is missing".into() });
    }
    std::fs::rename(&from, layout.retired_dir(id)).map_err(FlowError::io(&from))?;
    sync_dir(&retired);
    sync_dir(&layout.root.join(INSTANCES));
    Ok(projection::sync(&layout, guard.epoch()).into())
}

/// Cuts a torn last line from instance `id`'s log, back to its last verified line, so appends
/// resume. Returns the bytes removed. Refused when the log verifies or its chain is broken.
pub fn truncate_torn_tail(ledger: &mut Ledger, id: &InstanceId) -> Result<u64> {
    let guard = ledger.lock()?;
    guard.verify()?;
    let layout = guard.layout().clone();
    refuse_retired(&layout, id)?;
    let path = layout.events(id);
    let Some(bytes) = read_log(&layout, id)? else {
        return Err(FlowError::UnknownInstance { instance: id.to_string() });
    };
    match log::verify_prefix(id, &bytes) {
        Ok(_) => Err(refused(format!("Instance {id}'s log verifies to its end, so nothing was truncated."))),
        Err((FlowError::TornTail { .. }, prefix)) => {
            let file = layout::nofollow(OpenOptions::new().write(true))
                .open(&path)
                .map_err(|e| layout::open_error(&path, e))?;
            file.set_len(prefix.len).and_then(|()| file.sync_all()).map_err(FlowError::io(&path))?;
            let _ = projection::sync(&layout, guard.epoch());
            Ok(bytes.len() as u64 - prefix.len)
        }
        Err((e, _)) => Err(e),
    }
}

/// Rebuilds the SQLite projection from the logs.
pub fn rebuild_projection(ledger: &mut Ledger) -> Result<()> {
    ledger.lock()?.rebuild_projection()
}
