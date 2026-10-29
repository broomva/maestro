//! Read-only surface adapters for caller identity and the authenticated T3 effect bridge (`BRO-3268`).
//!
//! Resolves who is calling across [`Surface::Paseo`], [`Surface::T3`], and
//! [`Surface::Direct`]:
//! - [`paseo::PaseoRegistry`] ports `server/callers.ts` and `server/permissions.ts`
//!   from the Paseo plugin;
//! - [`t3::T3Registry`] verifies threads against T3's `statev2.sqlite` (or JSON snapshot)
//!   registry and discovers runtime transport/version metadata;
//! - [`DirectRegistry`] verifies bare processes against per-launch tokens issued at
//!   dispatch;
//! - [`SurfaceRegistry`] routes by surface, verifies node and short-lived tuple-bound
//!   [`BridgeCredential`] bindings with HMAC-SHA256, tracks persistent incarnation-bound
//!   session generations to refuse ID reuse, refuses cross-surface spoofing, and projects
//!   live permission prompts into [`Need`]s.

pub mod paseo;
pub mod t3;

pub use maestro_core::session::{
    BridgeCredential, BridgeDiscovery, BridgeTransport, CallerClaim, Harness, IdentityRefusal,
    MAX_BRIDGE_CREDENTIAL_TTL_MS, NodeId, PeerCredentials, PermissionPrompt, RefusalReason, SessionError,
    SessionLiveness, SessionRef, SessionSnapshot, SessionUpdate, Surface, VerifiedSession, verified_identity,
};
pub use paseo::{
    PARENT_AGENT_LABEL, PaseoAgent, PaseoRegistry, PermissionLike, describe_permission, permission_request_id,
    resolve_real, within,
};
pub use t3::{T3Registry, T3Thread, is_safe_t3_id};

use maestro_core::need::Need;
use paseo::{check_peer, is_safe_id, short_id};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct GenerationEntry {
    generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    incarnation: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct GenerationLedger {
    #[serde(default)]
    ledger_id: String,
    #[serde(default)]
    entries: HashMap<String, GenerationEntry>,
}

static SHARED_GENERATIONS: OnceLock<Mutex<HashMap<String, GenerationEntry>>> = OnceLock::new();
static HELD_LOCKS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
static TMP_SEQ: AtomicU64 = AtomicU64::new(1);

fn shared_generations() -> &'static Mutex<HashMap<String, GenerationEntry>> {
    SHARED_GENERATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn held_locks() -> &'static Mutex<HashSet<PathBuf>> {
    HELD_LOCKS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn set_lock_held(dir: &Path, on: bool) {
    if let Ok(mut set) = held_locks().lock() {
        if on {
            set.insert(dir.to_path_buf());
        } else {
            set.remove(dir);
        }
    }
}

fn is_lock_held_here(dir: &Path) -> bool {
    held_locks().lock().map(|s| s.contains(dir)).unwrap_or(false)
}

/// Current wall-clock timestamp in milliseconds since the Unix epoch.
pub fn current_time_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX)).unwrap_or(0)
}

fn now_secs_f64() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    if let Ok(mut f) = File::open("/dev/urandom")
        && f.read_exact(&mut buf).is_ok()
    {
        return buf.iter().map(|b| format!("{b:02x}")).collect();
    }
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let fallback = format!("{}:{}:{seq}", std::process::id(), current_time_ms());
    hex_sha256(fallback.as_bytes())[..bytes * 2].to_string()
}

