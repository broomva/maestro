//! Running a node's command: the only place the kernel starts a process, and only from
//! `complete` (replay never runs anything).
//!
//! Every run is bounded:
//!
//! - **argv only.** The first word is the program and the rest are its arguments, passed as
//!   they are. No shell reads them; a definition that wants one names `sh -c` itself.
//! - **No inherited secrets.** The environment is cleared, then only the names in [`ENV_ALLOW`]
//!   are copied from the kernel's own. Standard input is empty.
//! - **A working directory the flow declares** (`workdir` in the definition), absolute and an
//!   existing directory, checked by the caller.
//! - **A timeout, and a process group.** The command leads a new process group. When it exits,
//!   runs past [`Limits::timeout`] or floods its output, the whole group is killed, so nothing it
//!   started outlives it or keeps its pipes open.
//! - **Captured output, with a byte cap.** Standard output and error are read whole up to
//!   [`Limits::max_output`] bytes each; more than that kills the group and fails the run, since
//!   a hash of a truncated output would describe something the command did not print.
//!
//! Accepted: a process that leaves the group (`setsid`) is not killed with it. If it keeps the
//! pipes open, the run waits [`READ_GRACE`] for them and then fails `output_unclosed`, leaving
//! one reader thread blocked until that process exits.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The only environment names a command inherits from the kernel.
pub const ENV_ALLOW: &[&str] = &["PATH", "HOME", "LANG", "LC_ALL", "TMPDIR"];

/// The longest timeout a caller may set.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(3600);

/// The most bytes of standard output, or of standard error, a caller may let a command print.
pub const MAX_OUTPUT: usize = 16 << 20;

/// How long a finished or killed command's pipes may stay open before the run fails.
pub const READ_GRACE: Duration = Duration::from_secs(1);

/// How often the runner looks at the command while it runs.
const TICK: Duration = Duration::from_millis(2);

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

/// What a run came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ran {
    /// The command exited by itself; `code` is `None` when a signal ended it.
    Exited { code: Option<i32>, stdout: Vec<u8>, stderr: Vec<u8> },
    /// The run failed before the command could finish: `code` is the refusal's code.
    Failed { code: &'static str, detail: String, stderr: Vec<u8> },
}

