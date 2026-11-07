//! The flows lock: OS file locks (`flock` on unix, through `std::fs::File::lock`) on the lock
//! file beside the root, and for writers then on the root directory itself. Writers take them
//! exclusive, readers take the lock file shared. The lock is released when its guard drops, and
//! the kernel drops it before any slow external work (filing an ask can wait 30 s on the ask
//! ledger's own lock).

use crate::error::{FlowError, Result};
use std::fs::{File, TryLockError};
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Exclusive,
    Shared,
}

/// Takes `mode` on `file`, waiting up to `wait` for the current holder.
pub(crate) fn take(file: &File, mode: Mode, wait: Duration, lock: &Path) -> Result<()> {
    let deadline = Instant::now() + wait;
    loop {
        let attempt = match mode {
            Mode::Exclusive => file.try_lock(),
            Mode::Shared => file.try_lock_shared(),
        };
        match attempt {
            Ok(()) => return Ok(()),
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(e)) => return Err(FlowError::io(lock)(e)),
        }
        if Instant::now() >= deadline {
            return Err(FlowError::Busy { lock: lock.to_path_buf() });
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Whether the file at `path` is still the one `file` locked. A lock file deleted or replaced
/// after it was opened would let two holders each lock a different file.
#[cfg(unix)]
pub(crate) fn same_file(file: &File, path: &Path) -> Result<bool> {
    use std::os::unix::fs::MetadataExt as _;
    let held = file.metadata().map_err(FlowError::io(path))?;
    match std::fs::metadata(path) {
        Ok(now) => Ok(held.dev() == now.dev() && held.ino() == now.ino()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(FlowError::io(path)(e)),
    }
}

#[cfg(not(unix))]
pub(crate) fn same_file(_file: &File, path: &Path) -> Result<bool> {
    Ok(path.exists())
}

/// A file's identity: device and inode on unix.
pub(crate) type FileId = (u64, u64);

#[cfg(unix)]
pub(crate) fn file_id(meta: &std::fs::Metadata) -> FileId {
    use std::os::unix::fs::MetadataExt as _;
    (meta.dev(), meta.ino())
}

#[cfg(not(unix))]
pub(crate) fn file_id(_meta: &std::fs::Metadata) -> FileId {
    (0, 0)
}

/// The root directory's identity now, by `lstat`: a link put in its place is never taken for it.
/// Refused `root_moved` when it is gone or not a directory.
pub(crate) fn root_id(root: &Path) -> Result<FileId> {
    match std::fs::symlink_metadata(root) {
        Ok(m) if m.is_dir() => Ok(file_id(&m)),
        Ok(_) => Err(FlowError::RootMoved { root: root.to_path_buf() }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(FlowError::RootMoved { root: root.to_path_buf() }),
        Err(e) => Err(FlowError::io(root)(e)),
    }
}

/// Locks the root directory itself, so writers exclude each other even when the lock file was
/// replaced. Which root it is, each write checks ([`root_id`]).
pub(crate) fn lock_root(root: &Path, mode: Mode, wait: Duration) -> Result<File> {
    let dir = match File::open(root) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(FlowError::RootMoved { root: root.to_path_buf() });
        }
        Err(e) => return Err(FlowError::io(root)(e)),
    };
    take(&dir, mode, wait, root)?;
    Ok(dir)
}
