//! The owner's answers, written by Maestro (BRO-4042; owner decisions 2026-11-20).
//!
//! An answer is signed with the owner's passkey ([`crate::passkey`]), held for an undo window
//! ([`UNDO`]), then written: the ledger edit [`maestro_store::answer::answered`] makes, in
//! Maestro's own checkout of the workspace, committed with the signature in its trailers and
//! pushed to `maestro/answers`. The answer takes effect there, on its signature. Then the
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
    /// Written to `maestro/answers`: in effect.
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
    pub t3: Option<PathBuf>,
    /// Whether to push and land (off in tests).
    pub remote: bool,
}

pub const BRANCH: &str = "maestro/answers";

/// Signed answers by receipt id.
#[derive(Clone, Default)]
pub struct Book {
    inner: Arc<Mutex<HashMap<String, Receipt>>>,
}

impl Book {
    pub fn get(&self, id: &str) -> Option<Receipt> {
        self.inner.lock().ok()?.get(id).cloned()
    }

    pub fn put(&self, r: Receipt) {
        if let Ok(mut m) = self.inner.lock() {
            m.insert(r.id.clone(), r);
        }
    }

    /// Undo inside the window: only a receipt still signed turns undone.
    pub fn undo(&self, id: &str, now_ms: i64) -> Result<Receipt, String> {
        let mut m = self.inner.lock().map_err(|_| "busy".to_string())?;
        let r = m.get_mut(id).ok_or("no such answer")?;
        if r.stage != Stage::Signed {
            return Err("it was already written".into());
        }
        if now_ms >= r.applies_at_ms {
            return Err("the undo window has closed".into());
        }
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
    let dir = pending_dir(owner_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("pending: {e}"))?;
    let tmp = dir.join(format!(".{}.tmp", p.receipt.id));
    let mut f = std::fs::File::create(&tmp).map_err(|e| format!("pending: {e}"))?;
    f.write_all(serde_json::to_string(p).map_err(|e| e.to_string())?.as_bytes())
        .map_err(|e| format!("pending: {e}"))?;
    f.sync_all().map_err(|e| format!("pending: {e}"))?;
    std::fs::rename(&tmp, dir.join(format!("{}.json", p.receipt.id))).map_err(|e| format!("pending: {e}"))
}

/// Settle a pending answer: written, failed or undone, it no longer waits.
pub fn settle(owner_dir: &Path, id: &str) {
    let _ = std::fs::remove_file(pending_dir(owner_dir).join(format!("{id}.json")));
    let _ = std::fs::remove_file(pending_dir(owner_dir).join(format!("{id}.undo")));
}

/// What a write that found its answer undone reports.
pub const UNDONE: &str = "undone before it was written";

/// Record an undo on disk, durably, before it is acknowledged.
pub fn mark_undone(owner_dir: &Path, id: &str) -> Result<(), String> {
    let dir = pending_dir(owner_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("undo: {e}"))?;
    let f = std::fs::File::create(dir.join(format!("{id}.undo"))).map_err(|e| format!("undo: {e}"))?;
    f.sync_all().map_err(|e| format!("undo: {e}"))
}

/// Withdraw an undo marker the write already overtook.
pub fn unmark_undone(owner_dir: &Path, id: &str) {
    let _ = std::fs::remove_file(pending_dir(owner_dir).join(format!("{id}.undo")));
}

/// Whether an undo is recorded on disk for `id`.
pub fn undone(owner_dir: &Path, id: &str) -> bool {
    pending_dir(owner_dir).join(format!("{id}.undo")).exists()
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

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// The open landing PR's number, if one is open.
fn landing_pr(dir: &Path) -> Result<Option<String>, String> {
    let out = Command::new("gh")
        .current_dir(dir)
        .args([
            "pr",
            "list",
            "--head",
            BRANCH,
            "--base",
            "main",
            "--state",
            "open",
            "--json",
            "number",
            "--jq",
            ".[0].number // empty",
        ])
        .output()
        .map_err(|e| format!("gh: {e}"))?;
    if !out.status.success() {
        return Err(format!("gh pr list: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let n = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((!n.is_empty()).then_some(n))
}

/// Maestro's checkout, made if missing, reset to a clean tree (nothing staged, nothing untracked)
/// at the base the answer goes on: `maestro/answers` with main merged in when the branch exists,
/// so an answer it holds that main lacks is kept, never pushed over; else main itself. Returns
/// the branch's tip when it exists.
fn prepare(w: &Writer) -> Result<Option<String>, String> {
    if !w.checkout.join(".git").exists() {
        if let Some(parent) = w.checkout.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        git(&w.workspace, &["worktree", "add", "--detach", &w.checkout.to_string_lossy(), "HEAD"])?;
    }
    // Whatever another process left here, staged or not, is dropped before anything is written.
    let _ = git(&w.checkout, &["merge", "--abort"]);
    git(&w.checkout, &["reset", "-q", "--hard"])?;
    git(&w.checkout, &["clean", "-q", "-f", "-d", "-x"])?;
    let tip = if w.remote {
        git(&w.checkout, &["fetch", "-q", "origin", "main"])?;
        if git(&w.checkout, &["ls-remote", "--heads", "origin", BRANCH])?.is_empty() {
            None
        } else {
            git(
                &w.checkout,
                &["fetch", "-q", "origin", &format!("+refs/heads/{BRANCH}:refs/remotes/origin/{BRANCH}")],
            )?;
            Some(git(&w.checkout, &["rev-parse", &format!("refs/remotes/origin/{BRANCH}")])?)
        }
    } else {
        None
    };
    let main = if w.remote { "refs/remotes/origin/main" } else { "HEAD" };
    match &tip {
        Some(tip) => {
            git(&w.checkout, &["checkout", "-q", "--detach", tip])?;
            let merged = git(
                &w.checkout,
                &[
                    "-c",
                    "user.name=Maestro",
                    "-c",
                    "user.email=maestro@broomva.local",
                    "merge",
                    "-q",
                    "--no-edit",
                    main,
                ],
            );
            if let Err(e) = merged {
                let _ = git(&w.checkout, &["merge", "--abort"]);
                return Err(format!("main does not merge into {BRANCH}: {e}"));
            }
        }
        None => {
            let at = git(&w.checkout, &["rev-parse", main])?;
            git(&w.checkout, &["checkout", "-q", "--detach", &at])?;
        }
    }
    Ok(tip)
}

/// One write at a time across every Maestro process: an exclusive lock on a file beside the checkout.
fn exclusive(w: &Writer) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(&w.owner_dir).map_err(|e| e.to_string())?;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(w.owner_dir.join("write.lock"))
        .map_err(|e| format!("write lock: {e}"))?;
    f.lock().map_err(|e| format!("write lock: {e}"))?;
    Ok(f)
}

/// Write one signed answer: the ledger edit, a commit carrying the proof and nothing else, the push.
pub fn write(w: &Writer, r: &Receipt, assertion: &Assertion, nonce: &str, by: &str) -> Result<String, String> {
    let _lock = exclusive(w)?;
    // An undo recorded on disk, by this daemon or before a restart, wins over the write.
    if undone(&w.owner_dir, &r.id) {
        return Err(UNDONE.into());
    }
    let tip = prepare(w)?;
    // Written already (a restart after the push, before the pending record was settled): the
    // commit that carries this answer's id is the outcome, and nothing is written twice.
    let done = git(
        &w.checkout,
        &["log", "-1", "--format=%H", "--fixed-strings", &format!("--grep=Maestro-Answer: {}", r.id)],
    )?;
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
    git(&w.checkout, &["add", "--", &rel])?;
    // A pathspec commit takes this one file whatever else is staged.
    git(
        &w.checkout,
        &[
            "-c",
            "user.name=Maestro",
            "-c",
            "user.email=maestro@broomva.local",
            "commit",
            "-q",
            "-m",
            &message,
            "--",
            &rel,
        ],
    )?;
    // The commit holds exactly the signed edit: one file, these bytes.
    let changed = git(&w.checkout, &["diff", "--name-only", "HEAD~1", "HEAD"])?;
    let committed = git(&w.checkout, &["show", &format!("HEAD:{rel}")])?;
    if changed != rel || committed != edited.trim() {
        return Err(format!("the commit is not exactly the signed answer ({changed}); nothing was pushed"));
    }
    let commit = git(&w.checkout, &["rev-parse", "HEAD"])?;
    if w.remote {
        // A fast-forward only: the base holds the branch's tip, so nothing on it is pushed over.
        let _ = tip;
        git(&w.checkout, &["push", "-q", "origin", &format!("HEAD:refs/heads/{BRANCH}")])?;
    }
    Ok(commit)
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
         A landing PR brings it to main; do not wait for it. Resume the work this ask held, acting on the answer.",
        r.intent.ledger, r.intent.handle, r.label, r.intent.option
    )
}

/// Wake the session an ask names (`t3:<environment>/<thread>`), as its next turn.
pub fn wake(t3: Option<&Path>, session: Option<&str>, message: &str) -> String {
    let Some(session) = session else { return "no session is named on the ask".into() };
    let Some(thread) = session.strip_prefix("t3:").and_then(|s| s.split_once('/')).map(|(_, t)| t) else {
        return format!("{session} is not a T3 session");
    };
    let Some(t3) = t3 else { return "no T3 CLI is configured".into() };
    match Command::new(t3).args(["thread", "send", thread, message]).output() {
        Ok(o) if o.status.success() => format!("told T3 thread {thread}"),
        Ok(o) => format!("T3 did not take it: {}", String::from_utf8_lossy(&o.stderr).trim()),
        Err(e) => format!("T3 did not run: {e}"),
    }
}

/// Open the landing PR if none is open, and set it to merge when its checks pass.
pub fn land(w: &Writer) -> String {
    let dir = &w.checkout;
    let number = match landing_pr(dir) {
        Ok(Some(n)) => n,
        Ok(None) => {
            let out = Command::new("gh")
                .current_dir(dir)
                .args([
                    "pr",
                    "create",
                    "--head",
                    BRANCH,
                    "--base",
                    "main",
                    "--title",
                    "answers: the owner's passkey-signed answers",
                    "--body",
                    "Answers the owner signed with a passkey on Maestro's Decisions page, written by Maestro (BRO-4042). Each commit carries its proof in a `Maestro-Owner-Proof` trailer. The answers took effect when signed; this PR only brings them to main.",
                ])
                .output();
            match out {
                Ok(o) if o.status.success() => {
                    String::from_utf8_lossy(&o.stdout).trim().rsplit('/').next().unwrap_or("").to_string()
                }
                Ok(o) => return format!("no landing PR: {}", String::from_utf8_lossy(&o.stderr).trim()),
                Err(e) => return format!("no landing PR: {e}"),
            }
        }
        Err(e) => return format!("no landing PR: {e}"),
    };
    let merged = Command::new("gh").current_dir(dir).args(["pr", "merge", &number, "--auto", "--squash"]).output();
    match merged {
        Ok(o) if o.status.success() => format!("landing PR #{number}, set to merge when its checks pass"),
        Ok(o) => format!("landing PR #{number}; auto-merge not set: {}", String::from_utf8_lossy(&o.stderr).trim()),
        Err(e) => format!("landing PR #{number}; auto-merge not set: {e}"),
    }
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
            r.woke = Some(wake(w.t3.as_deref(), session, &wake_message(&r, &commit)));
            book.put(r.clone());
            if w.remote {
                r.landing = Some(land(w));
                book.put(r.clone());
            }
            let _ = log(&w.owner_dir, &r);
            // Settled last: a crash before this resumes the answer, which is found written by its
            // id, and the wake and the landing are tried again.
            settle(&w.owner_dir, id);
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
            id: "r1".into(),
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
        assert!(book.undo("r1", 9_001).is_err(), "after the window");
        assert_eq!(book.undo("r1", 2_000).unwrap().stage, Stage::Undone);
        assert!(book.undo("r1", 2_000).is_err(), "twice");
        assert!(book.take_due("r1").is_none(), "an undone answer is never written");
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
        apply(&w, &book, "r1", &assertion(), "n1", "owner (passkey)", Some("t3:env/thread-1"));
        let r = book.get("r1").unwrap();
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
        assert_eq!(logged(&w.owner_dir, "r1").unwrap().stage, Stage::Written);
    }

    #[test]
    fn a_change_staged_in_the_checkout_never_rides_along() {
        let (_ws, _state, w) = workspace(Some(true));
        // A first answer makes the checkout; then another process stages an unsigned change there.
        let book = Book::default();
        book.put(receipt(Intent::answer("t.yaml", "a", &hash_of("a"), "no", None), 0));
        apply(&w, &book, "r1", &assertion(), "n1", "owner", None);
        std::fs::write(w.checkout.join(".control/asks/evil.yaml"), "arc: e\nasks: []\n").unwrap();
        git(&w.checkout, &["add", "."]).unwrap();
        let mut r2 = receipt(Intent::answer("t.yaml", "b", &hash_of("b"), "go", None), 0);
        r2.id = "r2".into();
        book.put(r2);
        apply(&w, &book, "r2", &assertion(), "n2", "owner", None);
        assert_eq!(book.get("r2").unwrap().stage, Stage::Written, "{:?}", book.get("r2").unwrap().error);
        let files = git(&w.checkout, &["diff", "--name-only", "HEAD~1", "HEAD"]).unwrap();
        assert_eq!(files, ".control/asks/t.yaml");
        assert!(!w.checkout.join(".control/asks/evil.yaml").exists());
    }

    #[test]
    fn a_resumed_answer_already_written_is_not_written_twice() {
        let (_ws, _state, w) = workspace(Some(true));
        let book = Book::default();
        book.put(receipt(Intent::answer("t.yaml", "a", &hash_of("a"), "no", None), 0));
        apply(&w, &book, "r1", &assertion(), "n1", "owner", None);
        let first = book.get("r1").unwrap().commit.unwrap();
        // The daemon restarts before settling: the same signed receipt is applied again.
        let again = Book::default();
        again.put(receipt(Intent::answer("t.yaml", "a", &hash_of("a"), "no", None), 0));
        // Locally the base is the workspace HEAD, which lacks the first commit; build on it instead.
        git(&w.workspace, &["fetch", "-q", &w.checkout.to_string_lossy(), &first]).unwrap();
        git(&w.workspace, &["merge", "-q", "--ff-only", &first]).unwrap();
        apply(&w, &again, "r1", &assertion(), "n1", "owner", None);
        let r = again.get("r1").unwrap();
        assert_eq!((r.stage, r.commit.as_deref()), (Stage::Written, Some(first.as_str())), "{:?}", r.error);
    }

    #[test]
    fn without_the_validator_or_against_it_nothing_is_written() {
        for validator in [None, Some(false)] {
            let (_ws, _state, w) = workspace(validator);
            let book = Book::default();
            book.put(receipt(Intent::answer("t.yaml", "a", &hash_of("a"), "no", None), 0));
            apply(&w, &book, "r1", &assertion(), "n1", "owner", None);
            let r = book.get("r1").unwrap();
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
        assert!(book.take_due("r1").is_some());
        // Even with a wall clock moved back inside the window.
        assert!(book.undo("r1", 0).is_err());
        assert!(book.take_due("r1").is_none(), "taken once");
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
        settle(d.path(), "r1");
        assert!(pending(d.path()).is_empty());
    }

    #[test]
    fn a_session_is_woken_only_through_t3() {
        assert_eq!(wake(None, None, "m"), "no session is named on the ask");
        assert_eq!(wake(None, Some("paseo:x"), "m"), "paseo:x is not a T3 session");
        assert_eq!(wake(Some(Path::new("/bin/echo")), Some("t3:env/th"), "m"), "told T3 thread th");
    }
}
