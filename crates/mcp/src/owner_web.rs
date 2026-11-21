//! The owner's routes on the web pages (BRO-4042): passkey setup, and answering a decision.
//!
//! Every route here is a same-origin `POST` of JSON (or a `GET` of one receipt), refused unless
//! the request names the configured origin, so no other site and no plain form can drive it.
//! The proof is the passkey: an answer is accepted only with an assertion over a challenge
//! that commits to the ask's ledger, handle, entry hash, option and note ([`crate::passkey`]).

use crate::answers::{self, Book, Intent, Receipt, Stage, Writer};
use crate::passkey::{self, Assertion, Keyring, Party, Registration};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a challenge stays good.
const CHALLENGE_TTL: Duration = Duration::from_secs(180);

/// What answering needs: the party, the passkeys' folder, the writer, and the live state.
#[derive(Clone)]
pub struct Owner {
    /// The owner-role lock, held while any clone lives.
    #[allow(dead_code)]
    daemon: Arc<std::fs::File>,
    pub party: Party,
    pub dir: PathBuf,
    pub writer: Writer,
    pub book: Book,
    pending: Arc<Mutex<HashMap<String, Pending>>>,
    /// The one-time code that allows the first passkey to be set up; `None` once one is.
    enroll_code: Arc<Mutex<Option<String>>>,
}

#[derive(Clone)]
enum Pending {
    Enroll { challenge: String, at: Instant },
    Answer { intent: Intent, label: String, session: Option<String>, challenge: String, at: Instant },
}

impl Pending {
    fn at(&self) -> Instant {
        match self {
            Pending::Enroll { at, .. } | Pending::Answer { at, .. } => *at,
        }
    }
}

