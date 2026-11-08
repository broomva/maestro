//! Operator functions. None is a kernel verb and none is exposed to agents: each is a step an
//! operator takes on purpose, under the exclusive lock.
//!
//! - [`recover`] finishes a genesis that crashed, restores a lost lock from its root's own
//!   epoch, and removes crash debris. It never makes a new epoch, so a deleted lock beside a live
//!   root never re-epochs it.
//! - [`archive`] moves the root and its lock away together and leaves a marker, so no writer
//!   starts a new ledger there by itself.
//! - [`create_fresh_ledger`] makes a new ledger, with a new epoch, for a host, after the old root
//!   was archived. A normal writer never does this.
//! - [`retire`] moves an instance to `retired/<id>`, its log kept as it was.
//! - [`truncate_torn_tail`] cuts a torn last line so appends resume.
//! - [`rebuild_projection`] rebuilds the SQLite cache from the logs.

use crate::error::{FlowError, Result};
use crate::ids::{HostId, InstanceId};
use crate::layout::{self, EPOCH_FILE, EVENTS, Layout, RETIRED, State, is_epoch};
use crate::ledger::{
    CacheWrite, Ledger, Options, genesis, install_lock, instances, open_lock, random_epoch, refuse_retired, stage_root,
    sync_dir,
};
use crate::lock::{self, Mode, file_id};
use crate::log;
use rustix::fs::OFlags;
use std::fs::File;
use std::path::Path;

/// What [`recover`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovered {
    /// A genesis (or fresh ledger) crashed after its lock named its stage: the stage is now the
    /// root, and an archive marker is gone.
    FinishedGenesis { epoch: String },
    /// The lock was missing, empty or not an epoch beside a live root: a new lock names that
    /// root's own epoch.
    RestoredLock { epoch: String },
    /// The ledger was whole; only files left by crashes were removed.
    RemovedDebris { count: usize },
}

fn refused(message: impl Into<String>) -> FlowError {
    FlowError::OperatorRefused(message.into())
}

/// The epoch a live root carries.
fn root_epoch(root: &Path) -> Result<String> {
    layout::read_small(&root.join(EPOCH_FILE))?
        .filter(|e| is_epoch(e))
        .ok_or_else(|| FlowError::LedgerDamaged { detail: "the root's EPOCH is missing or unreadable".into() })
}

