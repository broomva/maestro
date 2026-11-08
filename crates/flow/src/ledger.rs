//! Opening the ledger, writing to it under the exclusive lock, and reading it under the shared one.

use crate::at::Dir;
use crate::error::{FlowError, Result};
use crate::ids::{HostId, InstanceId, check_kind};
use crate::layout::{self, EPOCH_FILE, EVENTS, HOST_FILE, INSTANCES, Layout, RETIRED, State};
use crate::lock::{self, FileId, Mode};
use crate::log::{self, Event, InstanceSummary, Log, NewEvent};
use crate::projection;
use rustix::fs::OFlags;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long to wait for the lock.
#[derive(Debug, Clone)]
pub struct Options {
    pub lock_wait: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self { lock_wait: Duration::from_secs(10) }
    }
}

/// Whether the projection took a write. The log is written first and is the truth, so a cache
/// that could not be written never undoes an append; the next writer or a rebuild repairs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheWrite {
    Updated,
    Failed(String),
}

impl<T> From<Result<T>> for CacheWrite {
    fn from(r: Result<T>) -> Self {
        match r {
            Ok(_) => CacheWrite::Updated,
            Err(e) => CacheWrite::Failed(e.to_string()),
        }
    }
}

/// An appended event and what became of the projection.
#[derive(Debug, Clone, PartialEq)]
pub struct Appended {
    pub event: Event,
    pub cache: CacheWrite,
}

/// A writer's handle on one flows root, bound to its host, to the epoch it opened and to the
/// root directory it opened.
///
/// Holding a `Ledger` holds no lock. [`Ledger::lock`] takes the exclusive lock and returns a
/// [`WriteGuard`]; dropping the guard releases it.
#[derive(Debug)]
pub struct Ledger {
    pub(crate) layout: Layout,
    host: HostId,
    epoch: String,
    root_id: FileId,
    lock_file: File,
    pub(crate) options: Options,
}

