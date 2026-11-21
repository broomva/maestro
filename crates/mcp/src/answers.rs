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

    /// Take a signed receipt to write once its window has closed: `None` if it was undone.
    fn take_due(&self, id: &str) -> Option<Receipt> {
        let m = self.inner.lock().ok()?;
        m.get(id).filter(|r| r.stage == Stage::Signed).cloned()
    }
}

/// Append a receipt to the log.
pub fn log(owner_dir: &Path, r: &Receipt) {
    use std::io::Write;
    let _ = std::fs::create_dir_all(owner_dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(owner_dir.join("receipts.jsonl")) {
        let _ = writeln!(f, "{}", serde_json::to_string(r).unwrap_or_default());
    }
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
        .args(["pr", "list", "--head", BRANCH, "--base", "main", "--state", "open", "--json", "number", "--jq", ".[0].number // empty"])
        .output()
        .map_err(|e| format!("gh: {e}"))?;
    if !out.status.success() {
        return Err(format!("gh pr list: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let n = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((!n.is_empty()).then_some(n))
}

/// Maestro's checkout, made if missing, at the base the answer goes on: `maestro/answers` while
/// a landing PR holds answers main lacks, else main itself. Returns the lease for the push.
fn prepare(w: &Writer) -> Result<Option<String>, String> {
    if !w.checkout.join(".git").exists() {
        if let Some(parent) = w.checkout.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        git(&w.workspace, &["worktree", "add", "--detach", &w.checkout.to_string_lossy(), "HEAD"])?;
    }
    let remote_answers = if w.remote {
        git(&w.checkout, &["fetch", "-q", "origin", "main"])?;
        let has = !git(&w.checkout, &["ls-remote", "--heads", "origin", BRANCH])?.is_empty();
        if has {
            git(&w.checkout, &["fetch", "-q", "origin", &format!("+refs/heads/{BRANCH}:refs/remotes/origin/{BRANCH}")])?;
            Some(git(&w.checkout, &["rev-parse", &format!("refs/remotes/origin/{BRANCH}")])?)
        } else {
            None
        }
    } else {
        None
    };
    let main = if w.remote { "refs/remotes/origin/main" } else { "HEAD" };
    let pending = match &remote_answers {
        Some(_) if w.remote => landing_pr(&w.checkout)?.is_some(),
        _ => false,
    };
    if let (Some(tip), true) = (&remote_answers, pending) {
        git(&w.checkout, &["checkout", "-q", "--detach", tip])?;
        if let Err(e) = git(&w.checkout, &["merge", "-q", "--no-edit", main]) {
            let _ = git(&w.checkout, &["merge", "--abort"]);
            return Err(format!("main does not merge into {BRANCH}: {e}"));
        }
    } else {
        let at = git(&w.checkout, &["rev-parse", main])?;
        git(&w.checkout, &["checkout", "-q", "--detach", &at])?;
    }
    Ok(remote_answers)
}

/// Write one signed answer: the ledger edit, a commit carrying the proof, the push.
pub fn write(w: &Writer, r: &Receipt, assertion: &Assertion, nonce: &str, by: &str) -> Result<String, String> {
    let lease = prepare(w)?;
    let rel = format!("{}/{}", w.asks, r.intent.ledger);
    let path = w.checkout.join(&rel);
    let source = std::fs::read_to_string(&path).map_err(|e| format!("{rel}: {e}"))?;
    let answer = maestro_store::answer::Answer { option: &r.intent.option, note: r.intent.note.as_deref(), by, at: &r.signed_at };
    let select = maestro_store::answer::Select { handle: &r.intent.handle };
    let edited = maestro_store::answer::answered(&source, &rel, &select, &r.intent.hash, &answer).map_err(|e| e.to_string())?;
    std::fs::write(&path, edited).map_err(|e| format!("{rel}: {e}"))?;
    // The validator of record has the last word on the ledger.
    let validator = w.checkout.join("scripts/ask_ledger.py");
    if validator.exists() {
        let out = Command::new("python3").arg(&validator).arg("validate").arg(&path).current_dir(&w.checkout).output();
        match out {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                let _ = git(&w.checkout, &["checkout", "--", &rel]);
                return Err(format!("the ledger validator refused the answer: {}", String::from_utf8_lossy(&o.stdout).trim()));
            }
            Err(e) => {
                let _ = git(&w.checkout, &["checkout", "--", &rel]);
                return Err(format!("the ledger validator did not run: {e}"));
            }
        }
    }
    // The intent exactly as signed (its canonical text), so the proof re-checks byte for byte.
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
    git(&w.checkout, &["-c", "user.name=Maestro", "-c", "user.email=maestro@broomva.local", "commit", "-q", "-m", &message])?;
    let commit = git(&w.checkout, &["rev-parse", "HEAD"])?;
    if w.remote {
        let lease = format!("--force-with-lease=refs/heads/{BRANCH}:{}", lease.as_deref().unwrap_or(""));
        git(&w.checkout, &["push", "-q", &lease, "origin", &format!("HEAD:refs/heads/{BRANCH}")])?;
    }
    Ok(commit)
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
                Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().rsplit('/').next().unwrap_or("").to_string(),
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

/// After the window: write, wake, land, logging each stage. Runs on a blocking thread.
pub fn apply(w: &Writer, book: &Book, id: &str, assertion: &Assertion, nonce: &str, by: &str, session: Option<&str>) {
    let Some(mut r) = book.take_due(id) else { return };
    match write(w, &r, assertion, nonce, by) {
        Ok(commit) => {
            r.stage = Stage::Written;
            r.commit = Some(commit.clone());
            book.put(r.clone());
            log(&w.owner_dir, &r);
            r.woke = Some(wake(w.t3.as_deref(), session, &wake_message(&r, &commit)));
            book.put(r.clone());
            if w.remote {
                r.landing = Some(land(w));
                book.put(r.clone());
            }
            log(&w.owner_dir, &r);
        }
        Err(e) => {
            r.stage = Stage::Failed;
            r.error = Some(e);
            book.put(r.clone());
            log(&w.owner_dir, &r);
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
        assert_eq!(i.canonical(), r#"{"verb":"ask.answer","ledger":"l.yaml","handle":"a","hash":"h","option":"no","note":null}"#);
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

    #[test]
    fn writes_the_answer_in_its_own_checkout_with_the_proof() {
        let ws = tempfile::tempdir().unwrap();
        let asks = ws.path().join(".control/asks");
        std::fs::create_dir_all(&asks).unwrap();
        let ledger = "arc: t\nasks:\n  - id: a\n    ask: \"First?\"\n    options:\n      - id: yes\n        label: \"Yes\"\n      - id: no\n        label: \"No\"\n    origin:\n      session: \"t3:env/thread-1\"\n";
        std::fs::write(asks.join("t.yaml"), ledger).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["add", "."], &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]] {
            git(ws.path(), args).unwrap();
        }
        let hash = {
            let l = maestro_store::answer::find;
            let text = std::fs::read_to_string(asks.join("t.yaml")).unwrap();
            let parsed: maestro_core::ask::AskLedger =
                maestro_core::ask::AskLedger::from_value(maestro_store::yaml::parse(&text).unwrap().value).unwrap().0;
            l(&parsed, &maestro_store::answer::Select { handle: "a" }).unwrap().hash()
        };
        let state = tempfile::tempdir().unwrap();
        let w = Writer {
            workspace: ws.path().into(),
            asks: ".control/asks".into(),
            checkout: state.path().join("checkout"),
            owner_dir: state.path().join("owner"),
            t3: None,
            remote: false,
        };
        let book = Book::default();
        book.put(receipt(Intent::answer("t.yaml", "a", &hash, "no", Some("after the soak")), 0));
        let a = Assertion { credential: "c".into(), client_data: "d".into(), authenticator_data: "e".into(), signature: "f".into() };
        apply(&w, &book, "r1", &a, "n1", "owner (passkey)", Some("t3:env/thread-1"));
        let r = book.get("r1").unwrap();
        assert_eq!(r.stage, Stage::Written, "{:?}", r.error);
        assert_eq!(r.woke.as_deref(), Some("no T3 CLI is configured"));
        let written = std::fs::read_to_string(w.checkout.join(".control/asks/t.yaml")).unwrap();
        assert!(written.contains("answer_option: \"no\""));
        let msg = git(&w.checkout, &["log", "-1", "--format=%B"]).unwrap();
        assert!(msg.contains("Maestro-Owner-Proof: {") && msg.contains(r#""intent":"{\"verb\":\"ask.answer\""#), "{msg}");
        // The owner's own checkout is untouched.
        assert_eq!(std::fs::read_to_string(asks.join("t.yaml")).unwrap(), ledger);
        // A second answer to the same ask is refused: it is no longer open at the checkout's base,
        // which in this test is the workspace HEAD, so it reads open again; the hash still binds it.
        let log = std::fs::read_to_string(w.owner_dir.join("receipts.jsonl")).unwrap();
        assert_eq!(log.lines().count(), 2);
    }

    #[test]
    fn a_session_is_woken_only_through_t3() {
        assert_eq!(wake(None, None, "m"), "no session is named on the ask");
        assert_eq!(wake(None, Some("paseo:x"), "m"), "paseo:x is not a T3 session");
        assert_eq!(wake(Some(Path::new("/bin/echo")), Some("t3:env/th"), "m"), "told T3 thread th");
    }
}
