//! Shared test helpers, and the child-process entry the concurrency tests spawn: each test
//! binary re-runs itself as a real second process, so the OS lock is exercised honestly.
#![allow(dead_code)]

use maestro_flow::{FlowError, HostId, InstanceId, Ledger, NewEvent, Options};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

pub const CHILD_ENV: &str = "MAESTRO_FLOW_TEST_CHILD";

pub fn host(name: &str) -> HostId {
    HostId::new(name).unwrap()
}

pub fn id(name: &str) -> InstanceId {
    InstanceId::new(name).unwrap()
}

pub fn quick() -> Options {
    Options { lock_wait: Duration::from_millis(150) }
}

/// `<tmp>/.broomva/flows`, the production shape.
pub fn root_in(tmp: &Path) -> PathBuf {
    tmp.join(".broomva").join("flows")
}

pub fn lock_of(tmp: &Path) -> PathBuf {
    tmp.join(".broomva").join("flows.lock")
}

pub fn event(kind: &str, n: u64) -> NewEvent {
    NewEvent::new(kind, serde_json::json!({ "n": n }))
}

/// Appends `event` at the log's current head, as a well-behaved writer does.
pub fn append_at_head(ledger: &mut Ledger, at: &InstanceId, ev: NewEvent) -> Result<u64, FlowError> {
    let mut guard = ledger.lock()?;
    let seq = match guard.log(at) {
        Ok(log) => log.seq(),
        Err(FlowError::UnknownInstance { .. }) => 0,
        Err(e) => return Err(e),
    };
    Ok(guard.append(at, seq, ev)?.event.seq)
}

pub fn code<T>(r: Result<T, FlowError>) -> String {
    match r {
        Ok(_) => "ok".to_string(),
        Err(e) => e.code().to_string(),
    }
}

/// Every entry under `dir`: kind, size, mtime and file bytes. Two equal snapshots mean nothing
/// was created, removed or written (a directory's mtime moves when an entry comes or goes).
pub fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (bool, u64, SystemTime, Vec<u8>)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let meta = std::fs::symlink_metadata(&d).unwrap();
        let bytes = if meta.is_file() { std::fs::read(&d).unwrap() } else { Vec::new() };
        out.insert(d.clone(), (meta.is_dir(), meta.len(), meta.modified().unwrap(), bytes));
        if meta.is_dir() {
            for e in std::fs::read_dir(&d).unwrap() {
                stack.push(e.unwrap().path());
            }
        }
    }
    out
}

/// The names in `dir`, sorted.
pub fn names(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> =
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    out.sort();
    out
}

/// Spawns this test binary again as a child running `spec` (fields joined by U+001F).
pub fn spawn_child(spec: &[&str]) -> Child {
    spawn_child_pausing(spec, None)
}

/// As [`spawn_child`], with the child pausing at `pause` (see [`Pause`]).
pub fn spawn_child_pausing(spec: &[&str], pause: Option<&Pause>) -> Child {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["child_process_entry", "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, spec.join("\u{1f}"))
        .env_remove(maestro_flow::hooks::PAUSE_ENV)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(p) = pause {
        cmd.env(maestro_flow::hooks::PAUSE_ENV, format!("{}@{}", p.point, p.dir.path().display()));
    }
    cmd.spawn().unwrap()
}

/// A pause point a child holds open (`maestro_flow::hooks`): the child writes `reached` there and
/// waits for `go`.
pub struct Pause {
    pub point: &'static str,
    pub dir: tempfile::TempDir,
}

impl Pause {
    pub fn at(point: &'static str) -> Pause {
        Pause { point, dir: tempfile::tempdir().unwrap() }
    }

    /// Waits until the child is holding the window open.
    pub fn reached(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !self.dir.path().join("reached").exists() {
            assert!(Instant::now() < deadline, "the child never reached {}", self.point);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    pub fn go(&self) {
        std::fs::write(self.dir.path().join("go"), "").unwrap();
    }
}

/// Replaces `path` with a copy of itself: same bytes, another inode.
pub fn replace_with_copy(path: &Path) {
    let tmp = path.with_extension("copy");
    std::fs::copy(path, &tmp).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

/// The child's `FLOW_RESULT` line: `ok` or a refusal code.
pub fn child_result(child: Child) -> String {
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find_map(|l| l.split_once("FLOW_RESULT ").map(|(_, r)| r.trim()))
        .map(str::to_string)
        .unwrap_or_else(|| format!("no result (exit {:?}): {text}", out.status.code()))
}

/// What a child does; a no-op in a normal test run.
///
/// - `genesis <root> <host> <go file>`: waits for the go file, then opens the ledger.
/// - `append <root> <host> <instance> <count> <lock wait ms>`: appends `count` events at the head.
/// - `open <root> <host>`: opens the ledger.
/// - `read <root>`: takes the read lock and lists the instances.
/// - `fresh <root> <host>`: `operator::create_fresh_ledger`.
pub fn child_main() {
    let Ok(spec) = std::env::var(CHILD_ENV) else { return };
    let parts: Vec<&str> = spec.split('\u{1f}').collect();
    let result = match parts.as_slice() {
        ["genesis", root, name, go] => {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !Path::new(go).exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ledger::open(Path::new(root), &host(name), Options::default()).map(|_| ())
        }
        ["append", root, name, instance, count, wait_ms] => (|| {
            let options = Options { lock_wait: Duration::from_millis(wait_ms.parse().unwrap()) };
            let mut ledger = Ledger::open(Path::new(root), &host(name), options)?;
            for _ in 0..count.parse::<u64>().unwrap() {
                let tick = NewEvent::new("Tick", serde_json::json!({ "pid": std::process::id() }));
                append_at_head(&mut ledger, &id(instance), tick)?;
            }
            Ok(())
        })(),
        ["open", root, name] => Ledger::open(Path::new(root), &host(name), Options::default()).map(|_| ()),
        ["read", root] => maestro_flow::Reader::new(Path::new(root), Options::default())
            .and_then(|r| r.lock().and_then(|g| g.instances()))
            .map(|_| ()),
        ["fresh", root, name] => {
            maestro_flow::operator::create_fresh_ledger(Path::new(root), &host(name), &Options::default()).map(|_| ())
        }
        _ => panic!("unknown child spec {spec:?}"),
    };
    println!("FLOW_RESULT {}", code(result));
}
