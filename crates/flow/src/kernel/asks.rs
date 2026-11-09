//! Human steps as Maestro asks (BRO-3635): [`ask`] files a human node's question through the asks
//! ledger's writer of record, links the row it filed, and takes the owner's answer from the
//! answers ledger as the node's receipt.
//!
//! # The verb
//!
//! `ask` is an agent write, like `claim`: only the instance's owner calls it, and only on a human
//! node that is `waiting` (every requirement met, no answer yet). It is called again until it says
//! `answered`; each call does the next step and says where the ask stands:
//!
//! 1. **Pin.** The first call mints an ask id (`flow-<node>-<12 hex>`), checked against every `id`
//!    and `uid` in the ledger's working tree and minted again on a collision, and builds the body:
//!    the agent's fields (whatever the validator of record requires: `class`, `gates`,
//!    `exhausted`, `blocking`, and so on), plus `id`, `ask` (the definition's question) and
//!    `asked_at`, read from the kernel's clock. It appends `AskPinned` with that exact body and
//!    the hash of its comparable part ([`comparable_hash`]) before anything is filed. `asked_at`
//!    is pinned here, before hashing, so a filing that is retried files the same bytes.
//! 2. **File.** With the flows lock released (the ask ledger's lock can wait 30 s), the pinned body
//!    goes to the writer of record through the agent path, `maestro_store::requests::file_ask`,
//!    which runs `ask_ledger.py file` and its validator. The flow crate writes no YAML and no
//!    answer. A busy ask-ledger lock (`FileError::Lock`) is retried by the kernel until its own
//!    deadline ([`ASK_BUSY_DEADLINE`], read from its clock), then refused `ask_refused_busy`: the
//!    ask stays pinned and the node stays `waiting` for the agent, not for the owner.
//! 3. **Confirm and link.** Whatever the writer said, the kernel reads the ledger's working tree
//!    (where the writer writes, never `origin/main`) for the row addressed by the pinned id. That
//!    id with that hash is linked (`AskLinked`): after a filing, and after a duplicate, which is a
//!    filing a crash left unlinked. That id with another hash is `ask_refused`, and nothing is
//!    linked. No row after a refusal by the writer means nothing was filed: the pin is withdrawn
//!    (`AskWithdrawn`, `ask_not_filed`) and the next call mints anew, with a body the agent can fix.
//!    No row otherwise, or a ledger that cannot be read, is `ask_unconfirmed`, and the next call
//!    files the same pin again.
//! 4. **Answer.** A linked ask's answer is read only from the answers ledger, the owner path:
//!    the asks ledger at `maestro/answers` (this clone's and origin's as last fetched, where
//!    Maestro's decisions land an answer) and at `origin/main` (where it lands). The working tree
//!    is not read for answers: any agent can write it. The verb takes no answer, and nothing an
//!    agent passes in stands in for one. The row is found by the pinned id, never by searching or
//!    listing the asks, and must hash as the linked body. Unanswered, the node stays `waiting`.
//!    Answered, the kernel appends `Answered`, the node's receipt, so it is done and its
//!    successors become ready.
//!
//! # The answer as outputs
//!
//! `Answered.outputs` holds the answered row's answer fields that are set, each as text:
//! `answer`, `answer_option`, `answer_note`, `answer_other`, `answered`, `resolution`, `outcome`,
//! `answered_at`, `defaulted_at`, `answered_by` and `status`. A guard downstream reads the owner's
//! choice as `.nodes.<node>.outputs.answer_option` (and the text as `.outputs.answer`).
//!
//! # Bounds
//!
//! The agent's body is at most [`MAX_ASK_BODY`] bytes as JSON, nested less than the log allows.
//! Each ledger read is one file, at most `maestro_store::LEDGER_LIMIT` (1 MiB), through the
//! store's YAML reader; the answers are read from at most [`ANSWER_SOURCES`] revisions. An answer's
//! outputs are at most [`MAX_OUTPUTS`](super::MAX_OUTPUTS) bytes. Minting tries [`MINT_TRIES`] ids.
//!
//! Adoption and retirement (ADR decision 6) are not here.

