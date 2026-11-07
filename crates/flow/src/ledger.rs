//! Opening the ledger, writing to it under the exclusive lock, and reading it under the shared one.

use crate::error::{FlowError, Result};
use crate::ids::{HostId, InstanceId, check_kind};
use crate::layout::{
    self, EPOCH_FILE, GENESIS, HOST_FILE, INSTANCES, Layout, RETIRED, State, nofollow, open_error, read_small, real_dir,
};
use crate::lock::{self, FileId, Mode};
use crate::log::{self, Event, InstanceSummary, Log, NewEvent};
use crate::projection;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::Path;
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

/// Creates `path` with `content`; fails when it exists.
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
pub(crate) fn stage_root(layout: &Layout, host: &HostId, epoch: &str) -> Result<std::path::PathBuf> {
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

fn open_lock_file(layout: &Layout) -> std::io::Result<File> {
    nofollow(OpenOptions::new().read(true)).open(&layout.lock)
}

/// Opens the lock file without creating it. `None` only on an empty tree: no lock, no root, no
/// archive marker, the one state a genesis may start from.
pub(crate) fn open_lock(layout: &Layout) -> Result<Option<File>> {
    match open_lock_file(layout) {
        Ok(f) => return Ok(Some(f)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(open_error(&layout.lock, e)),
    }
    // A genesis links its lock before it moves its root into place, so a root seen now means the
    // lock is there too, unless it was deleted.
    if std::fs::symlink_metadata(&layout.root).is_ok() {
        return match open_lock_file(layout) {
            Ok(f) => Ok(Some(f)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => match layout::inspect(layout)? {
                State::Empty | State::Live { .. } => {
                    Err(FlowError::LockLost { lock: layout.lock.clone(), detail: "the lock is missing".into() })
                }
            },
            Err(e) => Err(open_error(&layout.lock, e)),
        };
    }
    // No lock and no root: refused if a ledger was archived here.
    if layout::exists(&layout.archived_marker())? {
        return Err(FlowError::RootMoved { root: layout.root.clone() });
    }
    Ok(None)
}

/// Overwrites the lock's content in place: the file, and so every holder's lock, stays the same.
pub(crate) fn write_lock(file: &File, lock: &Path, content: &str) -> Result<()> {
    let mut f = file;
    let line = format!("{content}\n");
    f.seek(SeekFrom::Start(0))
        .and_then(|_| f.write_all(line.as_bytes()))
        .and_then(|()| file.set_len(line.len() as u64))
        .and_then(|()| file.sync_all())
        .map_err(FlowError::io(lock))
}

/// Creates a ledger on an empty tree, bound to `host`. Returns the lock file, held exclusive,
/// when this writer won; `None` when another first writer linked its lock first.
///
/// The lock is written and locked under a private name, then linked into place: a hard link
/// fails when the lock exists, so exactly one first writer is elected. It says
/// `genesis <epoch>` until [`finish_genesis`] has the root in place, so a crash at any point
/// leaves a state refused `genesis_incomplete` until `operator::recover`.
pub(crate) fn genesis(layout: &Layout, host: &HostId) -> Result<Option<File>> {
    std::fs::create_dir_all(&layout.parent).map_err(FlowError::io(&layout.parent))?;
    let epoch = random_epoch()?;
    let private = layout.parent.join(format!(".{}.lock.genesis-{epoch}", layout.name()));
    let mut file =
        OpenOptions::new().read(true).write(true).create_new(true).open(&private).map_err(FlowError::io(&private))?;
    let linked = (|| {
        lock::take(&file, Mode::Exclusive, Duration::ZERO, &private)?;
        file.write_all(format!("{GENESIS}{epoch}\n").as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(FlowError::io(&private))?;
        match std::fs::hard_link(&private, &layout.lock) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(FlowError::io(&layout.lock)(e)),
        }
    })();
    let _ = std::fs::remove_file(&private);
    if !linked? {
        return Ok(None);
    }
    pause!("genesis_linked");
    finish_genesis(layout, &file, host, &epoch)?;
    Ok(Some(file))
}

/// Builds the root for `epoch` at its stage, moves it into place, and only then makes the held
/// lock name the epoch.
pub(crate) fn finish_genesis(layout: &Layout, lock_file: &File, host: &HostId, epoch: &str) -> Result<()> {
    let stage = stage_root(layout, host, epoch)?;
    std::fs::rename(&stage, &layout.root).map_err(FlowError::io(&layout.root))?;
    sync_dir(&layout.parent);
    write_lock(lock_file, &layout.lock, epoch)
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
            Ok((epoch, lock::root_id(&layout.root)?))
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

    /// Takes the exclusive lock: the lock file, then the root directory itself. Checks the
    /// ledger is still the one this writer opened.
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
            lock::lock_root(&self.layout.root, Mode::Exclusive, self.options.lock_wait)
        })();
        match checked {
            Ok(root_dir) => Ok(WriteGuard { ledger: self, root_dir }),
            Err(e) => {
                let _ = self.lock_file.unlock();
                Err(e)
            }
        }
    }
}