/// The lock file, when there is one.
fn existing_lock(layout: &Layout) -> Result<Option<File>> {
    match File::open(&layout.lock) {
        Ok(f) => Ok(Some(f)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(FlowError::io(&layout.lock)(e)),
    }
}

/// Runs `run` holding `file` exclusive, releasing it however `run` ends.
fn under_lock<T>(file: &File, layout: &Layout, options: &Options, run: impl FnOnce() -> Result<T>) -> Result<T> {
    pause!("lock_opened");
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

/// Removes what crashes leave beside the root: stages no lock names (`keep` is the one the lock
/// names), and private lock files no process holds. A private name for the live lock itself (a
/// crash between linking and unlinking it) is only a name and always goes. Only the exact shapes
/// a genesis writes (`.<name>.genesis-<epoch>/`, `.<name>.lock.genesis-<epoch>`,
/// `.<name>.lock.new-<epoch>`) and file types count; anything else is left alone.
fn clear_debris(layout: &Layout, keep: Option<&str>) -> Result<usize> {
    let name = layout.name();
    let stage = format!(".{name}.genesis-");
    let private = [format!(".{name}.lock.genesis-"), format!(".{name}.lock.new-")];
    let live = std::fs::metadata(&layout.lock).ok().map(|m| file_id(&m));
    let mut count = 0;
    for entry in std::fs::read_dir(&layout.parent).map_err(FlowError::io(&layout.parent))? {
        let path = entry.map_err(FlowError::io(&layout.parent))?.path();
        let (Some(file), Ok(meta)) = (path.file_name().and_then(|n| n.to_str()), std::fs::symlink_metadata(&path))
        else {
            continue;
        };
        if let Some(epoch) = file.strip_prefix(&stage).filter(|e| is_epoch(e)) {
            if meta.is_dir() && keep != Some(epoch) {
                std::fs::remove_dir_all(&path).map_err(FlowError::io(&path))?;
                count += 1;
            }
        } else if private.iter().any(|p| file.strip_prefix(p.as_str()).is_some_and(is_epoch))
            && meta.is_file()
            && (Some(file_id(&meta)) == live || File::open(&path).is_ok_and(|f| f.try_lock().is_ok()))
        {
            std::fs::remove_file(&path).map_err(FlowError::io(&path))?;
            count += 1;
        }
    }
    Ok(count)
}

/// Repairs the states a crash or a deleted lock leaves, without ever making a new epoch, and
/// removes crash debris.
///
/// | State | Recovery |
/// |---|---|
/// | Lock names epoch `e`, no root, staged root for `e` beside it | the stage is moved into place |
/// | Lock missing, empty or not an epoch, live root | a new lock names the root's own epoch |
///
/// Restoring a missing lock takes the root directory's own lock first, so it waits for (or
/// refuses `busy` against) any writer still holding a guard through the deleted lock file.
/// Readers are not waited for: a read needs only its log rules, and the worst it sees is a torn
/// tail. A new lock is renamed into place, never written in place. Anything else is refused
/// with what was found.
pub fn recover(root: &Path, options: &Options) -> Result<Recovered> {
    let layout = Layout::new(root)?;
    let Some(file) = existing_lock(&layout)? else {
        if !layout::exists(&layout.root)? {
            return Err(refused("There is no flows ledger here to recover."));
        }
        let epoch = root_epoch(&layout.root)?;
        // A writer still holding a guard holds the root directory's lock: wait for it.
        let held_root = lock::lock_root(&layout.root, None, Mode::Exclusive, options.lock_wait)?;
        // Another recovery may have restored the lock during the wait.
        if layout::exists(&layout.lock)? {
            return Err(refused("A lock appeared while this recovery waited, so it changed nothing."));
        }
        clear_debris(&layout, None)?;
        install_lock(&layout, &epoch)?;
        drop(held_root);
        return Ok(Recovered::RestoredLock { epoch });
    };
    under_lock(&file, &layout, options, || {
        let content = layout::read_small(&layout.lock)?.unwrap_or_default();
        let named = Some(content.as_str()).filter(|c| is_epoch(c));
        let debris = clear_debris(&layout, named)?;
        if layout::exists(&layout.root)? {
            let epoch = root_epoch(&layout.root)?;
            if named.is_none() {
                install_lock(&layout, &epoch)?;
                return Ok(Recovered::RestoredLock { epoch });
            }
        } else if let Some(epoch) = named
            && layout::exists(&layout.stage(epoch))?
        {
            std::fs::rename(layout.stage(epoch), &layout.root).map_err(FlowError::io(&layout.root))?;
            let _ = std::fs::remove_file(layout.archived_marker());
            sync_dir(&layout.parent);
            return Ok(Recovered::FinishedGenesis { epoch: epoch.to_string() });
        }
        layout::inspect(&layout)?;
        // A crash after a new ledger's root went in, before its archive marker went, leaves the
        // marker beside a whole ledger: it no longer means anything.
        let marker = layout.archived_marker();
        let debris = debris + usize::from(layout::exists(&marker)?);
        let _ = std::fs::remove_file(&marker);
        if debris > 0 {
            return Ok(Recovered::RemovedDebris { count: debris });
        }
        Err(refused("The flows ledger is whole, so there is nothing to recover."))
    })
}

/// Whether `a` and `b` are on different filesystems, where a rename cannot move the root.
fn other_device(a: &Path, b: &Path) -> Result<bool> {
    let dev = |p: &Path| std::fs::metadata(p).map(|m| file_id(&m).0).map_err(FlowError::io(p));
    let differ = dev(a)? != dev(b)?;
    #[cfg(test)]
    let differ = differ || crate::hooks::other_device(a);
    Ok(differ)
}

/// Archives the ledger: the root moves to `dest/<name>` and the lock to `dest/<name>.lock`, and a
/// marker `<name>.archived` is left beside where the root was. Every writer, waiting or new, then
/// refuses `root_moved` and creates nothing, until an operator makes a fresh ledger there with
/// [`create_fresh_ledger`].
///
/// It takes the lock file (waiting for writers and readers) and the root directory's own lock (as
/// a writer does, so it waits for any guard, even one held through a replaced lock file), and
/// moves only a whole ledger. `dest`'s parent must exist, `dest` must be absent or empty, outside
/// the root and on the root's filesystem; all are checked before anything is written.
///
/// After a crash: before the root moved, the ledger is whole and an archive can simply run again
/// (an empty `dest` it made is reused). After the root moved and before its lock did, the lock
/// is left beside nothing and every writer, and another archive, refuses `root_moved`; the
/// operator moves the lock into `dest` by hand, or deletes it, to finish.
pub fn archive(root: &Path, dest: &Path, options: &Options) -> Result<()> {
    let layout = Layout::new(root)?;
    let Some(file) = open_lock(&layout)? else {
        return Err(FlowError::NoLedger { root: layout.root.clone() });
    };
    pause!("lock_opened");
    lock::take(&file, Mode::Exclusive, options.lock_wait, &layout.lock)?;
    let done = (|| {
        if !lock::same_file(&file, &layout.lock)? {
            return Err(FlowError::LockLost {
                lock: layout.lock.clone(),
                detail: "the lock file was replaced while the archive opened it".into(),
            });
        }
        // Only a whole ledger moves; a root already moved away (a crash mid-archive included) is
        // `root_moved` here, before the destination is looked at.
        let State::Live { epoch, .. } = layout::inspect(&layout)? else {
            return Err(FlowError::RootMoved { root: layout.root.clone() });
        };
        let parent = dest.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let Ok(parent) = std::fs::canonicalize(parent) else {
            return Err(refused(format!("{}'s parent does not exist, so nothing was archived.", dest.display())));
        };
        // `dest` itself is looked at, never followed: an empty real directory, as a crashed archive
        // leaves, is reused; anything else there (a file, a link, a full directory) is refused.
        let existing = std::fs::symlink_metadata(dest).ok();
        let reuse = existing.as_ref().is_some_and(|m| m.file_type().is_dir())
            && std::fs::read_dir(dest).is_ok_and(|mut d| d.next().is_none());
        if existing.is_some() && !reuse {
            return Err(refused(format!("{} already exists, so nothing was archived.", dest.display())));
        }
        if parent.starts_with(std::fs::canonicalize(&layout.root).map_err(FlowError::io(&layout.root))?) {
            return Err(refused(format!("{} is inside the root, so nothing was archived.", dest.display())));
        }
        // A reused `dest` is compared itself, so a mount point there is caught.
        if other_device(if reuse { dest } else { &parent }, &layout.parent)? {
            return Err(refused(format!("{} is on another filesystem, so nothing was archived.", dest.display())));
        }
        let held = lock::lock_root(&layout.root, None, Mode::Exclusive, options.lock_wait)?;
        pause!("archive_locked");
        // The root is moved by its path, so the path must still name the directory locked above.
        if lock::root_id(&layout.root)? != held.id()? {
            return Err(FlowError::RootMoved { root: layout.root.clone() });
        }
        if !reuse {
            std::fs::create_dir(dest).map_err(FlowError::io(dest))?;
        }
        let marker = layout.archived_marker();
        std::fs::write(&marker, format!("{}\n{epoch}\n", dest.display())).map_err(FlowError::io(&marker))?;
        // Until the root moves, the ledger is still live here: a failed move takes the marker
        // back, and the destination too when this call made it.
        if let Err(e) = std::fs::rename(&layout.root, dest.join(layout.name())) {
            let _ = std::fs::remove_file(&marker);
            if !reuse {
                let _ = std::fs::remove_dir(dest);
            }
            return Err(FlowError::io(&layout.root)(e));
        }
        std::fs::rename(&layout.lock, dest.join(format!("{}.lock", layout.name())))
            .map_err(FlowError::io(&layout.lock))?;
        sync_dir(dest);
        sync_dir(&layout.parent);
        Ok(())
    })();
    let _ = file.unlock();
    done
}

/// Makes a new ledger for `host`, with a new epoch, where the old root was archived or moved
/// away. Writers that opened the old ledger are refused. Refused while a root is in place.
///
/// After an [`archive`], the marker stays until the new ledger is whole. After a root was moved
/// away by hand, its lock stays until a new lock is renamed over it, so a crash before that
/// leaves the old lock (refused `root_moved`), and a crash after leaves a lock naming the new
/// stage, which [`recover`] moves into place. No normal writer starts a ledger in between.
pub fn create_fresh_ledger(root: &Path, host: &HostId, options: &Options) -> Result<String> {
    let layout = Layout::new(root)?;
    let marker = layout.archived_marker();
    let in_place = || layout::exists(&layout.root);
    let still_there = || refused("The flows root is still in place. Archive it first.");
    if in_place()? {
        return Err(still_there());
    }
    pause!("fresh_checked");
    let finished = |epoch: String| {
        pause!("root_in_place");
        let _ = std::fs::remove_file(&marker);
        sync_dir(&layout.parent);
        Ok(epoch)
    };
    let Some(file) = existing_lock(&layout)? else {
        // With no lock and no root, only another fresh ledger can race this one: whichever loses
        // is refused by the lock the other linked.
        let file = genesis(&layout, host, true)?.ok_or_else(|| refused("Another fresh ledger was made meanwhile."))?;
        let _ = file.unlock();
        return finished(root_epoch(&layout.root)?);
    };
    // A root moved away by hand left its lock behind: a new lock replaces it.
    under_lock(&file, &layout, options, || {
        if in_place()? {
            return Err(still_there());
        }
        let epoch = random_epoch()?;
        let stage = stage_root(&layout, host, &epoch)?;
        install_lock(&layout, &epoch)?;
        std::fs::rename(&stage, &layout.root).map_err(FlowError::io(&layout.root))?;
        finished(epoch)
    })
}

/// Retires instance `id`: under the lock, `instances/<id>` moves to `retired/<id>` with its log
/// as it was, through the root directory the guard holds. The id is never written again.
pub fn retire(ledger: &mut Ledger, id: &InstanceId) -> Result<CacheWrite> {
    let guard = ledger.lock()?;
    let root = guard.root();
    refuse_retired(root, id)?;
    let instances = instances(root)?;
    if !instances.is_dir(id.as_str())? {
        return Err(FlowError::UnknownInstance { instance: id.to_string() });
    }
    let retired =
        root.dir(RETIRED)?.ok_or_else(|| FlowError::LedgerDamaged { detail: "retired/ is missing".into() })?;
    instances.rename(id.as_str(), &retired, id.as_str())?;
    retired.sync();
    instances.sync();
    Ok(guard.sync_projection())
}

/// Cuts a torn last line from instance `id`'s log, back to its last verified line, so appends
/// resume. Returns the bytes removed. Refused when the log verifies or its chain is broken.
pub fn truncate_torn_tail(ledger: &mut Ledger, id: &InstanceId) -> Result<u64> {
    let guard = ledger.lock()?;
    let root = guard.root();
    refuse_retired(root, id)?;
    let unknown = || FlowError::UnknownInstance { instance: id.to_string() };
    let dir = instances(root)?.dir(id.as_str())?.ok_or_else(unknown)?;
    let bytes = dir.read(EVENTS, crate::log::MAX_LOG)?.ok_or_else(unknown)?;
    match log::verify_prefix(id, &bytes) {
        Ok(_) => Err(refused(format!("Instance {id}'s log verifies to its end, so nothing was truncated."))),
        Err((FlowError::TornTail { .. }, prefix)) => {
            let file = dir.file(EVENTS, OFlags::WRONLY)?.ok_or_else(unknown)?;
            file.set_len(prefix.len).and_then(|()| file.sync_all()).map_err(FlowError::io(dir.path.join(EVENTS)))?;
            let _ = guard.sync_projection();
            Ok(bytes.len() as u64 - prefix.len)
        }
        Err((e, _)) => Err(e),
    }
}

/// Rebuilds the SQLite projection from the logs.
pub fn rebuild_projection(ledger: &mut Ledger) -> Result<()> {
    ledger.lock()?.rebuild_projection()
}
