//! Where everything beside the root lives, and what state the identity files are in. Paths under
//! the root are never opened from here for writing: writes go through descriptors ([`crate::at`]).

use crate::error::{FlowError, Result};
use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};

pub(crate) const HOST_FILE: &str = "HOST";
pub(crate) const EPOCH_FILE: &str = "EPOCH";
pub(crate) const INSTANCES: &str = "instances";
pub(crate) const RETIRED: &str = "retired";
pub(crate) const EVENTS: &str = "events.jsonl";
pub(crate) const PROJECTION: &str = "projection.sqlite";
/// The stop gate's refusal counts, one file per session (`kernel::stop`).
pub(crate) const STOP_GATE: &str = "stop-gate";

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
        let name = root.file_name().and_then(|n| n.to_str()).ok_or_else(|| FlowError::Invalid {
            what: "Flows root",
            value: root.display().to_string(),
            reason: "it needs a plain final name",
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

    /// A lock written whole under a private name for `epoch`, before it is linked (`genesis`) or
    /// renamed (`new`) into place.
    pub(crate) fn private_lock(&self, kind: &str, epoch: &str) -> PathBuf {
        self.parent.join(format!(".{}.lock.{kind}-{epoch}", self.name))
    }

    /// Left beside the root by `operator::archive`: a ledger was here, so no writer starts another.
    pub(crate) fn archived_marker(&self) -> PathBuf {
        self.parent.join(format!("{}.archived", self.name))
    }
}

/// Reads a small identity file (the lock, `HOST`, `EPOCH`); `None` when it does not exist. Only
/// read, and every write under the root goes through descriptors, so a link here can mislead a
/// check at worst, never redirect a write.
pub(crate) fn read_small(path: &Path) -> Result<Option<String>> {
    match File::open(path) {
        Ok(f) => {
            let mut text = String::new();
            f.take(4096).read_to_string(&mut text).map_err(FlowError::io(path))?;
            Ok(Some(text.trim().to_string()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(FlowError::io(path)(e)),
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

/// What the lock file and the root say, read without writing anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum State {
    /// No lock and no root: only here may a genesis run.
    Empty,
    /// A whole ledger: the lock names the root's epoch.
    Live { host: String, epoch: String },
}

/// The state of the ledger at `layout`. Anything but [`State::Empty`] and [`State::Live`] is a
/// refusal naming its recovery. Writes nothing.
pub(crate) fn inspect(layout: &Layout) -> Result<State> {
    let lock = read_small(&layout.lock)?;
    let lock_epoch = lock.as_deref().filter(|t| is_epoch(t));
    if !exists(&layout.root)? {
        return match (&lock, lock_epoch) {
            (None, _) => Ok(State::Empty),
            (_, Some(e)) if exists(&layout.stage(e))? => Err(FlowError::GenesisIncomplete {
                detail: format!("the lock names epoch {e} and its staged root was never moved into place"),
            }),
            (Some(_), _) => Err(FlowError::RootMoved { root: layout.root.clone() }),
        };
    }
    let host = read_small(&layout.root.join(HOST_FILE))?;
    let epoch = read_small(&layout.root.join(EPOCH_FILE))?.filter(|e| is_epoch(e));
    let (Some(host), Some(epoch)) = (host, epoch) else {
        return Err(FlowError::LedgerDamaged { detail: "the root's HOST or EPOCH is missing or unreadable".into() });
    };
    match lock_epoch {
        None => Err(FlowError::LockLost {
            lock: layout.lock.clone(),
            detail: format!(
                "the lock is {} beside a live root",
                if lock.is_some() { "not an epoch" } else { "missing" }
            ),
        }),
        Some(e) if e != epoch => Err(FlowError::EpochMismatch { expected: e.to_string(), found: epoch }),
        Some(_) => Ok(State::Live { host, epoch }),
    }
}
