//! Agent requests on disk: an ask filed into its ledger, and the requests journal (BRO-3267).
//!
//! Until phase 2 Maestro is not the asks writer of record: it files *through* the workspace's
//! `scripts/ask_ledger.py`. Under the ledger's §5.5 lock ([`crate::locks`], the lock the Paseo
//! plugin answers under) it reads the ledger, appends the ask as one list entry, writes the result
//! to the plugin's temp name beside it, runs `ask_ledger.py validate` on that temp, and renames it
//! over the ledger only when the validator raised no error the ledger did not already have, the
//! YAML reads back as the old ledger plus exactly this ask, and the lock is still ours. Anything
//! else writes nothing.

use crate::locks::{self, AcquireOptions, LockError};
use crate::paths::Paths;
use maestro_core::authority::Principal;
use maestro_core::request::{RequestKind, RequestRecord};
use maestro_core::session::SessionRef;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// How long a filer waits for the ledger's lock. Longer than the plugin's 5 s for an owner's
/// answer: an agent can wait, and each holder runs the validator (~0.1 s) under it.
pub const FILE_WAIT: Duration = Duration::from_secs(30);

/// Keys that answer or close an ask. An agent never files one: that would be answering for the owner.
pub const OWNER_KEYS: &[&str] = &[
    "answer",
    "answered",
    "answered_at",
    "answered_by",
    "answer_option",
    "answer_note",
    "answer_other",
    "resolution",
    "outcome",
    "status",
    "archived_at",
    "origin",
];

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
    /// The validator's summary line for the written ledger.
    pub validator: String,
}

fn now_rfc3339() -> String {
    jiff::Timestamp::now().strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The ask as it is written: the caller's fields, `asked_at` when not given, and Maestro's
/// `origin` naming the session and whether it was verified.
pub fn compose_ask(
    ask: &Map<String, Value>,
    session: &SessionRef,
    caller: &Principal,
) -> Result<Map<String, Value>, FileError> {
    let id = ask.get("id").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    if id.is_empty() {
        return Err(refused("the ask needs an `id`"));
    }
    if let Some(key) = OWNER_KEYS.iter().find(|k| ask.contains_key(**k)) {
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
    let verified = matches!(caller, Principal::Agent { .. } | Principal::Owner { .. });
    // No `agent_id`: an answer is routed into a session only once its surface has vouched for it.
    out.insert("origin".into(), json!({ "via": "maestro", "session": session.to_string(), "verified": verified }));
    Ok(out)
}

/// The ask rendered as one block-sequence entry at `indent` spaces.
fn render_entry(ask: &Map<String, Value>, indent: usize) -> Result<String, FileError> {
    // serde_json's map is sorted; YAML keeps the order given, so `id` goes first by hand.
    let mut doc = serde_yaml::Mapping::new();
    let to_yaml = |v: &Value| serde_yaml::to_value(v).map_err(|e| refused(e.to_string()));
    doc.insert("id".into(), to_yaml(&ask["id"])?);
    for (k, v) in ask.iter().filter(|(k, _)| *k != "id") {
        doc.insert(k.as_str().into(), to_yaml(v)?);
    }
    let text = serde_yaml::to_string(&serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(doc)]))
        .map_err(|e| refused(e.to_string()))?;
    let pad = " ".repeat(indent);
    Ok(text.lines().map(|l| format!("{pad}{l}\n")).collect())
}

fn is_top_key(line: &str, key: &str) -> bool {
    line.strip_prefix(key).and_then(|r| r.strip_prefix(':')).is_some()
}

/// The ledger text with `entry` (rendered at the list's indent) appended to its `asks:` list.
/// A ledger whose `asks` is not a block list (or the empty flow list `[]`) is refused.
pub fn append_entry(text: &str, ask: &Map<String, Value>) -> Result<String, FileError> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let Some(at) = lines.iter().position(|l| is_top_key(l, "asks")) else {
        let mut out = text.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("\nasks:\n");
        out.push_str(&render_entry(ask, 2)?);
        return Ok(out);
    };
    let rest = lines[at]["asks:".len()..].trim();
    let rest = rest.split_once(" #").map_or(rest, |(v, _)| v).trim();
    if rest == "[]" {
        let mut out: String = lines[..at].concat();
        out.push_str("asks:\n");
        out.push_str(&render_entry(ask, 2)?);
        out.push_str(&lines[at + 1..].concat());
        return Ok(out);
    }
    if !(rest.is_empty() || rest.starts_with('#')) {
        return Err(refused("the ledger's `asks` is not a block list, so the entry cannot be appended"));
    }
    // The list's indent is its first item's; the block ends at the next line at column 0 that is
    // content and not one of its items.
    let mut indent = None;
    let mut end = lines.len();
    for (i, line) in lines.iter().enumerate().skip(at + 1) {
        let body = line.trim_end_matches(['\n', '\r']);
        let trimmed = body.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let col = body.len() - trimmed.len();
        if indent.is_none() && (trimmed == "-" || trimmed.starts_with("- ")) {
            indent = Some(col);
        }
        if col == 0 && !(indent == Some(0) && (trimmed == "-" || trimmed.starts_with("- "))) {
            end = i;
            break;
        }
    }
    // Trailing blank and comment lines stay with what follows the block.
    let mut cut = end;
    while cut > at + 1 && lines[cut - 1].trim().is_empty() {
        cut -= 1;
    }
    let mut out: String = lines[..cut].concat();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&render_entry(ask, indent.unwrap_or(2))?);
    out.push_str(&lines[cut..].concat());
    Ok(out)
}

