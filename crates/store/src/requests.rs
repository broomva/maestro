//! Agent requests on disk: an ask filed into its ledger, and the requests journal (BRO-3267).
//!
//! Until phase 2 Maestro is not the asks writer of record: it files *through* the workspace's
//! `scripts/ask_ledger.py file` (BRO-3406), which appends the entry in the line shape its own
//! `stamp` and `archive` edit, under the ledger's §5.5 lock (the lock the Paseo plugin answers
//! under) and its flock, and writes nothing unless the ledger reads back as its asks plus exactly
//! this one with no new validator error. Maestro writes no YAML: three review rounds of its own
//! emitter found entries the validator accepted and those writers later mis-edited (maestro#11).
//! The journal ([`record`]) is Maestro's own, under [`crate::locks`].

use crate::locks::{self, AcquireOptions, LockError};
use crate::paths::Paths;
use maestro_core::authority::Principal;
use maestro_core::request::{RequestKind, RequestRecord};
use maestro_core::session::SessionRef;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// How long a filer waits for the ledger's lock. Longer than the plugin's 5 s for an owner's
/// answer: an agent can wait, and each holder runs the validator (~0.1 s) under it.
pub const FILE_WAIT: Duration = Duration::from_secs(30);

/// Keys an agent never files, beside the plugin's `OUTCOME_KEYS` ([`maestro_core::ask::OUTCOME_KEYS`]):
/// the rest of an answer, a status, an archive mark, and the origin, which is Maestro's.
pub const FILER_REFUSED: &[&str] = &[
    "answered_by",
    "answer_option",
    "answer_note",
    "answer_other",
    "status",
    "archived_at",
    "archived_reason",
    "origin",
];

/// Whether `key` answers, closes or routes an ask, and so is never the filer's.
pub fn refused_key(key: &str) -> bool {
    maestro_core::ask::OUTCOME_KEYS.contains(&key) || FILER_REFUSED.contains(&key)
}

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    /// The request itself is wrong; nothing was written.
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    Lock(#[from] LockError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

fn refused(reason: impl Into<String>) -> FileError {
    FileError::Refused(reason.into())
}

/// Where the validator of record is: `MAESTRO_ASK_LEDGER`, else `<workspace>/scripts/ask_ledger.py`.
pub fn validator_script(paths: &Paths) -> PathBuf {
    std::env::var_os("MAESTRO_ASK_LEDGER")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| paths.workspace.join("scripts/ask_ledger.py"))
}

/// A ledger name as the plugin's `LEDGER` reads one: `^[^./][^/]*\.ya?ml$`.
pub fn ledger_name_ok(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['.', '/'])
        && !name.contains(['/', '\\', '\0'])
        && (name.ends_with(".yaml") || name.ends_with(".yml"))
}

/// What a filed ask became.
#[derive(Debug, Clone)]
pub struct Filed {
    pub ledger: PathBuf,
    pub id: String,
    /// What the validator of record said of the written ledger.
    pub validator: String,
}

