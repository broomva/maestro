//! A pure node's command run in a snapshot of one commit, never in the working tree (BRO-3636):
//! `verify@1`'s `check` runs the project's own command against `params.head`'s content alone, so
//! nothing that moves the repository while it runs (another commit checked out and back, a
//! concurrent writer) changes its result.
//!
//! The snapshot is a new repository in a new private folder under the kernel's temporary folder:
//! a `git clone --shared --no-checkout --template=` of the workdir's repository, then a detached
//! checkout of the commit. It has the repository's whole history and tags, so a check that reads
//! them (`HEAD^`, `git describe`) runs as it would at the head, and the repository's own object
//! format (SHA-1 or SHA-256). Its objects are the repository's, read in place (`--shared`), so
//! nothing is copied. The user's repository is only read: nothing is written to it (no worktree,
//! no ref, no config). Every git step is the trusted git (`exec::program`) on the fixed `PATH`,
//! with no system or global configuration, no hooks and no prompt.
//!
//! A check that moves the snapshot's HEAD has not checked the head, even if it comes back: after
//! the command, the snapshot's HEAD must still be the commit, detached, and every entry its HEAD
//! log gained must be at the commit (a hard reset in place is one), or the run is
//! `snapshot_moved`, whatever the command's exit. This catches a check's mistake, not a lie: the
//! project's command chooses its own exit status, and that is V3's result.
//!
//! The run's timeout is one deadline, set when the snapshot starts: each git step, then the
//! command, gets only what is left of it (at least 1 ms, as `cancel_capped` gives), so setup never
//! lets the command run past the claim's `cancel_at`.
//!
//! The folder is removed once the command has ended, whatever it came to (a failure, a timeout, a
//! flood: `exec` kills the command's whole group first), and when a step fails; a folder the
//! command made read-only is made writable and removed again. Only a kernel killed mid-run leaves
//! one behind, named `maestro-snapshot-<random>` in the temporary folder.
//!
//! The commit is a full object id, checked before anything runs (`snapshot_invalid`), so it never
//! reads as an option or a revision. A step that fails is `snapshot_failed`, and a check that moved
//! the snapshot `snapshot_moved`, each its own code: never an exit status of the command's, which is
//! the project's alone.

use super::exec::{self, Feed, Limits, Ran, Search};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The clone of the repository: its objects shared, nothing checked out, no template (so no
/// hooks); the repository and the folder after `--`.
const CLONE: [&str; 6] = ["clone", "-q", "--shared", "--no-checkout", "--template=", "--"];

/// The most of a snapshot's `HEAD` or HEAD log read back after the command.
const MAX_LOG: u64 = 1 << 20;

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
    // A timeout too long for the clock leaves no deadline to share; the limits' own is the bound.
    let deadline = Instant::now().checked_add(limits.timeout);
    run_by(repo, commit, argv, limits, deadline)
}

/// [`run_in`], every step and the command bounded by what is left before `deadline`.
fn run_by(repo: &Path, commit: &str, argv: &[String], limits: &Limits, deadline: Option<Instant>) -> Ran {
    let snapshot = match Snapshot::make(repo, commit, limits, deadline) {
        Ok(snapshot) => snapshot,
        Err((detail, stderr)) => return Ran::Failed { code: "snapshot_failed", detail, stderr },
    };
    let ran = exec::run(argv, &snapshot.root, &left(limits, deadline), Feed { search: Search::Caller, stdin: None });
    match snapshot.moved(commit) {
        None => ran,
        Some(detail) => {
            let stderr = match ran {
                Ran::Exited { stderr, .. } | Ran::Failed { stderr, .. } => stderr,
            };
            Ran::Failed { code: "snapshot_moved", detail, stderr }
        }
    }
}

/// `limits`, with the timeout no longer than what is left before `deadline`, and at least 1 ms.
fn left(limits: &Limits, deadline: Option<Instant>) -> Limits {
    let Some(deadline) = deadline else { return limits.clone() };
    let left = deadline.saturating_duration_since(Instant::now());
    Limits { timeout: limits.timeout.min(left).max(Duration::from_millis(1)), ..limits.clone() }
}

