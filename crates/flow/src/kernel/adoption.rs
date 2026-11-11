//! Owner-approved adoption and retirement (BRO-3635; ADR decisions 6 and 7):
//! [`propose_adoption`] and [`propose_retirement`] put a change of an instance's owner, or its
//! retirement, to the owner as a Maestro ask, and the kernel applies it only once the owner's
//! approval has landed on main of the answers remote.
//!
//! # The verbs
//!
//! Adoption and retirement change who owns a flow, so they are owner decisions. The agent verbs
//! only propose them, and carry out what the owner approved: their names say so (`propose_`, never
//! `adopt` or `retire`, and none of the owner words the authority suites check). Neither takes an
//! answer, and neither takes an ask id from the caller: the proposal's ask is the one pinned in
//! the instance's log. Each is called again until it says where the proposal ended; each call does
//! the next step and reports where it stands, as `asks::ask` does for a human node:
//!
//! 1. **Pin.** The first call takes the agent's filing (a ledger and the fields the ledger's
//!    validator requires, as for `ask`), mints an id (`flow-adopt-<12 hex>`,
//!    `flow-retire-<12 hex>`) against the ledger's working tree, and builds the body: the
//!    agent's fields, plus the kernel's `id`, `asked_at` (the kernel's clock), `headline`, `ask`,
//!    `options`, `default` (`keep`) and `preanswerable` (false), none of which the agent may give.
//!    The question names the instance and its flow, the owner and the proposing session (each as
//!    its `SessionRef` and the digest of its owner marker, never the marker), and the progress so
//!    far: the nodes done and the next ready ones. It appends `ProposalPinned` with that body,
//!    the proposing session (`by`) and the owner as the log has it now (`owner`), at the sequence
//!    the log was read at.
//! 2. **File, confirm and link.** With the flows lock released, the pinned body goes to the writer
//!    of record (`asks::file_and_confirm`, the very path a human node's ask takes), and the row is
//!    confirmed by its pinned id and hash in the working tree: `ProposalLinked`, or
//!    `ProposalWithdrawn` when the writer refused it and no row has its id, or a refusal
//!    (`ask_refused`, `ask_unconfirmed`, `ask_refused_busy`, `ask_ambiguous`) that records nothing.
//! 3. **Answer.** A linked proposal's answer is read only as a human node's is (`asks::landed`):
//!    on main of the answers remote compiled into Maestro, at the commit the remote names, object
//!    by object; never from the working tree, a local ref or the caller. Unanswered, it is
//!    `waiting` (with `answer_waiting_to_land` when the answer is on Maestro's answers branch and
//!    not on main). An answers source that does not read is `answers_unread`: unread is not
//!    unanswered, and nothing changes.
//! 4. **Act.** The owner approves by choosing the proposal's approve option, `adopt` or `retire`,
//!    exactly ([`Proposal::approve_option`], as the plugin reads `answer_option`). Any other
//!    answer (`keep`, another option, an answer with no option, a default) is `declined`, and
//!    nothing is recorded; called again with a filing, the session proposes anew. Approved, the
//!    kernel checks under the lock that the proposal still stands, linked, from the same owner:
//!    an instance that changed owner since it was pinned refuses `proposal_stale` (the reads say
//!    `stale`), and called with a filing the session proposes anew. Then:
//!    - **Adoption** appends `Adopted { from, to, approval }`, with the answer's evidence (the ask,
//!      the option, the revision read and the row's hash). From there on only `to` writes the
//!      instance (`write::check_owner`). Completed receipts stand. Claims taken before it are not
//!      transferred: the former owner is refused every write (`not_owner`), the new owner is
//!      refused the former owner's tokens (`former_owners_claim`), and a claim whose lease still
//!      runs holds its node (`already_claimed`) until it ends; the new owner then claims the
//!      next attempt and goes on from the next ready step.
//!    - **Retirement** appends `Retired { by, approval }` and then, under the same lock, moves the
//!      instance to `retired/<id>` as [`operator::retire`](crate::operator::retire) does, its log
//!      kept and never reopened: every later read or write naming it is refused `retired`. A crash
//!      between the two leaves a log ending in `Retired`, which takes no write (`retired`); the
//!      next `propose_retirement` call finishes the move.
//!
//! # Who proposes
//!
//! A verified session with an owner marker, and only that: an unverified caller is refused at the
//! surface, a session with no marker `no_owner_marker`. **Adoption** is proposed by a session
//! that does not own the instance (`already_owner` otherwise); the owner's approval names it as the
//! new owner. **Retirement** may be proposed by any such session, the owner included: an abandoned
//! instance's owner is usually gone, so the proposal comes from another session, and the owner's
//! approval is the gate either way. An instance with no recorded owner takes no proposal
//! (`no_owner`); an operator retires it.
//!
//! # Where the pins live
//!
//! In the instance's own log, beside the events they concern, and inert: `ProposalPinned`,
//! `ProposalLinked` and `ProposalWithdrawn` change the proposal alone. The owner, the claims, the
//! receipts and what is ready are what they would be without them ([`owners`] reads ownership from
//! `Started` and accepted `Adopted` events only, and `replay` skips the proposal events). So
//! without the owner's approval nothing changes, while the log still records, hash-chained, what
//! was proposed and filed. Each session has at most one standing proposal of each kind on an
//! instance; a new one replaces a linked one (the verb pins anew only after reading a decline)
//! or a stale one. The flow writes no other file: the ask goes through the asks ledger's writer of
//! record, and the flows lock is never held while it is filed.
//!
//! # Replay
//!
//! [`owners`] folds the log in order, and accepts each proposal event only if it fits what came
//! before: a pin from a session with a marker, naming the owner as the log has it (and for an
//! adoption, a session other than the owner); a link of a standing pin with its hash; a withdrawal
//! of a standing unlinked pin. `Adopted` counts only for a standing linked adoption proposal with
//! its ask id, proposed by `to`, naming `from` as the owner, `from` being the owner still, and the
//! approve option; `Retired` likewise for a retirement proposal. Anything else is set aside as
//! rejected and changes nothing, and nothing counts after `Retired`. `status` and `next` report the
//! current owner and each standing proposal (its kind, ask, ledger, proposer and state), never an
//! answer.