use super::error::{KernelError, Refusal};
use super::events::{Answered, AskLinked, AskPinned, AskWithdrawn, KernelEvent, Started};
use super::ir::{Class, Iteration};
use super::replay::{AskRec, State, ask_of, next_of, pinned};
use super::write::{MAX_OUTPUTS, check_owner, wall};
use crate::clock;
use crate::ids::InstanceId;
use crate::ledger::{Appended, Ledger, random_epoch};
use crate::log::{Log, MAX_NESTING, nesting};
use jiff::SignedDuration;
use maestro_core::ask::{Ask, AskState, OUTCOME_KEYS, entry_hash, text};
use maestro_core::authority::Principal;
use maestro_core::js::JsValue;
use maestro_core::session::VerifiedSession;
use maestro_store::locks::LockError;
use maestro_store::requests::{self, FileError};
use maestro_store::{ASKS, Paths, Store, TreeSource};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

/// The most bytes of ask body an agent gives, as JSON.
pub const MAX_ASK_BODY: usize = 64 << 10;

/// How long the kernel keeps retrying a filing the ask ledger's lock refuses, from its first try.
/// Each try may itself wait 30 s on the lock (`requests::FILE_WAIT`).
pub const ASK_BUSY_DEADLINE: SignedDuration = SignedDuration::from_secs(90);

/// The pause between tries on a busy ask ledger.
const ASK_RETRY_PAUSE: Duration = Duration::from_secs(1);

/// How many ids a mint tries before it refuses `ask_id_exhausted`.
pub const MINT_TRIES: u32 = 8;

/// Where an answer is read, in order, as `(ref, label)`: the owner path. Maestro's answers branch
/// (this clone's, then origin's as last fetched; `maestro_store::views::ANSWERS_REFS`) and origin's
/// main, where answers land. Never the working tree.
pub const ANSWER_SOURCES: [(&str, &str); 3] = [
    maestro_store::views::ANSWERS_REFS[0],
    maestro_store::views::ANSWERS_REFS[1],
    ("refs/remotes/origin/main", "origin/main"),
];

/// The answer fields an `Answered` carries as outputs, when set.
const ANSWER_FIELDS: [&str; 5] = ["answer_option", "answer_note", "answer_other", "answered_by", "status"];

/// Keys an agent's body may not hold: the kernel writes `id`, `ask` and `asked_at`, a `uid` would
/// be a second handle the kernel did not mint, and the rest are the owner's or the writer's.
const KERNEL_KEYS: [&str; 4] = ["id", "uid", "ask", "asked_at"];

/// What the agent gives for a node's first ask: the ledger it goes to, and the body's fields.
#[derive(Debug, Clone, PartialEq)]
pub struct Filing {
    pub ledger: String,
    pub body: Map<String, Value>,
}

/// What [`ask`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum Asked {
    /// The ask is filed and linked, and the answers ledger holds no answer to it yet. `appended`
    /// is the `AskLinked` this call wrote, if it linked it.
    Waiting { ask_id: String, ledger: String, appended: Option<Appended> },
    /// The owner's answer, appended as the node's receipt.
    Answered { ask_id: String, ledger: String, outputs: Value, source: String, appended: Appended },
    /// The writer of record refused the pinned ask and the working tree has no row of its id:
    /// nothing was filed, and the pin is withdrawn (`ask_not_filed`). The next call mints anew.
    Withdrawn { ask_id: String, ledger: String, code: &'static str, reason: String, appended: Appended },
}

/// The code an [`Asked::Withdrawn`] carries.
pub const ASK_NOT_FILED: &str = "ask_not_filed";

/// Files, links or takes the answer of human node `node` of `instance`, owned by `session`. See
/// the module docs. `filing` is used only when the node has no pinned ask; later calls ignore it.
pub fn ask(
    ledger: &mut Ledger,
    paths: &Paths,
    session: &VerifiedSession,
    instance: &InstanceId,
    node: &str,
    filing: Option<&Filing>,
) -> Result<Asked, KernelError> {
    let seen = look(ledger, session, instance, node)?;
    let rec = match seen.rec.clone() {
        Some(rec) => rec,
        None => pin(ledger, paths, instance, node, &seen, filing)?,
    };
    if !rec.linked {
        return file_and_link(ledger, paths, session, instance, node, rec);
    }
    take_answer(ledger, paths, session, instance, node, rec)
}

/// The node as the log has it, read under the flows lock, which is released on return.
struct Seen {
    seq: u64,
    question: String,
    rec: Option<AskRec>,
}

