//! A pure node's command run in a snapshot of one commit, never in the working tree (BRO-3636):
//! `verify@1`'s `check` runs the project's own command against `params.head`'s content alone, so
//! nothing that moves the repository while it runs (another commit checked out and back, a
//! concurrent writer) changes its result.
//!
//! The snapshot is a new repository in a new private folder under the kernel's temporary folder,
//! holding one commit: `git init --template=`, a fetch of that commit alone from the workdir's
//! repository (`--depth=1`, no tags), and a detached checkout of it. Another commit is not in it, so
//! the command cannot check one out. The user's repository is only read: the fetch reads its
//! objects, and nothing is written to it (no worktree, no ref, no config). Every git step is the
//! trusted git (`exec::program`) on the fixed `PATH`, with no system or global configuration, no
//! hooks and no prompt, each bounded by the run's limits.
//!
//! The folder is removed once the command has ended, whatever it came to (a failure, a timeout, a
//! flood: `exec` kills the command's whole group first), and when a step fails; a folder the
//! command made read-only is made writable and removed again. Only a kernel killed mid-run leaves
//! one behind, named `maestro-snapshot-<random>` in the temporary folder.
//!
//! The commit is a full object id, checked before anything runs (`snapshot_invalid`), so it never
//! reads as an option or a revision. A step that fails is `snapshot_failed`, its own code: never
//! an exit status of the command's, which is the project's alone.

use super::exec::{self, Feed, Limits, Ran, Search};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The fetch of the one commit: alone (`--depth=1`), no tags, the repository after `--`.
const FETCH: [&str; 7] = ["-c", "protocol.file.allow=always", "fetch", "-q", "--depth=1", "--no-tags", "--"];

/// The checkout of it, detached.
const CHECKOUT: [&str; 5] = ["-c", "advice.detachedHead=false", "checkout", "-q", "--detach"];

/// `fixed` then `rest`, as git's arguments.
fn words<'a>(fixed: &'a [&'a str], rest: &[&'a OsStr]) -> Vec<&'a OsStr> {
    fixed.iter().map(|w| OsStr::new(*w)).chain(rest.iter().copied()).collect()
}

/// Whether `commit` is a full object id: 40 or 64 lower-case hex digits.
pub(crate) fn is_object_id(commit: &str) -> bool {
    matches!(commit.len(), 40 | 64) && commit.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Runs `argv` in a snapshot of `commit` of the repository at `repo`, within `limits`. `commit`
/// passed [`is_object_id`]. See the module docs.
pub(crate) fn run_in(repo: &Path, commit: &str, argv: &[String], limits: &Limits) -> Ran {
    let snapshot = match Snapshot::make(repo, commit, limits) {
        Ok(snapshot) => snapshot,
        Err((detail, stderr)) => return Ran::Failed { code: "snapshot_failed", detail, stderr },
    };
    exec::run(argv, &snapshot.root, limits, Feed { search: Search::Caller, stdin: None })
}

/// A snapshot's folder, removed when dropped.
struct Snapshot {
    root: PathBuf,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        remove(&self.root);
    }
}

impl Snapshot {
    fn make(repo: &Path, commit: &str, limits: &Limits) -> Result<Snapshot, (String, Vec<u8>)> {
        let git = exec::program("git").ok().flatten().ok_or_else(|| ("no trusted git".to_string(), Vec::new()))?;
        let name = crate::ledger::random_epoch().map_err(|e| (e.to_string(), Vec::new()))?;
        let root = std::env::temp_dir().join(format!("maestro-snapshot-{name}"));
        std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
            .create(&root)
            .map_err(|e| (format!("the snapshot folder could not be made ({e})"), Vec::new()))?;
        // From here the folder is removed whatever happens.
        let snapshot = Snapshot { root };
        let (root, commit) = (snapshot.root.as_os_str(), OsStr::new(commit));
        let steps: [(&str, Vec<&OsStr>); 3] = [
            ("init", words(&["init", "-q", "--template="], &[root])),
            ("fetch", words(&["-C"], &[root]).into_iter().chain(words(&FETCH, &[repo.as_os_str(), commit])).collect()),
            ("checkout", words(&["-C"], &[root]).into_iter().chain(words(&CHECKOUT, &[commit])).collect()),
        ];
        for (step, args) in steps {
            let mut cmd = Command::new(&git);
            cmd.args(["-c", "core.hooksPath=/dev/null"]).args(args);
            cmd.current_dir("/").stdin(Stdio::null()).env_clear();
            cmd.env("PATH", maestro_store::tree::git_path()).env("xcrun_nocache", "1");
            cmd.env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_TERMINAL_PROMPT", "0");
            let commit = commit.to_string_lossy();
            match exec::supervise(cmd, limits).0 {
                Ran::Exited { code: Some(0), .. } => {}
                Ran::Exited { code, stderr, .. } => {
                    let code = code.map_or_else(|| "a signal".to_string(), |c| format!("exit {c}"));
                    return Err((format!("the snapshot's git {step} of {commit} failed ({code})"), stderr));
                }
                Ran::Failed { detail, stderr, .. } => {
                    return Err((format!("the snapshot's git {step} of {commit} failed ({detail})"), stderr));
                }
            }
        }
        Ok(snapshot)
    }
}

/// Removes `root` and everything in it; a folder the command left read-only is made writable
/// first. Links are removed, never followed.
fn remove(root: &Path) {
    if std::fs::remove_dir_all(root).is_ok() || std::fs::symlink_metadata(root).is_err() {
        return;
    }
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        let is_dir = std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir());
        if !is_dir {
            continue;
        }
        let _ = std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));
        if let Ok(entries) = std::fs::read_dir(&dir) {
            todo.extend(entries.flatten().map(|e| e.path()));
        }
    }
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_commit_is_a_full_lower_case_object_id() {
        assert!(is_object_id(&"a".repeat(40)));
        assert!(is_object_id(&"0".repeat(64)));
        for bad in ["HEAD", "main", "-x", &"a".repeat(39), &"a".repeat(41), &"A".repeat(40), &"g".repeat(40)] {
            assert!(!is_object_id(bad), "{bad}");
        }
    }

    #[test]
    fn a_read_only_snapshot_is_removed_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("snap");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/b/f"), "x").unwrap();
        std::os::unix::fs::symlink(tmp.path(), root.join("a/out")).unwrap();
        for dir in ["a/b", "a", ""] {
            let perms = std::os::unix::fs::PermissionsExt::from_mode(0o500);
            std::fs::set_permissions(root.join(dir), perms).unwrap();
        }
        remove(&root);
        assert!(!root.exists());
        assert!(tmp.path().is_dir(), "a link inside is removed, not followed");
    }
}