use super::asks::{self, Answer, Confirmed, Filing};
use super::error::{KernelError, Refusal};
use super::events::{
    Adopted, Approval, KernelEvent, Owner, Proposal, ProposalLinked, ProposalPinned, ProposalWithdrawn, Retired,
    Started,
};
use super::replay::{AskRec, Rejected, next_of, pinned};
use super::write::{owner, wall};
use crate::clock;
use crate::error::FlowError;
use crate::ids::InstanceId;
use crate::ledger::{Appended, Ledger, WriteGuard};
use crate::log::{Event, Log};
use maestro_core::ask::text;
use maestro_core::js::JsValue;
use maestro_core::session::{SessionRef, VerifiedSession};
use maestro_store::Paths;
use maestro_store::requests;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

/// Keys of a proposal's body that are the kernel's: the agent's filing may not hold them.
pub const PROPOSAL_KEYS: [&str; 8] =
    ["id", "uid", "ask", "asked_at", "headline", "options", "default", "preanswerable"];

/// The option that leaves everything as it is: the proposal's default.
pub const KEEP: &str = "keep";

/// The code a [`Proposed::Withdrawn`] carries.
pub const PROPOSAL_NOT_FILED: &str = "proposal_not_filed";

/// The most node ids the question names in each list of its progress.
const PROGRESS_NAMES: usize = 20;