/// The ledger read back must be the old one plus exactly `ask` at the end of `asks`.
fn check_appended(old: &str, new: &str, ask: &Map<String, Value>) -> Result<(), FileError> {
    let read = |t: &str| -> Result<serde_json::Value, FileError> {
        if t.trim().is_empty() {
            return Ok(json!({}));
        }
        let v: serde_yaml::Value =
            serde_yaml::from_str(t).map_err(|e| refused(format!("the ledger does not read: {e}")))?;
        serde_json::to_value(v).map_err(|e| refused(e.to_string()))
    };
    let (mut before, mut after) = (read(old)?, read(new)?);
    let (Some(b), Some(a)) = (before.as_object_mut(), after.as_object_mut()) else {
        return Err(refused("the ledger is not a mapping"));
    };
    let mut asks_before = match b.remove("asks") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(v)) => v,
        Some(_) => return Err(refused("the ledger's `asks` is not a list")),
    };
    let asks_after = match a.remove("asks") {
        Some(Value::Array(v)) => v,
        _ => return Err(refused("the appended ledger lost its `asks` list")),
    };
    asks_before.push(Value::Object(ask.clone()));
    if *b != *a || asks_before != asks_after {
        return Err(refused("the appended ledger does not read as the old one plus this ask; nothing was written"));
    }
    // Maestro's own reader, the plugin's subset, must read the new entry too.
    let read = crate::yaml::parse(new).map_err(|e| refused(format!("Maestro's reader refuses the result: {e}")))?;
    let (ledger, bad) = maestro_core::ask::AskLedger::from_value(read.value).map_err(refused)?;
    let id = ask["id"].as_str().unwrap_or_default();
    if !bad.is_empty() || ledger.asks.last().map(|a| a.id.as_str()) != Some(id) {
        return Err(refused("Maestro's reader does not read the appended ask; nothing was written"));
    }
    Ok(())
}