fn etime_seconds(etime: &str) -> Option<u64> {
    let etime = etime.trim();
    let (days, rest) = match etime.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
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

enum PsState {
    Unknown,
    Gone,
    Running(Option<i64>),
}

fn query_ps(pid: u32) -> PsState {
    if Path::new("/proc/self").exists() && !Path::new(&format!("/proc/{pid}")).exists() {
        return PsState::Gone;
    }
    let Ok(out) = Command::new("/bin/ps").args(["-o", "etime=", "-p", &pid.to_string()]).stdin(Stdio::null()).output()
    else {
        return PsState::Unknown;
    };
    if out.status.success() {
        if out.stdout.iter().all(u8::is_ascii_whitespace) {
            return PsState::Gone;
        }
        return PsState::Running(
            etime_seconds(&String::from_utf8_lossy(&out.stdout)).map(|e| (now_secs_f64() - e as f64).round() as i64),
        );
    }
    if out.status.code() == Some(1) && out.stdout.iter().all(u8::is_ascii_whitespace) && out.stderr.is_empty() {
        return PsState::Gone;
    }
    PsState::Unknown
}

/// Process start timestamp in whole seconds since Unix epoch for the current process.
pub fn own_process_started_secs() -> i64 {
    static OWN: OnceLock<i64> = OnceLock::new();
    if let Some(s) = OWN.get() {
        return *s;
    }
    match query_ps(std::process::id()) {
        PsState::Running(Some(s)) => *OWN.get_or_init(|| s),
        _ => {
            static FIRST: OnceLock<i64> = OnceLock::new();
            *FIRST.get_or_init(|| now_secs_f64().round() as i64)
        }
    }
}

fn parse_pid_file(text: &str) -> Option<(u32, Option<i64>)> {
    let mut words = text.split_whitespace();
    let pid: u32 = words.next()?.parse().ok().filter(|p| *p > 0)?;
    let start = words.next().and_then(|s| s.parse::<f64>().ok()).filter(|s| s.is_finite()).map(|s| s as i64);
    Some((pid, start))
}

fn holder_alive(pid_text: &str) -> bool {
    let Some((pid, start)) = parse_pid_file(pid_text) else {
        return false;
    };
    match (query_ps(pid), start) {
        (PsState::Gone, _) => false,
        (PsState::Running(Some(actual)), Some(s)) => (actual - s).abs() <= 2,
        _ => true,
    }
}

fn path_age(path: &Path) -> Option<Duration> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(SystemTime::now().duration_since(modified).unwrap_or_default())
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for b in digest {
        let _ = write!(&mut hex, "{b:02x}");
    }
    hex
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut key_block = [0u8; 64];
    if key.len() > 64 {
        let hashed = Sha256::digest(key);
        key_block[..32].copy_from_slice(&hashed);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

struct LockGuard {
    flock_file: File,
    dir: PathBuf,
    token: String,
}

impl LockGuard {
    fn still_ours(&self) -> bool {
        std::fs::read_to_string(self.dir.join("token")).is_ok_and(|t| t.trim() == self.token)
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        set_lock_held(&self.dir, false);
        if self.still_ours() {
            let _ = std::fs::remove_file(self.dir.join("token"));
            let _ = std::fs::remove_file(self.dir.join("pid"));
            let _ = std::fs::remove_dir_all(&self.dir);
        }
        let _ = self.flock_file.unlock();
    }
}

/// Whether `dir` is an abandoned lock directory whose holder is confirmed dead.
/// Missing or empty `pid` files are treated as still initializing until 2 seconds have elapsed,
/// so a concurrent contender can never reclaim a freshly created lock directory during initialization.
pub fn lock_dir_stale(dir: &Path, canonical_dir: &Path) -> Option<bool> {
    let age = path_age(dir)?;
    let pid_text = std::fs::read_to_string(dir.join("pid")).ok().filter(|s| !s.trim().is_empty());
    if let Some((pid, start)) = pid_text.as_deref().and_then(parse_pid_file)
        && pid == std::process::id()
        && start.is_some_and(|s| (s - own_process_started_secs()).abs() <= 2)
        && !is_lock_held_here(canonical_dir)
    {
        return Some(true);
    }
    match &pid_text {
        None => Some(age >= Duration::from_secs(2)),
        Some(text) => Some(!holder_alive(text)),
    }
}

fn reclaim_stale_lock(dir: &Path, surface: &Surface) -> Result<(), SessionError> {
    let reclaim_mutex = PathBuf::from(format!("{}.reclaim.d", dir.display()));
    if reclaim_mutex.exists() {
        let cleanup = if reclaim_mutex.is_dir() {
            std::fs::remove_dir_all(&reclaim_mutex)
        } else {
            std::fs::remove_file(&reclaim_mutex)
        };
        if let Err(e) = cleanup {
            return Err(SessionError::RegistryDisconnected {
                surface: surface.clone(),
                detail: format!("stale generation reclaim mutex {} could not be cleared: {e}", reclaim_mutex.display()),
            });
        }
    }
    let aside = PathBuf::from(format!("{}.stale-{}", dir.display(), random_hex(4)));
    std::fs::rename(dir, &aside).map_err(|e| SessionError::RegistryDisconnected {
        surface: surface.clone(),
        detail: format!("stale generation lock {} could not be moved aside: {e}", dir.display()),
    })?;
    if !lock_dir_stale(&aside, dir).unwrap_or(true) {
        let _ = std::fs::rename(&aside, dir);
        return Err(SessionError::RegistryDisconnected {
            surface: surface.clone(),
            detail: format!("generation lock {} became active during reclaim", dir.display()),
        });
    }
    let _ = std::fs::remove_file(aside.join("pid"));
    let _ = std::fs::remove_file(aside.join("token"));
    if let Err(e) = std::fs::remove_dir(&aside) {
        let _ = std::fs::rename(&aside, dir);
        return Err(SessionError::RegistryDisconnected {
            surface: surface.clone(),
            detail: format!("stale generation lock {} could not be removed: {e}", dir.display()),
        });
    }
    Ok(())
}

fn write_synced_file(target: &Path, content: &[u8]) -> std::io::Result<()> {
    let mut f = OpenOptions::new().write(true).create_new(true).open(target)?;
    f.write_all(content)?;
    f.sync_all()
}

fn acquire_generation_lock(path: &Path, surface: &Surface) -> Result<LockGuard, SessionError> {
    let flock_path = PathBuf::from(format!("{}.flock", path.display()));
    let flock_file =
        OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&flock_path).map_err(|e| {
            SessionError::RegistryDisconnected {
                surface: surface.clone(),
                detail: format!("generation lock {} could not be opened: {e}", flock_path.display()),
            }
        })?;

    let lock_dir = PathBuf::from(format!("{}.lock.d", path.display()));
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if flock_file.try_lock().is_err() {
            if Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            return Err(SessionError::RegistryDisconnected {
                surface: surface.clone(),
                detail: format!("generation flock {} held by active writer past timeout", flock_path.display()),
            });
        }
        if lock_dir.exists() {
            if lock_dir_stale(&lock_dir, &lock_dir) == Some(true) {
                if let Err(err) = reclaim_stale_lock(&lock_dir, surface) {
                    let _ = flock_file.unlock();
                    return Err(err);
                }
            } else {
                let _ = flock_file.unlock();
                if Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                return Err(SessionError::RegistryDisconnected {
                    surface: surface.clone(),
                    detail: format!("generation lock {} held by active writer past timeout", lock_dir.display()),
                });
            }
        }

        // Populate `pid` and `token` inside a private staging directory BEFORE publishing `lock_dir`
        // via `rename`, so no contender can ever observe `lock_dir` with a missing or empty `pid` file.
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let stage_dir = PathBuf::from(format!("{}.stage.{}.{seq}", lock_dir.display(), std::process::id()));
        let token = random_hex(16);
        let pid_line = format!(
            "{} {}
",
            std::process::id(),
            own_process_started_secs()
        );
        let token_line = format!(
            "{token}
"
        );
        let staged = (|| -> std::io::Result<()> {
            std::fs::create_dir(&stage_dir)?;
            write_synced_file(&stage_dir.join("token"), token_line.as_bytes())?;
            write_synced_file(&stage_dir.join("pid"), pid_line.as_bytes())?;
            std::fs::rename(&stage_dir, &lock_dir)
        })();
        match staged {
            Ok(()) => {
                set_lock_held(&lock_dir, true);
                return Ok(LockGuard { flock_file, dir: lock_dir, token });
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::DirectoryNotEmpty) => {
                let _ = std::fs::remove_dir_all(&stage_dir);
                let _ = flock_file.unlock();
                if Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                return Err(SessionError::RegistryDisconnected {
                    surface: surface.clone(),
                    detail: format!("generation lock {} could not be acquired before timeout: {e}", lock_dir.display()),
                });
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&stage_dir);
                let _ = flock_file.unlock();
                return Err(SessionError::RegistryDisconnected {
                    surface: surface.clone(),
                    detail: format!("generation lock {} could not be initialized: {e}", lock_dir.display()),
                });
            }
        }
    }
}