/// What a proposal call did.
#[derive(Debug, Clone, PartialEq)]
pub enum Proposed {
    /// Filed and linked, with no answer on main yet. `appended` is the `ProposalLinked` this call
    /// wrote, if it did; `landing` names the answers branch an answer waits on, if one does.
    Waiting { proposal: Proposal, ask_id: String, ledger: String, appended: Option<Appended>, landing: Option<String> },
    /// The writer of record refused the pinned proposal and no row has its id: nothing was filed,
    /// and the pin is withdrawn. The next call, with a filing, proposes anew.
    Withdrawn {
        proposal: Proposal,
        ask_id: String,
        ledger: String,
        code: &'static str,
        reason: String,
        appended: Appended,
    },
    /// The owner answered on main, and not with the approve option: nothing changed.
    Declined { proposal: Proposal, ask_id: String, ledger: String, answer_option: Option<String>, source: String },
    /// The owner approved: the instance is the proposing session's.
    Adopted { ask_id: String, ledger: String, from: Owner, to: Owner, source: String, appended: Appended },
    /// The owner approved: the instance is retired, its log kept under `retired/`. `appended` is
    /// the `Retired` this call wrote; none when it finished a move a crash left undone.
    Retired { ask_id: String, ledger: String, source: Option<String>, appended: Option<Appended> },
}

/// A standing proposal, as the log has it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProposalRec {
    pub proposal: Proposal,
    pub by: Owner,
    /// The owner when it was pinned.
    pub owner: Owner,
    pub ask: AskRec,
}

/// Who owns an instance, and what is proposed for it, as its log has it (see the module docs).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Owners {
    /// `Started`'s owner, or the `to` of the latest accepted `Adopted`.
    pub owner: Option<Owner>,
    /// The position of the latest accepted `Adopted`; 0 when there is none. A claim taken before
    /// it is the former owner's.
    pub since: u64,
    pub proposals: Vec<ProposalRec>,
    /// The ask id of an accepted `Retired`: the instance takes no write.
    pub retired: Option<String>,
    pub rejected: Vec<Rejected>,
}

/// The event kinds [`owners`] reads, which replay's node fold skips.
pub(crate) const KINDS: [&str; 5] =
    [ProposalPinned::KIND, ProposalLinked::KIND, ProposalWithdrawn::KIND, Adopted::KIND, Retired::KIND];

fn body<T: DeserializeOwned>(log: &Log, e: &Event) -> Result<T, KernelError> {
    serde_json::from_value(e.body.clone()).map_err(|err| KernelError::Malformed {
        instance: log.instance.to_string(),
        seq: e.seq,
        detail: format!("a {} body that does not parse ({err})", e.kind),
    })
}

/// Who owns the instance of `log` and what is proposed for it. Reads events only; runs nothing.
pub(crate) fn owners(log: &Log, started: &Started) -> Result<Owners, KernelError> {
    let mut o =
        Owners { owner: started.owner.clone(), since: 0, proposals: Vec::new(), retired: None, rejected: Vec::new() };
    for e in log.events.iter().skip(1).filter(|e| KINDS.contains(&e.kind.as_str())) {
        if let Err(reason) = o.accept(log, e)? {
            let subject = e.body.get("ask_id").or_else(|| e.body.get("approval").and_then(|a| a.get("ask_id")));
            let subject = subject.and_then(Value::as_str).unwrap_or_default().to_string();
            o.rejected.push(Rejected { seq: e.seq, kind: e.kind.clone(), subject, reason: reason.into() });
        }
    }
    Ok(o)
}

