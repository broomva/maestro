//! Where everything lives, and what state the identity files are in.

use crate::error::{FlowError, Result};
use crate::ids::{HostId, InstanceId};
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

    /// Where a genesis builds the root for `epoch` before renaming it into place.
    pub(crate) fn stage(&self, epoch: &str) -> PathBuf {
        self.parent.join(format!(".{}.genesis-{epoch}", self.name))
    }

    /// A private name for the lock file before it is linked into place.
    pub(crate) fn lock_stage(&self, epoch: &str) -> PathBuf {
        self.parent.join(format!(".{}.lock.genesis-{epoch}", self.name))
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

/// What the lock file and the root say, read without writing anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum State {
    /// No lock and no root: only here may a genesis run.
    Empty,
    /// A whole ledger: the lock names the root's epoch.
    Live { host: String, epoch: String },
}

/// Reads a small identity file; `None` when it does not exist.
fn read_small(path: &Path) -> Result<Option<String>> {
    match std::fs::File::open(path) {
        Ok(f) => {
            let mut text = String::new();
            f.take(4096).read_to_string(&mut text).map_err(FlowError::io(path))?;
            Ok(Some(text.trim().to_string()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(FlowError::io(path)(e)),
    }
}

fn exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(FlowError::io(path)(e)),
    }
}

pub(crate) fn is_epoch(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
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
