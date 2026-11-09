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
//! Memory is bounded by the timeout only: the kernel sets no memory limit on the child.
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
//! # The entry point
//!
//! The child is [`std::env::current_exe`] started with [`GUARD_ENV`] set and [`ENTRY_ARGS`]. A
//! binary that runs the writers calls [`guard_entry`] first thing in `main`: with the variable set
//! it answers and exits, before it reads any argument. A test binary has a test named
//! `guard_process_entry` that calls it; the arguments are the test harness's filter for it. A
//! binary without the entry runs as itself and gives no verdict, which is refused `guard_error`.

use super::events::digest;
use super::exec::{self, Limits, Ran};
use super::jq;
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// How long a writer lets a guard run.
pub const GUARD_TIMEOUT: Duration = Duration::from_secs(2);

/// The most bytes of input document a guard reads. A choice reads at most `ir::MAX_FAN_IN` (16)
/// receipts of at most `write::MAX_OUTPUTS` (64 KiB) each, plus the params (`ir::MAX_PARAMS`).
pub const MAX_GUARD_INPUT: usize = 2 << 20;

/// The most bytes the evaluator's child may print.
pub const GUARD_OUTPUT: usize = 64 << 10;

/// The longest detail a refusal carries from the evaluator.
pub const MAX_DETAIL: usize = 512;

/// The variable that makes this binary the guard evaluator.
pub const GUARD_ENV: &str = "MAESTRO_FLOW_GUARD";

/// The arguments the child is started with: a test harness's filter for its entry test.
pub const ENTRY_ARGS: &[&str] = &["guard_process_entry", "--nocapture", "--test-threads=1"];

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
    let refuse = |code: &'static str, detail: String| Evaluated { outcome: Err((code, cut(detail))), pid: None };
    if input.len() > MAX_GUARD_INPUT {
        let detail = format!("the guard's input is {} bytes, more than {MAX_GUARD_INPUT}", input.len());
        return refuse("guard_input_too_large", detail);
    }
    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(e) => return refuse("guard_error", format!("the evaluator cannot be found ({e})")),
    };
    let stdin = match envelope(code, input) {
        Ok((file, _)) => file,
        Err(e) => return refuse("guard_error", format!("the guard's input cannot be staged ({e})")),
    };
    let mut cmd = Command::new(program);
    cmd.args(ENTRY_ARGS).env_clear().env(GUARD_ENV, "1").stdin(Stdio::from(stdin));
    let (ran, pid) = exec::supervise(cmd, &Limits { timeout, max_output: GUARD_OUTPUT });
    let outcome = match ran {
        Ran::Exited { code: Some(0), stdout, .. } => answer(&stdout),
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

/// The evaluator's entry point. With [`GUARD_ENV`] set, reads the guard and its input from
/// standard input, prints its answer and exits; otherwise returns at once. See the module docs.
pub fn guard_entry() {
    if std::env::var_os(GUARD_ENV).is_none() {
        return;
    }
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
    jq::first(&code, &bytes[split + 1..])
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Instant;

    /// A guard the child answers with the names in its environment (`child`, tests only).
    pub(crate) const PROBE_ENV: &str = "\"maestro-flow probe: env\"";

    thread_local! {
        /// Run by [`judge`] on this thread as a guard is evaluated, with the flows lock released:
        /// the tests that move the clock or the log while a guard runs set it.
        pub(crate) static WHILE_JUDGING: std::cell::RefCell<Option<Box<dyn Fn()>>> = const { std::cell::RefCell::new(None) };
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
    fn a_guard_s_input_reaches_the_child_through_a_file_unlinked_before_it_is_written() {
        let (mut file, path) = envelope("true", br#"{"a": 1}"#).unwrap();
        assert!(!path.exists(), "a guard's input was left on disk at {}", path.display());
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
        // Through `evaluate`, past the denylist: the bound must hold for whatever reaches the
        // child. A deep reduce, and a count the log supplies (jaq refuses a count that is not an
        // integer, so 1e15), pass the denylist as they are.
        let timeout = Duration::from_millis(150);
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
                    let evaluated = evaluate(code, &serde_json::to_vec(&input).unwrap(), timeout);
                    let elapsed = started.elapsed();
                    let (refusal, detail) = evaluated.outcome.unwrap_err();
                    assert!(refused.contains(&refusal), "{code}: {refusal} {detail}");
                    assert!(elapsed < timeout + Duration::from_secs(1), "{code}: {elapsed:?}");
                    assert!(gone(evaluated.pid.unwrap()), "{code}: the evaluator outlived its verdict");
                });
            }
            // The two that pass the denylist are refused by the timeout through `judge` too.
            let deep_input = json!(xs);
            scope.spawn(move || {
                assert_eq!(judge(deep, &deep_input, timeout).unwrap_err().0, "guard_timeout");
            });
        });
    }

    #[test]
    fn the_evaluator_inherits_no_environment() {
        // cargo runs every test with CARGO_* set, so a child that inherited would show them.
        assert!(std::env::var_os("CARGO_PKG_NAME").is_some(), "the test process has a variable to leak");
        let evaluated = evaluate(PROBE_ENV, b"null", GUARD_TIMEOUT);
        let (refusal, names) = evaluated.outcome.unwrap_err();
        assert_eq!((refusal, names.as_str()), ("guard_error", GUARD_ENV));
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
}