impl Owners {
    fn accept(&mut self, log: &Log, e: &Event) -> Result<Result<(), &'static str>, KernelError> {
        if self.retired.is_some() {
            return Ok(Err("retired"));
        }
        match e.kind.as_str() {
            ProposalPinned::KIND => {
                let p: ProposalPinned = body(log, e)?;
                if self.owner.as_ref() != Some(&p.owner) {
                    return Ok(Err("not_the_owner_named"));
                }
                if p.proposal == Proposal::Adoption && p.by == p.owner {
                    return Ok(Err("already_owner"));
                }
                if self.proposals.iter().any(|r| r.ask.ask_id == p.ask_id) {
                    return Ok(Err("already_pinned"));
                }
                // One standing proposal of each kind per session; a new one replaces one that was
                // linked, or that names an owner since replaced.
                if let Some(i) = self.proposals.iter().position(|r| r.proposal == p.proposal && r.by == p.by) {
                    if !self.proposals[i].ask.linked && self.proposals[i].owner == p.owner {
                        return Ok(Err("already_pinned"));
                    }
                    self.proposals.remove(i);
                }
                let ask =
                    AskRec { ask_id: p.ask_id, ledger: p.ledger, body: p.body, body_hash: p.body_hash, linked: false };
                self.proposals.push(ProposalRec { proposal: p.proposal, by: p.by, owner: p.owner, ask });
            }
            ProposalLinked::KIND => {
                let l: ProposalLinked = body(log, e)?;
                match self.proposals.iter_mut().find(|r| r.ask.ask_id == l.ask_id) {
                    Some(r) if !r.ask.linked && r.ask.body_hash == l.body_hash => r.ask.linked = true,
                    _ => return Ok(Err("not_pinned")),
                }
            }
            ProposalWithdrawn::KIND => {
                let w: ProposalWithdrawn = body(log, e)?;
                let Some(i) = self.proposals.iter().position(|r| !r.ask.linked && r.ask.ask_id == w.ask_id) else {
                    return Ok(Err("not_pinned"));
                };
                self.proposals.remove(i);
            }
            Adopted::KIND => {
                let a: Adopted = body(log, e)?;
                let i = match self.approved(Proposal::Adoption, &a.approval, &a.to) {
                    Ok(i) => i,
                    Err(reason) => return Ok(Err(reason)),
                };
                if self.proposals[i].owner != a.from {
                    return Ok(Err("not_the_owner_named"));
                }
                self.proposals.remove(i);
                self.owner = Some(a.to);
                self.since = e.seq;
            }
            _ => {
                let r: Retired = body(log, e)?;
                if let Err(reason) = self.approved(Proposal::Retirement, &r.approval, &r.by) {
                    return Ok(Err(reason));
                }
                self.retired = Some(r.approval.ask_id);
            }
        }
        Ok(Ok(()))
    }

    /// The standing linked proposal of `kind` by `by` that `approval` answers with the approve
    /// option, while the owner it named still owns the instance.
    fn approved(&self, kind: Proposal, approval: &Approval, by: &Owner) -> Result<usize, &'static str> {
        let Some(i) = self.proposals.iter().position(|r| r.ask.ask_id == approval.ask_id && r.proposal == kind) else {
            return Err("not_proposed");
        };
        let r = &self.proposals[i];
        if !r.ask.linked {
            return Err("not_linked");
        }
        if r.by != *by {
            return Err("not_the_proposer");
        }
        if self.owner.as_ref() != Some(&r.owner) {
            return Err("owner_changed");
        }
        if approval.answer_option != kind.approve_option() {
            return Err("not_approved");
        }
        Ok(i)
    }
}

/// A standing proposal, as `status` and `next` report it: no body and no answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProposalStatus {
    pub proposal: Proposal,
    pub ask_id: String,
    pub ledger: String,
    /// The session that proposes it.
    pub by: SessionRef,
    /// `unfiled` until it is linked, then `owner` (the owner's answer, once it lands on main);
    /// `stale` once the instance has changed owner since it was pinned, so no answer to it
    /// applies (the proposer proposes anew). The reads make `owner` `answer_waiting_to_land` when
    /// the answer waits on Maestro's answers branch ([`landing`]).
    pub state: &'static str,
}

impl Owners {
    /// The standing proposals, for the reads.
    pub(crate) fn report(&self) -> Vec<ProposalStatus> {
        self.proposals
            .iter()
            .map(|r| ProposalStatus {
                proposal: r.proposal,
                ask_id: r.ask.ask_id.clone(),
                ledger: r.ask.ledger.clone(),
                by: r.by.session.clone(),
                state: match (self.owner.as_ref() == Some(&r.owner), r.ask.linked) {
                    (false, _) => "stale",
                    (true, true) => "owner",
                    (true, false) => "unfiled",
                },
            })
            .collect()
    }
}