fn load_generations_ledger(
    path: &Path,
    surface: &Surface,
    require_existing: bool,
) -> Result<(GenerationLedger, bool), SessionError> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            if let Ok(ledger) = serde_json::from_str::<GenerationLedger>(&text)
                && !ledger.ledger_id.is_empty()
            {
                return Ok((ledger, false));
            }
            if let Ok(legacy_map) = serde_json::from_str::<HashMap<String, GenerationEntry>>(&text) {
                return Ok((GenerationLedger { ledger_id: random_hex(16), entries: legacy_map }, true));
            }
            Err(SessionError::RegistryDisconnected {
                surface: surface.clone(),
                detail: format!("generation ledger {} is corrupt", path.display()),
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if require_existing {
                return Err(SessionError::RegistryDisconnected {
                    surface: surface.clone(),
                    detail: format!("generation ledger {} is missing for presented credential", path.display()),
                });
            }
            Ok((GenerationLedger { ledger_id: random_hex(16), entries: HashMap::new() }, true))
        }
        Err(e) => Err(SessionError::RegistryDisconnected {
            surface: surface.clone(),
            detail: format!("generation ledger {} could not be read: {e}", path.display()),
        }),
    }
}

fn store_generations_ledger(
    path: &Path,
    surface: &Surface,
    lock: &LockGuard,
    ledger: &GenerationLedger,
) -> Result<(), SessionError> {
    if !lock.still_ours() {
        return Err(SessionError::RegistryDisconnected {
            surface: surface.clone(),
            detail: format!("generation lock {} was lost before commit", lock.dir.display()),
        });
    }
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_path = PathBuf::from(format!("{}.tmp.{}.{seq}", path.display(), std::process::id()));
    let encoded = serde_json::to_vec(ledger).map_err(|e| SessionError::RegistryDisconnected {
        surface: surface.clone(),
        detail: format!("generation ledger serialize failed: {e}"),
    })?;
    let write_res = (|| -> std::io::Result<()> {
        let mut f = File::create(&tmp_path)?;
        f.write_all(&encoded)?;
        f.sync_all()?;
        drop(f);
        if !lock.still_ours() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "generation lock token changed before rename",
            ));
        }
        std::fs::rename(&tmp_path, path)
    })();
    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(SessionError::RegistryDisconnected {
            surface: surface.clone(),
            detail: format!("generation ledger {} could not be persisted: {e}", path.display()),
        });
    }
    Ok(())
}

/// A registered [`Surface::Direct`] session and its per-launch token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectSession {
    pub id: String,
    pub cwd: String,
    #[serde(default, skip_serializing)]
    pub token: String,
    pub liveness: SessionLiveness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<Harness>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_permissions: Vec<PermissionPrompt>,
}

impl fmt::Debug for DirectSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirectSession")
            .field("id", &self.id)
            .field("cwd", &self.cwd)
            .field("token", &"<redacted>")
            .field("liveness", &self.liveness)
            .field("harness", &self.harness)
            .field("pending_permissions", &self.pending_permissions)
            .finish()
    }
}

/// Registry of per-launch tokens issued for [`Surface::Direct`] sessions.
#[derive(Debug, Default)]
pub struct DirectRegistry {
    sessions: Mutex<HashMap<String, DirectSession>>,
    file_path: Option<PathBuf>,
    expected_uid: Option<u32>,
}