/// The node is a human node of `started` and waiting now; its iteration and inputs.
fn waiting(log: &Log, started: &Started, node: &str) -> Result<(Iteration, BTreeMap<String, String>), KernelError> {
    let Some(n) = started.ir.nodes.get(node) else {
        return Err(Refusal::UnknownNode { node: node.to_string() }.into());
    };
    if !matches!(n.class, Class::Human { .. }) {
        return Err(
            Refusal::WrongVerb { node: node.to_string(), class: n.class.name().to_string(), verb: "claim" }.into()
        );
    }
    let next = next_of(log, started, wall(log)?)?;
    match next.pending.get(node) {
        Some(s) if s.state == State::Waiting => Ok((s.iteration.clone(), s.inputs.clone())),
        other => {
            let state = other.and_then(|s| serde_json::to_value(s.state).ok());
            let state = state.and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "done".into());
            Err(Refusal::NotEnabled { node: node.to_string(), state }.into())
        }
    }
}

fn look(
    ledger: &mut Ledger,
    session: &VerifiedSession,
    instance: &InstanceId,
    node: &str,
) -> Result<Seen, KernelError> {
    let guard = ledger.lock()?;
    let log = guard.log(instance)?;
    let started = pinned(&log)?;
    check_owner(&started, instance, session)?;
    waiting(&log, &started, node)?;
    let question = match started.ir.nodes.get(node).map(|n| &n.class) {
        Some(Class::Human { ask }) => ask.clone(),
        _ => String::new(),
    };
    Ok(Seen { seq: log.seq(), question, rec: ask_of(&log, &started, node)? })
}

/// Appends `event` under the flows lock if the node's ask is still `expect` and the node still
/// waiting; `Ok(None)` when another call of the owner's linked it meanwhile. Anything else is
/// `ask_changed`.
fn append_if(
    ledger: &mut Ledger,
    session: &VerifiedSession,
    instance: &InstanceId,
    node: &str,
    expect: &AskRec,
    event: impl FnOnce(&Iteration, &BTreeMap<String, String>) -> Result<crate::NewEvent, KernelError>,
) -> Result<Option<Appended>, KernelError> {
    let mut guard = ledger.lock()?;
    let log = guard.log(instance)?;
    let started = pinned(&log)?;
    check_owner(&started, instance, session)?;
    let (iteration, inputs) = waiting(&log, &started, node)?;
    match ask_of(&log, &started, node)? {
        Some(now) if now == *expect => {}
        Some(now) if now.ask_id == expect.ask_id && now.linked && !expect.linked => return Ok(None),
        _ => {
            return Err(Refusal::AskChanged { node: node.to_string() }.into());
        }
    }
    let ev = event(&iteration, &inputs)?;
    Ok(Some(guard.append(instance, log.seq(), ev)?))
}

/// The hash a row is compared by: the plugin's entry hash of the row without the keys a filer
/// never writes (`requests::refused_key`: the `origin` the writer of record adds, and every
/// answer, status and archive key the owner's path adds), so the row the kernel filed hashes as
/// its pinned body before and after the writer stamps it and the owner answers it.
pub fn comparable_hash(row: &JsValue) -> String {
    let mut row = row.clone();
    if let Some(map) = row.as_object_mut() {
        map.retain(|k, _| !requests::refused_key(k));
    }
    entry_hash(&row)
}

fn invalid(detail: impl Into<String>) -> KernelError {
    Refusal::AskBodyInvalid { detail: detail.into() }.into()
}