/// For the reads (`next`, `status`): each linked proposal whose answer waits to land on Maestro's
/// answers branch becomes `answer_waiting_to_land`. Reads git only; writes nothing.
pub fn landing(paths: &Paths, proposals: &mut [ProposalStatus]) {
    for p in proposals.iter_mut().filter(|p| p.state == "owner") {
        if asks::waiting_to_land(paths, &p.ledger, &p.ask_id).is_some() {
            p.state = "answer_waiting_to_land";
        }
    }
}

/// Proposes that the owner hand `instance` to `session`, and carries out the owner's answer. See
/// the module docs. `filing` is used only when a proposal is to be pinned; later calls ignore it.
pub fn propose_adoption(
    ledger: &mut Ledger,
    paths: &Paths,
    session: &VerifiedSession,
    instance: &InstanceId,
    filing: Option<&Filing>,
) -> Result<Proposed, KernelError> {
    propose(ledger, paths, session, instance, Proposal::Adoption, filing)
}

/// Proposes that the owner retire `instance`, and carries out the owner's answer. See the module
/// docs. `filing` is used only when a proposal is to be pinned; later calls ignore it.
pub fn propose_retirement(
    ledger: &mut Ledger,
    paths: &Paths,
    session: &VerifiedSession,
    instance: &InstanceId,
    filing: Option<&Filing>,
) -> Result<Proposed, KernelError> {
    propose(ledger, paths, session, instance, Proposal::Retirement, filing)
}

/// The instance as the log has it, read under the flows lock, which is released on return.
struct Seen {
    seq: u64,
    started: Started,
    owners: Owners,
    /// The caller's standing proposal of the kind asked for.
    rec: Option<ProposalRec>,
    /// The progress the question names: the nodes done, and the next ready.
    done: Vec<String>,
    ready: Vec<String>,
}

fn propose(
    ledger: &mut Ledger,
    paths: &Paths,
    session: &VerifiedSession,
    instance: &InstanceId,
    proposal: Proposal,
    filing: Option<&Filing>,
) -> Result<Proposed, KernelError> {
    let me = owner(session)?;
    let seen = look(ledger, instance, proposal, &me)?;
    if let Some(ask_id) = seen.owners.retired.clone() {
        // A retirement the owner approved, whose move a crash left undone.
        if proposal != Proposal::Retirement {
            return Err(FlowError::Retired { instance: instance.to_string() }.into());
        }
        return finish_retirement(ledger, instance, ask_id);
    }
    // A proposal naming an owner since replaced is answered to no effect; with a filing, the
    // session proposes anew, naming the owner as it is now.
    let stale = seen.rec.as_ref().is_some_and(|r| seen.owners.owner.as_ref() != Some(&r.owner));
    let rec = match seen.rec.clone() {
        Some(rec) if !(stale && filing.is_some()) => rec,
        _ => pin(ledger, paths, instance, proposal, &me, &seen, filing)?,
    };
    if !rec.ask.linked {
        return file_and_link(ledger, paths, session, instance, rec);
    }
    let (row, source) = match asks::landed(paths, &rec.ask)? {
        Answer::Landed { row, source } => (row, source),
        Answer::Landing { at } => return Ok(waiting(&rec, None, Some(at))),
        Answer::Open => return Ok(waiting(&rec, None, None)),
    };
    let answer_option = text(row.row.get("answer_option"));
    if answer_option.as_deref() != Some(proposal.approve_option()) {
        // Declined: nothing changes. With a filing, the session proposes anew.
        if filing.is_some() {
            let again = Seen { rec: None, ..seen };
            let rec = pin(ledger, paths, instance, proposal, &me, &again, filing)?;
            return file_and_link(ledger, paths, session, instance, rec);
        }
        let (ask_id, ledger) = (rec.ask.ask_id, rec.ask.ledger);
        return Ok(Proposed::Declined { proposal, ask_id, ledger, answer_option, source });
    }
    let approval = Approval {
        ask_id: rec.ask.ask_id.clone(),
        ledger: rec.ask.ledger.clone(),
        answer_option: proposal.approve_option().to_string(),
        source: source.clone(),
        row_hash: row.hash(),
    };
    apply(ledger, instance, &rec, approval)
}