/// The exclusive lock on the ledger. Every write happens through it, and every write first
/// checks the lock file, the root and its epoch are still the ones the guard was taken on.
/// Dropping it releases the lock, which the kernel does before any slow external work.
#[derive(Debug)]
pub struct WriteGuard<'a> {
    ledger: &'a mut Ledger,
    root_dir: File,
}

impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        let _ = self.root_dir.unlock();
        let _ = self.ledger.lock_file.unlock();
    }
}

/// Refuses an id whose instance was retired. Stats `retired/<id>`, never reads or writes in it.
pub(crate) fn refuse_retired(layout: &Layout, id: &InstanceId) -> Result<()> {
    let retired = layout.retired_dir(id);
    match std::fs::symlink_metadata(&retired) {
        Ok(_) => Err(FlowError::Retired { instance: id.to_string() }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(FlowError::io(retired)(e)),
    }
}

/// `instances/` must be a real directory: only a genesis makes it.
fn instances_dir(layout: &Layout) -> Result<std::path::PathBuf> {
    let dir = layout.root.join(INSTANCES);
    if !real_dir(&dir)? {
        return Err(FlowError::LedgerDamaged { detail: "instances/ is missing".into() });
    }
    Ok(dir)
}

/// The log's bytes; `None` when the instance has none. Follows no link under the root.
pub(crate) fn read_log(layout: &Layout, id: &InstanceId) -> Result<Option<Vec<u8>>> {
    instances_dir(layout)?;
    if !real_dir(&layout.instance_dir(id))? {
        return Ok(None);
    }
    let path = layout.events(id);
    let mut file = match nofollow(OpenOptions::new().read(true)).open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(open_error(&path, e)),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(FlowError::io(&path))?;
    Ok(Some(bytes))
}

/// An instance's verified log, refusing a retired one.
pub(crate) fn load_log(layout: &Layout, id: &InstanceId) -> Result<Log> {
    refuse_retired(layout, id)?;
    match read_log(layout, id)? {
        Some(bytes) => log::verify(id, &bytes),
        None => Err(FlowError::UnknownInstance { instance: id.to_string() }),
    }
}

/// The live instances, by name. Entries that are not real directories are not instances.
pub(crate) fn list_instances(layout: &Layout) -> Result<Vec<InstanceId>> {
    let dir = instances_dir(layout)?;
    let entries = std::fs::read_dir(&dir).map_err(FlowError::io(&dir))?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(FlowError::io(&dir))?;
        if !entry.file_type().map_err(FlowError::io(entry.path()))?.is_dir() {
            continue;
        }
        if let Some(id) = entry.file_name().to_str().and_then(|n| InstanceId::new(n).ok()) {
            out.push(id);
        }
    }
    out.sort();
    Ok(out)
}

