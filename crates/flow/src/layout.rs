//! Where everything lives, what state the identity files are in, and how paths under the root
//! are opened without following a symbolic link out of it.

use crate::error::{FlowError, Result};
use crate::ids::{HostId, InstanceId};
use std::fs::{File, OpenOptions};
use std::io::Read as _;
use std::path::{Path, PathBuf};

pub(crate) const HOST_FILE: &str = "HOST";
pub(crate) const EPOCH_FILE: &str = "EPOCH";
pub(crate) const INSTANCES: &str = "instances";
pub(crate) const RETIRED: &str = "retired";
pub(crate) const EVENTS: &str = "events.jsonl";
pub(crate) const PROJECTION: &str = "projection.sqlite";

/// The paths of one flows root.
#[derive(Debug, Clone)]
pub(crate) struct Layout {
    pub(crate) root: PathBuf,
    pub(crate) parent: PathBuf,
    pub(crate) lock: PathBuf,
    name: String,
}

impl Layout {
    pub(crate) fn new(root: &Path) -> Result<Self> {
        let name = root.file_name().and_then(|n| n.to_str()).filter(|n| !n.starts_with('.')).ok_or_else(|| {
            FlowError::Invalid {
                what: "Flows root",
                value: root.display().to_string(),
                reason: "it needs a plain final name",
            }
        })?;
        let parent = match root.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        Ok(Self { root: root.to_path_buf(), lock: parent.join(format!("{name}.lock")), name: name.to_string(), parent })
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Where a genesis builds the root for `epoch` before renaming it into place.
    pub(crate) fn stage(&self, epoch: &str) -> PathBuf {
        self.parent.join(format!(".{}.genesis-{epoch}", self.name))
    }

    /// The genesis's own lock file for `epoch`, held by the genesis from before its stage exists
    /// until its root is in place, then linked as the lock.
    pub(crate) fn lock_stage(&self, epoch: &str) -> PathBuf {
        self.parent.join(format!(".{}.lock.genesis-{epoch}", self.name))
    }

    /// Left beside the root by `operator::archive`: a ledger was here, so no writer starts another.
    pub(crate) fn archived_marker(&self) -> PathBuf {
        self.parent.join(format!("{}.archived", self.name))
    }

    pub(crate) fn instance_dir(&self, id: &InstanceId) -> PathBuf {
        self.root.join(INSTANCES).join(id.as_str())
    }

    pub(crate) fn events(&self, id: &InstanceId) -> PathBuf {
        self.instance_dir(id).join(EVENTS)
    }

    pub(crate) fn retired_dir(&self, id: &InstanceId) -> PathBuf {
        self.root.join(RETIRED).join(id.as_str())
    }

    pub(crate) fn projection(&self) -> PathBuf {
        self.root.join(PROJECTION)
    }
}

/// `opts` with `O_NOFOLLOW`: opening a symbolic link fails instead of following it.
pub(crate) fn nofollow(opts: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    opts
}

/// An open error, reading a refused symbolic link as [`FlowError::Symlink`].
pub(crate) fn open_error(path: &Path, e: std::io::Error) -> FlowError {
    #[cfg(unix)]
    if e.raw_os_error() == Some(libc::ELOOP) {
        return FlowError::Symlink { path: path.to_path_buf() };
    }
    FlowError::io(path)(e)
}

/// Whether `path` is a real directory: `false` when absent, refused when it is a link or a file.
pub(crate) fn real_dir(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => Err(FlowError::Symlink { path: path.to_path_buf() }),
        Ok(m) if m.is_dir() => Ok(true),
        Ok(_) => Err(FlowError::LedgerDamaged { detail: format!("{} is not a directory", path.display()) }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(FlowError::io(path)(e)),
    }
}

/// Reads a small identity file without following a link; `None` when it does not exist.
pub(crate) fn read_small(path: &Path) -> Result<Option<String>> {
    match nofollow(OpenOptions::new().read(true)).open(path) {
        Ok(f) => {
            let mut text = String::new();
            f.take(4096).read_to_string(&mut text).map_err(FlowError::io(path))?;
            Ok(Some(text.trim().to_string()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(open_error(path, e)),
    }
}

pub(crate) fn exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(FlowError::io(path)(e)),
    }
}

pub(crate) fn is_epoch(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Stages left by a genesis that crashed before linking its lock. A genesis in flight holds its
/// lock stage locked from before its stage exists, so a stage whose lock stage is missing or
/// unlocked is an orphan. Writes nothing (taking and dropping a file lock changes no file).
pub(crate) fn orphan_stages(layout: &Layout) -> Result<Vec<PathBuf>> {
    let prefix = format!(".{}.genesis-", layout.name);
    let entries = match std::fs::read_dir(&layout.parent) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(FlowError::io(&layout.parent)(e)),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(FlowError::io(&layout.parent))?;
        let name = entry.file_name();
        let Some(epoch) = name.to_str().and_then(|n| n.strip_prefix(&prefix)) else { continue };
        let in_flight = match File::open(layout.lock_stage(epoch)) {
            Ok(f) => matches!(f.try_lock_shared(), Err(std::fs::TryLockError::WouldBlock)),
            Err(_) => false,
        };
        if !in_flight {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(out)
}

/// What the lock file and the root say, read without writing anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum State {
    /// No lock, no root, no archive marker and no orphan stage: only here may a genesis run.
    Empty,
    /// A whole ledger: the lock names the root's epoch.
    Live { host: String, epoch: String },
}

/// The state of the ledger at `layout`. Anything but [`State::Empty`] and [`State::Live`] is a
/// refusal naming its recovery. Writes nothing.
pub(crate) fn inspect(layout: &Layout) -> Result<State> {
    let lock = read_small(&layout.lock)?;
    let root = exists(&layout.root)?;
    let lock_epoch = lock.as_deref().filter(|t| !t.is_empty());
    if let Some(e) = lock_epoch
        && !is_epoch(e)
    {
        return Err(FlowError::LockLost { lock: layout.lock.clone(), detail: "it does not hold an epoch".into() });
    }
    if !root {
        return match (lock.is_some(), lock_epoch) {
            (false, _) if exists(&layout.archived_marker())? => Err(FlowError::RootMoved { root: layout.root.clone() }),
            (false, _) if !orphan_stages(layout)?.is_empty() => Err(FlowError::GenesisIncomplete {
                detail: "a genesis stopped before linking its lock and left its stage".to_string(),
            }),
            (false, _) => Ok(State::Empty),
            (true, Some(e)) if exists(&layout.stage(e))? => Err(FlowError::GenesisIncomplete {
                detail: format!("the lock names epoch {e} and its staged root was never moved into place"),
            }),
            (true, Some(_)) => Err(FlowError::RootMoved { root: layout.root.clone() }),
            (true, None) => {
                Err(FlowError::GenesisIncomplete { detail: "the lock is empty and there is no root".to_string() })
            }
        };
    }
    if !real_dir(&layout.root)? {
        return Err(FlowError::RootMoved { root: layout.root.clone() });
    }
    let host = read_small(&layout.root.join(HOST_FILE))?.filter(|h| HostId::new(h.as_str()).is_ok());
    let epoch = read_small(&layout.root.join(EPOCH_FILE))?.filter(|e| is_epoch(e));
    let (Some(host), Some(epoch)) = (host, epoch) else {
        return Err(FlowError::LedgerDamaged { detail: "the root's HOST or EPOCH is missing or unreadable".into() });
    };
    match lock_epoch {
        None => Err(FlowError::LockLost {
            lock: layout.lock.clone(),
            detail: format!("the lock is {} beside a live root", if lock.is_some() { "empty" } else { "missing" }),
        }),
        Some(e) if e != epoch => Err(FlowError::EpochMismatch { expected: e.to_string(), found: epoch }),
        Some(_) => Ok(State::Live { host, epoch }),
    }
}