/// `ask_ledger.py validate <file>`: its error lines (path-free) and its summary.
fn validate(script: &Path, workspace: &Path, file: &Path) -> Result<(Vec<String>, String), FileError> {
    if !script.is_file() {
        return Err(refused(format!(
            "the validator of record is not at {}; nothing was written (set MAESTRO_ASK_LEDGER)",
            script.display()
        )));
    }
    let preauth =
        [".broomva/preauth.yaml", ".control/preauth.yaml"].iter().map(|r| workspace.join(r)).find(|p| p.is_file());
    let mut cmd = Command::new("python3");
    cmd.arg(script).arg("validate").arg(file).current_dir(workspace).stdin(Stdio::null());
    match preauth {
        Some(p) => cmd.arg("--preauth").arg(p),
        None => cmd.arg("--preauth").arg("/nonexistent/preauth.yaml"),
    };
    let out = cmd.output().map_err(|e| refused(format!("the validator did not run: {e}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout).replace(&file.display().to_string(), "<ledger>");
    let errors: Vec<String> =
        stdout.lines().filter(|l| l.trim_start().starts_with("ERROR")).map(|l| l.trim().to_string()).collect();
    let summary = stdout.lines().rev().find(|l| l.starts_with("ask-ledger:")).unwrap_or_default().to_string();
    if summary.is_empty() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(refused(format!(
            "the validator gave no verdict (exit {:?}): {}",
            out.status.code(),
            stderr.lines().last().unwrap_or_default()
        )));
    }
    Ok((errors, summary))
}

/// Files `ask` into `<asks dir>/<ledger>` through the validator of record, under the ledger's lock.
pub fn file_ask(
    paths: &Paths,
    asks_dir: &Path,
    ledger: &str,
    ask: &Map<String, Value>,
    session: &SessionRef,
    caller: &Principal,
) -> Result<Filed, FileError> {
    let lock_root = paths.ask_locks.as_path();
    let entry = compose_ask(ask, session, caller)?;
    if !ledger_name_ok(ledger) {
        return Err(refused(format!("`{ledger}` is not a ledger name (a plain `*.yaml` file name)")));
    }
    if !asks_dir.is_dir() {
        return Err(refused(format!("no asks folder at {}", asks_dir.display())));
    }
    let id = entry["id"].as_str().unwrap_or_default().to_string();
    let path = asks_dir.join(ledger);
    let script = validator_script(paths);
    let options = AcquireOptions { wait: FILE_WAIT, what: ledger.to_string(), ..Default::default() };
    locks::with_lock(&path, lock_root, &options, |lock| {
        // A ledger opens with its arc, lanes and time zone (ask_ledger.py requires them), which a
        // filer cannot know: it is made in a workspace change, and asks are filed into it.
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_file() => m,
            Ok(_) => return Err(refused(format!("{ledger} is not a regular file; nothing was written"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(refused(format!(
                    "no ledger {ledger} in {}: a ledger is opened with its arc, lanes and tz in a workspace change; nothing was written",
                    asks_dir.display()
                )));
            }
            Err(e) => return Err(e.into()),
        };
        let old = std::fs::read_to_string(&path)?;
        if let Ok(read) = crate::yaml::parse(&old)
            && let Ok((existing, _)) = maestro_core::ask::AskLedger::from_value(read.value)
            && existing.asks.iter().any(|a| a.id == id || a.uid.as_deref() == Some(id.as_str()))
        {
            return Err(refused(format!("{ledger} already has an ask `{id}`; nothing was written")));
        }
        let new = append_entry(&old, &entry)?;
        check_appended(&old, &new, &entry)?;
        let temp = asks_dir.join(format!(".{ledger}.tmp-{}", lock.token));
        let written = (|| {
            std::fs::write(&temp, &new)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(meta.permissions().mode() & 0o777))?;
            }
            let (after, summary) = validate(&script, &paths.workspace, &temp)?;
            let mut known = validate(&script, &paths.workspace, &path)?.0;
            let fresh: Vec<String> = after
                .into_iter()
                .filter(|e| match known.iter().position(|k| k == e) {
                    Some(i) => {
                        known.swap_remove(i);
                        false
                    }
                    None => true,
                })
                .collect();
            if !fresh.is_empty() {
                return Err(refused(format!(
                    "ask_ledger.py refused the ask; nothing was written:\n{}",
                    fresh.join("\n")
                )));
            }
            if !locks::still_ours(lock) {
                return Err(refused("this ledger's lock was taken over; nothing was written"));
            }
            std::fs::rename(&temp, &path)?;
            Ok(summary)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        Ok(Filed { ledger: path.clone(), id: id.clone(), validator: written? })
    })
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

    fn ask(id: &str) -> Map<String, Value> {
        json!({ "id": id, "ask": "q?" }).as_object().unwrap().clone()
    }

    fn appended(text: &str) -> serde_yaml::Value {
        let new = append_entry(text, &ask("new")).unwrap();
        check_appended(text, &new, &ask("new")).unwrap_or_else(|e| panic!("{e}\n{new}"));
        serde_yaml::from_str(&new).unwrap()
    }

    fn ids(v: &serde_yaml::Value) -> Vec<String> {
        v["asks"].as_sequence().unwrap().iter().map(|a| a["id"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn appends_to_every_list_shape() {
        let indented = "arc: a\nasks:\n  - id: one\n    ask: x\n\n# trailing note\ndecisions: []\n";
        let v = appended(indented);
        assert_eq!(ids(&v), ["one", "new"]);
        assert!(v["decisions"].as_sequence().unwrap().is_empty());
        let column0 = "asks:\n- id: one\n  ask: x\n- id: two\n  ask: y\nlanes: [a]\n";
        assert_eq!(ids(&appended(column0)), ["one", "two", "new"]);
        assert_eq!(ids(&appended("arc: a\nasks: []\nlanes: [a]\n")), ["new"]);
        assert_eq!(ids(&appended("arc: a\nasks: [] # none yet\n")), ["new"]);
        assert_eq!(ids(&appended("arc: a\nlanes: [a]")), ["new"]);
        assert_eq!(ids(&appended("asks:\n  - id: one\n    ask: |\n      x\n\n      y\n")), ["one", "new"]);
        assert!(append_entry("asks: [{id: one}]\n", &ask("new")).is_err(), "a flow list is refused");
    }

    #[test]
    fn a_changed_ledger_is_caught() {
        let old = "arc: a\nasks:\n  - id: one\n";
        let lost = "arc: a\nasks:\n  - id: new\n    ask: q?\n";
        assert!(check_appended(old, lost, &ask("new")).is_err(), "an entry lost");
        let moved = "arc: b\nasks:\n  - id: one\n  - id: new\n    ask: q?\n";
        assert!(check_appended(old, moved, &ask("new")).is_err(), "another key changed");
        let fine = "arc: a\nasks:\n  - id: one\n  - id: new\n    ask: q?\n";
        check_appended(old, fine, &ask("new")).unwrap();
    }

    #[test]
    fn the_filer_writes_no_owner_key_and_no_routable_origin() {
        let session: SessionRef = "paseo:-/a1".parse().unwrap();
        for key in OWNER_KEYS {
            let mut a = ask("x");
            a.insert((*key).to_string(), json!("v"));
            assert!(compose_ask(&a, &session, &Principal::Unverified).is_err(), "{key}");
        }
        let out = compose_ask(&ask("x"), &session, &Principal::Unverified).unwrap();
        assert_eq!(out["origin"], json!({ "via": "maestro", "session": "paseo:-/a1", "verified": false }));
        assert!(out["asked_at"].as_str().is_some_and(|t| t.ends_with('Z')));
        assert!(compose_ask(&ask("  "), &session, &Principal::Unverified).is_err());
    }
}