/// `path`'s content, at most [`MAX_LOG`] bytes, if it is a regular file (a link is not followed).
fn read_file(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
    let file = std::fs::OpenOptions::new().read(true).custom_flags(flags.bits() as i32).open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_LOG).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// A snapshot's folder, removed when dropped, and its HEAD log as the checkout left it.
struct Snapshot {
    root: PathBuf,
    log: Vec<u8>,
}

impl Snapshot {
    /// Why the command's run is not a check of `commit`, if it moved the snapshot's HEAD.
    fn moved(&self, commit: &str) -> Option<String> {
        let git = self.root.join(".git");
        let head = read_file(&git.join("HEAD"));
        if head.as_deref().map(|h| h.trim_ascii()) != Some(commit.as_bytes()) {
            return Some(format!("the check left the snapshot's HEAD off {commit}"));
        }
        let log = read_file(&git.join("logs/HEAD")).unwrap_or_default();
        let Some(gained) = log.strip_prefix(self.log.as_slice()) else {
            return Some("the check rewrote the snapshot's HEAD log".into());
        };
        // An entry is `<old> <new> <who> <when>\t<message>`: each the check made must be at `commit`.
        let away = gained
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .any(|line| line.split(|b| *b == b' ').nth(1) != Some(commit.as_bytes()));
        away.then(|| format!("the check checked out another commit than {commit} in the snapshot"))
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        remove(&self.root);
    }
}