impl WriteGuard<'_> {
    pub(crate) fn layout(&self) -> &Layout {
        &self.ledger.layout
    }

    pub(crate) fn epoch(&self) -> &str {
        &self.ledger.epoch
    }

    /// Re-checks, at the moment of a write, what the guard was taken on: the lock path is still
    /// the file this writer locked, the root is still the directory it opened, and its epoch is
    /// unchanged. Every write calls it first; nothing under the root is written otherwise.
    pub(crate) fn verify(&self) -> Result<()> {
        let ledger = &*self.ledger;
        if !lock::same_file(&ledger.lock_file, &ledger.layout.lock)? {
            return Err(FlowError::LockLost {
                lock: ledger.layout.lock.clone(),
                detail: "the lock file was replaced while this writer held it".into(),
            });
        }
        if lock::root_id(&ledger.layout.root)? != ledger.root_id {
            return Err(FlowError::RootMoved { root: ledger.layout.root.clone() });
        }
        match read_small(&ledger.layout.root.join(EPOCH_FILE))? {
            Some(found) if found == ledger.epoch => Ok(()),
            Some(found) => Err(FlowError::EpochMismatch { expected: ledger.epoch.clone(), found }),
            None => Err(FlowError::LedgerDamaged { detail: "the root's EPOCH is missing".into() }),
        }
    }

    /// The live instances.
    pub fn instances(&self) -> Result<Vec<InstanceId>> {
        list_instances(self.layout())
    }

    /// An instance's verified log.
    pub fn log(&self, id: &InstanceId) -> Result<Log> {
        load_log(self.layout(), id)
    }

    /// Appends `event` to instance `id` if its log is at `expected_seq` (0 creates the
    /// instance). This, under the lock, is the one concurrency rule: a caller that read the log,
    /// released the lock and came back is refused `stale_seq` if anything was appended meanwhile.
    pub fn append(&mut self, id: &InstanceId, expected_seq: u64, event: NewEvent) -> Result<Appended> {
        check_kind(&event.kind)?;
        self.verify()?;
        let layout = self.layout().clone();
        refuse_retired(&layout, id)?;
        let existing = read_log(&layout, id)?;
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
        let dir = layout.instance_dir(id);
        let path = layout.events(id);
        pause!("append_checked");
        let written = (|| {
            // Only the instance's own directory is ever made here, never the root or `instances/`.
            if !real_dir(&dir)? {
                std::fs::create_dir(&dir).map_err(FlowError::io(&dir))?;
            }
            let mut file = nofollow(OpenOptions::new().append(true).create_new(created))
                .open(&path)
                .map_err(|e| open_error(&path, e))?;
            file.write_all(&line).and_then(|()| file.sync_data()).map_err(FlowError::io(&path))
        })();
        if let Err(e) = written {
            // A root that moved, or a lock replaced, mid-write reads as that, not as a bare io error.
            self.verify()?;
            return Err(e);
        }
        if created {
            sync_dir(&dir);
            sync_dir(&layout.root.join(INSTANCES));
        }
        let cache = projection::sync(&layout, self.epoch()).into();
        Ok(Appended { event: stored, cache })
    }

    /// Rebuilds the whole projection from the logs.
    pub fn rebuild_projection(&mut self) -> Result<()> {
        self.verify()?;
        let layout = self.layout().clone();
        projection::rebuild(&layout, self.epoch())
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
                State::Live { host, epoch } => Ok((host, epoch)),
                State::Empty => Err(FlowError::RootMoved { root: layout.root.clone() }),
            }
        })();
        match checked {
            Ok((host, epoch)) => Ok(ReadGuard { layout: layout.clone(), file, host, epoch }),
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
        list_instances(&self.layout)
    }

    /// An instance's verified log. A torn tail is `torn_tail`: unread, never a shorter log.
    pub fn log(&self, id: &InstanceId) -> Result<Log> {
        load_log(&self.layout, id)
    }

    /// Every row of the projection, read-only; `None` when there is no projection file. For tests.
    #[doc(hidden)]
    pub fn projection_dump(&self) -> Result<Option<projection::ProjectionDump>> {
        projection::dump(&self.layout)
    }

    /// The instance's summary from its log, compared with the projection. The log wins.
    pub fn summary(&self, id: &InstanceId) -> Result<Summary> {
        let from_log = self.log(id)?.summary();
        let cache = match projection::read(&self.layout, &self.epoch, id) {
            Ok(Some(Some(row))) if row.state == "ok" && row.summary == from_log => CacheState::Agrees,
            Ok(Some(row)) => CacheState::Disagrees(row.map(|r| r.summary)),
            Ok(None) => CacheState::Missing,
            Err(e) => CacheState::Unreadable(e.to_string()),
        };
        Ok(Summary { summary: from_log, cache })
    }
}