/// Runs `argv` in `cwd` within `limits`. `argv` is not empty and `limits` passed
/// [`Limits::check`]; the caller sees to both.
pub(crate) fn run(argv: &[String], cwd: &Path, limits: &Limits) -> Ran {
    let Some((program, args)) = argv.split_first() else {
        return failed("spawn_failed", "the command is empty".into(), Vec::new());
    };
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(cwd).env_clear().stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for name in ENV_ALLOW {
        if let Some(value) = std::env::var_os(name) {
            cmd.env(name, value);
        }
    }
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return failed("spawn_failed", format!("the command did not start ({e})"), Vec::new()),
    };
    let over = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    for (stream, pipe) in [
        (0, child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>)),
        (1, child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>)),
    ] {
        let (tx, over, cap) = (tx.clone(), over.clone(), limits.max_output);
        std::thread::spawn(move || {
            let bytes = pipe.map(|p| drain(p, cap, &over)).unwrap_or_default();
            let _ = tx.send((stream, bytes));
        });
    }
    drop(tx);

    let deadline = Instant::now() + limits.timeout;
    let ended = loop {
        // A flood is killed at once; the verdict is the one check below, once the pipes close,
        // so output that goes over just as the command exits fails the same way.
        if over.load(Ordering::SeqCst) {
            kill_group(&child);
        }
        match child.try_wait() {
            // Whatever the command started dies with it, so nothing holds its pipes open.
            Ok(Some(status)) => {
                kill_group(&child);
                break Ok(status.code());
            }
            Ok(None) => {}
            Err(e) => {
                kill_group(&child);
                break Err(("spawn_failed", format!("the command could not be waited on ({e})")));
            }
        }
        if Instant::now() >= deadline {
            kill_group(&child);
            break Err(("verify_timeout", format!("the command ran past {} ms", limits.timeout.as_millis())));
        }
        std::thread::sleep(TICK);
    };
    let _ = child.wait();

    let mut out = [Vec::new(), Vec::new()];
    let grace = Instant::now() + READ_GRACE;
    for _ in 0..2 {
        match rx.recv_timeout(grace.saturating_duration_since(Instant::now())) {
            Ok((stream, bytes)) => out[stream] = bytes,
            Err(_) => {
                let [_, stderr] = out;
                return failed("output_unclosed", "the command's output stayed open after it ended".into(), stderr);
            }
        }
    }
    let [stdout, stderr] = out;
    if over.load(Ordering::SeqCst) {
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

/// Reads `pipe` to its end, keeping at most `cap` bytes; past that, flags `over` and discards.
fn drain(mut pipe: impl Read, cap: usize, over: &AtomicBool) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) if kept.len().saturating_add(n) > cap => over.store(true, Ordering::SeqCst),
            Ok(n) => kept.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    kept
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
        let ran = run(&argv(&["echo", "$(touch pwned)", ";", "touch", "pwned"]), tmp.path(), &quick(2000));
        let Ran::Exited { code, stdout, .. } = ran else { panic!("{ran:?}") };
        assert_eq!(code, Some(0));
        assert_eq!(stdout, b"$(touch pwned) ; touch pwned\n");
        assert!(!tmp.path().join("pwned").exists(), "no shell read the words");
        // It ran in the directory it was given.
        let ran = run(&argv(&["pwd", "-P"]), tmp.path(), &quick(2000));
        let Ran::Exited { stdout, .. } = ran else { panic!("{ran:?}") };
        let here = tmp.path().canonicalize().unwrap();
        assert_eq!(String::from_utf8(stdout).unwrap().trim(), here.to_str().unwrap());
    }

    #[test]
    fn a_command_inherits_only_the_allowed_environment() {
        let tmp = tempfile::tempdir().unwrap();
        // `env` as the program prints exactly what the command got.
        let ran = run(&argv(&["env"]), tmp.path(), &quick(2000));
        let Ran::Exited { stdout, .. } = ran else { panic!("{ran:?}") };
        let text = String::from_utf8(stdout).unwrap();
        let names: Vec<&str> = text.lines().filter_map(|l| l.split_once('=').map(|(k, _)| k)).collect();
        // cargo runs every test with CARGO_* set, so a run that inherited would show them.
        assert!(std::env::var_os("CARGO_PKG_NAME").is_some(), "the test process has a variable to leak");
        assert!(names.iter().all(|n| ENV_ALLOW.contains(n)), "{names:?}");
        assert!(names.contains(&"PATH"), "the allowed names are passed on: {names:?}");
    }

    #[test]
    fn a_command_that_hangs_is_killed_at_its_timeout_with_its_whole_group() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "sleep 3 & echo $! > child; wait";
        let started = Instant::now();
        let ran = run(&argv(&["sh", "-c", script]), tmp.path(), &quick(150));
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
            let ran = run(&argv(&["sh", "-c", stream]), tmp.path(), &quick(3000));
            assert!(matches!(ran, Ran::Failed { code: "output_too_large", .. }), "{stream}: {ran:?}");
        }
        assert!(started.elapsed() < Duration::from_secs(2), "killed at the cap, not the timeout");
        // Exactly the cap is kept; one byte more fails.
        let limits = Limits { timeout: Duration::from_secs(2), max_output: 4 };
        let ran = run(&argv(&["printf", "abcd"]), tmp.path(), &limits);
        assert_eq!(ran, Ran::Exited { code: Some(0), stdout: b"abcd".to_vec(), stderr: Vec::new() });
        let ran = run(&argv(&["printf", "abcde"]), tmp.path(), &limits);
        assert!(matches!(ran, Ran::Failed { code: "output_too_large", .. }), "{ran:?}");
    }

    #[test]
    fn a_child_that_outlives_its_command_is_killed_and_holds_nothing_open() {
        let tmp = tempfile::tempdir().unwrap();
        // The command exits at once; its child would keep stdout open for 3 s.
        let started = Instant::now();
        let ran = run(&argv(&["sh", "-c", "sleep 3 & echo $! > child; echo done"]), tmp.path(), &quick(10_000));
        assert_eq!(ran, Ran::Exited { code: Some(0), stdout: b"done\n".to_vec(), stderr: Vec::new() });
        assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
        let child = std::fs::read_to_string(tmp.path().join("child")).unwrap();
        assert!(gone(child.trim()), "the outliving child was killed");
    }

    #[test]
    fn a_command_that_does_not_start_fails_and_limits_are_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let ran = run(&argv(&["/nonexistent/program"]), tmp.path(), &quick(1000));
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