impl DirectRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_file(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut sessions = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(rows) = serde_json::from_str::<Vec<DirectSession>>(&text)
        {
            for s in rows {
                sessions.insert(s.id.clone(), s);
            }
        }
        Self { sessions: Mutex::new(sessions), file_path: Some(path), expected_uid: None }
    }

    pub fn with_expected_uid(mut self, uid: u32) -> Self {
        self.expected_uid = Some(uid);
        self
    }

    /// Register or re-issue a Direct session; returns `true` when an existing session ID was replaced.
    pub fn register(&self, session: DirectSession) -> bool {
        let id = session.id.clone();
        let inc = format!("direct:{}:{}", session.cwd, hex_sha256(session.token.as_bytes()));
        let replaced = self.sessions.lock().unwrap().insert(id.clone(), session).is_some();
        let key = format!("direct:{id}");
        let mut map = shared_generations().lock().unwrap();
        let entry = map.entry(key).or_default();
        if replaced || (entry.incarnation.is_some() && entry.incarnation.as_deref() != Some(inc.as_str())) {
            entry.generation += 1;
        }
        entry.incarnation = Some(inc);
        replaced
    }

    pub fn find(&self, id: &str) -> Option<DirectSession> {
        if !is_safe_id(id) {
            return None;
        }
        if let Some(path) = &self.file_path
            && let Ok(text) = std::fs::read_to_string(path)
            && let Ok(rows) = serde_json::from_str::<Vec<DirectSession>>(&text)
            && let Some(found) = rows.into_iter().find(|s| s.id == id)
        {
            return Some(found);
        }
        self.sessions.lock().unwrap().get(id).cloned()
    }

    pub fn list_live_sessions(&self) -> Vec<DirectSession> {
        let mut combined: HashMap<String, DirectSession> = self.sessions.lock().unwrap().clone();
        if let Some(path) = &self.file_path
            && let Ok(text) = std::fs::read_to_string(path)
            && let Ok(rows) = serde_json::from_str::<Vec<DirectSession>>(&text)
        {
            for s in rows {
                combined.insert(s.id.clone(), s);
            }
        }
        let mut out: Vec<DirectSession> = combined.into_values().filter(|s| s.liveness.is_live()).collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub fn verify(&self, claim: &CallerClaim) -> Result<VerifiedSession, IdentityRefusal> {
        if claim.surface != Surface::Direct {
            return Err(IdentityRefusal::new(
                RefusalReason::CrossSurface,
                format!("Surface `{}` cannot be verified by the Direct registry.", claim.surface.as_str()),
            ));
        }
        check_peer(self.expected_uid, claim.peer)?;
        let Some(id) = claim.session_id.as_deref().filter(|s| !s.is_empty()) else {
            return Err(IdentityRefusal::new(
                RefusalReason::MissingSessionId,
                "Direct session claim carried no session id.",
            ));
        };
        let Some(cwd) = claim.cwd.as_deref().filter(|s| !s.is_empty()) else {
            return Err(IdentityRefusal::new(RefusalReason::MissingCwd, "Direct session claim carried no folder."));
        };
        if !Path::new(cwd).is_absolute() {
            return Err(IdentityRefusal::new(
                RefusalReason::RelativeCwd,
                format!("The shell's folder must be an absolute path, not {cwd}."),
            ));
        }
        let session = match (self.find(id), claim.token.as_deref().filter(|t| !t.is_empty())) {
            (Some(s), Some(t))
                if !s.token.is_empty() && Sha256::digest(t.as_bytes()) == Sha256::digest(s.token.as_bytes()) =>
            {
                s
            }
            _ => {
                return Err(IdentityRefusal::new(
                    RefusalReason::InvalidToken,
                    "Direct session id or launch token is invalid.",
                ));
            }
        };
        if !session.liveness.is_live() {
            let short = short_id(id);
            return Err(IdentityRefusal::new(
                RefusalReason::NotLive,
                format!("Session {short} is {}, not live.", session.liveness.as_str()),
            ));
        }
        if !within(Path::new(cwd), Path::new(&session.cwd)) {
            let short = short_id(id);
            return Err(IdentityRefusal::new(
                RefusalReason::OutsideCwd,
                format!(
                    "This shell is in {cwd}, but session {short} runs in {}. Run maestro from inside that folder.",
                    session.cwd
                ),
            ));
        }
        let incarnation = format!("direct:{}:{}", session.cwd, hex_sha256(session.token.as_bytes()));
        Ok(VerifiedSession::new(
            SessionRef { surface: Surface::Direct, environment: None, id: session.id },
            session.cwd,
            session.harness,
            None,
            session.pending_permissions,
        )
        .with_incarnation(incarnation))
    }
}

/// Combined read-only surface registry across Paseo, T3, and Direct.
#[derive(Debug, Default)]
pub struct SurfaceRegistry {
    pub paseo: Option<PaseoRegistry>,
    pub t3: Option<T3Registry>,
    pub direct: Option<DirectRegistry>,
    expected_node: Option<NodeId>,
    bridge_secret: Option<Vec<u8>>,
    generations_file: Option<PathBuf>,
    generations: Mutex<GenerationLedger>,
}

impl SurfaceRegistry {
    pub fn new(paseo: Option<PaseoRegistry>, t3: Option<T3Registry>, direct: Option<DirectRegistry>) -> Self {
        Self {
            paseo,
            t3,
            direct,
            generations: Mutex::new(GenerationLedger {
                ledger_id: "in-memory-default".to_string(),
                entries: HashMap::new(),
            }),
            ..Self::default()
        }
    }

