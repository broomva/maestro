//! Running a node's command: the only place the kernel starts a process, and only from
//! `complete` (replay never runs anything).
//!
//! Every run is bounded:
//!
//! - **argv only.** The first word is the program and the rest are its arguments, passed as
//!   they are. No shell reads them; a definition that wants one names `sh -c` itself.
//! - **Programs the caller cannot stand in for (BRO-3636).** A first word of `maestro` runs the
//!   kernel's own executable (`current_exe`), never a `maestro` found on a `PATH`, so the verifier
//!   that judges a receipt is the kernel's. `sh`, `git` and `test` run as the one system file each
//!   names ([`program`]): its real path, owned by root and writable by no one else, as is every
//!   folder above it (`maestro_store::tree::check_root_owned`); with none, the run fails
//!   `program_untrusted`. Any other bare word is looked up on the command's `PATH`.
//! - **A fixed `PATH`.** A command searches `maestro_store::tree::git_path()` (the system folders,
//!   git's first), whatever the caller's `PATH`, so a `git` that `sh -c` names is the system's
//!   too. The one exception is the project's own command in a snapshot (`snapshot`, [`Search::Caller`]):
//!   it needs the project's toolchain (cargo, bun, ...), so it searches the caller's `PATH`. A
//!   toolchain faked there takes the user-level write that can already replace Maestro's own
//!   binary, the residual risk the owner accepted (BRO-3635, "Set the floor").
//! - **No inherited secrets.** The environment is cleared, then only the names in [`ENV_ALLOW`]
//!   are copied from the kernel's own (`PATH` as above). Standard input is empty, except a work
//!   node's verify's, which reads the outputs it judges as JSON ([`Feed::stdin`]): bytes in an
//!   unlinked file, so writing them never blocks and needs no thread.
//! - **A working directory the flow declares** (`workdir` in the definition), absolute and an
//!   existing directory, checked by the caller.
//! - **A timeout, and a process group.** The command leads a new process group. When it exits,
//!   runs past [`Limits::timeout`] or floods its output, the whole group is killed, so nothing it
//!   started outlives it or keeps its pipes open.
//! - **Captured output, with a byte cap.** Standard output and error are read whole up to
//!   [`Limits::max_output`] bytes each; more than that kills the group and fails the run, since
//!   a hash of a truncated output would describe something the command did not print.
//! - **No thread, no blocking read.** Both pipes are non-blocking and read in the runner's own
//!   loop, which waits on them with `poll`. Nothing the command does can leave a thread or a
//!   descriptor of the run behind.
//!
//! A process that leaves the group (`setsid`) is not killed with it. If it keeps the pipes open,
//! the run reads them for [`READ_GRACE`], then closes them and fails `output_unclosed`: the run
//! returns within the grace and holds nothing, and the process's next write to them fails.

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::io::Errno;
use std::io::{Seek, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The only environment names a command inherits from the kernel.
pub const ENV_ALLOW: &[&str] = &["PATH", "HOME", "LANG", "LC_ALL", "TMPDIR"];

/// The longest timeout a caller may set.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(3600);

/// The most bytes of standard output, or of standard error, a caller may let a command print.
pub const MAX_OUTPUT: usize = 16 << 20;

/// How long a finished or killed command's pipes may stay open before the run fails.
pub const READ_GRACE: Duration = Duration::from_secs(1);

/// The longest the runner waits on the pipes before it looks at the command again.
const TICK: Duration = Duration::from_millis(2);

/// The most bytes one read takes from a pipe. One read per pipe per wait, so a flood never keeps
/// the runner from its clock.
const CHUNK: usize = 64 << 10;

/// How long a command may run and how much it may print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    pub timeout: Duration,
    pub max_output: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { timeout: Duration::from_secs(300), max_output: 1 << 20 }
    }
}