/// Mints an id, appends `AskPinned`, and returns the pin. The flows lock is not held while the
/// ledger is read; the pin is appended only if the log is still at `seen.seq`.
fn pin(
    ledger: &mut Ledger,
    paths: &Paths,
    instance: &InstanceId,
    node: &str,
    seen: &Seen,
    filing: Option<&Filing>,
) -> Result<AskRec, KernelError> {
    let Some(filing) = filing else {
        return Err(invalid("a node's first ask needs a ledger and an ask body"));
    };
    if !requests::ledger_name_ok(&filing.ledger) {
        return Err(invalid(format!("`{}` is not a ledger name (a plain `*.yaml` file name)", filing.ledger)));
    }
    if let Some(key) = filing.body.keys().find(|k| KERNEL_KEYS.contains(&k.as_str()) || requests::refused_key(k)) {
        return Err(invalid(format!(
            "`{key}` is not the agent's: the kernel writes id, ask and asked_at, and the owner's path the rest"
        )));
    }
    let given = Value::Object(filing.body.clone());
    let size = serde_json::to_vec(&given).map_or(usize::MAX, |b| b.len());
    if size > MAX_ASK_BODY {
        return Err(invalid(format!("the body is {size} bytes, more than {MAX_ASK_BODY}")));
    }
    // `AskPinned` carries the body two levels inside its event body.
    if nesting(&given) + 2 >= MAX_NESTING {
        return Err(invalid("the body nests too deep"));
    }
    let taken = taken(paths, &filing.ledger)?;
    let ask_id = mint(node, &taken, &filing.ledger)?;
    let mut body = Map::new();
    body.insert("id".into(), Value::String(ask_id.clone()));
    body.insert("ask".into(), Value::String(seen.question.clone()));
    // Pinned before hashing: every filing of this pin files these bytes.
    body.insert("asked_at".into(), Value::String(clock::now().strftime("%Y-%m-%dT%H:%M:%SZ").to_string()));
    body.extend(filing.body.clone());
    let body = requests::compose_ask(&body).map_err(|e| invalid(e.to_string()))?;
    let body_hash = comparable_hash(&JsValue::from(Value::Object(body.clone())));
    let pin = AskPinned {
        node: node.to_string(),
        iteration: Iteration::new(),
        ask_id: ask_id.clone(),
        ledger: filing.ledger.clone(),
        body: Value::Object(body),
        body_hash,
    };
    let mut guard = ledger.lock()?;
    let log = guard.log(instance)?;
    let started = pinned(&log)?;
    let (iteration, _) = waiting(&log, &started, node)?;
    let pin = AskPinned { iteration, ..pin };
    // Nothing appended since `look`, or the pin is not appended (`stale_seq`): the only concurrency rule.
    guard.append(instance, seen.seq, pin.to_event()?)?;
    Ok(AskRec { ask_id: pin.ask_id, ledger: pin.ledger, body: pin.body, body_hash: pin.body_hash, linked: false })
}

/// Every `id` and `uid` in the ledger's working tree, trimmed as the plugin addresses them.
fn taken(paths: &Paths, ledger: &str) -> Result<BTreeSet<String>, KernelError> {
    let unread = |detail: String| Refusal::AskLedgerUnread { ledger: ledger.to_string(), detail };
    let tree = Store::new(paths.clone(), TreeSource::WorkingTree);
    let Some(l) = tree.ask_ledger(ledger).map_err(unread)? else {
        return Err(unread("there is no such ledger in the asks folder".into()).into());
    };
    Ok(l.asks.into_iter().flat_map(|a| [Some(a.id), a.uid]).flatten().collect())
}

/// A fresh id: `flow-<node>-<12 hex>`, none that `taken` holds.
fn mint(node: &str, taken: &BTreeSet<String>, ledger: &str) -> Result<String, KernelError> {
    let node: String = node.chars().take(40).collect();
    for _ in 0..MINT_TRIES {
        let id = format!("flow-{node}-{}", suffix()?);
        if !taken.contains(&id) {
            return Ok(id);
        }
    }
    Err(Refusal::AskIdExhausted { ledger: ledger.to_string(), tries: MINT_TRIES }.into())
}

fn suffix() -> Result<String, KernelError> {
    #[cfg(test)]
    if let Some(s) = seams::SUFFIXES.with(|q| q.borrow_mut().pop_front()) {
        return Ok(s);
    }
    Ok(random_epoch()?[..12].to_string())
}

/// What the ledger holds at a pinned id.
enum Found {
    None,
    Row(Box<Ask>),
    /// More than one row is addressed by it.
    Ambiguous,
}

/// The row of `ledger` addressed by `id` (its `id`, or a `uid`), read from `store` alone: the
/// one ledger file, never a listing of the asks.
fn find(store: &Store, ledger: &str, id: &str) -> Result<Found, String> {
    let Some(l) = store.ask_ledger(ledger)? else { return Ok(Found::None) };
    let mut rows = l.asks.into_iter().filter(|a| a.id == id || a.uid.as_deref() == Some(id));
    Ok(match (rows.next(), rows.next()) {
        (None, _) => Found::None,
        (Some(a), None) => Found::Row(Box::new(a)),
        _ => Found::Ambiguous,
    })
}