    /// Construct a `SurfaceRegistry` from environment variables (`MAESTRO_PASEO_AGENTS_DIR`,
    /// `MAESTRO_T3_HOME`, `MAESTRO_DIRECT_REGISTRY`, `MAESTRO_GENERATIONS_FILE`,
    /// `MAESTRO_NODE`, `MAESTRO_BRIDGE_SECRET`).
    pub fn from_env() -> Self {
        let paseo = std::env::var("MAESTRO_PASEO_AGENTS_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|p| PaseoRegistry::new(PathBuf::from(p)));
        let t3 =
            std::env::var("MAESTRO_T3_HOME").ok().filter(|s| !s.is_empty()).map(|p| T3Registry::new(PathBuf::from(p)));
        let direct =
            std::env::var("MAESTRO_DIRECT_REGISTRY").ok().filter(|s| !s.is_empty()).map(DirectRegistry::from_file);
        let mut reg = Self::new(paseo, t3, direct);
        if let Ok(gen_file) = std::env::var("MAESTRO_GENERATIONS_FILE")
            && !gen_file.is_empty()
        {
            reg = reg.with_generations_file(PathBuf::from(gen_file));
        }
        if let Ok(node_raw) = std::env::var("MAESTRO_NODE")
            && let Ok(node) = node_raw.trim().parse::<NodeId>()
        {
            reg = reg.with_expected_node(node);
        }
        if let Ok(secret) = std::env::var("MAESTRO_BRIDGE_SECRET")
            && !secret.is_empty()
        {
            reg = reg.with_bridge_secret(secret.into_bytes());
        }
        reg
    }

    pub fn with_expected_node(mut self, node: NodeId) -> Self {
        if let Some(t3) = self.t3.take() {
            self.t3 = Some(t3.with_expected_node(node.clone()));
        }
        self.expected_node = Some(node);
        self
    }

    pub fn with_bridge_secret(mut self, secret: impl Into<Vec<u8>>) -> Self {
        let bytes = secret.into();
        self.bridge_secret = (!bytes.is_empty()).then_some(bytes);
        self
    }

    pub fn has_bridge_secret(&self) -> bool {
        self.bridge_secret.is_some()
    }

    pub fn with_generations_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.generations_file = Some(path.into());
        self
    }

    pub fn has_surface(&self, surface: &Surface) -> bool {
        match surface {
            Surface::Paseo => self.paseo.is_some(),
            Surface::T3 => self.t3.is_some(),
            Surface::Direct => self.direct.is_some(),
            Surface::Other(_) => false,
        }
    }

    /// Synchronize and return `(ledger_id, generation)` for `(surface, session_id)` and its
    /// observed `incarnation`. Fails closed (`RegistryDisconnected`) if `generations_file` is
    /// configured and unreadable, corrupt, missing when `require_existing` is true, or unwritable.
    fn sync_generation(
        &self,
        surface: &Surface,
        session_id: &str,
        incarnation: Option<&str>,
        require_existing: bool,
    ) -> Result<(String, u64), SessionError> {
        let key = format!("{}:{session_id}", surface.as_str());
        if let Some(path) = &self.generations_file {
            let lock = acquire_generation_lock(path, surface)?;
            let (mut ledger, created_or_migrated) = load_generations_ledger(path, surface, require_existing)?;
            let mut dirty = created_or_migrated;
            let entry = ledger.entries.entry(key.clone()).or_insert_with(|| {
                dirty = true;
                GenerationEntry::default()
            });
            if let Some(inc) = incarnation {
                match entry.incarnation.as_deref() {
                    Some(prev) if prev != inc => {
                        entry.generation += 1;
                        entry.incarnation = Some(inc.to_string());
                        dirty = true;
                    }
                    None => {
                        entry.incarnation = Some(inc.to_string());
                        dirty = true;
                    }
                    _ => {}
                }
            }
            let gen_val = entry.generation;
            let cloned_entry = entry.clone();
            let ledger_id = ledger.ledger_id.clone();
            if dirty {
                store_generations_ledger(path, surface, &lock, &ledger)?;
            }
            let mut local = self.generations.lock().unwrap();
            local.ledger_id = ledger_id.clone();
            local.entries.insert(key.clone(), cloned_entry.clone());
            shared_generations().lock().unwrap().insert(key, cloned_entry);
            return Ok((ledger_id, gen_val));
        }

        let mut local = self.generations.lock().unwrap();
        if local.ledger_id.is_empty() {
            local.ledger_id = "in-memory-default".to_string();
        }
        let ledger_id = local.ledger_id.clone();
        let entry = local.entries.entry(key).or_default();
        if let Some(inc) = incarnation {
            match entry.incarnation.as_deref() {
                Some(prev) if prev != inc => {
                    entry.generation += 1;
                    entry.incarnation = Some(inc.to_string());
                }
                None => {
                    entry.incarnation = Some(inc.to_string());
                }
                _ => {}
            }
        }
        Ok((ledger_id, entry.generation))
    }

    /// Current generation counter for `(surface, session_id)`.
    pub fn generation_of(&self, surface: &Surface, session_id: &str) -> u64 {
        let key = format!("{}:{session_id}", surface.as_str());
        if let Some(path) = &self.generations_file
            && let Ok((ledger, _)) = load_generations_ledger(path, surface, false)
            && let Some(entry) = ledger.entries.get(&key)
        {
            return entry.generation;
        }
        self.generations.lock().unwrap().entries.get(&key).map(|e| e.generation).unwrap_or(0)
    }