fn waiting(rec: &ProposalRec, appended: Option<Appended>, landing: Option<String>) -> Proposed {
    Proposed::Waiting {
        proposal: rec.proposal,
        ask_id: rec.ask.ask_id.clone(),
        ledger: rec.ask.ledger.clone(),
        appended,
        landing,
    }
}

fn look(ledger: &mut Ledger, instance: &InstanceId, proposal: Proposal, me: &Owner) -> Result<Seen, KernelError> {
    let guard = ledger.lock()?;
    let log = guard.log(instance)?;
    let started = pinned(&log)?;
    let owners = owners(&log, &started)?;
    let (mut done, mut ready) = (Vec::new(), Vec::new());
    if owners.retired.is_none() {
        let Some(current) = &owners.owner else {
            return Err(Refusal::NoOwner { instance: instance.to_string() }.into());
        };
        if proposal == Proposal::Adoption && current == me {
            return Err(Refusal::AlreadyOwner { instance: instance.to_string() }.into());
        }
        let next = next_of(&log, &started, wall(&log)?)?;
        ready = next.enabled.iter().map(|e| e.node.clone()).collect();
        done =
            started.ir.nodes.keys().filter(|n| !ready.contains(n) && !next.pending.contains_key(*n)).cloned().collect();
    }
    let rec = owners.proposals.iter().find(|r| r.proposal == proposal && r.by == *me).cloned();
    Ok(Seen { seq: log.seq(), started, owners, rec, done, ready })
}

/// At most [`PROGRESS_NAMES`] of `names`, joined, and how many more there are.
fn names(names: &[String]) -> String {
    if names.is_empty() {
        return "none".into();
    }
    let shown = names.iter().take(PROGRESS_NAMES).cloned().collect::<Vec<_>>().join(", ");
    match names.len().saturating_sub(PROGRESS_NAMES) {
        0 => shown,
        more => format!("{shown} and {more} more"),
    }
}

/// The kernel's part of a proposal's body: everything that says what the owner approves.
fn question(instance: &InstanceId, proposal: Proposal, me: &Owner, seen: &Seen, current: &Owner) -> Map<String, Value> {
    let who = |o: &Owner| format!("{} (owner marker {})", o.session, o.marker_sha256);
    let total = seen.started.ir.nodes.len();
    let progress = format!(
        "Progress so far: {} of {total} nodes done ({}); next ready: {}.",
        seen.done.len(),
        names(&seen.done),
        names(&seen.ready)
    );
    let flow = &seen.started.flow;
    let (headline, ask, approve) = match proposal {
        Proposal::Adoption => (
            format!("Hand flow instance {instance} to {}?", me.session),
            format!(
                "Flow instance {instance} ({flow}) is owned by session {}. Session {} asks to adopt it and go on from the next ready step. {progress} Completed steps stand, and claims in flight run out on their leases. Answer adopt to hand it over, or keep to leave it with its owner.",
                who(current),
                who(me)
            ),
            json!({
                "id": Proposal::Adoption.approve_option(),
                "label": "Hand it over",
                "consequence": format!("Session {} owns the instance and goes on from the next ready step; {} writes it no more.", me.session, current.session),
                "reversible": false,
            }),
        ),
        Proposal::Retirement => (
            format!("Retire flow instance {instance}?"),
            format!(
                "Session {} proposes retiring flow instance {instance} ({flow}), owned by session {}. {progress} A retired instance keeps its log and is never written again. Answer retire to retire it, or keep to leave it as it is.",
                who(me),
                who(current)
            ),
            json!({
                "id": Proposal::Retirement.approve_option(),
                "label": "Retire it",
                "consequence": "The instance is retired: its log is kept, and it is never written again.",
                "reversible": false,
            }),
        ),
    };
    let keep = json!({ "id": KEEP, "label": "Keep it as it is", "consequence": "Nothing changes.", "reversible": true, "status_quo": true });
    let mut out = Map::new();
    out.insert("headline".into(), Value::String(headline));
    out.insert("ask".into(), Value::String(ask));
    out.insert("options".into(), json!([approve, keep]));
    out.insert("default".into(), Value::String(KEEP.into()));
    out.insert("preanswerable".into(), Value::Bool(false));
    out
}

