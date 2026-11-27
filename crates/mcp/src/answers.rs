//! The owner's answers, written by Maestro (BRO-4042; owner decisions 2026-11-20).
//!
//! An answer is signed with the owner's passkey ([`crate::passkey`]), held for an undo window
//! ([`UNDO`]), then written: the ledger edit [`maestro_store::answer::answered`] makes, in
//! Maestro's own checkout of the workspace, committed with the signature in its trailers and
//! pushed to `maestro/ledger`. The answer takes effect there, on its signature. Then the
//! session that filed the ask (the ask's `origin.session`) is woken with the answer, and a
//! landing PR into main is opened and set to merge when its checks pass: bookkeeping that
//! nothing waits on.
//!
//! Every stage is a receipt line in `<owner dir>/receipts.jsonl`, which the page polls.

use crate::passkey::Assertion;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long a signed answer can be undone before it is written.
pub const UNDO: Duration = Duration::from_secs(8);

/// What the owner signs: the verb and everything it changes, in this field order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    pub verb: String,
    pub ledger: String,
    pub handle: String,
    pub hash: String,
    pub option: String,
    pub note: Option<String>,
}

impl Intent {
    pub fn answer(ledger: &str, handle: &str, hash: &str, option: &str, note: Option<&str>) -> Self {
        let note = note.map(str::trim).filter(|n| !n.is_empty()).map(str::to_string);
        Intent {
            verb: "ask.answer".into(),
            ledger: ledger.into(),
            handle: handle.into(),
            hash: hash.into(),
            option: option.into(),
            note,
        }
    }

    /// The canonical JSON the challenge commits to.
    pub fn canonical(&self) -> String {
        serde_json::to_string(self).expect("an intent serialises")
    }
}

/// Where an answer stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Signed and held for the undo window.
    Signed,
    /// The window closed and the write began: it can no longer be undone.
    Applying,
    /// Undone inside the window: nothing was written.
    Undone,
    /// Written to `maestro/ledger`: in effect.
    Written,
    /// Refused or failed: nothing was written.
    Failed,
}

/// One answer's receipt, as the page and the log show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub id: String,
    pub intent: Intent,
    pub label: String,
    pub stage: Stage,
    pub signed_at: String,
    /// When the undo window closes, in milliseconds since the epoch (server time).
    pub applies_at_ms: i64,
    pub commit: Option<String>,
    /// The session told, or why none was.
    pub woke: Option<String>,
    pub landing: Option<String>,
    pub error: Option<String>,
}

/// Where Maestro writes, and with what.
#[derive(Debug, Clone)]
pub struct Writer {
    /// The workspace repository (its `.git` holds Maestro's checkout as a worktree).
    pub workspace: PathBuf,
    /// The asks folder, relative to the workspace root.
    pub asks: String,
    /// Maestro's own checkout of the workspace, detached.
    pub checkout: PathBuf,
    /// The receipts log and passkeys.
    pub owner_dir: PathBuf,
    /// The T3 CLI that wakes a session, if any.
    pub t3: Option<T3>,
    /// Fallback T3 thread id (e.g. Meta Orchestrator) to triage an answered ask when the ask has
    /// no origin session, a non-T3 origin, or an unreachable origin thread.
    pub fallback_thread: Option<String>,
    /// Whether to push and land (off in tests).
    pub remote: bool,
}

pub const BRANCH: &str = maestro_store::ledger::BRANCH;

impl Writer {
    fn shared(&self) -> maestro_store::ledger::Writer {
        maestro_store::ledger::Writer {
            workspace: self.workspace.clone(),
            checkout: self.checkout.clone(),
            owner_dir: self.owner_dir.clone(),
            remote: self.remote,
        }
    }
}

use maestro_store::ledger::git;

/// Which T3 CLI wakes a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum T3 {
    /// This one (`MAESTRO_T3_BIN`).
    Fixed(PathBuf),
    /// The CLI of the T3 server running now, under this T3 home, resolved at each wake: prefer the
    /// live server recorded in `userdata/server-runtime.json` (which matches the running Desktop
    /// app or service), then `runtime/service-state.json`, with `bin/t3` as fallback.
    Active(PathBuf),
}

fn version_cli(home: &Path, version: &str) -> Option<PathBuf> {
    let ok = !version.is_empty()
        && version != "."
        && version != ".."
        && !version.starts_with('-')
        && version.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !ok {
        return None;
    }
    let cli = home.join("runtime/versions").join(version).join("t3");
    cli.exists().then_some(cli)
}

fn json_str(path: &Path, key: &str) -> Option<String> {
    let state: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    state.get(key)?.as_str().map(str::to_string)
}

