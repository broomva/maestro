//! The guard evaluator (BRO-3633 PR 3b2b): how a writer gets a verdict from a jq guard, a choice
//! node's edge guard (`choose`), an effect's preconditions (`complete`) or a repeat's `until`
//! (when an iteration settles). Replay never evaluates a guard; each verdict is recorded in the
//! log with the digest of the input it read.
//!
//! # Bounds
//!
//! A guard never runs in the kernel's own process. It runs in a child process: this binary run
//! again through a hidden entry point ([`guard_entry`]), supervised the way a node's command is
//! (`exec::supervise`):
//!
//! - **Killable.** The child leads its own process group. When it answers, runs past its timeout
//!   ([`GUARD_TIMEOUT`] for every writer) or floods its output, the whole group is killed and the
//!   child reaped. The kernel's side starts no thread and leaves no descriptor of it behind.
//! - **Ends itself.** A child whose kernel died (a crash, a kill) is reaped by no one, so it also
//!   ends itself at its own deadline, the timeout plus [`CHILD_GRACE`], passed in [`GUARD_ENV`]: a
//!   watchdog thread in the child exits the whole child process with [`DEADLINE_EXIT`]. The thread
//!   lives and dies with the child; the kernel's own process never runs a guard or a thread.
//! - **Bounded input and output.** The input document is at most [`MAX_GUARD_INPUT`] bytes, or the
//!   guard is refused `guard_input_too_large` before anything starts. It reaches the child as its
//!   standard input, through a temporary file unlinked before it is written. The child may print
//!   [`GUARD_OUTPUT`] bytes; a refusal's detail is cut to [`MAX_DETAIL`] bytes.
//! - **No inherited secrets.** The child's environment is cleared but for [`GUARD_ENV`], and its
//!   standard input is the envelope alone. A guard reads no file, variable or clock: jaq defines
//!   none for it, and the denylist refuses the words that would.
//! - **The compile-time denylist again** (`jq::check`). A pinned IR in a log is not compiled
//!   again, so the evaluator checks a guard before the child starts and refuses `guard_invalid`.
//!   The denylist does not bound a guard's work (a `reduce` over the input does not need a banned
//!   word); the timeout does.
//!
//! - **Memory, where the OS enforces it.** On Linux the child limits its own address space to
//!   [`CHILD_MEMORY`] before it reads anything. macOS does not enforce `RLIMIT_AS` or
//!   `RLIMIT_DATA`, so there a guard's memory is bounded by the timeout only (a `reduce` that
//!   doubles a string can take gigabytes within it).
//!
//! # The verdict
//!
//! A guard holds when its first output is `true` and does not when it is `false`. Anything else (a
//! runtime error, another value, no output, a crash) is refused `guard_error`, and a run past the
//! timeout `guard_timeout`. A guard reads one document, built by replay from the log
//! (`replay::guard_input`, `replay::awaiting`): the instance's `params` and, under `nodes`, the
//! receipts it may read, each with its `outputs` and its `value` (the observed value or the chosen
//! edge). The digest a verdict records is the sha256 of that document.
//!
//! # The host: [`guard_entry`]
//!
//! **A binary that calls `claim`, `complete` or `choose` must call [`guard_entry`] first thing in
//! `main`.** The call does two things:
//!
//! - It registers the process as a guard host. A write that needs a guard in a process that never
//!   called it is refused `guard_host_missing` and starts nothing, since running this binary
//!   again would run its own `main`, not the evaluator.
//! - In the child the kernel starts (this binary again, [`std::env::current_exe`], with
//!   [`GUARD_ENV`] set to the child's own deadline and [`ENTRY_ARGS`]), it evaluates the guard,
//!   answers and exits, before the binary reads its arguments. It does so only when both the
//!   variable and [`EVALUATOR_ARG`] are there, so a variable left in a shell does nothing.
//!
//! A test binary has a test named `guard_process_entry` that calls it (the arguments are the test
//! harness's filters: its name, and [`EVALUATOR_ARG`], which names no test), and its fixtures call
//! it to register.