impl Limits {
    /// Whether these limits are within the kernel's own: a timeout from a millisecond to
    /// [`MAX_TIMEOUT`], and an output cap from a byte to [`MAX_OUTPUT`].
    pub(crate) fn check(&self) -> Result<(), String> {
        if self.timeout < Duration::from_millis(1) || self.timeout > MAX_TIMEOUT {
            return Err(format!("the timeout must be from 1 ms to {} s", MAX_TIMEOUT.as_secs()));
        }
        if self.max_output == 0 || self.max_output > MAX_OUTPUT {
            return Err(format!("the output cap must be from 1 byte to {MAX_OUTPUT} bytes"));
        }
        Ok(())
    }
}

/// The system files `sh` and `test` may be, in order: the first whose real path is root-owned
/// runs. `git` is `maestro_store::tree::git()`, the one the store's reads trust.
const PINNED: &[(&str, &[&str])] = &[("sh", &["/bin/sh", "/usr/bin/sh"]), ("test", &["/bin/test", "/usr/bin/test"])];

/// The `PATH` a command searches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Search {
    /// `maestro_store::tree::git_path()`: the system folders.
    #[default]
    Trusted,
    /// The caller's: only the project's own command in a snapshot (see the module docs).
    Caller,
}

/// What a command reads besides its argv: the `PATH` it searches, and its standard input.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Feed<'a> {
    pub search: Search,
    /// A work node's verify's: the outputs it judges, as JSON. Empty input otherwise.
    pub stdin: Option<&'a [u8]>,
}

/// The file a command's first word runs: the kernel's own executable for `maestro`, the trusted
/// system file for `sh`, `git` and `test`, and `None` for any other word, which is run as it is.
pub(crate) fn program(name: &str) -> Result<Option<PathBuf>, String> {
    if name == "maestro" {
        let exe = std::env::current_exe().map_err(|e| format!("the kernel's own executable is unknown ({e})"))?;
        return Ok(Some(exe));
    }
    let candidates: Vec<&str> = match name {
        "git" => vec![maestro_store::tree::git()],
        _ => match PINNED.iter().find(|(n, _)| *n == name) {
            Some((_, paths)) => paths.to_vec(),
            None => return Ok(None),
        },
    };
    trusted(name, &candidates).map(Some)
}

/// The real path of the first of `candidates` that is root-owned all the way up. That path, not
/// the link that led to it, is what runs, so nothing can be swapped in between.
fn trusted(name: &str, candidates: &[&str]) -> Result<PathBuf, String> {
    let mut why = Vec::new();
    for c in candidates {
        let checked = std::fs::canonicalize(c)
            .map_err(|e| e.to_string())
            .and_then(|real| maestro_store::tree::check_root_owned(&real).map(|()| real));
        match checked {
            Ok(real) => return Ok(real),
            Err(e) => why.push(format!("{c}: {e}")),
        }
    }
    Err(format!("no root-owned {name} on this system ({})", why.join("; ")))
}

/// An unlinked file holding `bytes`, read from its start: a command's standard input.
fn stdin_file(bytes: &[u8]) -> std::io::Result<std::fs::File> {
    let name = crate::ledger::random_epoch().map_err(std::io::Error::other)?;
    let path = std::env::temp_dir().join(format!("maestro-stdin-{name}"));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&path)?;
    std::fs::remove_file(&path)?;
    file.write_all(bytes)?;
    file.rewind()?;
    Ok(file)
}

/// What a run came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ran {
    /// The command exited by itself; `code` is `None` when a signal ended it.
    Exited { code: Option<i32>, stdout: Vec<u8>, stderr: Vec<u8> },
    /// The run failed before the command could finish: `code` is the refusal's code.
    Failed { code: &'static str, detail: String, stderr: Vec<u8> },
}

/// One of the command's output pipes, read without blocking.
struct Pipe {
    /// `None` once the pipe reached its end.
    fd: Option<OwnedFd>,
    kept: Vec<u8>,
}