fn now_rfc3339() -> String {
    jiff::Timestamp::now().strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The ask as it is handed to the writer of record: the caller's fields, `id` first, and
/// `asked_at` when not given. The origin is the writer's (`--maestro-session`): `{via: maestro,
/// session, verified: false}`, no `agent_id`, so no answer is routed into a session until its
/// surface vouches for it (host-identity). The id is passed as given: the writer refuses one
/// outside its pattern rather than Maestro trimming it into another.
pub fn compose_ask(ask: &Map<String, Value>) -> Result<Map<String, Value>, FileError> {
    let id = ask.get("id").and_then(Value::as_str).unwrap_or_default();
    if id.is_empty() {
        return Err(refused("the ask needs an `id`"));
    }
    if let Some(key) = ask.keys().find(|k| refused_key(k)) {
        return Err(refused(format!(
            "`{key}` is not the filer's: answers, status and origin are written by the owner's verbs and by Maestro"
        )));
    }
    let mut out = Map::new();
    out.insert("id".into(), Value::String(id.to_string()));
    for (k, v) in ask {
        if k != "id" {
            out.insert(k.clone(), v.clone());
        }
    }
    if !out.contains_key("asked_at") {
        out.insert("asked_at".into(), Value::String(now_rfc3339()));
    }
    Ok(out)
}

/// How `ask_ledger.py file` ended: 0 filed, 2 refused, 75 the lock was busy or taken over.
const EX_TEMPFAIL: i32 = 75;

/// Files `ask` into `<asks dir>/<ledger>` with the writer of record, `ask_ledger.py file`
/// (BRO-3406). It takes the ledger's §5.5 lock (the lock root is passed on, so it is Maestro's)
/// and the flock, appends the ask in the line shape `stamp` and `archive` edit, records Maestro's
/// unverified origin from `--maestro-session`, and writes nothing unless the result reads as the
/// ledger's asks plus exactly this one with no new validator error. Maestro holds no lock of its
/// own around it: the §5.5 lock is not reentrant.
pub fn file_ask(
    paths: &Paths,
    asks_dir: &Path,
    ledger: &str,
    ask: &Map<String, Value>,
    session: &SessionRef,
    caller: &Principal,
) -> Result<Filed, FileError> {
    let _ = caller; // every filing's origin is unverified until host-identity (see compose_ask)
    let entry = compose_ask(ask)?;
    if !ledger_name_ok(ledger) {
        return Err(refused(format!("`{ledger}` is not a ledger name (a plain `*.yaml` file name)")));
    }
    if !asks_dir.is_dir() {
        return Err(refused(format!("no asks folder at {}", asks_dir.display())));
    }
    let path = asks_dir.join(ledger);
    // A ledger opens with its arc, lanes and time zone (ask_ledger.py requires them), which a
    // filer cannot know: it is made in a workspace change, and asks are filed into it.
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.is_file() => {}
        Ok(_) => return Err(refused(format!("{ledger} is not a regular file; nothing was written"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(refused(format!(
                "no ledger {ledger} in {}: a ledger is opened with its arc, lanes and tz in a workspace change; nothing was written",
                asks_dir.display()
            )));
        }
        Err(e) => return Err(e.into()),
    }
    let script = validator_script(paths);
    if !script.is_file() {
        return Err(refused(format!(
            "the writer of record is not at {}; nothing was written (set MAESTRO_ASK_LEDGER)",
            script.display()
        )));
    }
    let id = entry["id"].as_str().unwrap_or_default().to_string();
    let body = serde_json::to_string(&entry).map_err(|e| refused(e.to_string()))?;
    // -P: neither the workspace (the cwd) nor the script's folder shadows a module it imports.
    let mut child = Command::new("python3")
        .arg("-P")
        .arg(&script)
        .arg("file")
        .arg(&path)
        // One `--opt=value` word: a session starting with `-` is a value, never an option.
        .args(["--json", "-", &format!("--maestro-session={session}"), "--wait"])
        .arg(FILE_WAIT.as_secs().to_string())
        .env("MAESTRO_PASEO_ASK_LOCKS", &paths.ask_locks)
        .current_dir(&paths.workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| refused(format!("the writer of record did not run: {e}")))?;
    {
        use std::io::Write as _;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        stdin.write_all(body.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    let said = |bytes: &[u8]| {
        let text = String::from_utf8_lossy(bytes);
        let text = text.trim();
        text.strip_prefix("error: ").unwrap_or(text).to_string()
    };
    match out.status.code() {
        Some(0) => Ok(Filed { ledger: path, id, validator: said(&out.stdout) }),
        Some(EX_TEMPFAIL) => Err(FileError::Lock(LockError::Busy { holder: said(&out.stderr), what: ledger.into() })),
        Some(2) => Err(refused(said(&out.stderr))),
        code => Err(refused(format!(
            "ask_ledger.py file failed (exit {code:?}); nothing is known to be written: {}",
            said(&out.stderr).lines().last().unwrap_or_default()
        ))),
    }
}

/// The requests journal: `$BROOMVA_HOME/ledger/requests/requests.jsonl`.
pub fn journal(paths: &Paths) -> PathBuf {
    paths.ledger("requests").expect("a plain stream name").join("requests.jsonl")
}

/// Appends one request to the journal under its lock (lock root `$BROOMVA_HOME/locks`).
pub fn record(paths: &Paths, record: &RequestRecord) -> Result<(), FileError> {
    let file = journal(paths);
    let dir = file.parent().expect("journal has a folder");
    std::fs::create_dir_all(dir)?;
    let root = paths.broomva_home.join("locks");
    let line = format!("{}\n", serde_json::to_string(record).map_err(|e| refused(e.to_string()))?);
    let options = AcquireOptions { wait: FILE_WAIT, what: "the requests journal".into(), ..Default::default() };
    locks::with_lock(&file, &root, &options, |_| {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&file)?;
        f.write_all(line.as_bytes())?;
        f.sync_data()?;
        Ok::<_, FileError>(())
    })
}

/// A new journal record for `kind` from `session`, as `caller` was verified.
pub fn new_record(kind: RequestKind, session: SessionRef, caller: &Principal, payload: Value) -> RequestRecord {
    let id = random_id().unwrap_or_else(|| format!("{:016x}", jiff::Timestamp::now().as_nanosecond() as u64));
    RequestRecord {
        kind: "agent.request".into(),
        id,
        at: now_rfc3339(),
        request: kind,
        session,
        verified: !matches!(caller, Principal::Unverified),
        disposition: kind.disposition(caller),
        payload,
        result: None,
    }
}

fn random_id() -> Option<String> {
    use std::io::Read as _;
    let mut buf = [0u8; 8];
    std::fs::File::open("/dev/urandom").ok()?.read_exact(&mut buf).ok()?;
    Some(buf.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ask(id: &str) -> Map<String, Value> {
        json!({ "id": id, "ask": "q?" }).as_object().unwrap().clone()
    }

    #[test]
    fn the_filer_writes_no_owner_key_and_no_routable_origin() {
        for key in maestro_core::ask::OUTCOME_KEYS.iter().chain(FILER_REFUSED) {
            let mut a = ask("x");
            a.insert((*key).to_string(), json!("v"));
            assert!(compose_ask(&a).is_err(), "{key}");
        }
        let out = compose_ask(&ask("x")).unwrap();
        assert!(out.get("origin").is_none(), "the origin is the writer of record's");
        assert!(out["asked_at"].as_str().is_some_and(|t| t.ends_with('Z')));
        assert!(compose_ask(&ask("")).is_err());
    }
}