use super::events::digest;
use super::exec::{self, Limits, Ran};
use super::jq;
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How long a writer lets a guard run. Real guards answer in milliseconds; the child's start is
/// most of it.
pub const GUARD_TIMEOUT: Duration = Duration::from_secs(1);

/// The most bytes of input document a guard reads. A choice reads at most `ir::MAX_FAN_IN` (16)
/// receipts of at most `write::MAX_OUTPUTS` (64 KiB) each, plus the params (`ir::MAX_PARAMS`).
pub const MAX_GUARD_INPUT: usize = 2 << 20;

/// The most bytes the evaluator's child may print.
pub const GUARD_OUTPUT: usize = 64 << 10;

/// The longest detail a refusal carries from the evaluator.
pub const MAX_DETAIL: usize = 512;

/// The variable that makes this binary the guard evaluator: its value is the child's own deadline,
/// in milliseconds.
pub const GUARD_ENV: &str = "MAESTRO_FLOW_GUARD";

/// How long past the kernel's timeout the child waits before it ends itself.
pub const CHILD_GRACE: Duration = Duration::from_millis(500);

/// The address space the child allows itself, where the OS enforces it (Linux).
pub const CHILD_MEMORY: u64 = 1 << 30;

/// The exit status of a child that ended itself at its own deadline.
pub const DEADLINE_EXIT: i32 = 124;

/// The argument that, with [`GUARD_ENV`], makes [`guard_entry`] the evaluator. To a test harness it
/// is a filter that names no test.
pub const EVALUATOR_ARG: &str = "maestro-flow-guard-evaluator";

/// The arguments the child is started with: a test harness's filters for its entry test.
pub const ENTRY_ARGS: &[&str] = &["guard_process_entry", EVALUATOR_ARG, "--nocapture", "--test-threads=1"];

/// Set by [`guard_entry`]: this process can be its own guard evaluator.
static HOST: AtomicBool = AtomicBool::new(false);

/// The line the child answers with.
const ANSWER: &str = "MAESTRO_FLOW_GUARD ";

/// A refusal: a stable code and a reason.
pub(crate) type Unfit = (&'static str, String);

/// A verdict, and the digest of the input it was evaluated over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Judged {
    pub holds: bool,
    pub digest: String,
}

/// What an evaluation came to, and the process id the child ran under, if it started.
#[derive(Debug)]
pub(crate) struct Evaluated {
    pub outcome: Result<bool, Unfit>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub pid: Option<u32>,
}

/// Evaluates `code` over `input`, bounded (see the module docs), within `timeout`.
pub(crate) fn judge(code: &str, input: &Value, timeout: Duration) -> Result<Judged, Unfit> {
    // The crate's own unit tests only: what the calling thread asked to happen while a guard runs.
    #[cfg(test)]
    tests::WHILE_JUDGING.with(|hook| {
        if let Some(hook) = hook.borrow().as_ref() {
            hook();
        }
    });
    jq::check(code).map_err(|detail| ("guard_invalid", cut(detail)))?;
    let bytes =
        serde_json::to_vec(input).map_err(|e| ("guard_error", format!("the input does not serialize ({e})")))?;
    let holds = evaluate(code, &bytes, timeout).outcome?;
    Ok(Judged { holds, digest: digest(input) })
}

/// Runs `code` over the JSON `input` in the evaluator's child process. No denylist: [`judge`]
/// checks it first, and the tests that prove the bounds call this directly.
pub(crate) fn evaluate(code: &str, input: &[u8], timeout: Duration) -> Evaluated {
    evaluate_within(code, input, timeout, own_deadline(timeout))
}

/// The child's own deadline for a kernel `timeout`.
fn own_deadline(timeout: Duration) -> Duration {
    timeout.saturating_add(CHILD_GRACE)
}