impl Pipe {
    fn new(fd: Option<OwnedFd>) -> Self {
        Pipe { fd, kept: Vec::new() }
    }
}

/// Runs `argv` in `cwd` within `limits`, with `feed`. `argv` is not empty and `limits` passed
/// [`Limits::check`]; the caller sees to both.
pub(crate) fn run(argv: &[String], cwd: &Path, limits: &Limits, feed: Feed) -> Ran {
    let Some((name, args)) = argv.split_first() else {
        return failed("spawn_failed", "the command is empty".into(), Vec::new());
    };
    let mut cmd = match program(name) {
        Ok(Some(file)) => {
            let mut cmd = Command::new(file);
            std::os::unix::process::CommandExt::arg0(&mut cmd, name);
            cmd
        }
        Ok(None) => Command::new(name),
        Err(detail) => return failed("program_untrusted", detail, Vec::new()),
    };
    cmd.args(args).current_dir(cwd).env_clear();
    for name in ENV_ALLOW.iter().filter(|n| **n != "PATH") {
        if let Some(value) = std::env::var_os(name) {
            cmd.env(name, value);
        }
    }
    match feed.search {
        Search::Trusted => {
            cmd.env("PATH", maestro_store::tree::git_path());
        }
        Search::Caller => {
            if let Some(path) = std::env::var_os("PATH") {
                cmd.env("PATH", path);
            }
        }
    }
    match feed.stdin.map(stdin_file) {
        None => cmd.stdin(Stdio::null()),
        Some(Ok(file)) => cmd.stdin(Stdio::from(file)),
        Some(Err(e)) => {
            return failed("stdin_failed", format!("the command's input could not be written ({e})"), Vec::new());
        }
    };
    supervise(cmd, limits).0
}

/// Runs `cmd` within `limits`, as [`run`] runs a node's command: its own process group, killed
/// when it exits, times out or floods, its output captured up to the cap. The caller has set its
/// program, arguments, directory, environment and standard input; this sets its output pipes and
/// its group. Also returns the process id it ran under, once it started (for the tests that check
/// nothing of a run is left).
pub(crate) fn supervise(mut cmd: Command, limits: &Limits) -> (Ran, Option<u32>) {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return (failed("spawn_failed", format!("the command did not start ({e})"), Vec::new()), None),
    };
    let pid = Some(child.id());
    (wait(&mut child, limits), pid)
}

/// Waits for a started command within `limits`, reading its pipes, and reaps it.
fn wait(child: &mut Child, limits: &Limits) -> Ran {
    let mut pipes =
        [Pipe::new(child.stdout.take().map(OwnedFd::from)), Pipe::new(child.stderr.take().map(OwnedFd::from))];
    if pipes.iter().filter_map(|p| p.fd.as_ref()).any(|fd| rustix::io::ioctl_fionbio(fd, true).is_err()) {
        reap(child);
        return failed("run_failed", "the command's output could not be read without blocking".into(), Vec::new());
    }
    let cap = limits.max_output;
    let mut buf = vec![0u8; CHUNK];
    let mut over = false;

    let deadline = Instant::now() + limits.timeout;
    let ended = loop {
        // A flood is killed at once; the verdict is the one check below, once the pipes close,
        // so output that goes over just as the command exits fails the same way.
        if over {
            kill_group(child);
        }
        match child.try_wait() {
            // Whatever the command started dies with it, so nothing holds its pipes open.
            Ok(Some(status)) => {
                kill_group(child);
                break Ok(status.code());
            }
            Ok(None) => {}
            Err(e) => {
                kill_group(child);
                break Err(("run_failed", format!("the command could not be waited on ({e})")));
            }
        }
        if Instant::now() >= deadline {
            kill_group(child);
            break Err(("verify_timeout", format!("the command ran past {} ms", limits.timeout.as_millis())));
        }
        over |= pump(&mut pipes, &mut buf, cap, TICK);
    };
    reap(child);

    // What the command printed before it ended is still in the pipes: read them to their end,
    // for at most the grace.
    let grace = Instant::now() + READ_GRACE;
    while pipes.iter().any(|p| p.fd.is_some()) {
        let left = grace.saturating_duration_since(Instant::now());
        if left.is_zero() {
            // Something that left the group holds a pipe. Closing this end now leaves nothing of
            // the run behind; the holder's next write fails.
            let stderr = std::mem::take(&mut pipes[1].kept);
            drop(pipes);
            return failed("output_unclosed", "the command's output stayed open after it ended".into(), stderr);
        }
        over |= pump(&mut pipes, &mut buf, cap, left);
    }
    let [stdout, stderr] = pipes.map(|p| p.kept);
    if over {
        let detail = format!("the command printed more than {} bytes", limits.max_output);
        return failed("output_too_large", detail, Vec::new());
    }
    match ended {
        Ok(code) => Ran::Exited { code, stdout, stderr },
        Err((code, detail)) => failed(code, detail, stderr),
    }
}