pub(crate) fn random_epoch() -> Result<String> {
    let mut buf = [0u8; 16];
    File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)).map_err(FlowError::io("/dev/urandom"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Creates `path` with `content`, whole and synced; fails when it exists.
pub(crate) fn write_new(path: &Path, content: &str) -> Result<()> {
    let mut f = OpenOptions::new().write(true).create_new(true).open(path).map_err(FlowError::io(path))?;
    f.write_all(content.as_bytes()).and_then(|()| f.sync_all()).map_err(FlowError::io(path))
}

/// Best effort: makes a directory's entries durable.
pub(crate) fn sync_dir(path: &Path) {
    if let Ok(dir) = File::open(path) {
        let _ = dir.sync_all();
    }
}

/// Builds a whole root for `epoch` at its stage path: `HOST`, `EPOCH`, `instances/`, `retired/`.
/// A stage no lock names is inert: nothing reads it, and `operator::recover` sweeps it.
pub(crate) fn stage_root(layout: &Layout, host: &HostId, epoch: &str) -> Result<PathBuf> {
    let stage = layout.stage(epoch);
    std::fs::create_dir(&stage).map_err(FlowError::io(&stage))?;
    write_new(&stage.join(HOST_FILE), &format!("{host}\n"))?;
    write_new(&stage.join(EPOCH_FILE), &format!("{epoch}\n"))?;
    for dir in [INSTANCES, RETIRED] {
        std::fs::create_dir(stage.join(dir)).map_err(FlowError::io(stage.join(dir)))?;
    }
    sync_dir(&stage);
    Ok(stage)
}

/// Puts a lock naming `epoch` in place. The lock is never rewritten: a new one is written whole
/// under a private name, synced, and renamed over it. A crash before the rename leaves the old
/// lock as it was. A holder of the old lock file sees `lock_lost` afterwards.
pub(crate) fn install_lock(layout: &Layout, epoch: &str) -> Result<()> {
    let private = layout.private_lock("new", epoch);
    let _ = std::fs::remove_file(&private);
    write_new(&private, &format!("{epoch}\n"))?;
    pause!("lock_written");
    std::fs::rename(&private, &layout.lock).map_err(FlowError::io(&layout.lock))?;
    sync_dir(&layout.parent);
    Ok(())
}

/// Opens the lock file without creating it. `None` only on an empty tree: no lock, no root, no
/// archive marker, the one state a genesis may start from.
pub(crate) fn open_lock(layout: &Layout) -> Result<Option<File>> {
    let open = || File::open(&layout.lock);
    match open() {
        Ok(f) => return Ok(Some(f)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(FlowError::io(&layout.lock)(e)),
    }
    // A genesis links its lock before it moves its root into place, so a root seen now means the
    // lock is there too, unless it was deleted.
    if std::fs::symlink_metadata(&layout.root).is_ok() {
        return match open() {
            Ok(f) => Ok(Some(f)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(FlowError::LockLost { lock: layout.lock.clone(), detail: "the lock is missing".into() })
            }
            Err(e) => Err(FlowError::io(&layout.lock)(e)),
        };
    }
    // No lock and no root: refused if a ledger was archived here.
    if layout::exists(&layout.archived_marker())? {
        return Err(FlowError::RootMoved { root: layout.root.clone() });
    }
    Ok(None)
}

/// Creates a ledger on an empty tree, bound to `host`. Returns the lock file, held exclusive,
/// when this writer won; `None` when another first writer linked its lock first.
///
/// The root is built whole at a stage path first. Then the lock, written whole and locked under
/// a private name, is linked into place: a hard link fails when the lock exists, so exactly one
/// first writer is elected. Only then is the stage renamed into place. A crash between the two
/// leaves a lock naming a stage, refused `genesis_incomplete` until `operator::recover` moves it
/// in; a crash before the link leaves only an inert stage.
pub(crate) fn genesis(layout: &Layout, host: &HostId) -> Result<Option<File>> {
    std::fs::create_dir_all(&layout.parent).map_err(FlowError::io(&layout.parent))?;
    let epoch = random_epoch()?;
    let stage = stage_root(layout, host, &epoch)?;
    let private = layout.private_lock("genesis", &epoch);
    let linked = (|| {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&private)
            .map_err(FlowError::io(&private))?;
        lock::take(&file, Mode::Exclusive, Duration::ZERO, &private)?;
        file.write_all(format!("{epoch}\n").as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(FlowError::io(&private))?;
        match std::fs::hard_link(&private, &layout.lock) {
            Ok(()) => Ok(Some(file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(e) => Err(FlowError::io(&layout.lock)(e)),
        }
    })();
    let _ = std::fs::remove_file(&private);
    let Some(file) = linked? else {
        let _ = std::fs::remove_dir_all(&stage);
        return Ok(None);
    };
    pause!("genesis_linked");
    std::fs::rename(&stage, &layout.root).map_err(FlowError::io(&layout.root))?;
    sync_dir(&layout.parent);
    Ok(Some(file))
}

/// The one identity rule every writer passes under the lock: a live ledger, on this host, and
/// (once opened) the epoch it opened.
fn check_identity(state: State, layout: &Layout, host: &HostId, epoch: Option<&str>) -> Result<String> {
    let (owner, found) = match state {
        State::Live { host, epoch } => (host, epoch),
        State::Empty => return Err(FlowError::RootMoved { root: layout.root.clone() }),
    };
    // The epoch first: a writer of a ledger that was replaced is refused for that, whatever host the new one has.
    if let Some(expected) = epoch
        && found != expected
    {
        return Err(FlowError::EpochMismatch { expected: expected.to_string(), found });
    }
    if owner != host.as_str() {
        return Err(FlowError::ForeignHost { host: host.to_string(), owner });
    }
    Ok(found)
}

impl Ledger {
    /// Opens the ledger at `root` for `host`. On an empty tree (no lock, no root, no archive
    /// marker) this is the genesis that binds the root to `host`; anywhere else it only checks
    /// what is there.
    pub fn open(root: &Path, host: &HostId, options: Options) -> Result<Ledger> {
        let layout = Layout::new(root)?;
        let file = match open_lock(&layout)? {
            Some(f) => {
                pause!("lock_opened");
                f
            }
            None => match genesis(&layout, host)? {
                Some(f) => f,
                None => open_lock(&layout)?.ok_or_else(|| FlowError::LockLost {
                    lock: layout.lock.clone(),
                    detail: "another first writer's lock vanished".into(),
                })?,
            },
        };
        // A racing first writer that lost waits here until the winner's root is in place. A
        // no-op for the winner, which holds it exclusive already.
        lock::take(&file, Mode::Shared, options.lock_wait, &layout.lock)?;
        let checked = (|| {
            if !lock::same_file(&file, &layout.lock)? {
                return Err(FlowError::LockLost {
                    lock: layout.lock.clone(),
                    detail: "the lock file was replaced while this writer opened it".into(),
                });
            }
            let epoch = check_identity(layout::inspect(&layout)?, &layout, host, None)?;
            Ok((epoch, Dir::open_root(&layout.root)?.id()?))
        })();
        let _ = file.unlock();
        let (epoch, root_id) = checked?;
        Ok(Ledger { layout, host: host.clone(), epoch, root_id, lock_file: file, options })
    }

    pub fn root(&self) -> &Path {
        &self.layout.root
    }

    pub fn host(&self) -> &HostId {
        &self.host
    }

    /// The epoch this writer opened. A root recreated with another epoch is refused.
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// Takes the exclusive lock: the lock file, then the root directory itself, which must be
    /// the very directory this writer opened. The guard writes only through that directory.
    pub fn lock(&mut self) -> Result<WriteGuard<'_>> {
        lock::take(&self.lock_file, Mode::Exclusive, self.options.lock_wait, &self.layout.lock)?;
        let checked = (|| {
            check_identity(layout::inspect(&self.layout)?, &self.layout, &self.host, Some(&self.epoch))?;
            if !lock::same_file(&self.lock_file, &self.layout.lock)? {
                return Err(FlowError::LockLost {
                    lock: self.layout.lock.clone(),
                    detail: "the lock file was deleted or replaced after this writer opened it".into(),
                });
            }
            lock::lock_root(&self.layout.root, Some(self.root_id), Mode::Exclusive, self.options.lock_wait)
        })();
        match checked {
            Ok(root) => Ok(WriteGuard { ledger: self, root }),
            Err(e) => {
                let _ = self.lock_file.unlock();
                Err(e)
            }
        }
    }
}

/// The exclusive lock on the ledger. It holds the root directory it verified, and every write
/// goes through that descriptor: a root moved or replaced by path afterwards cannot redirect a
/// write. Dropping it releases the lock, which the kernel does before any slow external work.
#[derive(Debug)]
pub struct WriteGuard<'a> {
    ledger: &'a mut Ledger,
    root: Dir,
}

impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        let _ = self.ledger.lock_file.unlock();
    }
}

/// `instances/` under `root`: only a genesis makes it.
pub(crate) fn instances(root: &Dir) -> Result<Dir> {
    root.dir(INSTANCES)?.ok_or_else(|| FlowError::LedgerDamaged { detail: "instances/ is missing".into() })
}

/// Refuses an id whose instance was retired. Stats `retired/<id>`, never opens anything in it.
pub(crate) fn refuse_retired(root: &Dir, id: &InstanceId) -> Result<()> {
    match root.dir(RETIRED)? {
        Some(retired) if retired.exists(id.as_str())? => Err(FlowError::Retired { instance: id.to_string() }),
        _ => Ok(()),
    }
}

/// The log's bytes; `None` when the instance has none.
pub(crate) fn read_log(root: &Dir, id: &InstanceId) -> Result<Option<Vec<u8>>> {
    match instances(root)?.dir(id.as_str())? {
        Some(dir) => dir.read(EVENTS),
        None => Ok(None),
    }
}

/// An instance's verified log, refusing a retired one.
pub(crate) fn load_log(root: &Dir, id: &InstanceId) -> Result<Log> {
    refuse_retired(root, id)?;
    match read_log(root, id)? {
        Some(bytes) => log::verify(id, &bytes),
        None => Err(FlowError::UnknownInstance { instance: id.to_string() }),
    }
}

/// The live instances: the real directories under `instances/` with a valid id.
pub(crate) fn list_instances(root: &Dir) -> Result<Vec<InstanceId>> {
    Ok(instances(root)?.subdirs()?.into_iter().filter_map(|n| InstanceId::new(n).ok()).collect())
}

impl WriteGuard<'_> {
    pub(crate) fn root(&self) -> &Dir {
        &self.root
    }

    /// Brings the projection up to the logs. The cache needs a path (SQLite), so it is written
    /// only while the root's path still names the directory this guard holds; otherwise the
    /// write is skipped and reported. It is a cache: the log wins either way.
    pub(crate) fn sync_projection(&self) -> CacheWrite {
        projection::sync(&self.root, &self.ledger.layout, &self.ledger.epoch, self.ledger.root_id).into()
    }

    /// The live instances.
    pub fn instances(&self) -> Result<Vec<InstanceId>> {
        list_instances(&self.root)
    }

    /// An instance's verified log.
    pub fn log(&self, id: &InstanceId) -> Result<Log> {
        load_log(&self.root, id)
    }

    /// Appends `event` to instance `id` if its log is at `expected_seq` (0 creates the
    /// instance). This, under the lock, is the one concurrency rule: a caller that read the log,
    /// released the lock and came back is refused `stale_seq` if anything was appended meanwhile.
    pub fn append(&mut self, id: &InstanceId, expected_seq: u64, event: NewEvent) -> Result<Appended> {
        check_kind(&event.kind)?;
        refuse_retired(&self.root, id)?;
        let instances = instances(&self.root)?;
        let dir = instances.dir(id.as_str())?;
        let existing = match &dir {
            Some(dir) => dir.read(EVENTS)?,
            None => None,
        };
        let created = existing.is_none();
        let log = match existing {
            Some(bytes) => log::verify(id, &bytes)?,
            None if expected_seq == 0 => Log { instance: id.clone(), events: Vec::new(), last_hash: None },
            None => return Err(FlowError::UnknownInstance { instance: id.to_string() }),
        };
        if expected_seq != log.seq() {
            return Err(FlowError::StaleSeq { instance: id.to_string(), expected: expected_seq, current: log.seq() });
        }
        let (stored, line) = log::next_line(&log, &event)?;
        pause!("append_checked");
        let dir = match dir {
            Some(dir) => dir,
            None => {
                // Only the instance's own directory is ever made here, inside the held root.
                instances.mkdir(id.as_str())?;
                instances.dir(id.as_str())?.ok_or_else(|| FlowError::UnknownInstance { instance: id.to_string() })?
            }
        };
        let mut file = dir
            .file(EVENTS, OFlags::WRONLY | OFlags::APPEND | OFlags::CREATE)?
            .ok_or_else(|| FlowError::UnknownInstance { instance: id.to_string() })?;
        file.write_all(&line).and_then(|()| file.sync_data()).map_err(FlowError::io(dir.path.join(EVENTS)))?;
        if created {
            dir.sync();
            instances.sync();
        }
        Ok(Appended { event: stored, cache: self.sync_projection() })
    }

    /// Rebuilds the whole projection from the logs.
    pub fn rebuild_projection(&mut self) -> Result<()> {
        projection::rebuild(&self.root, &self.ledger.layout, &self.ledger.epoch, self.ledger.root_id)
    }
}