/// [`evaluate`], with the child's own deadline (`own`) apart from the kernel's timeout.
fn evaluate_within(code: &str, input: &[u8], timeout: Duration, own: Duration) -> Evaluated {
    let refuse = |code: &'static str, detail: String| Evaluated { outcome: Err((code, cut(detail))), pid: None };
    if !HOST.load(Ordering::SeqCst) {
        let detail = "this process never called kernel::guard_entry, so it cannot evaluate a guard";
        return refuse("guard_host_missing", detail.to_string());
    }
    if input.len() > MAX_GUARD_INPUT {
        let detail = format!("the guard's input is {} bytes, more than {MAX_GUARD_INPUT}", input.len());
        return refuse("guard_input_too_large", detail);
    }
    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(e) => return refuse("guard_error", format!("the evaluator cannot be found ({e})")),
    };
    // The crate's own unit tests only: another program in the evaluator's place.
    #[cfg(test)]
    let program = tests::PROGRAM.with(|p| p.borrow().clone()).unwrap_or(program);
    let stdin = match envelope(code, input) {
        Ok((file, _)) => file,
        Err(e) => return refuse("guard_error", format!("the guard's input cannot be staged ({e})")),
    };
    let mut cmd = Command::new(program);
    cmd.args(ENTRY_ARGS).env_clear().env(GUARD_ENV, own.as_millis().to_string()).stdin(Stdio::from(stdin));

    let (ran, pid) = exec::supervise(cmd, &Limits { timeout, max_output: GUARD_OUTPUT });
    let outcome = match ran {
        Ran::Exited { code: Some(0), stdout, .. } => answer(&stdout),
        Ran::Exited { code: Some(DEADLINE_EXIT), .. } => {
            Err(("guard_timeout", format!("the guard ran past the evaluator's own {} ms", own.as_millis())))
        }
        Ran::Exited { code: Some(code), .. } => Err(("guard_error", format!("the evaluator exited {code}"))),
        Ran::Exited { code: None, .. } => Err(("guard_error", "the evaluator crashed".to_string())),
        Ran::Failed { code: "verify_timeout", .. } => {
            Err(("guard_timeout", format!("the guard ran past {} ms", timeout.as_millis())))
        }
        Ran::Failed { detail, .. } => Err(("guard_error", detail)),
    };
    Evaluated { outcome: outcome.map_err(|(code, detail)| (code, cut(detail))), pid }
}

/// The child's standard input: the guard as a JSON string on the first line, then the input. A
/// temporary file, unlinked before anything is written to it, so nothing of it stays on disk;
/// also returns the path it had, for the test that checks it is gone.
fn envelope(code: &str, input: &[u8]) -> std::io::Result<(File, PathBuf)> {
    let first = serde_json::to_string(code).map_err(std::io::Error::other)?;
    let dir = std::env::temp_dir();
    let mut tries = 0;
    let (mut file, path) = loop {
        let name = crate::ledger::random_epoch().map_err(std::io::Error::other)?;
        let path = dir.join(format!(".maestro-flow-guard-{name}"));
        match OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).open(&path) {
            Ok(file) => {
                std::fs::remove_file(&path)?;
                break (file, path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && tries < 4 => tries += 1,
            Err(e) => return Err(e),
        }
    };
    file.write_all(first.as_bytes())?;
    file.write_all(b"\n")?;
    file.write_all(input)?;
    file.seek(SeekFrom::Start(0))?;
    Ok((file, path))
}

/// The verdict on the child's answer line.
fn answer(stdout: &[u8]) -> Result<bool, Unfit> {
    let text = String::from_utf8_lossy(stdout);
    let line = text.lines().find_map(|l| l.strip_prefix(ANSWER));
    match line {
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(other) => Err(("guard_error", other.strip_prefix("error ").unwrap_or(other).to_string())),
        None => Err(("guard_error", "the evaluator gave no verdict".to_string())),
    }
}

/// `detail`, cut to [`MAX_DETAIL`] bytes at a character boundary.
fn cut(mut detail: String) -> String {
    if detail.len() > MAX_DETAIL {
        let mut end = MAX_DETAIL;
        while !detail.is_char_boundary(end) {
            end -= 1;
        }
        detail.truncate(end);
    }
    detail
}

/// The guard host's entry point; see the module docs. Started as the evaluator (with
/// [`GUARD_ENV`] and [`EVALUATOR_ARG`]), it reads the guard and its input from standard input,
/// prints its answer and exits. Otherwise it registers this process as a guard host and returns.
pub fn guard_entry() {
    let evaluator = std::env::args_os().any(|a| a == EVALUATOR_ARG);
    let Some(own) = std::env::var_os(GUARD_ENV).filter(|_| evaluator) else {
        HOST.store(true, Ordering::SeqCst);
        return;
    };
    limit_memory();
    // The child's own deadline; a value that does not read as one gets the writers' default.
    let ms = own.to_str().and_then(|ms| ms.parse::<u64>().ok());
    let own = ms.map_or(GUARD_TIMEOUT.saturating_add(CHILD_GRACE), Duration::from_millis);
    std::thread::spawn(move || {
        std::thread::sleep(own);
        std::process::exit(DEADLINE_EXIT);
    });
    let answer = match child() {
        Ok(true) => "true".to_string(),
        Ok(false) => "false".to_string(),
        Err(detail) => format!("error {}", cut(detail).replace('\n', " ")),
    };
    // On a line of its own: a test harness has printed the test's name, with no newline, before.
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "\n{ANSWER}{answer}");
    let _ = out.flush();
    std::process::exit(0);
}

