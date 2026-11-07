//! The flows lock: an OS file lock (`flock` on unix, through `std::fs::File::lock`) on the lock
//! file beside the root. Writers take it exclusive, readers shared. The lock is released when its
//! guard drops, and the kernel drops it before any slow external work (filing an ask can wait
//! 30 s on the ask ledger's own lock).

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