/// Hands the pinned body to the writer of record, through the agent path. A busy ask-ledger lock
/// is retried until [`ASK_BUSY_DEADLINE`] after the first try, by the kernel's clock, then refused
/// `ask_refused_busy`. Any other outcome is returned for the caller to confirm.
fn file_pinned(
    paths: &Paths,
    dir: &Path,
    rec: &AskRec,
    body: &Map<String, Value>,
    session: &VerifiedSession,
) -> Result<Result<requests::Filed, FileError>, KernelError> {
    let caller = Principal::Agent { session: session.session().clone() };
    let deadline = clock::now().checked_add(ASK_BUSY_DEADLINE).map_err(|_| Refusal::ClockOutOfRange)?;
    loop {
        match requests::file_ask(paths, dir, &rec.ledger, body, session.session(), &caller) {
            Err(FileError::Lock(LockError::Busy { holder, .. })) => {
                if clock::now() >= deadline {
                    return Err(Refusal::AskRefusedBusy { ledger: rec.ledger.clone(), holder }.into());
                }
                std::thread::sleep(ASK_RETRY_PAUSE);
            }
            other => return Ok(other),
        }
    }
}

/// Files the pinned ask (flows lock released), confirms it in the working tree, and links it.
fn file_and_link(
    ledger: &mut Ledger,
    paths: &Paths,
    session: &VerifiedSession,
    instance: &InstanceId,
    node: &str,
    rec: AskRec,
) -> Result<Asked, KernelError> {
    let unconfirmed =
        |detail: String| Refusal::AskUnconfirmed { ledger: rec.ledger.clone(), ask_id: rec.ask_id.clone(), detail };
    // The working tree: where the writer of record writes, and where a filing is confirmed.
    let here = Store::new(paths.clone(), TreeSource::WorkingTree);
    let dir = here.rel(ASKS).map_err(|e| unconfirmed(format!("no asks folder: {e}")))?;
    let dir = here.paths.workspace.join(dir);
    let Some(body) = rec.body.as_object() else {
        return Err(unconfirmed("the pinned body is not an object".into()).into());
    };
    // No flows guard is alive here: `look`, `pin` and `append_if` each drop theirs on return.
    let outcome = file_pinned(&here.paths, &dir, &rec, body, session)?;
    #[cfg(test)]
    if seams::CRASH_AFTER_FILE.with(|c| c.replace(false)) {
        return Err(KernelError::Encode("a crash after filing (test)".into()));
    }
    // Confirm whatever the writer said: a filing, a duplicate (a filing a crash left unlinked), or
    // a failure. Only the working tree's row, by the pinned id and hash, links.
    let row = match find(&here, &rec.ledger, &rec.ask_id).map_err(unconfirmed)? {
        Found::Row(row) => row,
        Found::Ambiguous => {
            let at = format!("worktree:{}", rec.ledger);
            return Err(Refusal::AskAmbiguous { ledger: rec.ledger.clone(), ask_id: rec.ask_id.clone(), at }.into());
        }
        Found::None => match outcome {
            // Refused, and no row: nothing was filed. The pin is withdrawn.
            Err(FileError::Refused(reason)) => {
                let withdrawn = AskWithdrawn {
                    node: node.to_string(),
                    iteration: Iteration::new(),
                    ask_id: rec.ask_id.clone(),
                    code: ASK_NOT_FILED.into(),
                    reason: reason.clone(),
                };
                let appended = append_if(ledger, session, instance, node, &rec, |iteration, _| {
                    AskWithdrawn { iteration: iteration.clone(), ..withdrawn }.to_event()
                })?;
                let Some(appended) = appended else {
                    return Ok(Asked::Waiting { ask_id: rec.ask_id, ledger: rec.ledger, appended: None });
                };
                return Ok(Asked::Withdrawn {
                    ask_id: rec.ask_id,
                    ledger: rec.ledger,
                    code: ASK_NOT_FILED,
                    reason,
                    appended,
                });
            }
            Ok(_) => {
                return Err(unconfirmed("the writer filed it, and the working tree has no row of its id".into()).into());
            }
            Err(e) => return Err(unconfirmed(e.to_string()).into()),
        },
    };
    let found = comparable_hash(&row.row);
    if found != rec.body_hash {
        return Err(Refusal::AskRefused {
            ledger: rec.ledger.clone(),
            ask_id: rec.ask_id.clone(),
            pinned: rec.body_hash.clone(),
            found,
        }
        .into());
    }
    let linked = AskLinked {
        node: node.to_string(),
        iteration: Iteration::new(),
        ask_id: rec.ask_id.clone(),
        ledger: rec.ledger.clone(),
        body_hash: rec.body_hash.clone(),
    };
    let appended = append_if(ledger, session, instance, node, &rec, |iteration, _| {
        AskLinked { iteration: iteration.clone(), ..linked }.to_event()
    })?;
    Ok(Asked::Waiting { ask_id: rec.ask_id, ledger: rec.ledger, appended })
}