/// The child's address space, where the OS enforces a limit on it.
fn limit_memory() {
    #[cfg(target_os = "linux")]
    {
        let limit = rustix::process::Rlimit { current: Some(CHILD_MEMORY), maximum: Some(CHILD_MEMORY) };
        let _ = rustix::process::setrlimit(rustix::process::Resource::As, limit);
    }
}

/// The child's side: read the envelope and run the guard. The kernel wrote the envelope within
/// [`MAX_GUARD_INPUT`] and a guard from a pinned IR (`ir::MAX_IR`), and kills the child at its
/// timeout, so nothing here bounds it again.
fn child() -> Result<bool, String> {
    let mut bytes = Vec::new();
    std::io::stdin().lock().read_to_end(&mut bytes).map_err(|e| format!("cannot read ({e})"))?;
    let split = bytes.iter().position(|b| *b == b'\n').ok_or("the envelope has no guard line")?;
    let code: String = serde_json::from_slice(&bytes[..split]).map_err(|e| format!("the guard line ({e})"))?;
    // The crate's own unit tests only: report the environment the child got.
    #[cfg(test)]
    if code == tests::PROBE_ENV {
        return Err(std::env::vars_os().map(|(k, _)| k.to_string_lossy().into_owned()).collect::<Vec<_>>().join(","));
    }
    // The crate's own unit tests on Linux only: report the address space limit the child runs under.
    #[cfg(all(test, target_os = "linux"))]
    if code == tests::PROBE_MEMORY {
        let limit = rustix::process::getrlimit(rustix::process::Resource::As);
        return Err(format!("{:?}", limit.current));
    }
    jq::first(&code, &bytes[split + 1..])
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Instant;

    /// A guard the child answers with the names in its environment (`child`, tests only).
    pub(crate) const PROBE_ENV: &str = "\"maestro-flow probe: env\"";

    /// A guard the child answers with its address space limit (`child`, tests on Linux only).
    #[cfg(target_os = "linux")]
    pub(crate) const PROBE_MEMORY: &str = "\"maestro-flow probe: memory\"";

    thread_local! {
        /// Run by [`judge`] on this thread as a guard is evaluated, with the flows lock released:
        /// the tests that move the clock or the log while a guard runs set it.
        pub(crate) static WHILE_JUDGING: std::cell::RefCell<Option<Box<dyn Fn()>>> = const { std::cell::RefCell::new(None) };
        /// Started by [`evaluate`] on this thread in place of this binary: the tests of a
        /// program that is not the evaluator set it.
        pub(crate) static PROGRAM: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    }

    /// Not a test: the entry the evaluator's child runs (see the module docs).
    #[test]
    fn guard_process_entry() {
        guard_entry();
    }

    /// Waits up to a second for `pid` to be gone.
    fn gone(pid: u32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if !alive {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    fn holds(code: &str, input: Value) -> Result<bool, Unfit> {
        guard_entry();
        judge(code, &input, GUARD_TIMEOUT).map(|j| j.holds)
    }

    #[test]
    fn a_guard_holds_on_true_only_and_anything_else_is_refused() {
        let input = json!({ "params": {}, "nodes": { "a": { "outputs": { "ok": true }, "value": "up" } } });
        assert_eq!(holds(".nodes.a.outputs.ok", input.clone()), Ok(true));
        assert_eq!(holds(".nodes.a.value == \"down\"", input.clone()), Ok(false));
        // Only the first output counts.
        assert_eq!(holds("(false, true)", input.clone()), Ok(false));
        for (code, why) in [("1", "not true or false"), ("empty", "no output"), (".nodes.a.value | keys", "failed")] {
            let (refusal, detail) = holds(code, input.clone()).unwrap_err();
            assert_eq!(refusal, "guard_error", "{code}");
            assert!(detail.contains(why), "{code}: {detail}");
        }
        // The verdict records the digest of the input it read.
        let judged = judge("true", &input, GUARD_TIMEOUT).unwrap();
        assert_eq!(judged.digest, digest(&input));
    }

    #[test]
    fn a_guard_s_input_reaches_the_child_through_a_private_file_unlinked_before_it_is_written() {
        use std::os::unix::fs::PermissionsExt;
        let (mut file, path) = envelope("true", br#"{"a": 1}"#).unwrap();
        assert!(!path.exists(), "a guard's input was left on disk at {}", path.display());
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600, "readable by its owner only");
        let mut text = String::new();
        file.read_to_string(&mut text).unwrap();
        assert_eq!(text, "\"true\"\n{\"a\": 1}");
    }

    #[test]
    fn a_guard_the_compiler_would_refuse_never_starts_a_child() {
        // A pinned IR is not compiled again, so the evaluator checks the denylist itself.
        for code in ["def f: f; f", "last(range(1e15))", "now > 0", "\"x\" * 3"] {
            let (refusal, _) = holds(code, json!({})).unwrap_err();
            assert_eq!(refusal, "guard_invalid", "{code}");
        }
    }

    #[test]
    fn a_guard_input_over_its_bound_is_refused_before_anything_starts() {
        guard_entry();
        let fits = vec![b' '; MAX_GUARD_INPUT];
        let over = vec![b' '; MAX_GUARD_INPUT + 1];
        let started = evaluate("true", &over, GUARD_TIMEOUT);
        assert_eq!(started.outcome.unwrap_err().0, "guard_input_too_large");
        assert_eq!(started.pid, None, "nothing started");
        // At the bound it runs (the input is whitespace, so it does not parse: an error, not a size).
        let ran = evaluate("true", &fits, GUARD_TIMEOUT);
        assert_eq!(ran.outcome.unwrap_err().0, "guard_error");
        assert!(ran.pid.is_some());
    }

    #[test]
    fn a_guard_that_runs_without_bound_is_killed_at_its_timeout_and_leaves_nothing() {
        // Through `evaluate_within`, past the denylist: the bound must hold for whatever reaches
        // the child. A deep reduce, and a count the log supplies (jaq refuses a count that is not
        // an integer, so 1e15), pass the denylist as they are. The child's own deadline is far
        // off, so the kernel's timeout is what ends them.
        guard_entry();
        let timeout = Duration::from_millis(150);
        let own = Duration::from_secs(5);
        let deep = "reduce (.[] as $a | .[] as $b | .[] as $c | .[]) as $x (0; . + $x)";
        let huge = ".nodes.a.outputs as $o | [limit($o.n; $o.xs[] as $a | $o.xs[] as $b | $o.xs[])] | length > 0";
        let xs: Vec<u32> = (0..2000).collect();
        let cases = [
            // Unbounded recursion loops (jaq calls in tail position) or overflows the stack.
            ("def f: f; f", json!(null), &["guard_timeout", "guard_error"][..]),
            ("last(range(1e15))", json!(null), &["guard_timeout"]),
            (deep, json!(xs), &["guard_timeout"]),
            (
                huge,
                json!({ "nodes": { "a": { "outputs": { "n": 1_000_000_000_000_000u64, "xs": xs } } } }),
                &["guard_timeout"],
            ),
        ];
        // The cases run side by side (the test's threads, not the evaluator's), so the test takes
        // about one timeout.
        std::thread::scope(|scope| {
            for (code, input, refused) in cases {
                scope.spawn(move || {
                    let started = Instant::now();
                    let evaluated = evaluate_within(code, &serde_json::to_vec(&input).unwrap(), timeout, own);
                    let elapsed = started.elapsed();
                    let (refusal, detail) = evaluated.outcome.unwrap_err();
                    assert!(refused.contains(&refusal), "{code}: {refusal} {detail}");
                    assert!(elapsed < timeout + Duration::from_millis(600), "{code}: {elapsed:?}");
                    assert!(gone(evaluated.pid.unwrap()), "{code}: the evaluator outlived its verdict");
                });
            }
        });
    }

    #[test]
    fn an_evaluator_its_kernel_never_kills_ends_itself_at_its_default_deadline() {
        // As after a kernel crash: the kernel's timeout is far off, and the child has the deadline
        // the writers give it, their timeout plus the grace (1.5 s).
        guard_entry();
        let started = Instant::now();
        let own = own_deadline(GUARD_TIMEOUT);
        let evaluated = evaluate_within("last(range(1e15))", b"null", Duration::from_secs(10), own);
        let elapsed = started.elapsed();
        assert_eq!(evaluated.outcome.unwrap_err().0, "guard_timeout");
        assert!(elapsed < Duration::from_millis(2100), "{elapsed:?}");
        assert!(gone(evaluated.pid.unwrap()));
    }

    #[test]
    fn the_evaluator_inherits_no_environment() {
        guard_entry();
        // cargo runs every test with CARGO_* set, so a child that inherited would show them.
        assert!(std::env::var_os("CARGO_PKG_NAME").is_some(), "the test process has a variable to leak");
        let evaluated = evaluate(PROBE_ENV, b"null", GUARD_TIMEOUT);
        let (refusal, names) = evaluated.outcome.unwrap_err();
        assert_eq!((refusal, names.as_str()), ("guard_error", GUARD_ENV));
    }

    #[test]
    fn the_variable_alone_does_not_make_a_binary_the_evaluator() {
        // A MAESTRO_FLOW_GUARD left in a shell: the entry test runs as a test, reads nothing and
        // answers nothing, since the evaluator's argument is not there.
        let run = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["guard_process_entry", "--nocapture", "--test-threads=1"])
            .env(GUARD_ENV, "60000")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        assert!(run.status.success(), "{stdout}");
        assert!(!stdout.contains(ANSWER) && stdout.contains("1 passed"), "{stdout}");
    }

    #[test]
    fn an_answer_is_read_from_its_own_line_and_a_detail_is_cut() {
        assert_eq!(answer(b"running 1 test\nMAESTRO_FLOW_GUARD true\n"), Ok(true));
        assert_eq!(answer(b"MAESTRO_FLOW_GUARD false\n"), Ok(false));
        assert_eq!(answer(b"MAESTRO_FLOW_GUARD error boom\n"), Err(("guard_error", "boom".to_string())));
        assert_eq!(answer(b"running 0 tests\n").unwrap_err().1, "the evaluator gave no verdict");
        let long = "é".repeat(MAX_DETAIL);
        let short = cut(long);
        assert!(short.len() <= MAX_DETAIL && short.len() > MAX_DETAIL - 2, "{}", short.len());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_evaluator_runs_under_its_memory_limit() {
        // Linux enforces the child's address space limit; the child reports the one it runs under.
        // macOS does not enforce one (see the module docs), so this test is Linux only.
        guard_entry();
        let evaluated = evaluate(PROBE_MEMORY, b"null", GUARD_TIMEOUT);
        let (refusal, limit) = evaluated.outcome.unwrap_err();
        assert_eq!((refusal, limit), ("guard_error", format!("{:?}", Some(CHILD_MEMORY))));
    }
}