fn failed(code: &'static str, detail: String, stderr: Vec<u8>) -> Ran {
    Ran::Failed { code, detail, stderr }
}

/// Kills the command's whole process group. The group outlives its leader while any member
/// lives, so its id is not reused while anything in it can still run.
fn kill_group(child: &Child) {
    let _ = rustix::process::kill_process_group(rustix::process::Pid::from_child(child), rustix::process::Signal::KILL);
}

/// Kills the leader by itself and reaps it, so reaping can never wait on a command still
/// running. A no-op kill once it has been reaped.
fn reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Waits up to `wait` for the open pipes, then takes one read from each: what fits under `cap`
/// is kept, more than that is discarded. A pipe at its end, or one that fails, is closed.
/// Returns whether a pipe went over `cap`.
fn pump(pipes: &mut [Pipe; 2], buf: &mut [u8], cap: usize, wait: Duration) -> bool {
    let mut fds: Vec<PollFd<'_>> =
        pipes.iter().filter_map(|p| p.fd.as_ref()).map(|fd| PollFd::new(fd, PollFlags::IN)).collect();
    if fds.is_empty() {
        std::thread::sleep(wait);
        return false;
    }
    let timeout = Timespec::try_from(wait).unwrap_or(Timespec { tv_sec: 0, tv_nsec: 2_000_000 });
    match rustix::event::poll(&mut fds, Some(&timeout)) {
        Ok(_) | Err(Errno::INTR) => {}
        Err(_) => std::thread::sleep(wait.min(TICK)),
    }
    let mut over = false;
    for p in pipes.iter_mut() {
        let Some(fd) = &p.fd else { continue };
        match rustix::io::read(fd, &mut *buf) {
            Ok(0) => p.fd = None,
            Ok(n) if p.kept.len().saturating_add(n) > cap => over = true,
            Ok(n) => p.kept.extend_from_slice(&buf[..n]),
            Err(Errno::AGAIN | Errno::INTR) => {}
            Err(_) => p.fd = None,
        }
    }
    over
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn quick(ms: u64) -> Limits {
        Limits { timeout: Duration::from_millis(ms), max_output: 64 << 10 }
    }

    /// Waits up to a second for `pid` to be gone.
    fn gone(pid: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            let alive = Command::new("kill").args(["-0", pid]).stderr(Stdio::null()).status().unwrap().success();
            if !alive {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn a_command_runs_from_its_argv_with_no_shell_and_its_output_is_captured() {
        let tmp = tempfile::tempdir().unwrap();
        let ran =
            run(&argv(&["echo", "$(touch pwned)", ";", "touch", "pwned"]), tmp.path(), &quick(2000), Feed::default());
        let Ran::Exited { code, stdout, .. } = ran else { panic!("{ran:?}") };
        assert_eq!(code, Some(0));
        assert_eq!(stdout, b"$(touch pwned) ; touch pwned\n");
        assert!(!tmp.path().join("pwned").exists(), "no shell read the words");
        // It ran in the directory it was given.
        let ran = run(&argv(&["pwd", "-P"]), tmp.path(), &quick(2000), Feed::default());
        let Ran::Exited { stdout, .. } = ran else { panic!("{ran:?}") };
        let here = tmp.path().canonicalize().unwrap();
        assert_eq!(String::from_utf8(stdout).unwrap().trim(), here.to_str().unwrap());
    }

    #[test]
    fn maestro_sh_git_and_test_are_never_taken_from_a_path() {
        // BRO-3636: the kernel's own executable, and the system's root-owned files at their real
        // path, so no link or folder a user can write leads elsewhere.
        assert_eq!(program("maestro").unwrap(), Some(std::env::current_exe().unwrap()));
        for name in ["sh", "git", "test"] {
            let file = program(name).unwrap().unwrap_or_else(|| panic!("{name} is pinned"));
            assert!(file.is_absolute(), "{name}: {file:?}");
            assert_eq!(file, std::fs::canonicalize(&file).unwrap(), "{name}: the real path, not a link");
            maestro_store::tree::check_root_owned(&file).unwrap();
        }
        assert_eq!(program("python3").unwrap(), None, "any other word is looked up on the fixed PATH");
        // A candidate a user owns is never run.
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("sh");
        std::fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
        let e = trusted("sh", &[fake.to_str().unwrap()]).unwrap_err();
        assert!(e.contains("no root-owned sh"), "{e}");
        // The command still sees the name it was given as its own.
        let ran = run(&argv(&["sh", "-c", "echo $0"]), tmp.path(), &quick(2000), Feed::default());
        assert_eq!(ran, Ran::Exited { code: Some(0), stdout: b"sh\n".to_vec(), stderr: Vec::new() });
    }

    #[test]
    fn a_command_reads_its_input_from_a_file_that_never_blocks_the_kernel() {
        let tmp = tempfile::tempdir().unwrap();
        let feed = |bytes: &'static [u8]| Feed { search: Search::Trusted, stdin: Some(bytes) };
        let ran = run(&argv(&["cat"]), tmp.path(), &quick(2000), feed(b"{\"receipt\":\"r\"}"));
        assert_eq!(ran, Ran::Exited { code: Some(0), stdout: b"{\"receipt\":\"r\"}".to_vec(), stderr: Vec::new() });
        // No input but the empty one without a feed.
        let ran = run(&argv(&["cat"]), tmp.path(), &quick(2000), Feed::default());
        assert_eq!(ran, Ran::Exited { code: Some(0), stdout: Vec::new(), stderr: Vec::new() });
        // A command that never reads more than a pipe holds does not hold the kernel up.
        static BIG: [u8; 1 << 20] = [b'x'; 1 << 20];
        let started = Instant::now();
        let ran = run(&argv(&["true"]), tmp.path(), &quick(5000), feed(&BIG));
        assert_eq!(ran, Ran::Exited { code: Some(0), stdout: Vec::new(), stderr: Vec::new() });
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    }

    #[test]
    fn a_command_inherits_only_the_allowed_environment() {
        let tmp = tempfile::tempdir().unwrap();
        // `env` as the program prints exactly what the command got.
        let ran = run(&argv(&["env"]), tmp.path(), &quick(2000), Feed::default());
        let Ran::Exited { stdout, .. } = ran else { panic!("{ran:?}") };
        let text = String::from_utf8(stdout).unwrap();
        let names: Vec<&str> = text.lines().filter_map(|l| l.split_once('=').map(|(k, _)| k)).collect();
        // cargo runs every test with CARGO_* set, so a run that inherited would show them.
        assert!(std::env::var_os("CARGO_PKG_NAME").is_some(), "the test process has a variable to leak");
        // The list itself, not just the names checked against it: a wider list fails here.
        let allowed = ["PATH", "HOME", "LANG", "LC_ALL", "TMPDIR"];
        assert_eq!(ENV_ALLOW, allowed);
        assert!(names.iter().all(|n| allowed.contains(n)), "{names:?}");
        assert!(names.contains(&"PATH"), "the allowed names are passed on: {names:?}");
        // PATH is set apart from the loop, so the loop's copy needs a name of its own to show.
        assert!(std::env::var_os("HOME").is_some(), "the test process has HOME to pass on");
        assert!(names.contains(&"HOME"), "the allowed names besides PATH are passed on: {names:?}");
    }

    #[test]
    fn a_command_that_hangs_is_killed_at_its_timeout_with_its_whole_group() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "sleep 3 & echo $! > child; wait";
        let started = Instant::now();
        let ran = run(&argv(&["sh", "-c", script]), tmp.path(), &quick(150), Feed::default());
        assert!(matches!(ran, Ran::Failed { code: "verify_timeout", .. }), "{ran:?}");
        assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
        let child = std::fs::read_to_string(tmp.path().join("child")).unwrap();
        assert!(gone(child.trim()), "the command's own child died with it");
    }

    #[test]
    fn a_command_that_floods_its_output_is_killed_and_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let started = Instant::now();
        for stream in ["yes", "yes >&2"] {
            let ran = run(&argv(&["sh", "-c", stream]), tmp.path(), &quick(3000), Feed::default());
            assert!(matches!(ran, Ran::Failed { code: "output_too_large", .. }), "{stream}: {ran:?}");
        }
        assert!(started.elapsed() < Duration::from_secs(2), "killed at the cap, not the timeout");
        // Exactly the cap is kept; one byte more fails.
        let limits = Limits { timeout: Duration::from_secs(2), max_output: 4 };
        let ran = run(&argv(&["printf", "abcd"]), tmp.path(), &limits, Feed::default());
        assert_eq!(ran, Ran::Exited { code: Some(0), stdout: b"abcd".to_vec(), stderr: Vec::new() });
        let ran = run(&argv(&["printf", "abcde"]), tmp.path(), &limits, Feed::default());
        assert!(matches!(ran, Ran::Failed { code: "output_too_large", .. }), "{ran:?}");
    }

    #[test]
    fn a_child_that_outlives_its_command_is_killed_and_holds_nothing_open() {
        let tmp = tempfile::tempdir().unwrap();
        // The command exits at once; its child would keep stdout open for 3 s.
        let started = Instant::now();
        let ran = run(
            &argv(&["sh", "-c", "sleep 3 & echo $! > child; echo done"]),
            tmp.path(),
            &quick(10_000),
            Feed::default(),
        );
        assert_eq!(ran, Ran::Exited { code: Some(0), stdout: b"done\n".to_vec(), stderr: Vec::new() });
        assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
        let child = std::fs::read_to_string(tmp.path().join("child")).unwrap();
        assert!(gone(child.trim()), "the outliving child was killed");
    }

    #[test]
    fn a_command_that_does_not_start_fails_and_limits_are_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let ran = run(&argv(&["/nonexistent/program"]), tmp.path(), &quick(1000), Feed::default());
        assert!(matches!(ran, Ran::Failed { code: "spawn_failed", .. }), "{ran:?}");
        let ok = |timeout: Duration, max_output: usize| Limits { timeout, max_output }.check().is_ok();
        assert!(ok(Duration::from_millis(1), 1));
        assert!(ok(MAX_TIMEOUT, MAX_OUTPUT));
        assert!(!ok(Duration::from_micros(999), 1));
        assert!(!ok(MAX_TIMEOUT + Duration::from_millis(1), 1));
        assert!(!ok(Duration::from_secs(1), 0));
        assert!(!ok(Duration::from_secs(1), MAX_OUTPUT + 1));
    }
}