/// A reader's handle on one flows root. Reading never creates, writes or repairs anything.
#[derive(Debug, Clone)]
pub struct Reader {
    layout: Layout,
    options: Options,
}

impl Reader {
    pub fn new(root: &Path, options: Options) -> Result<Reader> {
        Ok(Reader { layout: Layout::new(root)?, options })
    }

    /// Takes the shared lock and checks there is a whole ledger to read. Reads need no more: a
    /// write in flight reads as a torn tail, never as a shorter log.
    pub fn lock(&self) -> Result<ReadGuard> {
        let layout = &self.layout;
        let file = match open_lock(layout)? {
            Some(f) => f,
            None => return Err(FlowError::NoLedger { root: layout.root.clone() }),
        };
        pause!("lock_opened");
        lock::take(&file, Mode::Shared, self.options.lock_wait, &layout.lock)?;
        let checked = (|| {
            if !lock::same_file(&file, &layout.lock)? {
                return Err(FlowError::LockLost {
                    lock: layout.lock.clone(),
                    detail: "the lock file was replaced while this reader opened it".into(),
                });
            }
            match layout::inspect(layout)? {
                State::Live { host, epoch } => Ok((host, epoch, Dir::open_root(&layout.root)?)),
                State::Empty => Err(FlowError::RootMoved { root: layout.root.clone() }),
            }
        })();
        match checked {
            Ok((host, epoch, root)) => Ok(ReadGuard { layout: layout.clone(), root, file, host, epoch }),
            Err(e) => {
                let _ = file.unlock();
                Err(e)
            }
        }
    }
}