impl Owner {
    /// From the environment: `MAESTRO_WEB_ORIGIN` names the page's secure origin. Unset, the
    /// pages offer no answering, and say why.
    pub fn from_env(workspace: PathBuf, asks: String) -> Result<Self, String> {
        let origin = std::env::var("MAESTRO_WEB_ORIGIN")
            .ok()
            .filter(|v| !v.is_empty())
            .ok_or("Maestro is not served at a secure address, which a passkey needs")?;
        let party = Party::parse(&origin)?;
        let dir = passkey::owner_dir();
        let home = std::env::home_dir().unwrap_or_default();
        let checkout = std::env::var_os("MAESTRO_ANSWERS_CHECKOUT")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state/maestro/answers-checkout"));
        let t3 = std::env::var_os("MAESTRO_T3_BIN")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| Some(home.join(".t3/bin/t3")).filter(|p| p.exists()));
        let remote = std::env::var("MAESTRO_ANSWERS_LOCAL").ok().as_deref() != Some("1");
        let writer = Writer { workspace, asks, checkout, owner_dir: dir.clone(), t3, remote };
        Self::claim(party, dir, writer)
    }

    pub fn new(party: Party, dir: PathBuf, writer: Writer) -> Self {
        Self::claim(party, dir, writer).expect("a fresh owner directory")
    }

    /// The owner role, held by one daemon at a time: an exclusive lock on `<dir>/daemon.lock`,
    /// kept for the daemon's life. A second daemon on the same directory takes no answers.
    pub fn claim(party: Party, dir: PathBuf, writer: Writer) -> Result<Self, String> {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("daemon.lock"))
            .map_err(|e| format!("owner lock: {e}"))?;
        lock.try_lock().map_err(|_| "another Maestro daemon already takes the owner's answers here".to_string())?;
        let owner = Owner {
            daemon: Arc::new(lock),
            party,
            dir,
            writer,
            book: Book::default(),
            pending: Arc::default(),
            enroll_code: Arc::default(),
        };
        owner.recover();
        if Keyring::load(&owner.dir).is_ok_and(|k| k.credentials.is_empty()) {
            let code = passkey::nonce()[..8].to_ascii_uppercase();
            eprintln!("maestro: set up the owner's passkey at {}/owner with the code {code}", owner.party.origin);
            let _ = std::fs::create_dir_all(&owner.dir);
            let _ = std::fs::write(owner.dir.join("setup-code"), format!("{code}\n"));
            *owner.enroll_code.lock().expect("fresh") = Some(code);
        }
        Ok(owner)
    }

    /// Pending answers left by a stopped daemon: each is written once its window has closed, or
    /// at once if it already has. Needs a running Tokio runtime; without one (a unit test) none is resumed.
    fn recover(&self) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else { return };
        for p in answers::pending(&self.dir) {
            if answers::undone(&self.dir, &p.receipt.id) {
                let mut r = p.receipt.clone();
                r.stage = Stage::Undone;
                let _ = answers::log(&self.dir, &r);
                answers::settle(&self.dir, &r.id);
                continue;
            }
            self.book.put(p.receipt.clone());
            let wait = (p.receipt.applies_at_ms - jiff::Timestamp::now().as_millisecond()).max(0) as u64;
            let owner = self.clone();
            rt.spawn(async move {
                tokio::time::sleep(Duration::from_millis(wait)).await;
                let _ = tokio::task::spawn_blocking(move || {
                    answers::apply(
                        &owner.writer,
                        &owner.book,
                        &p.receipt.id,
                        &p.assertion,
                        &p.nonce,
                        &p.by,
                        p.session.as_deref(),
                    )
                })
                .await;
            });
        }
    }

    /// A receipt by id: the running daemon's, else the last one logged.
    pub fn receipt(&self, id: &str) -> Option<Receipt> {
        self.book.get(id).or_else(|| answers::logged(&self.dir, id))
    }

    /// Undo inside the window; the pending record is settled so a restart does not write it.
    pub fn undo(&self, id: &str, now_ms: i64) -> Result<Receipt, String> {
        // Durable first: the write checks this marker under its lock, here and after a restart.
        answers::mark_undone(&self.dir, id)?;
        match self.book.undo(id, now_ms) {
            Ok(r) => {
                let _ = answers::log(&self.dir, &r);
                Ok(r)
            }
            Err(e) => {
                // The write began first: the marker is withdrawn and the answer stands.
                answers::unmark_undone(&self.dir, id);
                Err(e)
            }
        }
    }

    /// The setup code, for tests.
    pub fn setup_code(&self) -> Option<String> {
        self.enroll_code.lock().ok()?.clone()
    }

    /// How the decision pages offer answering.
    pub fn state(&self) -> maestro_view::render::Owner {
        match Keyring::load(&self.dir) {
            Ok(k) if k.credentials.is_empty() => maestro_view::render::Owner::Enroll,
            Ok(_) => maestro_view::render::Owner::Ready,
            Err(e) => maestro_view::render::Owner::Off(format!("the enrolled passkeys could not be read ({e})")),
        }
    }

    fn keep(&self, nonce: &str, p: Pending) {
        if let Ok(mut m) = self.pending.lock() {
            m.retain(|_, v| v.at().elapsed() < CHALLENGE_TTL);
            m.insert(nonce.to_string(), p);
        }
    }

    fn take(&self, nonce: &str) -> Option<Pending> {
        let mut m = self.pending.lock().ok()?;
        m.remove(nonce).filter(|p| p.at().elapsed() < CHALLENGE_TTL)
    }

    /// Start setting up a passkey: only while none is enrolled, and only with the setup code.
    pub fn enroll_begin(&self, code: &str) -> Result<serde_json::Value, String> {
        let keyring = Keyring::load(&self.dir)?;
        if !keyring.credentials.is_empty() {
            return Err("a passkey is already set up".into());
        }
        let expected = self.enroll_code.lock().map_err(|_| "busy")?.clone().ok_or("setup is closed")?;
        if !constant_eq(code.trim().to_ascii_uppercase().as_bytes(), expected.as_bytes()) {
            return Err("that setup code is not the one Maestro printed".into());
        }
        let nonce = passkey::nonce();
        let challenge = passkey::challenge("{\"verb\":\"owner.enroll\"}", &nonce);
        self.keep(&nonce, Pending::Enroll { challenge: challenge.clone(), at: Instant::now() });
        Ok(
            serde_json::json!({ "nonce": nonce, "challenge": challenge, "rp_id": self.party.id, "user_id": passkey::nonce() }),
        )
    }

    pub fn enroll_finish(&self, nonce: &str, r: &Registration, at: &str) -> Result<(), String> {
        let Some(Pending::Enroll { challenge, .. }) = self.take(nonce) else {
            return Err("that setup request expired; start again".into());
        };
        let mut keyring = Keyring::load(&self.dir)?;
        if !keyring.credentials.is_empty() {
            return Err("a passkey is already set up".into());
        }
        keyring.credentials.push(passkey::register(&self.party, &challenge, r, at)?);
        keyring.save(&self.dir)?;
        if let Ok(mut c) = self.enroll_code.lock() {
            *c = None;
        }
        let _ = std::fs::remove_file(self.dir.join("setup-code"));
        Ok(())
    }

    /// Start an answer: the ask must be open with the hash the page showed, and the option one of
    /// its own. Returns the challenge the passkey signs.
    pub fn answer_begin(&self, store: &maestro_store::Store, req: &AnswerBegin) -> Result<serde_json::Value, String> {
        let keyring = Keyring::load(&self.dir)?;
        if keyring.credentials.is_empty() {
            return Err("no passkey is set up yet".into());
        }
        let ledger = store.ask_ledger(&req.ledger)?.ok_or("no such ledger")?;
        let ask = maestro_store::answer::find(&ledger, &maestro_store::answer::Select { handle: &req.handle })
            .map_err(|e| e.to_string())?;
        if ask.hash() != req.hash {
            return Err("the ask changed since you opened it; reload to see the new version".into());
        }
        if ask.state() != maestro_core::ask::AskState::Open {
            return Err("the ask is no longer open".into());
        }
        let option = ask.options.iter().find(|o| o.id == req.option).ok_or("that is not one of the ask's options")?;
        if req.note.chars().count() > 2000 {
            return Err("the note is longer than 2000 characters".into());
        }
        let intent = Intent::answer(&req.ledger, &req.handle, &req.hash, &option.id, Some(&req.note));
        let nonce = passkey::nonce();
        let challenge = passkey::challenge(&intent.canonical(), &nonce);
        let session = ask.origin.as_ref().and_then(|o| o.session.clone());
        self.keep(
            &nonce,
            Pending::Answer {
                intent,
                label: option.label.clone(),
                session,
                challenge: challenge.clone(),
                at: Instant::now(),
            },
        );
        let ids: Vec<&str> =
            keyring.credentials.iter().filter(|c| c.rp_id == self.party.id).map(|c| c.id.as_str()).collect();
        Ok(serde_json::json!({ "nonce": nonce, "challenge": challenge, "rp_id": self.party.id, "credentials": ids }))
    }

    /// Finish an answer: the assertion must verify over the challenge. The answer is then held for
    /// the undo window and written by a background task.
    pub fn answer_finish(&self, nonce: &str, a: &Assertion, now: jiff::Timestamp) -> Result<Receipt, String> {
        let Some(Pending::Answer { intent, label, session, challenge, .. }) = self.take(nonce) else {
            return Err("that answer request expired; answer again".into());
        };
        let keyring = Keyring::load(&self.dir)?;
        let cred = passkey::verify(&self.party, &keyring, &challenge, a)?;
        let receipt = Receipt {
            id: passkey::nonce()[..16].to_string(),
            intent,
            label,
            stage: Stage::Signed,
            signed_at: now.strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
            applies_at_ms: now.as_millisecond() + answers::UNDO.as_millis() as i64,
            commit: None,
            woke: None,
            landing: None,
            error: None,
        };
        let by = format!("owner (passkey {})", &cred.id[..cred.id.len().min(8)]);
        // Durable before acknowledged: a restart inside or after the window resumes it.
        let pending = answers::Pending {
            receipt: receipt.clone(),
            assertion: a.clone(),
            nonce: nonce.to_string(),
            by: by.clone(),
            session: session.clone(),
        };
        answers::persist(&self.dir, &pending)?;
        if let Err(e) = answers::log(&self.dir, &receipt) {
            answers::settle(&self.dir, &receipt.id);
            return Err(e);
        }
        self.book.put(receipt.clone());
        let (owner, id, assertion, nonce) = (self.clone(), receipt.id.clone(), a.clone(), nonce.to_string());
        tokio::spawn(async move {
            tokio::time::sleep(answers::UNDO).await;
            let _ = tokio::task::spawn_blocking(move || {
                answers::apply(&owner.writer, &owner.book, &id, &assertion, &nonce, &by, session.as_deref())
            })
            .await;
        });
        Ok(receipt)
    }
}

