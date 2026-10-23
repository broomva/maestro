//! Agent requests on disk: an ask filed into its ledger, and the requests journal (BRO-3267).
//!
//! Until phase 2 Maestro is not the asks writer of record: it files *through* the workspace's
//! `scripts/ask_ledger.py`. Under the ledger's §5.5 lock ([`crate::locks`], the lock the Paseo
//! plugin answers under) it reads the ledger, appends the ask as one list entry, and writes the
//! result to the plugin's temp name beside it. The validator of record's own `load`, `validate`
//! and `asks_of` then read the ledger and the temp, each judged as the ledger's real path (its
//! name rules and repo are the ledger's). The temp is renamed over the ledger only when:
//! the validator raised no error the ledger did not already have; its `asks_of` reads the temp as
//! the ledger's asks plus exactly this ask, value for value; Maestro's reader agrees and reads the
//! new ask as open; and the lock is still ours. Anything else writes nothing.

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

/// Keys an agent never files, beside the plugin's `OUTCOME_KEYS` ([`maestro_core::ask::OUTCOME_KEYS`]):
/// the rest of an answer, a status, an archive mark, and the origin, which is Maestro's.
pub const FILER_REFUSED: &[&str] =
    &["answered_by", "answer_option", "answer_note", "answer_other", "status", "archived_at", "origin"];

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
    let verified = matches!(caller, Principal::Agent { .. } | Principal::Owner { .. });
    // No `agent_id`: an answer is routed into a session only once its surface has vouched for it.
    out.insert("origin".into(), json!({ "via": "maestro", "session": session.to_string(), "verified": verified }));
    Ok(out)
}

/// A key written plain when it is an identifier, else as a JSON (YAML double-quoted) string.
fn yaml_key(k: &str) -> String {
    let plain = k.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if plain { k.to_string() } else { Value::String(k.into()).to_string() }
}

/// A scalar or an empty collection on one line. Every string is double-quoted (JSON's escapes are
/// YAML's), so YAML 1.1 (PyYAML: `no`, `on`, timestamps) and 1.2 readers read the same value.
fn yaml_inline(v: &Value) -> Option<String> {
    match v {
        Value::Array(a) if a.is_empty() => Some("[]".into()),
        Value::Object(m) if m.is_empty() => Some("{}".into()),
        Value::Array(_) | Value::Object(_) => None,
        scalar => Some(scalar.to_string()),
    }
}

/// `v` as block YAML lines under a parent at `indent`.
fn yaml_block(v: &Value, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    match v {
        Value::Object(m) => {
            for (k, item) in m {
                match yaml_inline(item) {
                    Some(line) => out.push_str(&format!("{pad}{}: {line}\n", yaml_key(k))),
                    None => {
                        out.push_str(&format!("{pad}{}:\n", yaml_key(k)));
                        yaml_block(item, indent + 2, out);
                    }
                }
            }
        }
        Value::Array(a) => {
            for item in a {
                match yaml_inline(item) {
                    Some(line) => out.push_str(&format!("{pad}- {line}\n")),
                    None => {
                        // The item's first line takes the dash; the rest line up under it.
                        let mut inner = String::new();
                        yaml_block(item, indent + 2, &mut inner);
                        out.push_str(&format!("{pad}- {}", &inner[indent + 2..]));
                    }
                }
            }
        }
        scalar => out.push_str(&format!("{pad}{scalar}\n")),
    }
}

/// The ask rendered as one block-sequence entry at `indent` spaces, `id` first.
fn render_entry(ask: &Map<String, Value>, indent: usize) -> Result<String, FileError> {
    let mut ordered = Map::new();
    let mut body = String::new();
    // serde_json's map is sorted, so `id` is written first by hand.
    ordered.insert("id".into(), ask["id"].clone());
    let mut first = String::new();
    yaml_block(&Value::Object(ordered), indent + 2, &mut first);
    let rest: Map<String, Value> =
        ask.iter().filter(|(k, _)| *k != "id").map(|(k, v)| (k.clone(), v.clone())).collect();
    yaml_block(&Value::Object(rest), indent + 2, &mut body);
    Ok(format!("{}- {}{body}", " ".repeat(indent), &first[indent + 2..]))
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
    // Trailing blank lines stay with what follows the block.
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
    // Maestro's own reader (the plugin's subset and its `asksKey`) reads the old asks, then this
    // one, open: an `asks` list beside an `open_asks` one, or an entry that reads as answered, fails.
    let asks_of = |t: &str| -> Result<Vec<maestro_core::ask::Ask>, FileError> {
        let read = crate::yaml::parse(t).map_err(|e| refused(format!("Maestro's reader refuses the ledger: {e}")))?;
        let (ledger, bad) = maestro_core::ask::AskLedger::from_value(read.value).map_err(refused)?;
        if bad.is_empty() { Ok(ledger.asks) } else { Err(refused("Maestro's reader skips an entry of the ledger")) }
    };
    if new.len() > crate::LEDGER_LIMIT {
        return Err(refused(format!(
            "the ledger would be over {} bytes, and then no reader reads any ask in it; nothing was written",
            crate::LEDGER_LIMIT
        )));
    }
    let (was, now) = (asks_of(old)?, asks_of(new)?);
    // An ask is addressed by its uid, else its id, as the plugin's `text()` reads them (trimmed, a
    // number as text): the new ask's names are no other ask's.
    if let Some(added) = now.last() {
        let names = std::iter::once(added.id.as_str()).chain(added.uid.as_deref());
        if let Some(taken) = names.into_iter().find(|n| was.iter().any(|a| a.id == *n || a.uid.as_deref() == Some(*n)))
        {
            return Err(refused(format!("the ledger already has an ask `{taken}`; nothing was written")));
        }
    }
    let id = ask["id"].as_str().unwrap_or_default();
    let same = now.len() == was.len() + 1 && now.iter().zip(&was).all(|(n, w)| n.row == w.row);
    match now.last() {
        Some(last) if same && last.id == id && last.state() == maestro_core::ask::AskState::Open => Ok(()),
        _ => Err(refused("Maestro's reader does not read the old asks plus this one, open; nothing was written")),
    }
}

