//! The one lock discipline, ported from the Paseo plugin's `server/locks.ts` (BRO-2997, the
//! control interface's §5.5), so Maestro, the plugin and bstack's ledger writers meet on one lock.
//!
//! `<lock root>/<k>.d`, k the first 16 hex of the SHA-256 of realpath(dirname(file)) + "/" +
//! basename(file); made by mkdir, holding a `pid` (`<pid> <start>\n`, the start in whole seconds
//! since the epoch) and a `token`. A live holder is never reclaimed. A dead one is, once the lock
//! is [`LOCK_STALE`] old, by capture: under a `<k>.reclaim.d` mutex the directory is renamed
//! aside and judged again as captured, so a lock released and taken anew between the look and the
//! rename goes back to its holder.
//!
//! Liveness is read from `ps` (`unsafe` is forbidden here, so no `kill(pid, 0)`): a pid `ps` does
//! not list is dead; one it lists whose start differs by more than 2 s from the pid file's is a
//! reused pid, dead too. When `ps` cannot be run at all the holder counts as alive, as the
//! plugin's does when its start cannot be told.

use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A lock this old whose holder is dead is reclaimed; one with no pid yet counts as live until then.
pub const LOCK_STALE: Duration = Duration::from_secs(120);
/// The reclaim mutex is broken past this, as the plugin and tick.sh do.
const RECLAIM_STALE: Duration = Duration::from_secs(60);

/// The ask ledgers' lock root: `MAESTRO_PASEO_ASK_LOCKS`, else `~/.local/state/control-asks/locks`,
/// the plugin's own default (`server/paths.ts`).
pub fn ask_lock_root() -> PathBuf {
    if let Some(root) = std::env::var_os("MAESTRO_PASEO_ASK_LOCKS").filter(|v| !v.is_empty()) {
        return PathBuf::from(root);
    }
    let home = std::env::var_os("HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| ".".into());
    home.join(".local/state/control-asks/locks")
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another writer held the lock for the whole wait.
    #[error("Another writer holds {what}'s lock ({holder}), so nothing was written. Try again in a moment.")]
    Busy { holder: String, what: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// `<k>`: the first 16 hex of the SHA-256 of realpath(dirname(path)) + "/" + basename(path).
pub fn lock_key(path: &Path) -> io::Result<String> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let dir = std::fs::canonicalize(dir)?;
    let base = path.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let mut hash = Sha256::new();
    hash.update(dir.as_os_str().as_encoded_bytes());
    hash.update(b"/");
    hash.update(base.as_encoded_bytes());
    Ok(hash.finalize().iter().take(8).map(|b| format!("{b:02x}")).collect())
}

/// `ps -o etime=`'s `[[dd-]hh:]mm:ss` in seconds.
pub fn etime_seconds(etime: &str) -> Option<u64> {
    let etime = etime.trim();
    let (days, rest) = match etime.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
        None => (0, etime),
    };
    let parts: Vec<&str> = rest.split(':').collect();
    let num =
        |s: &str| (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse::<u64>().ok()).flatten();
    let (h, m, s) = match parts.as_slice() {
        [m, s] => (0, num(m)?, num(s)?),
        [h, m, s] => (num(h)?, num(m)?, num(s)?),
        _ => return None,
    };
    Some(((days * 24 + h) * 60 + m) * 60 + s)
}

fn now_secs() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// What `ps` says of a pid.
enum Ps {
    /// `ps` could not be run.
    Unknown,
    /// Not listed: no such process.
    Gone,
    /// Listed, with its start in whole seconds since the epoch when its etime reads.
    Running(Option<i64>),
}

fn ps(pid: u32) -> Ps {
    let Ok(out) = Command::new("/bin/ps")
        .args(["-o", "etime=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Ps::Unknown;
    };
    if !out.status.success() || out.stdout.iter().all(u8::is_ascii_whitespace) {
        return Ps::Gone;
    }
    Ps::Running(etime_seconds(&String::from_utf8_lossy(&out.stdout)).map(|e| (now_secs() - e as f64).round() as i64))
}

/// This process's start in whole seconds since the epoch, from `ps` as other writers read it;
/// kept once `ps` answers, the clock standing in until then.
fn started() -> i64 {
    static OWN: OnceLock<i64> = OnceLock::new();
    if let Some(s) = OWN.get() {
        return *s;
    }
    match ps(std::process::id()) {
        Ps::Running(Some(s)) => *OWN.get_or_init(|| s),
        _ => now_secs().round() as i64,
    }
}

/// A pid file's `<pid> <start>`; the start is absent from a writer that does not write one.
pub fn pid_of(pid_file: &str) -> Option<(u32, Option<i64>)> {
    let mut words = pid_file.split_whitespace();
    let pid: u32 = words.next()?.parse().ok().filter(|p| *p > 0)?;
    let start = words.next().and_then(|s| s.parse::<f64>().ok()).filter(|s| s.is_finite()).map(|s| s as i64);
    Some((pid, start))
}

/// Whether the holder a pid file names is alive: `ps` lists it, and it started when it said.
pub fn holder_alive(pid_file: &str) -> bool {
    let Some((pid, start)) = pid_of(pid_file) else { return false };
    match (ps(pid), start) {
        (Ps::Gone, _) => false,
        (Ps::Running(Some(actual)), Some(s)) => (actual - s).abs() <= 2,
        _ => true,
    }
}

/// The locks this process holds now. One of its own it does not hold was left behind.
fn held() -> &'static Mutex<HashSet<PathBuf>> {
    static HELD: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    HELD.get_or_init(Default::default)
}

fn is_held(dir: &Path) -> bool {
    held().lock().map(|h| h.contains(dir)).unwrap_or(false)
}

fn set_held(dir: &Path, on: bool) {
    if let Ok(mut h) = held().lock() {
        if on {
            h.insert(dir.to_path_buf());
        } else {
            h.remove(dir);
        }
    }
}

fn age_of(path: &Path) -> Option<Duration> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(SystemTime::now().duration_since(modified).unwrap_or_default())
}