/// The answered row of a linked ask, from the first answers source that has it answered.
fn read_answer(paths: &Paths, rec: &AskRec) -> Result<Option<(Box<Ask>, &'static str)>, KernelError> {
    let mut unread: Option<(String, String)> = None;
    for (rev, label) in ANSWER_SOURCES {
        let at = Store::new(paths.clone(), TreeSource::GitRev(rev.to_string()));
        if let Some(reason) = &at.tree.unresolved {
            // A ref that is not there holds no answer; one there that does not resolve is unread.
            match at.tree.has_ref(rev) {
                Ok(false) => {}
                Ok(true) => unread = unread.or(Some((label.to_string(), reason.clone()))),
                Err(e) => unread = unread.or(Some((label.to_string(), e))),
            }
            continue;
        }
        match find(&at, &rec.ledger, &rec.ask_id) {
            Err(e) => unread = unread.or(Some((label.to_string(), e))),
            Ok(Found::None) => {}
            Ok(Found::Ambiguous) => {
                let at = format!("{label}:{}", rec.ledger);
                return Err(Refusal::AskAmbiguous { ledger: rec.ledger.clone(), ask_id: rec.ask_id.clone(), at }.into());
            }
            Ok(Found::Row(row)) => {
                if row.state() == AskState::Answered {
                    return Ok(Some((row, label)));
                }
            }
        }
    }
    match unread {
        Some((at, detail)) => Err(Refusal::AnswersUnread { at, detail }.into()),
        None => Ok(None),
    }
}

/// An answered row's answer fields that are set, as text (see the module docs).
fn outputs_of(row: &Ask) -> Map<String, Value> {
    OUTCOME_KEYS
        .iter()
        .chain(ANSWER_FIELDS.iter())
        .filter_map(|k| text(row.row.get(k)).map(|v| (k.to_string(), Value::String(v))))
        .collect()
}

/// Reads the linked ask's answer from the owner path and, when there is one, appends `Answered`.
fn take_answer(
    ledger: &mut Ledger,
    paths: &Paths,
    session: &VerifiedSession,
    instance: &InstanceId,
    node: &str,
    rec: AskRec,
) -> Result<Asked, KernelError> {
    let Some((row, source)) = read_answer(paths, &rec)? else {
        return Ok(Asked::Waiting { ask_id: rec.ask_id, ledger: rec.ledger, appended: None });
    };
    let found = comparable_hash(&row.row);
    if found != rec.body_hash {
        return Err(Refusal::AnswerMismatch {
            ask_id: rec.ask_id.clone(),
            at: source.to_string(),
            linked: rec.body_hash.clone(),
            found,
        }
        .into());
    }
    let outputs = Value::Object(outputs_of(&row));
    let size = serde_json::to_vec(&outputs).map_or(usize::MAX, |b| b.len());
    if size > MAX_OUTPUTS {
        return Err(Refusal::AnswerTooLarge { ask_id: rec.ask_id.clone(), size, limit: MAX_OUTPUTS }.into());
    }
    let answered = |iteration: &Iteration, inputs: &BTreeMap<String, String>| Answered {
        node: node.to_string(),
        iteration: iteration.clone(),
        ask_id: rec.ask_id.clone(),
        ledger: rec.ledger.clone(),
        inputs: inputs.clone(),
        outputs: outputs.clone(),
        source: source.to_string(),
        row_hash: row.hash(),
    };
    let appended =
        append_if(ledger, session, instance, node, &rec, |iteration, inputs| answered(iteration, inputs).to_event())?;
    let Some(appended) = appended else {
        return Err(Refusal::AskChanged { node: node.to_string() }.into());
    };
    Ok(Asked::Answered { ask_id: rec.ask_id, ledger: rec.ledger, outputs, source: source.to_string(), appended })
}

/// Test seams, in this crate's unit tests only: the ids a mint draws, and a crash right after a
/// filing (before it is confirmed and linked).
#[cfg(test)]
pub(crate) mod seams {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    thread_local! {
        pub(crate) static SUFFIXES: RefCell<VecDeque<String>> = const { RefCell::new(VecDeque::new()) };
        pub(crate) static CRASH_AFTER_FILE: Cell<bool> = const { Cell::new(false) };
    }
}

#[cfg(test)]
#[path = "asked.rs"]
mod asked;