/// Runs the validator of record's own functions on `file`, judged as the ledger at `real`:
/// `load(file)`, `validate(ledger, preauth, path=real)` and `asks_of(ledger)`.
const DRIVER: &str = r#"
import importlib.util, json, sys
from pathlib import Path
script, file, real, preauth = sys.argv[1:5]
spec = importlib.util.spec_from_file_location("ask_ledger_of_record", script)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
try:
    ledger = m.load(Path(file))
    pre = m.load(Path(preauth)) if preauth and Path(preauth).is_file() else {}
except Exception as e:
    print(json.dumps({"refused": "%s: %s" % (type(e).__name__, e)}))
    sys.exit(0)
errors = m.validate(ledger, pre, path=Path(real))
print(json.dumps({"errors": [str(e) for e in errors], "asks": m.asks_of(ledger)}, default=str))
"#;

/// What the validator of record read: its errors and its `asks_of`.
struct Verdict {
    errors: Vec<String>,
    asks: Value,
}

fn validate(script: &Path, workspace: &Path, file: &Path, real: &Path) -> Result<Verdict, FileError> {
    if !script.is_file() {
        return Err(refused(format!(
            "the validator of record is not at {}; nothing was written (set MAESTRO_ASK_LEDGER)",
            script.display()
        )));
    }
    let preauth =
        [".broomva/preauth.yaml", ".control/preauth.yaml"].iter().map(|r| workspace.join(r)).find(|p| p.is_file());
    // -P: neither the workspace (the cwd) nor the script's folder shadows a module the driver imports.
    let out = Command::new("python3")
        .arg("-P")
        .arg("-c")
        .arg(DRIVER)
        .arg(script)
        .arg(file)
        .arg(real)
        .arg(preauth.unwrap_or_default())
        .current_dir(workspace)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| refused(format!("the validator did not run: {e}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let verdict: Value = match stdout.lines().last().map(serde_json::from_str::<Value>) {
        Some(Ok(v)) if out.status.success() => v,
        _ => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(refused(format!(
                "the validator gave no verdict (exit {:?}): {}",
                out.status.code(),
                stderr.lines().last().unwrap_or_default()
            )));
        }
    };
    if let Some(why) = verdict["refused"].as_str() {
        return Err(refused(format!("the validator of record does not read the ledger: {why}")));
    }
    let errors =
        verdict["errors"].as_array().map(|a| a.iter().filter_map(|e| e.as_str().map(str::to_string)).collect());
    Ok(Verdict { errors: errors.unwrap_or_default(), asks: verdict["asks"].clone() })
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
            let before = validate(&script, &paths.workspace, &path, &path)?;
            let after = validate(&script, &paths.workspace, &temp, &path)?;
            // The validator of record's reading: the ledger's asks, then exactly this one.
            let mut want = before.asks.as_array().cloned().unwrap_or_default();
            want.push(Value::Object(entry.clone()));
            if after.asks != Value::Array(want) {
                return Err(refused(
                    "ask_ledger.py does not read the result as the ledger's asks plus this ask; nothing was written",
                ));
            }
            let summary = format!("ask_ledger.py: {} ask(s), no new error", after.asks.as_array().map_or(0, Vec::len));
            let mut known = before.errors;
            let fresh: Vec<String> = after
                .errors
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
        assert_eq!(ids(&appended("arc: a\nlanes: [a]\n")), ["new"]);
        let unread = "arc: a\nlanes: [a]";
        let new = append_entry(unread, &ask("new")).unwrap();
        assert!(check_appended(unread, &new, &ask("new")).is_err(), "a ledger Maestro cannot read is not filed into");
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
        for key in maestro_core::ask::OUTCOME_KEYS.iter().chain(FILER_REFUSED) {
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
