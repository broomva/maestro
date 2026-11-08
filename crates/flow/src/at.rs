//! Everything under the root goes through directory file descriptors: `openat`, `mkdirat`,
//! `renameat` and `statat`, with `O_NOFOLLOW` and `AT_SYMLINK_NOFOLLOW` (through `rustix`, so no
//! `unsafe`). Once a writer holds the root's descriptor, swapping a path cannot redirect a write,
//! and no symbolic link under the root is ever followed: opening one is refused `symlink`.

use crate::error::{FlowError, Result};
use crate::lock::FileId;
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;
use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};

const DIR: OFlags = OFlags::RDONLY.union(OFlags::DIRECTORY).union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);

/// An open directory, and its path for messages only: nothing is ever opened by that path.
#[derive(Debug)]
pub(crate) struct Dir {
    pub(crate) fd: File,
    pub(crate) path: PathBuf,
}

fn error(path: PathBuf, e: Errno) -> FlowError {
    if e == Errno::LOOP {
        return FlowError::Symlink { path };
    }
    FlowError::Io { path, source: e.into() }
}

impl Dir {
    /// Opens the root itself, refusing a link at its last component.
    pub(crate) fn open_root(root: &Path) -> Result<Dir> {
        match rustix::fs::open(root, DIR, Mode::empty()) {
            Ok(fd) => Ok(Dir { fd: fd.into(), path: root.to_path_buf() }),
            Err(Errno::NOENT) => Err(FlowError::RootMoved { root: root.to_path_buf() }),
            // Linux says `ELOOP` and macOS `ENOTDIR` for a link opened this way: ask the link itself.
            Err(_) if std::fs::symlink_metadata(root).is_ok_and(|m| m.file_type().is_symlink()) => {
                Err(FlowError::Symlink { path: root.to_path_buf() })
            }
            Err(e) => Err(FlowError::Io { path: root.to_path_buf(), source: e.into() }),
        }
    }

    pub(crate) fn id(&self) -> Result<FileId> {
        let st = rustix::fs::fstat(&self.fd).map_err(|e| error(self.path.clone(), e))?;
        Ok((st.st_dev as u64, st.st_ino as u64))
    }

    fn kind(&self, name: &str) -> Result<Option<FileType>> {
        match rustix::fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => Ok(Some(FileType::from_raw_mode(st.st_mode as _))),
            Err(Errno::NOENT) => Ok(None),
            Err(e) => Err(error(self.path.join(name), e)),
        }
    }

    /// Whether `name` exists, whatever it is, without following it.
    pub(crate) fn exists(&self, name: &str) -> Result<bool> {
        Ok(self.kind(name)?.is_some())
    }

    /// Whether `name` is a real directory (never a link to one).
    pub(crate) fn is_dir(&self, name: &str) -> Result<bool> {
        Ok(self.kind(name)? == Some(FileType::Directory))
    }

    /// The directory `name`; `None` when it does not exist. A link is refused `symlink`.
    pub(crate) fn dir(&self, name: &str) -> Result<Option<Dir>> {
        match rustix::fs::openat(&self.fd, name, DIR, Mode::empty()) {
            Ok(fd) => Ok(Some(Dir { fd: fd.into(), path: self.path.join(name) })),
            Err(Errno::NOENT) => Ok(None),
            // Linux says `ELOOP` and macOS `ENOTDIR` for a link opened this way: ask the link itself.
            Err(_) if self.kind(name)? == Some(FileType::Symlink) => {
                Err(FlowError::Symlink { path: self.path.join(name) })
            }
            Err(e) => Err(FlowError::Io { path: self.path.join(name), source: e.into() }),
        }
    }

    pub(crate) fn mkdir(&self, name: &str) -> Result<()> {
        rustix::fs::mkdirat(&self.fd, name, Mode::from_raw_mode(0o755)).map_err(|e| error(self.path.join(name), e))
    }

    /// Opens the file `name` with `flags` (and `O_NOFOLLOW`); `None` when it does not exist.
    pub(crate) fn file(&self, name: &str, flags: OFlags) -> Result<Option<File>> {
        match rustix::fs::openat(&self.fd, name, flags | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::from_raw_mode(0o644))
        {
            Ok(fd) => Ok(Some(fd.into())),
            Err(Errno::NOENT) => Ok(None),
            Err(e) => Err(error(self.path.join(name), e)),
        }
    }

    /// The bytes of the file `name`, which must be at most `limit` bytes; `None` when it does not
    /// exist. The size is checked before anything is read, and no more than one byte past `limit`
    /// is ever read, should the file grow in between.
    pub(crate) fn read(&self, name: &str, limit: u64) -> Result<Option<Vec<u8>>> {
        let Some(file) = self.file(name, OFlags::RDONLY)? else { return Ok(None) };
        let size = file.metadata().map_err(FlowError::io(self.path.join(name)))?.len();
        if size > limit {
            return Err(FlowError::TooLarge { what: "log", size, limit });
        }
        let mut bytes = Vec::new();
        file.take(limit.saturating_add(1)).read_to_end(&mut bytes).map_err(FlowError::io(self.path.join(name)))?;
        if bytes.len() as u64 > limit {
            return Err(FlowError::TooLarge { what: "log", size: bytes.len() as u64, limit });
        }
        Ok(Some(bytes))
    }

    /// Moves `name` here to `to_name` in `to`, both through their descriptors.
    pub(crate) fn rename(&self, name: &str, to: &Dir, to_name: &str) -> Result<()> {
        rustix::fs::renameat(&self.fd, name, &to.fd, to_name).map_err(|e| error(self.path.join(name), e))
    }

    /// The names of the real directories here (never links).
    pub(crate) fn subdirs(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let entries = rustix::fs::Dir::read_from(&self.fd).map_err(|e| error(self.path.clone(), e))?;
        for entry in entries {
            let entry = entry.map_err(|e| error(self.path.clone(), e))?;
            let Ok(name) = entry.file_name().to_str() else { continue };
            if name != "." && name != ".." && self.is_dir(name)? {
                out.push(name.to_string());
            }
        }
        out.sort();
        Ok(out)
    }

    /// The `stat` of `name` as a key that changes whenever the file does: length, modification
    /// and change times, inode. `missing` when it does not exist.
    pub(crate) fn stat_key(&self, name: &str) -> String {
        match rustix::fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => format!(
                "{}:{}.{}:{}.{}:{}",
                st.st_size, st.st_mtime, st.st_mtime_nsec, st.st_ctime, st.st_ctime_nsec, st.st_ino
            ),
            Err(_) => "missing".to_string(),
        }
    }

    pub(crate) fn sync(&self) {
        let _ = self.fd.sync_all();
    }
}