impl T3 {
    /// Ordered, deduplicated CLI candidates to try when waking a T3 session.
    pub fn candidates(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut push = |p: PathBuf| {
            if !out.contains(&p) {
                out.push(p);
            }
        };
        match self {
            T3::Fixed(p) => push(p.clone()),
            T3::Active(home) => {
                let live = json_str(&home.join("userdata/server-runtime.json"), "serverVersion");
                let active = json_str(&home.join("runtime/service-state.json"), "activeVersion");
                if live.is_none() && active.is_none() {
                    return out;
                }
                let bin = home.join("bin/t3");
                let has_bin = bin.exists();
                // The running server writes its own version to `userdata/server-runtime.json`.
                // When the Desktop app owns the database, its version is served by `bin/t3`, not a
                // stale standalone `runtime/service-state.json`.
                if let Some(v) = live.as_deref() {
                    if let Some(cli) = version_cli(home, v) {
                        push(cli);
                    } else if has_bin {
                        push(bin.clone());
                    }
                }
                if let Some(v) = active.as_deref()
                    && let Some(cli) = version_cli(home, v)
                {
                    push(cli);
                }
                if has_bin {
                    push(bin);
                }
            }
        }
        out
    }

    pub fn resolve(&self) -> Option<PathBuf> {
        self.candidates().into_iter().next()
    }
}

/// Signed answers by receipt id.
#[derive(Clone, Default)]
pub struct Book {
    inner: Arc<Mutex<HashMap<String, Receipt>>>,
}

impl Book {
    pub fn get(&self, id: &str) -> Option<Receipt> {
        self.inner.lock().ok()?.get(id).cloned()
    }