/// Mints an id, appends `ProposalPinned`, and returns the pin. The flows lock is not held while
/// the ledger is read; the pin is appended only if the log is still at `seen.seq`.
fn pin(
    ledger: &mut Ledger,
    paths: &Paths,
    instance: &InstanceId,
    proposal: Proposal,
    me: &Owner,
    seen: &Seen,
    filing: Option<&Filing>,
) -> Result<ProposalRec, KernelError> {
    let Some(filing) = filing else {
        return Err(asks::invalid("a first proposal needs a ledger and an ask body"));
    };
    asks::check_filing(filing, &PROPOSAL_KEYS)?;
    let Some(current) = seen.owners.owner.clone() else {
        return Err(Refusal::NoOwner { instance: instance.to_string() }.into());
    };
    let taken = asks::taken(paths, &filing.ledger)?;
    let ask_id = asks::mint(proposal.approve_option(), &taken, &filing.ledger)?;
    let mut body = Map::new();
    body.insert("id".into(), Value::String(ask_id.clone()));
    // Pinned before hashing: every filing of this pin files these bytes.
    body.insert("asked_at".into(), Value::String(clock::now().strftime("%Y-%m-%dT%H:%M:%SZ").to_string()));
    body.extend(question(instance, proposal, me, seen, &current));
    body.extend(filing.body.clone());
    let body = requests::compose_ask(&body).map_err(|e| asks::invalid(e.to_string()))?;
    let body_hash = asks::comparable_hash(&JsValue::from(Value::Object(body.clone())));
    let pin = ProposalPinned {
        proposal,
        by: me.clone(),
        owner: current.clone(),
        ask_id: ask_id.clone(),
        ledger: filing.ledger.clone(),
        body: Value::Object(body),
        body_hash,
    };
    let mut guard = ledger.lock()?;
    // Nothing appended since `look`, or the pin is not appended (`stale_seq`): the only concurrency rule.
    guard.append(instance, seen.seq, pin.to_event()?)?;
    let ask =
        AskRec { ask_id: pin.ask_id, ledger: pin.ledger, body: pin.body, body_hash: pin.body_hash, linked: false };
    Ok(ProposalRec { proposal, by: pin.by, owner: pin.owner, ask })
}

/// Under the flows lock: the log, its owners, and the caller's proposal `expect` still standing as
/// it was read (`proposal_changed` otherwise). `Ok(None)` with the log when another call of the
/// session's linked it meanwhile.
fn standing(
    guard: &WriteGuard,
    instance: &InstanceId,
    expect: &ProposalRec,
) -> Result<(Log, Owners, Option<ProposalRec>), KernelError> {
    let log = guard.log(instance)?;
    let started = pinned(&log)?;
    let owners = owners(&log, &started)?;
    if owners.retired.is_some() {
        return Err(FlowError::Retired { instance: instance.to_string() }.into());
    }
    let now = owners.proposals.iter().find(|r| r.proposal == expect.proposal && r.by == expect.by).cloned();
    match &now {
        Some(now) if now == expect => {}
        Some(now) if now.ask.ask_id == expect.ask.ask_id && now.ask.linked && !expect.ask.linked => {
            return Ok((log, owners, None));
        }
        _ => return Err(Refusal::ProposalChanged { ask_id: expect.ask.ask_id.clone() }.into()),
    }
    Ok((log, owners, now))
}