impl Snapshot {
    fn make(
        repo: &Path,
        commit: &str,
        limits: &Limits,
        deadline: Option<Instant>,
    ) -> Result<Snapshot, (String, Vec<u8>)> {
        let git = exec::program("git").ok().flatten().ok_or_else(|| ("no trusted git".to_string(), Vec::new()))?;
        let name = crate::ledger::random_epoch().map_err(|e| (e.to_string(), Vec::new()))?;
        let root = std::env::temp_dir().join(format!("maestro-snapshot-{name}"));
        std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
            .create(&root)
            .map_err(|e| (format!("the snapshot folder could not be made ({e})"), Vec::new()))?;
        // From here the folder is removed whatever happens.
        let mut snapshot = Snapshot { root, log: Vec::new() };
        let (root, commit) = (snapshot.root.as_os_str(), OsStr::new(commit));
        let steps: [(&str, Vec<&OsStr>); 2] = [
            ("clone", words(&CLONE, &[repo.as_os_str(), root])),
            ("checkout", words(&["-C"], &[root]).into_iter().chain(words(&CHECKOUT, &[commit])).collect()),
        ];
        for (step, args) in steps {
            let mut cmd = Command::new(&git);
            cmd.args(["-c", "core.hooksPath=/dev/null"]).args(args);
            cmd.current_dir("/").stdin(Stdio::null()).env_clear();
            cmd.env("PATH", maestro_store::tree::git_path()).env("xcrun_nocache", "1");
            cmd.env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_TERMINAL_PROMPT", "0");
            let commit = commit.to_string_lossy();
            match exec::supervise(cmd, &left(limits, deadline)).0 {
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
        snapshot.log = read_file(&snapshot.root.join(".git/logs/HEAD")).unwrap_or_default();
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

    /// A repository at `dir` in `format` with one commit; its id. Its hooks are an empty folder.
    fn repository(dir: &Path, format: &str) -> String {
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@localhost", "-c", "commit.gpgsign=false"])
                .args(["-c", "core.hooksPath=/dev/null"])
                .args(args)
                .current_dir(dir)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q", &format!("--object-format={format}")]);
        std::fs::write(dir.join("at-head"), "content of the head\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "one"]);
        git(&["rev-parse", "HEAD"])
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn limits(secs: u64) -> Limits {
        Limits { timeout: Duration::from_secs(secs), max_output: 64 << 10 }
    }

    #[test]
    fn a_snapshot_is_made_in_the_object_format_of_its_commit() {
        for format in ["sha1", "sha256"] {
            let tmp = tempfile::tempdir().unwrap();
            let head = repository(tmp.path(), format);
            assert_eq!(head.len(), if format == "sha1" { 40 } else { 64 });
            let ran = run_in(tmp.path(), &head, &argv(&["sh", "-c", "test -f at-head"]), &limits(60));
            assert!(matches!(ran, Ran::Exited { code: Some(0), .. }), "{format}: {ran:?}");
        }
    }

    /// `git` in `dir`, as the tests' author.
    fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@localhost", "-c", "commit.gpgsign=false"])
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// The code a run failed with, or its exit status.
    fn outcome(ran: Ran) -> String {
        match ran {
            Ran::Exited { code, .. } => format!("exit {code:?}"),
            Ran::Failed { code, .. } => code.to_string(),
        }
    }

    #[test]
    fn a_check_reads_the_heads_history_and_tags() {
        let tmp = tempfile::tempdir().unwrap();
        let first = repository(tmp.path(), "sha1");
        git_in(tmp.path(), &["tag", "v1", &first]);
        git_in(tmp.path(), &["commit", "-q", "--allow-empty", "-m", "two"]);
        let head = git_in(tmp.path(), &["rev-parse", "HEAD"]);
        let check = format!("test \"$(git rev-parse HEAD^)\" = {first} && test \"$(git describe --tags HEAD^)\" = v1");
        let ran = run_in(tmp.path(), &head, &argv(&["sh", "-c", &check]), &limits(60));
        assert_eq!(outcome(ran), "exit Some(0)");
    }

    #[test]
    fn a_check_that_moves_the_snapshot_is_refused_whatever_it_exits() {
        let tmp = tempfile::tempdir().unwrap();
        let first = repository(tmp.path(), "sha1");
        git_in(tmp.path(), &["commit", "-q", "--allow-empty", "-m", "two"]);
        let head = git_in(tmp.path(), &["rev-parse", "HEAD"]);
        let author = "-c user.name=t -c user.email=t@localhost -c commit.gpgsign=false";
        let cases = [
            ("away and back", format!("git checkout -q {first} && git checkout -q {head}")),
            ("away and back, failing", format!("git checkout -q {first}; git checkout -q {head}; exit 3")),
            ("left away", format!("git checkout -q {first}")),
            ("a commit on top", format!("git {author} commit -q --allow-empty -m on-top")),
            ("onto a branch at the head", "git checkout -q -b b".to_string()),
            ("its log rewritten", "printf x > .git/logs/HEAD".to_string()),
        ];
        for (case, check) in cases {
            let ran = run_in(tmp.path(), &head, &argv(&["sh", "-c", &check]), &limits(60));
            assert_eq!(outcome(ran), "snapshot_moved", "{case}");
        }
        // Moving to the head itself is no move; a check that does nothing to HEAD passes.
        for check in [format!("git checkout -q {head}"), "git reset -q --hard HEAD".to_string(), "true".to_string()] {
            let ran = run_in(tmp.path(), &head, &argv(&["sh", "-c", &check]), &limits(60));
            assert_eq!(outcome(ran), "exit Some(0)", "{check}");
        }
    }

    #[test]
    fn setup_gets_only_what_is_left_of_the_runs_deadline() {
        let tmp = tempfile::tempdir().unwrap();
        let head = repository(tmp.path(), "sha1");
        // The deadline has passed: each git step has 1 ms, too little to make the snapshot, though
        // the limits' own timeout would be plenty.
        let ran = run_by(tmp.path(), &head, &argv(&["true"]), &limits(60), Some(Instant::now()));
        let Ran::Failed { code, detail, .. } = ran else { panic!("{ran:?}") };
        assert_eq!(code, "snapshot_failed");
        assert!(detail.contains("git clone"), "{detail}");
    }

    #[test]
    fn the_command_gets_only_what_setup_left_of_the_runs_deadline() {
        let tmp = tempfile::tempdir().unwrap();
        let head = repository(tmp.path(), "sha1");
        let begun = Instant::now();
        let deadline = Some(begun + Duration::from_secs(3));
        let ran = run_by(tmp.path(), &head, &argv(&["sleep", "20"]), &limits(60), deadline);
        let Ran::Failed { code, .. } = ran else { panic!("{ran:?}") };
        assert_eq!(code, "verify_timeout");
        assert!(begun.elapsed() < Duration::from_secs(10), "{:?}", begun.elapsed());
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