    /// Fallible explicit generation bump that fails closed if `generations_file` cannot be read or written.
    pub fn try_bump_generation(&self, surface: &Surface, session_id: &str) -> Result<u64, SessionError> {
        let key = format!("{}:{session_id}", surface.as_str());
        if let Some(path) = &self.generations_file {
            let lock = acquire_generation_lock(path, surface)?;
            let (mut ledger, _) = load_generations_ledger(path, surface, false)?;
            let entry = ledger.entries.entry(key.clone()).or_default();
            entry.generation += 1;
            let next = entry.generation;
            let cloned = entry.clone();
            store_generations_ledger(path, surface, &lock, &ledger)?;
            let mut local = self.generations.lock().unwrap();
            local.ledger_id = ledger.ledger_id;
            local.entries.insert(key.clone(), cloned.clone());
            shared_generations().lock().unwrap().insert(key, cloned);
            return Ok(next);
        }
        let mut local = self.generations.lock().unwrap();
        let entry = local.entries.entry(key.clone()).or_default();
        entry.generation += 1;
        let next = entry.generation;
        shared_generations().lock().unwrap().insert(key, entry.clone());
        Ok(next)
    }

    /// Increment the generation counter when a session ID is re-issued or restarted,
    /// invalidating any earlier [`BridgeCredential`] for that ID.
    pub fn bump_generation(&self, surface: &Surface, session_id: &str) -> u64 {
        self.try_bump_generation(surface, session_id).unwrap_or_else(|_| {
            let key = format!("{}:{session_id}", surface.as_str());
            let mut local = self.generations.lock().unwrap();
            let entry = local.entries.entry(key).or_default();
            entry.generation += 1;
            entry.generation
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn sign_tuple(
        secret: &[u8],
        ledger_id: &str,
        incarnation: &str,
        session: &SessionRef,
        node: &NodeId,
        cwd: &str,
        issued_at_ms: i64,
        expires_at_ms: i64,
        generation: u64,
    ) -> String {
        let mut msg = Vec::new();
        msg.extend_from_slice(ledger_id.as_bytes());
        msg.push(0);
        msg.extend_from_slice(incarnation.as_bytes());
        msg.push(0);
        msg.extend_from_slice(session.to_string().as_bytes());
        msg.push(0);
        msg.extend_from_slice(node.as_str().as_bytes());
        msg.push(0);
        msg.extend_from_slice(cwd.as_bytes());
        msg.push(0);
        msg.extend_from_slice(&issued_at_ms.to_be_bytes());
        msg.extend_from_slice(&expires_at_ms.to_be_bytes());
        msg.extend_from_slice(&generation.to_be_bytes());
        let mac = hmac_sha256(secret, &msg);
        let mut hex = String::with_capacity(64);
        for b in mac {
            let _ = write!(&mut hex, "{b:02x}");
        }
        hex
    }

    /// Issue a short-lived tuple-bound [`BridgeCredential`] for a verified session.
    pub fn issue_bridge_credential(
        &self,
        verified: &VerifiedSession,
        node: NodeId,
        issued_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<BridgeCredential, SessionError> {
        let Some(secret) = self.bridge_secret.as_deref() else {
            return Err(SessionError::Refused(IdentityRefusal::new(
                RefusalReason::InvalidToken,
                "Bridge secret is not configured on this host.",
            )));
        };
        let Some(incarnation) = verified.incarnation() else {
            return Err(SessionError::Refused(IdentityRefusal::new(
                RefusalReason::InvalidToken,
                "Session record lacks an authoritative incarnation marker.",
            )));
        };
        let Some(ttl) = expires_at_ms.checked_sub(issued_at_ms) else {
            return Err(SessionError::StaleCredential {
                session_id: verified.agent_id().to_string(),
                reason: "timestamp subtraction overflowed".to_string(),
            });
        };
        if ttl <= 0 || ttl > MAX_BRIDGE_CREDENTIAL_TTL_MS {
            return Err(SessionError::StaleCredential {
                session_id: verified.agent_id().to_string(),
                reason: "credential lifetime must be > 0 and at most 1 hour".to_string(),
            });
        }
        if let Some(want_node) = &self.expected_node
            && want_node != &node
        {
            return Err(SessionError::WrongNode { expected: want_node.clone(), actual: node });
        }
        let (ledger_id, generation) =
            self.sync_generation(&verified.session().surface, verified.agent_id(), Some(incarnation), false)?;
        let signature = Self::sign_tuple(
            secret,
            &ledger_id,
            incarnation,
            verified.session(),
            &node,
            verified.cwd(),
            issued_at_ms,
            expires_at_ms,
            generation,
        );
        Ok(BridgeCredential {
            session: verified.session().clone(),
            node,
            cwd: verified.cwd().to_string(),
            issued_at_ms,
            expires_at_ms,
            generation,
            signature,
        })
    }

    /// Verify a caller claim, returning a typed [`SessionError`] on failure.
    pub fn verify_session(&self, claim: &CallerClaim, now_ms: Option<i64>) -> Result<VerifiedSession, SessionError> {
        if let (Some(want_node), Some(actual_node)) = (&self.expected_node, &claim.node)
            && want_node != actual_node
        {
            return Err(SessionError::WrongNode { expected: want_node.clone(), actual: actual_node.clone() });
        }
        if self.bridge_secret.is_some()
            && claim.surface != Surface::Direct
            && claim.credential.is_none()
            && claim.peer.is_none()
        {
            return Err(SessionError::Refused(IdentityRefusal::new(
                RefusalReason::InvalidToken,
                "Unsigned session claim refused: a signed bridge credential or transport peer is required.",
            )));
        }
        if let Some(cred) = &claim.credential {
            if self.bridge_secret.is_none() {
                return Err(SessionError::Refused(IdentityRefusal::new(
                    RefusalReason::InvalidToken,
                    "Bridge secret is not configured on this host.",
                )));
            }
            if cred.session.surface != claim.surface {
                return Err(SessionError::Refused(IdentityRefusal::new(
                    RefusalReason::CrossSurface,
                    format!(
                        "Bridge credential for surface `{}` cannot authenticate `{}`.",
                        cred.session.surface.as_str(),
                        claim.surface.as_str()
                    ),
                )));
            }
            if claim.session_id.as_deref() != Some(cred.session.id.as_str()) {
                return Err(SessionError::Refused(IdentityRefusal::new(
                    RefusalReason::InvalidToken,
                    "Bridge credential session id does not match claim.",
                )));
            }
            if let Some(want_node) = &self.expected_node
                && &cred.node != want_node
            {
                return Err(SessionError::WrongNode { expected: want_node.clone(), actual: cred.node.clone() });
            }
            if let Some(claim_node) = &claim.node
                && &cred.node != claim_node
            {
                return Err(SessionError::WrongNode { expected: cred.node.clone(), actual: claim_node.clone() });
            }
            let Some(ttl) = cred.expires_at_ms.checked_sub(cred.issued_at_ms) else {
                return Err(SessionError::StaleCredential {
                    session_id: cred.session.id.clone(),
                    reason: "timestamp overflow".to_string(),
                });
            };
            if ttl <= 0 || ttl > MAX_BRIDGE_CREDENTIAL_TTL_MS {
                return Err(SessionError::StaleCredential {
                    session_id: cred.session.id.clone(),
                    reason: "lifetime outside 1..=3600000 ms".to_string(),
                });
            }
            let effective_now_ms = now_ms.unwrap_or_else(current_time_ms);
            if effective_now_ms < cred.issued_at_ms || effective_now_ms >= cred.expires_at_ms {
                return Err(SessionError::StaleCredential {
                    session_id: cred.session.id.clone(),
                    reason: format!("now={effective_now_ms} outside [{}, {})", cred.issued_at_ms, cred.expires_at_ms),
                });
            }
        }

        let verified = self.verify_surface(claim)?;
        let Some(incarnation) = verified.incarnation() else {
            return Err(SessionError::Refused(IdentityRefusal::new(
                RefusalReason::InvalidToken,
                "Session record lacks an authoritative incarnation marker.",
            )));
        };
        let (ledger_id, current_gen) =
            self.sync_generation(&claim.surface, verified.agent_id(), Some(incarnation), claim.credential.is_some())?;
        if let Some(cred) = &claim.credential {
            if cred.generation != current_gen {
                return Err(SessionError::IdReused {
                    session_id: cred.session.id.clone(),
                    expected_generation: current_gen,
                    presented_generation: cred.generation,
                });
            }
            let secret = self.bridge_secret.as_deref().unwrap();
            let expected_sig = Self::sign_tuple(
                secret,
                &ledger_id,
                incarnation,
                &cred.session,
                &cred.node,
                &cred.cwd,
                cred.issued_at_ms,
                cred.expires_at_ms,
                cred.generation,
            );
            if Sha256::digest(cred.signature.as_bytes()) != Sha256::digest(expected_sig.as_bytes()) {
                return Err(SessionError::Refused(IdentityRefusal::new(
                    RefusalReason::InvalidToken,
                    "Bridge credential signature is invalid.",
                )));
            }
            if !within(Path::new(&cred.cwd), Path::new(verified.cwd())) {
                return Err(SessionError::Refused(IdentityRefusal::new(
                    RefusalReason::OutsideCwd,
                    "Bridge credential folder does not match registered session folder.",
                )));
            }
        }
        if let Some(presented_gen) = claim.generation
            && presented_gen != current_gen
        {
            return Err(SessionError::IdReused {
                session_id: verified.agent_id().to_string(),
                expected_generation: current_gen,
                presented_generation: presented_gen,
            });
        }
        let node = claim
            .credential
            .as_ref()
            .map(|c| c.node.clone())
            .or_else(|| claim.node.clone())
            .or_else(|| self.expected_node.clone());
        Ok(verified.with_node_and_generation(node, current_gen))
    }

    fn verify_surface(&self, claim: &CallerClaim) -> Result<VerifiedSession, SessionError> {
        match &claim.surface {
            Surface::Paseo => {
                if let Some(id) = claim.session_id.as_deref().filter(|s| !s.is_empty()) {
                    let in_t3 = id.starts_with("mcp:") || self.t3.as_ref().is_some_and(|t3| t3.find(id).is_some());
                    let in_paseo = self.paseo.as_ref().is_some_and(|p| p.find(id).is_some());
                    if in_t3 && !in_paseo {
                        return Err(IdentityRefusal::new(
                            RefusalReason::CrossSurface,
                            format!("No Paseo session has the id {id}: {id} is a T3 thread, not a Paseo session."),
                        )
                        .into());
                    }
                }
                let Some(paseo) = &self.paseo else {
                    return Err(SessionError::RegistryDisconnected {
                        surface: Surface::Paseo,
                        detail: "Paseo registry is not configured on this host".to_string(),
                    });
                };
                paseo.verify(claim).map_err(Into::into)
            }
            Surface::T3 => {
                if let Some(id) = claim.session_id.as_deref().filter(|s| !s.is_empty()) {
                    let in_paseo = self.paseo.as_ref().is_some_and(|p| p.find(id).is_some());
                    let in_t3 = self.t3.as_ref().is_some_and(|t3| t3.find(id).is_some());
                    if in_paseo && !in_t3 {
                        return Err(IdentityRefusal::new(
                            RefusalReason::CrossSurface,
                            format!("No T3 thread has the id {id}: {id} is a Paseo session, not a T3 thread."),
                        )
                        .into());
                    }
                }
                let Some(t3) = &self.t3 else {
                    return Err(SessionError::RegistryDisconnected {
                        surface: Surface::T3,
                        detail: "T3 registry is not configured on this host".to_string(),
                    });
                };
                t3.verify(claim).map_err(Into::into)
            }
            Surface::Direct => {
                let Some(direct) = &self.direct else {
                    return Err(SessionError::RegistryDisconnected {
                        surface: Surface::Direct,
                        detail: "Direct registry is not configured on this host".to_string(),
                    });
                };
                direct.verify(claim).map_err(Into::into)
            }
            Surface::Other(name) => {
                Err(IdentityRefusal::new(RefusalReason::UnsupportedSurface, format!("Unsupported surface `{name}`."))
                    .into())
            }
        }
    }

    /// Verify a caller claim against its surface registry using the current system clock,
    /// refusing cross-surface spoofing, wrong node, stale credential, ID reuse, or disconnected registries.
    pub fn verify_caller(&self, claim: &CallerClaim) -> Result<VerifiedSession, IdentityRefusal> {
        self.verify_session(claim, None).map_err(SessionError::into_refusal)
    }

    /// Verify a caller claim at `now_ms`, checking bridge credential freshness when supplied.
    pub fn verify_caller_at(&self, claim: &CallerClaim, now_ms: i64) -> Result<VerifiedSession, IdentityRefusal> {
        self.verify_session(claim, Some(now_ms)).map_err(SessionError::into_refusal)
    }

    /// Capture a [`SessionSnapshot`] for a verified caller claim.
    pub fn snapshot_session(&self, claim: &CallerClaim, now_ms: Option<i64>) -> Result<SessionSnapshot, SessionError> {
        let verified = self.verify_session(claim, now_ms)?;
        let discovery = match &claim.surface {
            Surface::T3 => self.t3.as_ref().map(T3Registry::discover),
            _ => None,
        };
        Ok(verified.snapshot(discovery))
    }

    /// Compute [`SessionUpdate`] events between two snapshots of a session.
    pub fn diff_snapshots(before: Option<&SessionSnapshot>, after: Option<&SessionSnapshot>) -> Vec<SessionUpdate> {
        let mut updates = Vec::new();
        match (before, after) {
            (None, Some(curr)) => {
                updates.push(SessionUpdate::Snapshot { snapshot: Box::new(curr.clone()) });
            }
            (Some(prev), Some(curr)) => {
                if prev.liveness != curr.liveness {
                    updates.push(SessionUpdate::LivenessChanged {
                        session: curr.session.clone(),
                        liveness: curr.liveness,
                    });
                }
                if prev.pending_permissions != curr.pending_permissions {
                    updates.push(SessionUpdate::PermissionsChanged {
                        session: curr.session.clone(),
                        pending_permissions: curr.pending_permissions.clone(),
                    });
                }
                if let (Some(d1), Some(d2)) = (&prev.discovery, &curr.discovery)
                    && (d1.connected != d2.connected || d1.version != d2.version)
                {
                    updates.push(SessionUpdate::BridgeStateChanged {
                        surface: curr.session.surface.clone(),
                        connected: d2.connected,
                        version: Some(d2.version.clone()),
                    });
                }
            }
            (Some(prev), None) => {
                updates.push(SessionUpdate::Ended { session: prev.session.clone() });
            }
            (None, None) => {}
        }
        updates
    }

    /// Convert all live permission prompts on a verified session into [`Need`] items.
    pub fn needs_for(&self, session: &VerifiedSession, project: Option<&str>, since: Option<&str>) -> Vec<Need> {
        session.needs(project, since)
    }

    /// Enumerate all live permission prompts across configured Paseo, T3, and Direct registries
    /// and convert them into [`Need`] items.
    pub fn live_permission_needs(&self, project: Option<&str>, since: Option<&str>) -> Vec<Need> {
        let mut out = Vec::new();
        if let Some(paseo) = &self.paseo {
            for agent in paseo.list_live_agents() {
                let sref = SessionRef { surface: Surface::Paseo, environment: None, id: agent.id };
                for prompt in &agent.pending_permissions {
                    out.push(prompt.to_need(&sref, project.map(str::to_string), since.map(str::to_string)));
                }
            }
        }
        if let Some(t3) = &self.t3 {
            for thread in t3.list_live_threads() {
                let sref = SessionRef { surface: Surface::T3, environment: thread.environment_id, id: thread.id };
                for prompt in &thread.pending_permissions {
                    out.push(prompt.to_need(&sref, project.map(str::to_string), since.map(str::to_string)));
                }
            }
        }
        if let Some(direct) = &self.direct {
            for session in direct.list_live_sessions() {
                let sref = SessionRef { surface: Surface::Direct, environment: None, id: session.id };
                for prompt in &session.pending_permissions {
                    out.push(prompt.to_need(&sref, project.map(str::to_string), since.map(str::to_string)));
                }
            }
        }
        out
    }
}