/// The shared lock on the ledger. Writers wait while any is held; dropping it releases it.
#[derive(Debug)]
pub struct ReadGuard {
    layout: Layout,
    root: Dir,
    file: File,
    host: String,
    epoch: String,
}

impl Drop for ReadGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// What the projection says of an instance, beside what its log says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheState {
    Agrees,
    /// The cache says otherwise; the log wins.
    Disagrees(Option<InstanceSummary>),
    /// No projection file.
    Missing,
    Unreadable(String),
}

/// An instance's summary, always derived from its log, and whether the cache agreed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub summary: InstanceSummary,
    pub cache: CacheState,
}

impl ReadGuard {
    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    pub fn instances(&self) -> Result<Vec<InstanceId>> {
        list_instances(&self.root)
    }

    /// An instance's verified log. A torn tail is `torn_tail`: unread, never a shorter log.
    pub fn log(&self, id: &InstanceId) -> Result<Log> {
        load_log(&self.root, id)
    }

    /// Every row of the projection, read-only; `None` when there is no projection file. For tests.
    #[doc(hidden)]
    pub fn projection_dump(&self) -> Result<Option<projection::ProjectionDump>> {
        projection::dump(&self.layout)
    }

    /// The instance's summary from its log, compared with the projection. The log wins.
    pub fn summary(&self, id: &InstanceId) -> Result<Summary> {
        let from_log = self.log(id)?.summary();
        let cache = match projection::read(&self.layout, id) {
            Ok(Some(Some(row))) if row.summary == from_log => CacheState::Agrees,
            Ok(Some(row)) => CacheState::Disagrees(row.map(|r| r.summary)),
            Ok(None) => CacheState::Missing,
            Err(e) => CacheState::Unreadable(e.to_string()),
        };
        Ok(Summary { summary: from_log, cache })
    }
}