/// What the page sends to start an answer.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AnswerBegin {
    pub ledger: String,
    pub handle: String,
    pub hash: String,
    pub option: String,
    #[serde(default)]
    pub note: String,
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::passkey::tests::{keyring, party, sign};

    fn owner_writer(dir: &std::path::Path) -> Writer {
        Writer {
            workspace: dir.join("ws"),
            asks: ".control/asks".into(),
            checkout: dir.join("co"),
            owner_dir: dir.join("owner"),
            t3: None,
            remote: false,
        }
    }

    fn owner(dir: &std::path::Path) -> Owner {
        let writer = Writer {
            workspace: dir.join("ws"),
            asks: ".control/asks".into(),
            checkout: dir.join("co"),
            owner_dir: dir.join("owner"),
            t3: None,
            remote: false,
        };
        Owner::new(party(), dir.join("owner"), writer)
    }

    #[test]
    fn setup_needs_the_printed_code_and_closes_once_a_passkey_is_set_up() {
        let d = tempfile::tempdir().unwrap();
        let o = owner(d.path());
        let code = o.setup_code().unwrap();
        assert!(o.enroll_begin("WRONG123").is_err());
        assert!(o.enroll_begin(&code.to_lowercase()).is_ok());
        keyring().save(&o.dir).unwrap();
        assert!(o.enroll_begin(&code).is_err(), "closed once a passkey exists");
        assert_eq!(o.state(), maestro_view::render::Owner::Ready);
    }

    #[test]
    fn an_answer_is_accepted_only_with_a_signature_over_its_own_challenge() {
        let d = tempfile::tempdir().unwrap();
        let o = owner(d.path());
        keyring().save(&o.dir).unwrap();
        let intent = Intent::answer("t.yaml", "a", "h", "no", None);
        let nonce = passkey::nonce();
        let challenge = passkey::challenge(&intent.canonical(), &nonce);
        let keep = |n: &str, c: &str| {
            o.keep(
                n,
                Pending::Answer {
                    intent: intent.clone(),
                    label: "No".into(),
                    session: None,
                    challenge: c.into(),
                    at: Instant::now(),
                },
            )
        };
        // A signature over another challenge is refused, and the request is spent.
        keep(&nonce, &challenge);
        let now = jiff::Timestamp::from_second(1_800_000_000).unwrap();
        assert!(o.answer_finish(&nonce, &sign("another"), now).is_err());
        assert!(o.answer_finish(&nonce, &sign(&challenge), now).is_err(), "a spent request");
        // A nonce never issued is refused.
        assert!(o.answer_finish("never", &sign(&challenge), now).is_err());
    }

    #[test]
    fn one_daemon_holds_the_owner_role() {
        let d = tempfile::tempdir().unwrap();
        let first = owner(d.path());
        let second = Owner::claim(party(), d.path().join("owner"), first.writer.clone());
        assert!(second.is_err(), "a second daemon on the same owner directory");
        drop(first);
        // Free once the first is gone. A child another test forks may hold the descriptor for an
        // instant before its exec closes it, so the claim is tried for up to a second.
        let third = (0..20).find_map(|_| {
            Owner::claim(party(), d.path().join("owner"), owner_writer(d.path()))
                .map_err(|_| std::thread::sleep(std::time::Duration::from_millis(50)))
                .ok()
        });
        assert!(third.is_some(), "free once the first is gone");
    }

    #[test]
    fn an_undo_is_on_disk_before_it_is_acknowledged_and_a_restart_honours_it() {
        let d = tempfile::tempdir().unwrap();
        let o = owner(d.path());
        let receipt = Receipt {
            id: "r1".into(),
            intent: Intent::answer("t.yaml", "a", "h", "no", None),
            label: "No".into(),
            stage: Stage::Signed,
            signed_at: "t".into(),
            applies_at_ms: i64::MAX,
            commit: None,
            woke: None,
            landing: None,
            error: None,
        };
        o.book.put(receipt.clone());
        let a = crate::passkey::tests::sign("c");
        answers::persist(
            &o.dir,
            &answers::Pending { receipt, assertion: a, nonce: "n".into(), by: "o".into(), session: None },
        )
        .unwrap();
        o.undo("r1", 0).unwrap();
        assert!(answers::undone(&o.dir, "r1"));
        // The write, here or after a restart, finds the marker and writes nothing.
        let r = answers::write(&o.writer, &o.book.get("r1").unwrap(), &crate::passkey::tests::sign("c"), "n", "o");
        assert_eq!(r, Err(answers::UNDONE.to_string()));
    }

    #[test]
    fn codes_compare_in_constant_time_and_exactly() {
        assert!(constant_eq(b"ABCD", b"ABCD"));
        assert!(!constant_eq(b"ABCD", b"ABCE"));
        assert!(!constant_eq(b"ABC", b"ABCD"));
    }
}