    /// `(ledger, handle)` pairs currently in the undo window or being written to the ledger.
    pub fn in_flight(&self) -> std::collections::BTreeSet<(String, String)> {
        self.inner
            .lock()
            .ok()
            .map(|m| {
                m.values()
                    .filter(|r| matches!(r.stage, Stage::Signed | Stage::Applying))
                    .map(|r| (r.intent.ledger.clone(), r.intent.handle.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn put(&self, r: Receipt) {
        if let Ok(mut m) = self.inner.lock() {
            m.insert(r.id.clone(), r);
        }
    }

    /// Undo inside the window: only a receipt still signed turns undone, and only once `durable`
    /// (recording the undo on disk) has succeeded, all under the lock `take_due` takes. So an undo
    /// is either recorded and honoured, or refused with nothing recorded. Undoing an answer
    /// already undone is a no-op that succeeds.
    pub fn undo(&self, id: &str, now_ms: i64, durable: impl FnOnce() -> Result<(), String>) -> Result<Receipt, String> {
        let mut m = self.inner.lock().map_err(|_| "busy".to_string())?;
        let r = m.get_mut(id).ok_or("no such answer")?;
        match r.stage {
            Stage::Undone => return Ok(r.clone()),
            Stage::Signed => {}
            _ => return Err("it was already written".into()),
        }
        if now_ms >= r.applies_at_ms {
            return Err("the undo window has closed".into());
        }
        durable()?;
        r.stage = Stage::Undone;
        Ok(r.clone())
    }

    /// Take a signed receipt to write once its window has closed, turning it `Applying` under
    /// the lock undo takes, so an undo either lands before this or is refused: `None` if undone.
    fn take_due(&self, id: &str) -> Option<Receipt> {
        let mut m = self.inner.lock().ok()?;
        let r = m.get_mut(id).filter(|r| r.stage == Stage::Signed)?;
        r.stage = Stage::Applying;
        Some(r.clone())
    }
}

/// Append a receipt to the log, synced to disk.
pub fn log(owner_dir: &Path, r: &Receipt) -> Result<(), String> {
    use std::io::Write;
    std::fs::create_dir_all(owner_dir).map_err(|e| format!("receipts: {e}"))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(owner_dir.join("receipts.jsonl"))
        .map_err(|e| format!("receipts: {e}"))?;
    writeln!(f, "{}", serde_json::to_string(r).map_err(|e| e.to_string())?).map_err(|e| format!("receipts: {e}"))?;
    f.sync_data().map_err(|e| format!("receipts: {e}"))
}

/// The last logged state of receipt `id`, for a receipt the running daemon does not hold.
pub fn logged(owner_dir: &Path, id: &str) -> Option<Receipt> {
    let text = std::fs::read_to_string(owner_dir.join("receipts.jsonl")).ok()?;
    text.lines().rev().filter_map(|l| serde_json::from_str::<Receipt>(l).ok()).find(|r| r.id == id)
}

/// A signed answer not yet settled: everything needed to write it after a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub receipt: Receipt,
    pub assertion: Assertion,
    pub nonce: String,
    pub by: String,
    pub session: Option<String>,
}

fn pending_dir(owner_dir: &Path) -> PathBuf {
    owner_dir.join("pending")
}

/// Persist a pending answer before it is acknowledged: written whole, synced, then renamed.
pub fn persist(owner_dir: &Path, p: &Pending) -> Result<(), String> {
    use std::io::Write;
    if !valid_id(&p.receipt.id) {
        return Err("not an answer id".into());
    }
    let dir = pending_dir(owner_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("pending: {e}"))?;
    let tmp = dir.join(format!(".{}.tmp", p.receipt.id));
    let mut f = std::fs::File::create(&tmp).map_err(|e| format!("pending: {e}"))?;
    f.write_all(serde_json::to_string(p).map_err(|e| e.to_string())?.as_bytes())
        .map_err(|e| format!("pending: {e}"))?;
    f.sync_all().map_err(|e| format!("pending: {e}"))?;
    std::fs::rename(&tmp, dir.join(format!("{}.json", p.receipt.id))).map_err(|e| format!("pending: {e}"))?;
    sync_dir(&dir).map_err(|e| format!("pending: {e}"))
}

/// Settle a pending answer: written, failed or undone, it no longer waits.
pub fn settle(owner_dir: &Path, id: &str) {
    if !valid_id(id) {
        return;
    }
    let dir = pending_dir(owner_dir);
    // The answer first: a crash between the two leaves only a marker, which writes nothing.
    let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
    let _ = std::fs::remove_file(dir.join(format!("{id}.undo")));
    let _ = sync_dir(&dir);
}

/// Whether `id` is a receipt id as Maestro makes them: 16 base64url characters. Only such an
/// id is ever part of a path; anything else is refused before the file system is touched.
pub fn valid_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// What a write that found its answer undone reports.
pub const UNDONE: &str = "undone before it was written";

/// Record an undo on disk, durably (the file and its folder entry), before it is acknowledged.
pub fn mark_undone(owner_dir: &Path, id: &str) -> Result<(), String> {
    if !valid_id(id) {
        return Err("not an answer id".into());
    }
    let dir = pending_dir(owner_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("undo: {e}"))?;
    let f = std::fs::File::create(dir.join(format!("{id}.undo"))).map_err(|e| format!("undo: {e}"))?;
    f.sync_all().map_err(|e| format!("undo: {e}"))?;
    sync_dir(&dir).map_err(|e| format!("undo: {e}"))
}

/// Sync a folder, so an entry created, renamed or removed in it survives a crash.
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

/// Whether an undo is recorded on disk for `id`.
pub fn undone(owner_dir: &Path, id: &str) -> bool {
    valid_id(id) && pending_dir(owner_dir).join(format!("{id}.undo")).exists()
}

/// Every pending answer on disk, for recovery at start.
pub fn pending(owner_dir: &Path) -> Vec<Pending> {
    let Ok(dir) = std::fs::read_dir(pending_dir(owner_dir)) else { return Vec::new() };
    dir.filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
        .filter_map(|e| std::fs::read(e.path()).ok())
        .filter_map(|b| serde_json::from_slice(&b).ok())
        .collect()
}

/// Write one signed answer: the ledger edit, a commit carrying the proof and nothing else, the push.
pub fn write(w: &Writer, r: &Receipt, assertion: &Assertion, nonce: &str, by: &str) -> Result<String, String> {
    let _lock = maestro_store::ledger::exclusive(&w.shared())?;
    // An undo recorded on disk, by this daemon or before a restart, wins over the write.
    if undone(&w.owner_dir, &r.id) {
        return Err(UNDONE.into());
    }
    maestro_store::ledger::prepare(&w.shared())?;
    // Written already (a restart after the push, before the pending record was settled): the
    // commit whose trailer line is exactly this answer's id is the outcome, and nothing is
    // written twice. Ids are base64url, so they carry no regular-expression character.
    let done = git(&w.checkout, &["log", "-1", "--format=%H", &format!("--grep=^Maestro-Answer: {}$", r.id)])?;
    if !done.is_empty() {
        return Ok(done);
    }
    let rel = format!("{}/{}", w.asks, r.intent.ledger);
    let path = w.checkout.join(&rel);
    let source = std::fs::read_to_string(&path).map_err(|e| format!("{rel}: {e}"))?;
    let answer = maestro_store::answer::Answer {
        option: &r.intent.option,
        note: r.intent.note.as_deref(),
        by,
        at: &r.signed_at,
    };
    let select = maestro_store::answer::Select { handle: &r.intent.handle };
    let edited =
        maestro_store::answer::answered(&source, &rel, &select, &r.intent.hash, &answer).map_err(|e| e.to_string())?;
    // The validator of record has the last word, and without it nothing is written. As Maestro's
    // ask writer does, the answer is refused for any error the ledger did not already have.
    let validate = |text: &str| -> Result<Vec<String>, String> {
        std::fs::write(&path, text).map_err(|e| format!("{rel}: {e}"))?;
        let o = Command::new("python3")
            .arg(w.checkout.join("scripts/ask_ledger.py"))
            .arg("validate")
            .arg(&path)
            .current_dir(&w.checkout)
            .output()
            .map_err(|e| format!("the ledger validator did not run: {e}"))?;
        verdict(o.status.code(), &String::from_utf8_lossy(&o.stdout))
    };
    let before = validate(&source)?;
    let after = validate(&edited)?;
    let new: Vec<&String> = after.iter().filter(|e| !before.contains(e)).collect();
    if !new.is_empty() {
        let _ = std::fs::write(&path, &source);
        return Err(format!(
            "the ledger validator refused the answer: {}",
            new.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("; ")
        ));
    }
    let proof = serde_json::json!({ "intent": r.intent.canonical(), "nonce": nonce, "assertion": assertion });
    let message = format!(
        "answers: {}#{} -> {}\n\nThe owner answered with a passkey; Maestro wrote it (BRO-4042).\n\nMaestro-Answer: {}\nMaestro-Owner-Proof: {}\n",
        r.intent.ledger,
        r.intent.handle,
        r.intent.option,
        r.id,
        serde_json::to_string(&proof).unwrap_or_default()
    );
    maestro_store::ledger::commit_and_push(&w.shared(), &rel, &message, edited.as_bytes())
}

/// The validator's verdict, read strictly: its `ERROR` records, which must be exactly the count
/// its summary line states, with exit 0 for none and 1 for some. Anything else (a traceback, a
/// usage error, a missing summary) is the validator failing to run, and nothing is written.
fn verdict(code: Option<i32>, stdout: &str) -> Result<Vec<String>, String> {
    let errors: Vec<String> = stdout.lines().filter_map(|l| l.strip_prefix("  ERROR ")).map(str::to_string).collect();
    let stated = stdout.lines().rev().find_map(|l| {
        let rest = l.strip_prefix("ask-ledger: ")?;
        let n = rest.rsplit_once(", ")?.1.strip_suffix(" error(s)")?;
        n.parse::<usize>().ok()
    });
    match (code, stated) {
        (Some(0), Some(0)) if errors.is_empty() => Ok(errors),
        (Some(1), Some(n)) if n > 0 && n == errors.len() => Ok(errors),
        _ => Err(format!("the ledger validator gave no verdict (exit {code:?}): {}", stdout.trim())),
    }
}

/// The message the asking session gets.
pub fn wake_message(r: &Receipt, commit: &str) -> String {
    let note = r.intent.note.as_deref().map(|n| format!("\nTheir note: {n}")).unwrap_or_default();
    format!(
        "[maestro] The owner answered your ask {}#{}: {} (option `{}`).{note}\n\
         They signed it with their passkey and Maestro wrote it to `{BRANCH}` at {commit}, so it is in effect now. \
         A landing PR brings it to main; do not wait for it. Resume the work this ask held, acting on the answer.\n\
         Answer id {}: if you already acted on this id, this is a repeat after a restart; ignore it.",
        r.intent.ledger, r.intent.handle, r.label, r.intent.option, r.id
    )
}

/// The triage message the fallback orchestrator gets when the original asking session is missing,
/// not a T3 session, or no longer reachable.
pub fn fallback_wake_message(r: &Receipt, commit: &str, origin_session: Option<&str>, reason: &str) -> String {
    let note = r.intent.note.as_deref().map(|n| format!("\nTheir note: {n}")).unwrap_or_default();
    let origin = origin_session.unwrap_or("none");
    format!(
        "[maestro] Fallback triage: the owner answered {}#{}: {} (option `{}`).{note}\n\
         They signed it with their passkey and Maestro wrote it to `{BRANCH}` at {commit}, so it is in effect now. \
         Original session (`{origin}`) could not be woken ({reason}).\n\
         Inspect the ask, linked PR/ticket, and current fleet state to determine whether it still makes sense to carry this decision forward (route or dispatch work) or disregard it as already resolved/superseded, and record why.\n\
         Answer id {}: if you already triaged this id, this is a repeat after a restart; ignore it.",
        r.intent.ledger, r.intent.handle, r.label, r.intent.option, r.id
    )
}

fn t3_thread_of(session: &str) -> Option<&str> {
    session.strip_prefix("t3:").and_then(|s| s.split_once('/')).map(|(_, t)| t).filter(|t| {
        !t.is_empty()
            && *t != "."
            && *t != ".."
            && !t.starts_with('-')
            && t.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.'))
    })
}

/// Wake the session an ask names (`t3:<environment>/<thread>`), as its next turn.
pub fn wake(t3: Option<&Path>, session: Option<&str>, message: &str) -> String {
    wake_once(t3, session, message).0
}

fn wake_once(t3: Option<&Path>, session: Option<&str>, message: &str) -> (String, bool) {
    let Some(session) = session else { return ("no session is named on the ask".into(), false) };
    let Some(thread) = t3_thread_of(session) else {
        return (format!("{session} is not a T3 session"), false);
    };
    let Some(t3) = t3 else { return ("no T3 CLI is configured".into(), false) };
    match Command::new(t3).args(["thread", "send", thread, message]).output() {
        Ok(o) if o.status.success() => (format!("told T3 thread {thread}"), false),
        Ok(o) => {
            // Check full output for database-owner mismatch before summarizing to one line.
            let said = format!("{}\n{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            let db_mismatch = said.contains("ServerOwnsDatabaseError");
            let why = said
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .find(|l| l.contains("ServerOwnsDatabaseError") || l.contains("Error") || l.contains("error"))
                .or_else(|| said.lines().map(str::trim).rfind(|l| !l.is_empty()))
                .unwrap_or("no output");
            (format!("T3 did not take it (exit {:?}): {why}", o.status.code()), db_mismatch)
        }
        Err(e) => (format!("T3 did not run: {e}"), false),
    }
}

/// Try each T3 CLI candidate in order; retry on the next candidate when a CLI rejects the running
/// server's database schema (`ServerOwnsDatabaseError`).
pub fn wake_candidates(candidates: &[PathBuf], session: Option<&str>, message: &str) -> String {
    if candidates.is_empty() {
        return wake(None, session, message);
    }
    let mut last = String::new();
    for cli in candidates {
        let (out, db_mismatch) = wake_once(Some(cli.as_path()), session, message);
        if !db_mismatch {
            return out;
        }
        last = out;
    }
    last
}

/// Wake the ask's own T3 session; if none exists or it cannot be woken, route a triage message to
/// the configured fallback orchestrator thread (skipping if the origin already named that thread).
pub fn wake_with_fallback(
    candidates: &[PathBuf],
    session: Option<&str>,
    fallback_thread: Option<&str>,
    r: &Receipt,
    commit: &str,
) -> String {
    let primary = wake_candidates(candidates, session, &wake_message(r, commit));
    if primary.starts_with("told T3 thread ") {
        return primary;
    }
    let Some(fb) = fallback_thread.filter(|t| !t.is_empty()) else {
        return primary;
    };
    if session.and_then(t3_thread_of) == Some(fb) {
        return primary;
    }
    let fb_session = format!("t3:_/{fb}");
    let fb_msg = fallback_wake_message(r, commit, session, &primary);
    let fb_out = wake_candidates(candidates, Some(&fb_session), &fb_msg);
    if fb_out.starts_with("told T3 thread ") {
        format!("{primary}; routed to fallback T3 thread {fb}")
    } else {
        format!("{primary}; fallback T3 thread {fb} failed: {fb_out}")
    }
}

/// Open or keep the ledger landing PR.
pub fn land(w: &Writer) -> String {
    maestro_store::ledger::land(&w.shared())
}

/// After the window: write, wake, land, logging each stage. Runs on a blocking thread. The pending
/// record is settled only once the outcome is logged, so a crash before that is retried at start.
pub fn apply(w: &Writer, book: &Book, id: &str, assertion: &Assertion, nonce: &str, by: &str, session: Option<&str>) {
    let Some(mut r) = book.take_due(id) else { return };
    match write(w, &r, assertion, nonce, by) {
        Ok(commit) => {
            r.stage = Stage::Written;
            r.commit = Some(commit.clone());
            book.put(r.clone());
            let _ = log(&w.owner_dir, &r);
            let candidates = w.t3.as_ref().map(T3::candidates).unwrap_or_default();
            r.woke = Some(wake_with_fallback(&candidates, session, w.fallback_thread.as_deref(), &r, &commit));
            book.put(r.clone());
            r.landing = Some(if w.remote { land(w) } else { "local only (not pushed)".into() });
            book.put(r.clone());
            // Settled last, and only once the final receipt is on disk: a crash before this resumes
            // the answer, which is found written by its id, and the wake and the landing are tried again.
            if log(&w.owner_dir, &r).is_ok() {
                settle(&w.owner_dir, id);
            }
        }
        Err(e) if e == UNDONE => {
            r.stage = Stage::Undone;
            book.put(r.clone());
            let _ = log(&w.owner_dir, &r);
            settle(&w.owner_dir, id);
        }
        Err(e) => {
            r.stage = Stage::Failed;
            r.error = Some(e);
            book.put(r.clone());
            let _ = log(&w.owner_dir, &r);
            settle(&w.owner_dir, id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(intent: Intent, now_ms: i64) -> Receipt {
        Receipt {
            id: "r1AAAAAAAAAAAAAA".into(),
            intent,
            label: "No".into(),
            stage: Stage::Signed,
            signed_at: "2026-11-20T23:00:00Z".into(),
            applies_at_ms: now_ms + 8_000,
            commit: None,
            woke: None,
            landing: None,
            error: None,
        }
    }

    #[test]
    fn the_intent_is_canonical_and_a_blank_note_is_none() {
        let i = Intent::answer("l.yaml", "a", "h", "no", Some("  "));
        assert_eq!(
            i.canonical(),
            r#"{"verb":"ask.answer","ledger":"l.yaml","handle":"a","hash":"h","option":"no","note":null}"#
        );
    }

    #[test]
    fn undo_works_only_inside_the_window_and_only_once() {
        let book = Book::default();
        book.put(receipt(Intent::answer("l", "a", "h", "no", None), 1_000));
        assert!(book.undo("r1AAAAAAAAAAAAAA", 9_001, || Ok(())).is_err(), "after the window");
        assert_eq!(book.undo("r1AAAAAAAAAAAAAA", 2_000, || Ok(())).unwrap().stage, Stage::Undone);
        assert_eq!(
            book.undo("r1AAAAAAAAAAAAAA", 2_000, || Err("never called".into())).unwrap().stage,
            Stage::Undone,
            "twice is a no-op"
        );
        assert!(book.take_due("r1AAAAAAAAAAAAAA").is_none(), "an undone answer is never written");
    }

    const LEDGER: &str = "arc: t\nasks:\n  - id: a\n    ask: \"First?\"\n    options:\n      - id: yes\n        label: \"Yes\"\n      - id: no\n        label: \"No\"\n    origin:\n      session: \"t3:env/thread-1\"\n  - id: b\n    ask: \"Second?\"\n    options:\n      - id: go\n        label: \"Go\"\n";

    /// A workspace repo with the ledger, and a validator that passes (`ok`) or refuses.
    fn workspace(validator: Option<bool>) -> (tempfile::TempDir, tempfile::TempDir, Writer) {
        let ws = tempfile::tempdir().unwrap();
        let asks = ws.path().join(".control/asks");
        std::fs::create_dir_all(&asks).unwrap();
        std::fs::write(asks.join("t.yaml"), LEDGER).unwrap();
        if let Some(ok) = validator {
            std::fs::create_dir_all(ws.path().join("scripts")).unwrap();
            // The refusing one, like the real one, reports an error only once an answer is in the file.
            let ok_line = "print('ask-ledger: 2 ask(s), 0 decision(s), 0 error(s)')";
            let body = if ok {
                format!("import sys\n{ok_line}\nsys.exit(0)\n")
            } else {
                format!(
                    "import sys\nif 'answer_option' in open(sys.argv[2]).read():\n    print('  ERROR ask a: bad answer')\n    print('ask-ledger: 2 ask(s), 0 decision(s), 1 error(s)')\n    sys.exit(1)\n{ok_line}\nsys.exit(0)\n"
                )
            };
            std::fs::write(ws.path().join("scripts/ask_ledger.py"), body).unwrap();
        }
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["add", "."],
            &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"],
        ] {
            git(ws.path(), args).unwrap();
        }
        let state = tempfile::tempdir().unwrap();
        let w = Writer {
            workspace: ws.path().into(),
            asks: ".control/asks".into(),
            checkout: state.path().join("checkout"),
            owner_dir: state.path().join("owner"),
            t3: None,
            fallback_thread: None,
            remote: false,
        };
        (ws, state, w)
    }

    fn hash_of(handle: &str) -> String {
        let parsed =
            maestro_core::ask::AskLedger::from_value(maestro_store::yaml::parse(LEDGER).unwrap().value).unwrap().0;
        maestro_store::answer::find(&parsed, &maestro_store::answer::Select { handle }).unwrap().hash()
    }

    fn assertion() -> Assertion {
        Assertion {
            credential: "c".into(),
            client_data: "d".into(),
            authenticator_data: "e".into(),
            signature: "f".into(),
        }
    }

    #[test]
    fn writes_the_answer_in_its_own_checkout_with_the_proof() {
        let (ws, _state, w) = workspace(Some(true));
        let book = Book::default();
        book.put(receipt(Intent::answer("t.yaml", "a", &hash_of("a"), "no", Some("after the soak")), 0));
        apply(&w, &book, "r1AAAAAAAAAAAAAA", &assertion(), "n1", "owner (passkey)", Some("t3:env/thread-1"));
        let r = book.get("r1AAAAAAAAAAAAAA").unwrap();
        assert_eq!(r.stage, Stage::Written, "{:?}", r.error);
        assert_eq!(r.woke.as_deref(), Some("no T3 CLI is configured"));
        let written = std::fs::read_to_string(w.checkout.join(".control/asks/t.yaml")).unwrap();
        assert!(written.contains("answer_option: \"no\""));
        let msg = git(&w.checkout, &["log", "-1", "--format=%B"]).unwrap();
        assert!(
            msg.contains("Maestro-Owner-Proof: {") && msg.contains(r#""intent":"{\"verb\":\"ask.answer\""#),
            "{msg}"
        );
        // The owner's own checkout is untouched; the log has the signed line's outcome.
        assert_eq!(std::fs::read_to_string(ws.path().join(".control/asks/t.yaml")).unwrap(), LEDGER);
        assert_eq!(logged(&w.owner_dir, "r1AAAAAAAAAAAAAA").unwrap().stage, Stage::Written);
    }

    #[test]
    fn a_change_staged_in_the_checkout_never_rides_along() {
        let (_ws, _state, w) = workspace(Some(true));
        // A first answer makes the checkout; then another process stages an unsigned change there.
        let book = Book::default();
        book.put(receipt(Intent::answer("t.yaml", "a", &hash_of("a"), "no", None), 0));
        apply(&w, &book, "r1AAAAAAAAAAAAAA", &assertion(), "n1", "owner", None);
        std::fs::write(w.checkout.join(".control/asks/evil.yaml"), "arc: e\nasks: []\n").unwrap();
        git(&w.checkout, &["add", "."]).unwrap();
        // And leaves a file never staged: a module beside the validator would be imported by it.
        std::fs::write(w.checkout.join("scripts/yaml.py"), "raise SystemExit(0)\n").unwrap();
        let mut r2 = receipt(Intent::answer("t.yaml", "b", &hash_of("b"), "go", None), 0);
        r2.id = "r2AAAAAAAAAAAAAA".into();
        book.put(r2);
        apply(&w, &book, "r2AAAAAAAAAAAAAA", &assertion(), "n2", "owner", None);
        assert_eq!(
            book.get("r2AAAAAAAAAAAAAA").unwrap().stage,
            Stage::Written,
            "{:?}",
            book.get("r2AAAAAAAAAAAAAA").unwrap().error
        );
        let files = git(&w.checkout, &["diff", "--name-only", "HEAD~1", "HEAD"]).unwrap();
        assert_eq!(files, ".control/asks/t.yaml");
        assert!(!w.checkout.join(".control/asks/evil.yaml").exists());
        assert!(!w.checkout.join("scripts/yaml.py").exists(), "an untracked file survived the reset");
    }

    #[test]
    fn a_resumed_answer_already_written_is_not_written_twice() {
        let (_ws, _state, w) = workspace(Some(true));
        let book = Book::default();
        book.put(receipt(Intent::answer("t.yaml", "a", &hash_of("a"), "no", None), 0));
        apply(&w, &book, "r1AAAAAAAAAAAAAA", &assertion(), "n1", "owner", None);
        let first = book.get("r1AAAAAAAAAAAAAA").unwrap().commit.unwrap();
        // The daemon restarts before settling: the same signed receipt is applied again.
        let again = Book::default();
        again.put(receipt(Intent::answer("t.yaml", "a", &hash_of("a"), "no", None), 0));
        // Locally the base is the workspace HEAD, which lacks the first commit; build on it instead.
        git(&w.workspace, &["fetch", "-q", &w.checkout.to_string_lossy(), &first]).unwrap();
        git(&w.workspace, &["merge", "-q", "--ff-only", &first]).unwrap();
        apply(&w, &again, "r1AAAAAAAAAAAAAA", &assertion(), "n1", "owner", None);
        let r = again.get("r1AAAAAAAAAAAAAA").unwrap();
        assert_eq!((r.stage, r.commit.as_deref()), (Stage::Written, Some(first.as_str())), "{:?}", r.error);
    }

    #[test]
    fn without_the_validator_or_against_it_nothing_is_written() {
        for validator in [None, Some(false)] {
            let (_ws, _state, w) = workspace(validator);
            let book = Book::default();
            book.put(receipt(Intent::answer("t.yaml", "a", &hash_of("a"), "no", None), 0));
            apply(&w, &book, "r1AAAAAAAAAAAAAA", &assertion(), "n1", "owner", None);
            let r = book.get("r1AAAAAAAAAAAAAA").unwrap();
            assert_eq!(r.stage, Stage::Failed, "{validator:?}");
            assert!(git(&w.checkout, &["log", "-1", "--format=%s"]).unwrap() == "init", "{validator:?}: committed");
        }
    }

    #[test]
    fn only_a_complete_verdict_counts() {
        let ok = "ask-ledger: 2 ask(s), 0 decision(s), 0 error(s)\n";
        assert_eq!(verdict(Some(0), ok), Ok(vec![]));
        let errs = "  ERROR ask a: x\nask-ledger: 2 ask(s), 0 decision(s), 1 error(s)\n";
        assert_eq!(verdict(Some(1), errs), Ok(vec!["ask a: x".to_string()]));
        // A crash (exit 1, no summary), a count that disagrees, a usage error, a missing summary.
        for (code, out) in [
            (Some(1), ""),
            (Some(1), "Traceback (most recent call last):\n"),
            (Some(1), "  ERROR a\nask-ledger: 2 ask(s), 0 decision(s), 2 error(s)\n"),
            (Some(0), "  ERROR a\nask-ledger: 2 ask(s), 0 decision(s), 0 error(s)\n"),
            (Some(2), "error: no ledger\n"),
            (Some(0), ""),
            (None, ok),
        ] {
            assert!(verdict(code, out).is_err(), "{code:?} {out:?}");
        }
    }

    #[test]
    fn once_the_write_begins_undo_is_refused() {
        let book = Book::default();
        book.put(receipt(Intent::answer("l", "a", "h", "no", None), 1_000));
        assert!(book.take_due("r1AAAAAAAAAAAAAA").is_some());
        // Even with a wall clock moved back inside the window.
        assert!(book.undo("r1AAAAAAAAAAAAAA", 0, || panic!("an undo refused records nothing")).is_err());
        assert!(book.take_due("r1AAAAAAAAAAAAAA").is_none(), "taken once");
    }

    #[test]
    fn a_pending_answer_survives_on_disk_until_settled() {
        let d = tempfile::tempdir().unwrap();
        let p = Pending {
            receipt: receipt(Intent::answer("l", "a", "h", "no", None), 0),
            assertion: assertion(),
            nonce: "n".into(),
            by: "owner".into(),
            session: Some("t3:e/t".into()),
        };
        persist(d.path(), &p).unwrap();
        assert_eq!(pending(d.path()), vec![p]);
        settle(d.path(), "r1AAAAAAAAAAAAAA");
        assert!(pending(d.path()).is_empty());
    }

    #[test]
    fn the_active_t3_cli_is_the_running_servers_version() {
        let home = tempfile::tempdir().unwrap();
        let rt = home.path().join("runtime");
        std::fs::create_dir_all(rt.join("versions/0.2.0")).unwrap();
        std::fs::write(rt.join("versions/0.2.0/t3"), "").unwrap();
        std::fs::create_dir_all(home.path().join("bin")).unwrap();
        std::fs::write(home.path().join("bin/t3"), "").unwrap();
        let active = T3::Active(home.path().into());
        // No state yet: nothing names a version.
        assert_eq!(active.resolve(), None);
        std::fs::write(rt.join("service-state.json"), r#"{"protocol":4,"activeVersion":"0.2.0"}"#).unwrap();
        assert_eq!(active.resolve(), Some(rt.join("versions/0.2.0/t3")));
        // A version with no CLI, or one that names another path, falls back to bin/t3, even when
        // that path holds a t3.
        std::fs::create_dir_all(rt.join("x")).unwrap();
        std::fs::write(rt.join("x/t3"), "").unwrap();
        for v in ["0.3.0", "../x", ""] {
            std::fs::write(rt.join("service-state.json"), format!(r#"{{"activeVersion":"{v}"}}"#)).unwrap();
            assert_eq!(active.resolve(), Some(home.path().join("bin/t3")), "{v}");
        }
        assert_eq!(T3::Fixed("/x/t3".into()).resolve(), Some("/x/t3".into()));
    }

    #[test]
    fn a_session_is_woken_only_through_t3() {
        assert_eq!(wake(None, None, "m"), "no session is named on the ask");
        assert_eq!(wake(None, Some("paseo:x"), "m"), "paseo:x is not a T3 session");
        assert_eq!(wake(Some(Path::new("/bin/echo")), Some("t3:env/th"), "m"), "told T3 thread th");
    }

    #[test]
    fn active_t3_prefers_live_server_runtime_over_stale_service_state() {
        let home = tempfile::tempdir().unwrap();
        let stale_cli = home.path().join("runtime/versions/0.1.12-nightly/t3");
        let bin_cli = home.path().join("bin/t3");
        std::fs::create_dir_all(stale_cli.parent().unwrap()).unwrap();
        std::fs::create_dir_all(bin_cli.parent().unwrap()).unwrap();
        std::fs::create_dir_all(home.path().join("userdata")).unwrap();
        std::fs::write(&stale_cli, "").unwrap();
        std::fs::write(&bin_cli, "").unwrap();
        std::fs::write(home.path().join("runtime/service-state.json"), r#"{"activeVersion":"0.1.12-nightly"}"#)
            .unwrap();
        std::fs::write(home.path().join("userdata/server-runtime.json"), r#"{"serverVersion":"0.1.11"}"#).unwrap();
        let candidates = T3::Active(home.path().into()).candidates();
        assert_eq!(candidates, vec![bin_cli, stale_cli]);
    }

    #[test]
    fn wake_candidates_retries_when_first_cli_hits_server_owns_database_error() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad-t3");
        let good = dir.path().join("good-t3");
        std::fs::write(
            &bad,
            "#!/bin/sh
echo 'Error: failed to send' >&2\necho '  Caused by: ServerOwnsDatabaseError: mismatch' >&2
exit 1
",
        )
        .unwrap();
        std::fs::write(
            &good,
            "#!/bin/sh
exit 0
",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::set_permissions(&good, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(wake_candidates(&[bad, good], Some("t3:env/th-9"), "msg"), "told T3 thread th-9");
    }

    #[test]
    fn fallback_routes_to_orchestrator_when_origin_missing_non_t3_or_dead_and_avoids_self_loop() {
        let dir = tempfile::tempdir().unwrap();
        let log_file = dir.path().join("calls.log");
        let fake_t3 = dir.path().join("fake-t3");
        std::fs::write(
            &fake_t3,
            format!(
                "#!/bin/sh\nprintf \"CALL::%s\\n%s\\n\" \"$3\" \"$4\" >> '{}'\nif [ \"$3\" = \"dead-thread\" ] || [ \"$3\" = \"meta-orch\" -a -f '{}' ]; then echo 'Error: thread not found' >&2; exit 1; fi\nexit 0\n",
                log_file.display(),
                dir.path().join("fail-meta").display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_t3, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let r = receipt(Intent::answer("l.yaml", "a1", "h1", "yes", None), 0);
        let cands = vec![fake_t3];

        // 1. Missing origin session -> routed to fallback orchestrator with triage prompt.
        let out = wake_with_fallback(&cands, None, Some("meta-orch"), &r, "deadbeef");
        assert_eq!(out, "no session is named on the ask; routed to fallback T3 thread meta-orch");

        // 2. Non-T3 origin -> routed to fallback orchestrator.
        let out = wake_with_fallback(&cands, Some("paseo:local/abc"), Some("meta-orch"), &r, "deadbeef");
        assert_eq!(out, "paseo:local/abc is not a T3 session; routed to fallback T3 thread meta-orch");

        // 3. Dead T3 origin -> routed to fallback orchestrator.
        let out = wake_with_fallback(&cands, Some("t3:env/dead-thread"), Some("meta-orch"), &r, "deadbeef");
        assert!(out.contains("routed to fallback T3 thread meta-orch"), "got: {out}");

        // 4. Live T3 origin -> wakes origin directly, no fallback.
        let out = wake_with_fallback(&cands, Some("t3:env/live-thread"), Some("meta-orch"), &r, "deadbeef");
        assert_eq!(out, "told T3 thread live-thread");

        // 5. Origin IS the fallback thread and fails -> does not send twice.
        std::fs::write(dir.path().join("fail-meta"), "").unwrap();
        let before = std::fs::read_to_string(&log_file).unwrap().lines().filter(|l| l.starts_with("CALL::")).count();
        let out = wake_with_fallback(&cands, Some("t3:env/meta-orch"), Some("meta-orch"), &r, "deadbeef");
        assert!(out.starts_with("T3 did not take it"), "got: {out}");
        assert!(!out.contains("fallback T3 thread"), "must not retry fallback when origin is fallback");
        let after = std::fs::read_to_string(&log_file).unwrap().lines().filter(|l| l.starts_with("CALL::")).count();
        assert_eq!(after - before, 1, "only one attempt when origin == fallback");

        let logged = std::fs::read_to_string(&log_file).unwrap();
        assert!(logged.contains("Fallback triage:"), "fallback message must carry triage instruction");
    }
}