fn is_own(start: Option<i64>, pid: u32) -> bool {
    pid == std::process::id() && start.is_some_and(|s| (s - started()).abs() <= 2)
}

/// Whether a lock may be taken away, and by whom it is held; `None` when it is gone. `at` is the
/// path this process knows it by: a lock captured aside is judged as the lock it was.
fn staleness(dir: &Path, stale_after: Duration, at: &Path) -> Option<(bool, String)> {
    let age = age_of(dir)?;
    let pid_file = std::fs::read_to_string(dir.join("pid")).ok();
    let holder = pid_file.as_deref().and_then(pid_of);
    let words = holder.map(|(pid, _)| format!("pid {pid}")).unwrap_or_else(|| "a writer starting up".into());
    if let Some((pid, start)) = holder
        && is_own(start, pid)
    {
        return Some((!is_held(at), words));
    }
    let alive = match &pid_file {
        None => age < stale_after,
        Some(text) => holder_alive(text),
    };
    Some((!alive && age >= stale_after, words))
}

fn remove_tree(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

fn random_hex(bytes: usize) -> io::Result<String> {
    let mut buf = vec![0u8; bytes];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Takes a stale lock away by capture, under its reclaim mutex.
fn reclaim(dir: &Path, stale_after: Duration) -> io::Result<()> {
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mutex = dir.with_file_name(format!("{}.reclaim.d", name.strip_suffix(".d").unwrap_or(&name)));
    if let Err(e) = std::fs::create_dir(&mutex) {
        if e.kind() != io::ErrorKind::AlreadyExists {
            return Err(e);
        }
        if age_of(&mutex).is_some_and(|a| a >= RECLAIM_STALE) {
            remove_tree(&mutex);
        }
        return Ok(());
    }
    let result = (|| {
        if !staleness(dir, stale_after, dir).is_some_and(|(stale, _)| stale) {
            return Ok(());
        }
        let aside = dir.with_file_name(format!("{name}.stale-{}", random_hex(4)?));
        if std::fs::rename(dir, &aside).is_err() {
            return Ok(());
        }
        if staleness(&aside, stale_after, dir).is_none_or(|(stale, _)| stale) {
            remove_tree(&aside);
            return Ok(());
        }
        // Captured alive: back it goes. A writer that made a lock meanwhile leaves it nowhere to return to.
        if std::fs::rename(&aside, dir).is_err() {
            remove_tree(&aside);
        }
        Ok(())
    })();
    remove_tree(&mutex);
    result
}

/// A lock this process holds.
#[derive(Debug)]
pub struct Lock {
    pub dir: PathBuf,
    pub token: String,
}

#[derive(Debug, Clone)]
pub struct AcquireOptions {
    pub wait: Duration,
    /// How old a dead holder's lock must be before it is reclaimed.
    pub stale_after: Duration,
    /// What the lock covers, for the refusal's words.
    pub what: String,
}

impl Default for AcquireOptions {
    fn default() -> Self {
        Self { wait: Duration::from_secs(5), stale_after: LOCK_STALE, what: "this ledger".into() }
    }
}

/// Takes the lock of `path` in `root`, waiting up to `options.wait` for a live holder.
pub fn acquire(path: &Path, root: &Path, options: &AcquireOptions) -> Result<Lock, LockError> {
    let deadline = std::time::Instant::now() + options.wait;
    create_root(root)?;
    let dir = root.join(format!("{}.d", lock_key(path)?));
    let mut holder = "another writer".to_string();
    loop {
        match std::fs::create_dir(&dir) {
            Ok(()) => {
                // Held from the moment it is made: a pid of ours on a lock not yet marked held would read as left behind.
                set_held(&dir, true);
                let made = (|| -> io::Result<String> {
                    let token = random_hex(16)?;
                    std::fs::write(dir.join("pid"), format!("{} {}\n", std::process::id(), started()))?;
                    std::fs::write(dir.join("token"), format!("{token}\n"))?;
                    Ok(token)
                })();
                return match made {
                    Ok(token) => Ok(Lock { dir, token }),
                    Err(e) => {
                        // Made but not finished: taken back, so no half-written lock wedges the ledger.
                        set_held(&dir, false);
                        remove_tree(&dir);
                        Err(e.into())
                    }
                };
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        match staleness(&dir, options.stale_after, &dir) {
            Some((true, _)) => reclaim(&dir, options.stale_after)?,
            Some((false, who)) => holder = who,
            None => {}
        }
        if std::time::Instant::now() >= deadline {
            return Err(LockError::Busy { holder, what: options.what.clone() });
        }
        let jitter = random_hex(1).ok().and_then(|h| u8::from_str_radix(&h, 16).ok()).unwrap_or(0) as u64 % 50;
        std::thread::sleep(Duration::from_millis(50 + jitter));
    }
}

#[cfg(unix)]
fn create_root(root: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(root)
}

#[cfg(not(unix))]
fn create_root(root: &Path) -> io::Result<()> {
    std::fs::create_dir_all(root)
}

/// Whether the lock is still the one this holder took: a lock taken over has another token.
pub fn still_ours(lock: &Lock) -> bool {
    std::fs::read_to_string(lock.dir.join("token")).is_ok_and(|t| t.trim() == lock.token)
}

pub fn release(lock: Lock) {
    // No longer held here, whatever follows: a lock of ours left behind is then stale to our next write.
    set_held(&lock.dir, false);
    if !still_ours(&lock) {
        return;
    }
    let _ = std::fs::remove_file(lock.dir.join("token"));
    let _ = std::fs::remove_file(lock.dir.join("pid"));
    remove_tree(&lock.dir);
}

/// Runs `run` holding the lock of `path`, releasing it however `run` ends.
pub fn with_lock<T, E: From<LockError>>(
    path: &Path,
    root: &Path,
    options: &AcquireOptions,
    run: impl FnOnce(&Lock) -> Result<T, E>,
) -> Result<T, E> {
    let lock = acquire(path, root, options)?;
    let out = run(&lock);
    release(lock);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etime_reads_every_ps_shape() {
        assert_eq!(etime_seconds("00:07"), Some(7));
        assert_eq!(etime_seconds(" 01:02:03\n"), Some(3723));
        assert_eq!(etime_seconds("2-00:00:01"), Some(172_801));
        assert_eq!(etime_seconds(""), None);
        assert_eq!(etime_seconds("1:2:3:4"), None);
        assert_eq!(etime_seconds("a:00"), None);
    }

    #[test]
    fn pid_files_parse_as_the_plugin_parses_them() {
        assert_eq!(pid_of("123 1700000000\n"), Some((123, Some(1_700_000_000))));
        assert_eq!(pid_of("123"), Some((123, None)));
        assert_eq!(pid_of("0 5"), None);
        assert_eq!(pid_of("x"), None);
        assert_eq!(pid_of(""), None);
    }

    #[test]
    fn this_process_is_alive_and_a_reused_pid_is_not() {
        let me = std::process::id();
        assert!(holder_alive(&format!("{me} {}", started())));
        assert!(!holder_alive(&format!("{me} {}", started() - 3600)));
        assert!(!holder_alive("4194303 1"));
    }

    #[test]
    fn a_held_lock_refuses_then_releases() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = tmp.path().join("a.yaml");
        std::fs::write(&ledger, "asks: []\n").unwrap();
        let root = tmp.path().join("locks");
        let lock = acquire(&ledger, &root, &AcquireOptions::default()).unwrap();
        assert!(lock.dir.ends_with(format!("{}.d", lock_key(&ledger).unwrap())));
        let quick = AcquireOptions { wait: Duration::from_millis(120), ..Default::default() };
        assert!(matches!(acquire(&ledger, &root, &quick), Err(LockError::Busy { .. })));
        release(lock);
        let again = acquire(&ledger, &root, &quick).unwrap();
        release(again);
        assert!(std::fs::read_dir(&root).unwrap().next().is_none());
    }

    #[test]
    fn a_dead_holder_is_reclaimed_only_when_old() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = tmp.path().join("a.yaml");
        std::fs::write(&ledger, "").unwrap();
        let root = tmp.path().join("locks");
        std::fs::create_dir_all(root.join(format!("{}.d", lock_key(&ledger).unwrap()))).unwrap();
        std::fs::write(root.join(format!("{}.d/pid", lock_key(&ledger).unwrap())), "4194303 1\n").unwrap();
        let quick = AcquireOptions { wait: Duration::from_millis(150), ..Default::default() };
        // Dead but young: kept.
        assert!(matches!(acquire(&ledger, &root, &quick), Err(LockError::Busy { .. })));
        let reclaiming = AcquireOptions { stale_after: Duration::ZERO, ..quick };
        let lock = acquire(&ledger, &root, &reclaiming).unwrap();
        assert!(still_ours(&lock));
        release(lock);
    }

    #[test]
    fn a_lock_taken_over_is_not_released_by_its_old_holder() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = tmp.path().join("a.yaml");
        std::fs::write(&ledger, "").unwrap();
        let root = tmp.path().join("locks");
        let lock = acquire(&ledger, &root, &AcquireOptions::default()).unwrap();
        std::fs::write(lock.dir.join("token"), "someone-else\n").unwrap();
        assert!(!still_ours(&lock));
        let dir = lock.dir.clone();
        release(lock);
        assert!(dir.exists());
    }
}