/// Files the pinned proposal (flows lock released), confirms it in the working tree, and links
/// or withdraws it.
fn file_and_link(
    ledger: &mut Ledger,
    paths: &Paths,
    session: &VerifiedSession,
    instance: &InstanceId,
    rec: ProposalRec,
) -> Result<Proposed, KernelError> {
    // No flows guard is alive here: `look` and `pin` each drop theirs on return.
    asks::file_and_confirm(ledger, paths, session, rec.ask.clone(), |ledger, confirmed| {
        let mut guard = ledger.lock()?;
        let (log, _, still) = standing(&guard, instance, &rec)?;
        if still.is_none() {
            return Ok(waiting(&rec, None, None));
        }
        Ok(match confirmed {
            Confirmed::NotFiled { reason } => {
                let withdrawn =
                    ProposalWithdrawn { ask_id: rec.ask.ask_id.clone(), code: PROPOSAL_NOT_FILED.into(), reason };
                let appended = guard.append(instance, log.seq(), withdrawn.to_event()?)?;
                Proposed::Withdrawn {
                    proposal: rec.proposal,
                    ask_id: rec.ask.ask_id.clone(),
                    ledger: rec.ask.ledger.clone(),
                    code: PROPOSAL_NOT_FILED,
                    reason: withdrawn.reason,
                    appended,
                }
            }
            Confirmed::Row => {
                let linked = ProposalLinked {
                    ask_id: rec.ask.ask_id.clone(),
                    ledger: rec.ask.ledger.clone(),
                    body_hash: rec.ask.body_hash.clone(),
                };
                let appended = guard.append(instance, log.seq(), linked.to_event()?)?;
                waiting(&rec, Some(appended), None)
            }
        })
    })
}

/// Applies the owner's approval of `rec`, under the flows lock: only while the proposal still
/// stands, linked, and the owner it named still owns the instance.
fn apply(
    ledger: &mut Ledger,
    instance: &InstanceId,
    rec: &ProposalRec,
    approval: Approval,
) -> Result<Proposed, KernelError> {
    let mut guard = ledger.lock()?;
    let (log, owners, still) = standing(&guard, instance, rec)?;
    if still.is_none() {
        return Err(Refusal::ProposalChanged { ask_id: rec.ask.ask_id.clone() }.into());
    }
    if owners.owner.as_ref() != Some(&rec.owner) {
        return Err(Refusal::ProposalStale { instance: instance.to_string(), ask_id: rec.ask.ask_id.clone() }.into());
    }
    let (ask_id, ledger_name, source) = (approval.ask_id.clone(), approval.ledger.clone(), approval.source.clone());
    match rec.proposal {
        Proposal::Adoption => {
            let adopted = Adopted { from: rec.owner.clone(), to: rec.by.clone(), approval };
            let appended = guard.append(instance, log.seq(), adopted.to_event()?)?;
            Ok(Proposed::Adopted { ask_id, ledger: ledger_name, from: adopted.from, to: adopted.to, source, appended })
        }
        Proposal::Retirement => {
            let retired = Retired { by: rec.by.clone(), approval };
            let appended = guard.append(instance, log.seq(), retired.to_event()?)?;
            crate::operator::retire_held(&guard, instance)?;
            Ok(Proposed::Retired { ask_id, ledger: ledger_name, source: Some(source), appended: Some(appended) })
        }
    }
}

/// Moves an instance whose log already records `Retired` (a crash came between the two) to
/// `retired/`, under the flows lock.
fn finish_retirement(ledger: &mut Ledger, instance: &InstanceId, ask_id: String) -> Result<Proposed, KernelError> {
    let guard = ledger.lock()?;
    let log = guard.log(instance)?;
    let started = pinned(&log)?;
    if owners(&log, &started)?.retired.is_none() {
        return Err(Refusal::ProposalChanged { ask_id }.into());
    }
    crate::operator::retire_held(&guard, instance)?;
    let ledger_name = log
        .events
        .iter()
        .rev()
        .find(|e| e.kind == Retired::KIND)
        .and_then(|e| e.body.pointer("/approval/ledger"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(Proposed::Retired { ask_id, ledger: ledger_name, source: None, appended: None })
}

#[cfg(test)]
#[path = "adopted.rs"]
mod adopted;
