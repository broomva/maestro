//! The writers that take a node from enabled to done: [`claim`], [`heartbeat`], [`complete`] and
//! [`choose`], and the `until` verdicts they write when a repeat's iteration settles. They fulfil
//! the contract `replay` leaves to them (see its module docs).
//!
//! # Ownership
//!
//! An instance is owned by the session that started it: its `SessionRef` and the digest of its
//! owner marker, together, recorded in `Started`, until the owner approves an adoption (`Adopted`,
//! see `adoption`): the latest accepted `Adopted` names the owner from there on. Every write verb
//! takes the caller's [`VerifiedSession`] and refuses unless both match the current owner: a
//! session with no marker writes nothing (`no_owner_marker`), another session writes nothing
//! (`not_owner`), and neither does a replacement of the owner, which keeps the `SessionRef` with a
//! new marker, nor a former owner. A replacement may start an instance of its own, or propose to
//! adopt this one; no agent transfers an instance, and nothing a session hands another (a claim
//! token included) transfers it. An instance whose log records `Retired` takes no write
//! (`retired`).
//!
//! # Claims, tokens and leases
//!
//! - `claim` takes an enabled node of any class but human at its current iteration and appends
//!   `Claimed`: the attempt, a lease, the input digests `next` names for the node, and the sha256
//!   of a new token. The token goes back to the caller once and is never stored, nor returned by
//!   `next`, `status` or replay. A choice node's claim is finished with `choose`, every other
//!   class's with `complete`; the other verb is refused `wrong_verb`.
//! - The token is a handle, not a capability: every use checks ownership first, so a token in
//!   another session's hands grants nothing, nor in a replacement's (the same `SessionRef` with a
//!   new marker). A claim taken before the instance was adopted holds nothing for its new owner
//!   (`former_owners_claim`): it is not transferred, its lease runs out, and the new owner claims
//!   the node's next attempt. A resumed or restarted session keeps its marker (it survives
//!   turns, provider switches and restarts), so it may heartbeat and complete its own claim until
//!   the lease ends: the lease is the bound on a restart.
//! - A claim holds its node while it is the node's latest claim, its lease has not ended, it is
//!   not spent, its iteration is current and the receipts it was bound to are still the node's
//!   inputs. While one holds, the node cannot be claimed again (`already_claimed`).
//! - `heartbeat` moves the lease end to now plus the node's lease. Once the lease ends, the
//!   token is void: `complete` and `heartbeat` refuse `lease_expired`, the node is released, and
//!   work goes on only through a new claim, the next attempt.
//! - Attempts count claims per node and iteration. A refusal leaves the attempt as it is; the next
//!   claim takes the next number, with checked arithmetic (`attempt_overflow`).
//! - **Retry.** A node with `retry: {attempts: n}` fails once n of its claims in the iteration
//!   have ended without a receipt: replay projects it (`status` says `failed`, `next` does not
//!   enable it), and `claim` refuses `attempts_exhausted`. Without `retry` it takes any number. An
//!   attempt ends when its lease ends, it is cancelled, the inputs it was bound to change or a
//!   later claim takes the node. An effect that started over its current inputs is never
//!   exhausted: only its reconcile runs, and a reconcile takes no attempt.
//! - **Cancel.** A node with `cancel: {after: d}` gives each claim a deadline, `cancel_at`, the
//!   claim's time plus d, recorded in `Claimed`. The lease never runs past it, heartbeats or not,
//!   a command `complete` runs is killed at it, and from it on the token is refused `cancelled`
//!   and the node takes a new claim.
//!
//! # Complete
//!
//! `complete` checks the claim, then that the node is still enabled now (everything it consumes
//! current and fresh) and that the outputs fit, then runs the node's own command: a pure node's
//! `run`, a work node's `verify`, an observe node's `run` (its value is its exit status through
//! `exit_map`; any other status fails closed). A work node's verify reads the outputs it judges
//! as JSON on its standard input (BRO-3636); no command's argv or workdir is ever filled from
//! them, so a performer's text is never read as code. A pure node with a `snapshot` runs in a
//! snapshot of that commit, never in the working tree (`snapshot`); a snapshot that cannot be made
//! is refused `snapshot_failed`, and a run that moved the snapshot's HEAD `snapshot_moved`, never
//! as an exit status of the project's command. The commands
//! run as `exec` says: `maestro` is the kernel's own executable, `sh`, `git` and `test` the
//! system's root-owned files, on a fixed `PATH`. The flows lock is not held while the command runs:
//! it is dropped, and after the run everything is checked again under the lock, against the log
//! as it is then and at a fresh read of the clock, before anything is appended. A pass appends
//! `Completed`, the receipt, which spends the token. Anything else about the work appends
//! `Refused` with a code and keeps the token, so the caller can repair and complete again while
//! the lease holds. A completion an agent claims is never a receipt: only the command the kernel
//! ran, and its exit status, make one. Before the command runs, `complete` checks that the
//! receipt it would append fits an event line (the argv as JSON escapes it, the outputs, the
//! inputs), and refuses `receipt_too_large` otherwise, so a node is never wedged on a receipt
//! that can never be written.
//!
//! An observation's receipt is stamped with its run's start (`verify.ran_at`), and replay counts
//! its freshness from there (see `replay`), not from the append that came after the run.
//!
//! # Effects
//!
//! An effect node's command (`run`) acts on the world, so it runs at most once in an iteration.
//! `complete` checks its `preconditions` first: each a jq guard over the recorded state, bounded
//! (`guard`), evaluated with the lock released and the claim checked again after. When they all
//! hold, it appends `EffectStarted` under the lock, with the claim's inputs, then runs the
//! command. Exit 0 appends the receipt; anything else `Refused`. From `EffectStarted` on, the
//! effect is never run again over those inputs in its iteration, whatever happened after it (a
//! crash, a timeout, a failure, a second completion racing the first, which is refused
//! `effect_started`): every later completion, on the same token or a new claim's, runs the node's
//! `reconcile` instead. Reconcile exiting 0 appends the receipt, with the reconcile in `verify`;
//! anything else is refused `effect_unreconciled`. Two exceptions, both certain that the effect
//! did not act over its inputs: over new inputs (an upstream re-run with another value) it is a
//! new effect and runs; and an effect whose own command never started (`spawn_failed`: the
//! program is missing, say) withdraws its start, so the next completion runs it. A reconcile that
//! cannot start is refused `reconcile_spawn_failed`, which withdraws nothing: the effect may have
//! acted, so the next completion reconciles again.
//!
//! # Choose
//!
//! `choose` takes a choice node's claim and an edge. An edge the node does not declare is refused
//! `undeclared_edge`. A declared edge's jq guard is evaluated over the recorded state (the node's
//! inputs, see `replay::guard_input`), bounded, with the lock released; then everything is checked
//! again under the lock, as `complete` does. A guard that holds appends `Chosen`, with the digest
//! of the input it read, which spends the token; a guard that is false, fails or runs past its
//! timeout appends `Refused` (`guard_false`, `guard_error`, `guard_timeout`, and
//! `guard_input_too_large` or `guard_invalid` before it runs) and keeps the token.
//!
//! # Verdicts
//!
//! When a receipt settles a repeat's iteration (every body node done, or never to run), the
//! writer evaluates the repeat's `until` over the body's receipts and the params, bounded, with
//! the lock released, and appends `Guarded` with the verdict and the digest of its input, but
//! only if the repeat still awaits a verdict for that iteration over the same input. A verdict
//! that cannot be had (an error, a timeout) is not written: the repeat keeps awaiting it, and the
//! next write tries again. `complete` and `choose` do this after their receipt, and `claim`
//! before it takes a claim, so a verdict a crash left unwritten is written by the next claim.
//!
//! # The clock
//!
//! The writers take no clock from their caller. Each reads the wall clock ([`crate::clock`]), the
//! one the storage layer stamps every event's `at` with, under the flows lock and right before the
//! checks it serves: lease expiry, write-time freshness and the claim's inputs. A lease ends at
//! that read plus the node's lease. `complete` stamps `verify.ran_at` with a read taken as its
//! command starts, and after the run reads the clock again, under the lock, for every check before
//! the append; it never adds a measured duration to an earlier read (a monotonic clock stops while
//! the machine sleeps, so a run's end would fall behind the wall clock and a lease or an input
//! would be judged before its time). So no caller and no monotonic drift can make an observation
//! or a lease look fresher or less expired than the wall clock says.
//!
//! A wall clock that reads earlier than the instance's last event has stepped back. The write is
//! refused `clock_behind`, with nothing written, and goes on once the clock passes that event.
//! Judging at the last event's time instead would fail closed for the checks, but would stamp a
//! run's start and a lease's end later than the wall clock, making an observation younger and a
//! lease longer than they are; refusing never fails open.

use super::adoption::{Owners, owners};
use super::error::{KernelError, Refusal};
use super::events::{
    Chosen, Claimed, Completed, EffectStarted, Guarded, Heartbeat, KernelEvent, Owner, Performer, Refused, Started,
    VerifyRecord, digest, marker_digest,
};
use super::exec::{self, Feed, Limits, Ran, Search};
use super::guard::{self, GUARD_TIMEOUT, Judged};
use super::ir::{Class, Ir, Iteration, Node};
use super::replay::{Awaiting, Next, Reason, State, awaiting, guard_input, next_of, pinned, secs};
use super::snapshot;
use crate::FlowError;
use crate::clock;
use crate::ids::InstanceId;
use crate::ledger::{Appended, Ledger, WriteGuard, random_epoch};
use crate::log::{Log, MAX_LINE, MAX_NESTING, line_hash, nesting};
use jiff::{SignedDuration, Timestamp};
use maestro_core::session::VerifiedSession;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// A claim's lease, in seconds, for a node that declares no `wait.heartbeat`.
pub const DEFAULT_LEASE_SECS: u64 = 600;

/// The longest lease, whatever a node's `wait.heartbeat` says.
pub const MAX_LEASE_SECS: u64 = 86_400;

/// The most bytes of outputs `complete` takes, serialized.
pub const MAX_OUTPUTS: usize = 64 << 10;

/// The most bytes a command may have once its params are filled in, all its words together.
pub const MAX_ARGV: usize = 256 << 10;

/// The longest token `complete` and `heartbeat` read. A real one is about 70 bytes.
pub const MAX_TOKEN: usize = 256;

/// A claim taken.
#[derive(Debug, Clone, PartialEq)]
pub struct Claim {
    /// The single-use handle for `heartbeat` and `complete`. Only its sha256 is in the log.
    pub token: String,
    pub instance: InstanceId,
    pub node: String,
    pub iteration: Iteration,
    pub attempt: u32,
    pub lease_until: Timestamp,
    /// The input digests the claim is bound to.
    pub inputs: BTreeMap<String, String>,
    pub appended: Appended,
}

/// A lease extended.
#[derive(Debug, Clone, PartialEq)]
pub struct Beat {
    pub lease_until: Timestamp,
    pub appended: Appended,
}

/// What `complete` appended: the receipt, or a refusal that keeps the token.
#[derive(Debug, Clone, PartialEq)]
pub enum Completion {
    Completed {
        appended: Appended,
        verify: VerifyRecord,
        /// The command's standard error, for the caller; never written to the log.
        stderr: String,
        /// The `until` verdicts the receipt led to.
        verdicts: Vec<Verdict>,
    },
    Refused {
        appended: Appended,
        code: &'static str,
        reason: String,
        stderr: String,
    },
}

/// What `choose` appended: the edge, or a refusal that keeps the token.
#[derive(Debug, Clone, PartialEq)]
pub enum Choosing {
    Chosen {
        appended: Appended,
        edge: String,
        /// The digest of the input document the edge's guard held over.
        guard_digest: String,
        /// The `until` verdicts the choice led to.
        verdicts: Vec<Verdict>,
    },
    Refused {
        appended: Appended,
        code: &'static str,
        reason: String,
    },
}

/// An `until` verdict a write tried to record.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// `Guarded` appended.
    Recorded { repeat: String, iteration: u32, holds: bool, appended: Appended },
    /// Nothing appended: the repeat (when known) still awaits its verdict, and the next write
    /// tries again.
    Unrecorded { repeat: Option<String>, code: &'static str, reason: String },
}

/// The owner a session would be: its `SessionRef` and the digest of its marker.
pub(crate) fn owner(session: &VerifiedSession) -> Result<Owner, Refusal> {
    let marker = session.owner_marker().filter(|m| !m.is_empty()).ok_or(Refusal::NoOwnerMarker)?;
    Ok(Owner { session: session.session().clone(), marker_sha256: marker_digest(marker) })
}

/// Refuses any session but the instance's current owner (`Started`'s, or the latest accepted
/// `Adopted`'s), with the same marker, and any write once the log records `Retired`. Returns who
/// owns the instance, as the log has it.
pub(crate) fn check_owner(
    log: &Log,
    started: &Started,
    instance: &InstanceId,
    session: &VerifiedSession,
) -> Result<Owners, KernelError> {
    let me = owner(session)?;
    let owners = owners(log, started)?;
    if owners.retired.is_some() {
        return Err(FlowError::Retired { instance: instance.to_string() }.into());
    }
    let Some(owner) = &owners.owner else {
        return Err(Refusal::NoOwner { instance: instance.to_string() }.into());
    };
    if *owner != me {
        return Err(Refusal::NotOwner { instance: instance.to_string() }.into());
    }
    Ok(owners)
}

fn performer(session: &VerifiedSession) -> Performer {
    Performer {
        harness: session.harness().map_or("unknown", |h| h.as_str()).to_string(),
        model: None,
        session: Some(session.session().to_string()),
    }
}

/// A node's lease: its `wait.heartbeat`, or [`DEFAULT_LEASE_SECS`], at most [`MAX_LEASE_SECS`].
fn lease(node: &Node) -> SignedDuration {
    secs(node.class.wait().map_or(DEFAULT_LEASE_SECS, |w| w.heartbeat).min(MAX_LEASE_SECS))
}

fn until(now: Timestamp, lease: SignedDuration) -> Result<Timestamp, Refusal> {
    now.checked_add(lease).map_err(|_| Refusal::ClockOutOfRange)
}

/// A lease's end, never past the claim's cancel deadline.
fn capped(lease_until: Timestamp, cancel_at: Option<Timestamp>) -> Timestamp {
    cancel_at.map_or(lease_until, |at| lease_until.min(at))
}

/// The instance a token names, and the digest the log keeps of it.
fn parse_token(token: &str) -> Result<(InstanceId, String), Refusal> {
    if token.len() > MAX_TOKEN {
        return Err(Refusal::TokenMalformed);
    }
    let (instance, _) = token.rsplit_once('.').ok_or(Refusal::TokenMalformed)?;
    let instance = InstanceId::new(instance).map_err(|_| Refusal::TokenMalformed)?;
    Ok((instance, line_hash(token.as_bytes())))
}

/// A claim as the log has it now.
#[derive(Debug, Clone)]
struct ClaimRec {
    /// The `Claimed` event's position in the log.
    seq: u64,
    node: String,
    iteration: Iteration,
    attempt: u32,
    lease_until: Timestamp,
    /// When `cancel.after` ends the attempt.
    cancel_at: Option<Timestamp>,
    inputs: BTreeMap<String, String>,
    spent: bool,
}

/// Every claim in an instance's log, folded in log order.
struct Claims {
    /// By token digest.
    recs: BTreeMap<String, ClaimRec>,
    /// The latest claim's token digest for each node and iteration.
    latest: BTreeMap<(String, Iteration), String>,
    /// The effects started and not withdrawn, by node, iteration and inputs, with their attempt.
    effects: BTreeMap<(String, Iteration, BTreeMap<String, String>), u32>,
}

fn parse<T: DeserializeOwned>(log: &Log, seq: u64, kind: &str, body: &Value) -> Result<T, KernelError> {
    serde_json::from_value(body.clone()).map_err(|e| KernelError::Malformed {
        instance: log.instance.to_string(),
        seq,
        detail: format!("a {kind} body that does not parse ({e})"),
    })
}

fn stamp(log: &Log, seq: u64, text: &str) -> Result<Timestamp, KernelError> {
    text.parse().map_err(|_| KernelError::Malformed {
        instance: log.instance.to_string(),
        seq,
        detail: format!("{text} is not a timestamp"),
    })
}

/// The wall clock, read now, for the checks a write makes against `log`: refused `clock_behind`
/// when it reads earlier than the log's last event (see the module docs).
pub(crate) fn wall(log: &Log) -> Result<Timestamp, KernelError> {
    let now = clock::now();
    let Some(last) = log.events.last() else { return Ok(now) };
    let at = stamp(log, last.seq, &last.at)?;
    if now < at {
        return Err(Refusal::ClockBehind { now: now.to_string(), at: last.at.clone() }.into());
    }
    Ok(now)
}

impl Claims {
    /// A `Claimed` counts only with an attempt above its node's last one. A `Heartbeat` moves
    /// the lease of the latest claim of its node and iteration, and a `Completed` or `Chosen`
    /// spends that claim, each only when it is for the claim's attempt. An `EffectStarted` stands
    /// for its node, iteration and inputs unless a `Refused` with code `spawn_failed` for its
    /// attempt withdraws it (the command never started). A receipt need not clear it: a done node
    /// is claimed again only over new inputs, which no start stands for.
    fn fold(log: &Log) -> Result<Claims, KernelError> {
        let mut claims = Claims { recs: BTreeMap::new(), latest: BTreeMap::new(), effects: BTreeMap::new() };
        for e in &log.events {
            match e.kind.as_str() {
                Claimed::KIND => {
                    let c: Claimed = parse(log, e.seq, &e.kind, &e.body)?;
                    let key = (c.node.clone(), c.iteration.clone());
                    let last = claims.latest_of(&key).map_or(0, |r| r.attempt);
                    if c.attempt <= last {
                        continue;
                    }
                    let lease_until = stamp(log, e.seq, &c.lease_until)?;
                    let cancel_at = c.cancel_at.as_deref().map(|at| stamp(log, e.seq, at)).transpose()?;
                    let rec = ClaimRec {
                        seq: e.seq,
                        node: c.node,
                        iteration: c.iteration,
                        attempt: c.attempt,
                        lease_until,
                        cancel_at,
                        inputs: c.inputs,
                        spent: false,
                    };
                    claims.latest.insert(key, c.token_sha256.clone());
                    claims.recs.insert(c.token_sha256, rec);
                }
                Heartbeat::KIND => {
                    let h: Heartbeat = parse(log, e.seq, &e.kind, &e.body)?;
                    let lease_until = stamp(log, e.seq, &h.lease_until)?;
                    if let Some(rec) = claims.latest_mut(&(h.node, h.iteration))
                        && rec.attempt == h.attempt
                    {
                        rec.lease_until = lease_until;
                    }
                }
                Completed::KIND => {
                    let c: Completed = parse(log, e.seq, &e.kind, &e.body)?;
                    let key = (c.node, c.iteration);
                    if let Some(rec) = claims.latest_mut(&key)
                        && rec.attempt == c.attempt
                    {
                        rec.spent = true;
                    }
                }
                Chosen::KIND => {
                    let c: Chosen = parse(log, e.seq, &e.kind, &e.body)?;
                    if let Some(rec) = claims.latest_mut(&(c.node, c.iteration))
                        && rec.attempt == c.attempt
                    {
                        rec.spent = true;
                    }
                }
                EffectStarted::KIND => {
                    let started: EffectStarted = parse(log, e.seq, &e.kind, &e.body)?;
                    claims.effects.insert((started.node, started.iteration, started.inputs), started.attempt);
                }
                Refused::KIND => {
                    let r: Refused = parse(log, e.seq, &e.kind, &e.body)?;
                    if r.code == "spawn_failed" {
                        claims.effects.retain(|(node, iteration, _), attempt| {
                            !(*node == r.node && *iteration == r.iteration && *attempt == r.attempt)
                        });
                    }
                }
                _ => {}
            }
        }
        Ok(claims)
    }

    fn latest_of(&self, key: &(String, Iteration)) -> Option<&ClaimRec> {
        self.latest.get(key).and_then(|sha| self.recs.get(sha))
    }

    fn latest_mut(&mut self, key: &(String, Iteration)) -> Option<&mut ClaimRec> {
        self.latest.get(key).and_then(|sha| self.recs.get_mut(sha))
    }
}

/// A claim that holds its node at `now`, as [`claim`] judges `already_claimed`: the latest claim of
/// its node and iteration, not spent, its lease not ended.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Live {
    pub node: String,
    pub iteration: Iteration,
    /// Its `Claimed` event's position: a claim before the owner's epoch is a former owner's.
    pub seq: u64,
    pub lease_until: Timestamp,
    pub inputs: BTreeMap<String, String>,
}

/// Every claim that holds its node at `now` (see [`Live`]).
pub(crate) fn live_claims(log: &Log, now: Timestamp) -> Result<Vec<Live>, KernelError> {
    let claims = Claims::fold(log)?;
    let live =
        claims.latest.values().filter_map(|sha| claims.recs.get(sha)).filter(|r| !r.spent && now < r.lease_until);
    Ok(live
        .map(|r| Live {
            node: r.node.clone(),
            iteration: r.iteration.clone(),
            seq: r.seq,
            lease_until: r.lease_until,
            inputs: r.inputs.clone(),
        })
        .collect())
}

/// A node's current iteration and inputs as `next` reports them, and its state when it is not
/// enabled. `None` when the node is done.
fn current<'n>(next: &'n Next, node: &str) -> Option<(&'n Iteration, &'n BTreeMap<String, String>, Option<State>)> {
    if let Some(e) = next.enabled.iter().find(|e| e.node == node) {
        return Some((&e.iteration, &e.inputs, None));
    }
    next.pending.get(node).map(|s| (&s.iteration, &s.inputs, Some(s.state)))
}

fn state_name(state: Option<State>) -> String {
    let name = state.and_then(|s| serde_json::to_value(s).ok()).and_then(|v| v.as_str().map(str::to_string));
    name.unwrap_or_else(|| "done".to_string())
}

/// Takes a claim on `node` of `instance`, owned by `session`. See the module docs.
///
/// **Needs a guard host**: a write that evaluates a guard (a choice's edge, an effect's
/// preconditions, an `until` verdict) is refused `guard_host_missing` unless the process called
/// [`guard_entry`](super::guard_entry) first in `main` (see `guard`).
pub fn claim(
    ledger: &mut Ledger,
    session: &VerifiedSession,
    instance: &InstanceId,
    node: &str,
) -> Result<Claim, KernelError> {
    // A verdict a crash left unwritten is written first, so what waits on it can be claimed.
    settle(ledger, session, instance)?;
    let mut guard = ledger.lock()?;
    let log = guard.log(instance)?;
    let started = pinned(&log)?;
    check_owner(&log, &started, instance, session)?;
    let now = wall(&log)?;
    let Some(n) = started.ir.nodes.get(node) else {
        return Err(Refusal::UnknownNode { node: node.to_string() }.into());
    };
    if matches!(n.class, Class::Human { .. }) {
        return Err(Refusal::NotClaimable { node: node.to_string(), class: n.class.name().to_string() }.into());
    }
    let next = next_of(&log, &started, now)?;
    let Some(enabled) = next.enabled.iter().find(|e| e.node == node) else {
        // Replay projects a node whose attempts all ended without a receipt as failed.
        let exhausted = next.pending.get(node).and_then(|s| {
            s.unmet.iter().find_map(|r| match r {
                Reason::AttemptsExhausted { attempts } => Some(*attempts),
                _ => None,
            })
        });
        if let Some(attempts) = exhausted {
            return Err(Refusal::AttemptsExhausted { node: node.to_string(), attempts }.into());
        }
        let state = state_name(next.pending.get(node).map(|s| s.state));
        return Err(Refusal::NotEnabled { node: node.to_string(), state }.into());
    };
    let claims = Claims::fold(&log)?;
    let prior = claims.latest_of(&(node.to_string(), enabled.iteration.clone()));
    if let Some(p) = prior
        && !p.spent
        && now < p.lease_until
        && p.inputs == enabled.inputs
    {
        return Err(Refusal::AlreadyClaimed { node: node.to_string(), lease_until: p.lease_until.to_string() }.into());
    }
    let last = prior.map_or(0, |p| p.attempt);
    let attempt = last.checked_add(1).ok_or_else(|| Refusal::AttemptOverflow { node: node.to_string() })?;
    let cancel_at = n.cancel_after.map(|after| until(now, secs(after))).transpose()?;
    let lease_until = capped(until(now, lease(n))?, cancel_at);
    let token = format!("{instance}.{}", random_epoch()?);
    let claimed = Claimed {
        node: node.to_string(),
        iteration: enabled.iteration.clone(),
        attempt,
        lease_until: lease_until.to_string(),
        token_sha256: line_hash(token.as_bytes()),
        inputs: enabled.inputs.clone(),
        performer: Some(performer(session)),
        tool_use_id: None,
        cancel_at: cancel_at.map(|at| at.to_string()),
    };
    let appended = guard.append(instance, log.seq(), claimed.to_event()?)?;
    Ok(Claim {
        token,
        instance: instance.clone(),
        node: claimed.node,
        iteration: claimed.iteration,
        attempt,
        lease_until,
        inputs: claimed.inputs,
        appended,
    })
}

/// A claim that holds at `now`, with the log and the scheduler's view it was checked against.
struct Held {
    log: Log,
    started: Started,
    rec: ClaimRec,
    next: Next,
    /// The node is an effect started in this iteration and not completed (`EffectStarted`).
    effect_started: bool,
}

impl Held {
    fn node(&self) -> Result<&Node, Refusal> {
        self.started.ir.nodes.get(&self.rec.node).ok_or_else(|| Refusal::UnknownNode { node: self.rec.node.clone() })
    }
}

/// Checks, in order, the owner, the token, and that its claim still holds the node at `now`, the
/// wall clock read for `log` (by [`wall`], under the lock that read `log`).
fn hold(
    log: Log,
    session: &VerifiedSession,
    instance: &InstanceId,
    sha: &str,
    now: Timestamp,
) -> Result<Held, KernelError> {
    let started = pinned(&log)?;
    let owners = check_owner(&log, &started, instance, session)?;
    let claims = Claims::fold(&log)?;
    let Some(rec) = claims.recs.get(sha).cloned() else {
        return Err(Refusal::UnknownToken { instance: instance.to_string() }.into());
    };
    // A claim the former owner took holds nothing for the new one: it runs out on its lease.
    if rec.seq < owners.since {
        return Err(Refusal::FormerOwnersClaim { node: rec.node }.into());
    }
    if rec.spent {
        return Err(Refusal::TokenSpent { node: rec.node }.into());
    }
    let key = (rec.node.clone(), rec.iteration.clone());
    let latest = claims.latest.get(&key).map(String::as_str);
    if let Some(cancel_at) = rec.cancel_at.filter(|at| latest == Some(sha) && now >= *at) {
        return Err(Refusal::Cancelled { node: rec.node, cancel_at: cancel_at.to_string() }.into());
    }
    if latest != Some(sha) || now >= rec.lease_until {
        return Err(Refusal::LeaseExpired { node: rec.node, lease_until: rec.lease_until.to_string() }.into());
    }
    let next = next_of(&log, &started, now)?;
    let Some((iteration, inputs, state)) = current(&next, &rec.node) else {
        return Err(Refusal::NotEnabled { node: rec.node, state: state_name(None) }.into());
    };
    if *iteration != rec.iteration || state == Some(State::Exhausted) {
        return Err(Refusal::NotCurrentIteration { node: rec.node }.into());
    }
    if *inputs != rec.inputs {
        return Err(Refusal::InputsChanged { node: rec.node }.into());
    }
    let effect_started = claims.effects.contains_key(&(rec.node.clone(), rec.iteration.clone(), rec.inputs.clone()));
    Ok(Held { log, started, rec, next, effect_started })
}

/// Extends the lease of the claim `token` names. See the module docs.
pub fn heartbeat(ledger: &mut Ledger, session: &VerifiedSession, token: &str) -> Result<Beat, KernelError> {
    let (instance, sha) = parse_token(token)?;
    let mut guard = ledger.lock()?;
    let log = guard.log(&instance)?;
    let now = wall(&log)?;
    let held = hold(log, session, &instance, &sha, now)?;
    let lease_until = capped(until(now, lease(held.node()?))?, held.rec.cancel_at);
    let beat = Heartbeat {
        node: held.rec.node.clone(),
        iteration: held.rec.iteration.clone(),
        attempt: held.rec.attempt,
        lease_until: lease_until.to_string(),
    };
    let appended = guard.append(&instance, held.log.seq(), beat.to_event()?)?;
    Ok(Beat { lease_until, appended })
}

/// A command ready to run.
struct Plan {
    argv: Vec<String>,
    cwd: PathBuf,
    step: Step,
    /// A work node's verify reads the outputs it judges here, as JSON, on its standard input.
    stdin: Option<Vec<u8>>,
    /// A pure node's `snapshot`, filled: the commit its command runs in a snapshot of.
    snapshot: Option<String>,
}

/// What the command is to the node.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    /// A pure or observe node's `run`, or a work node's `verify`.
    Command,
    /// An effect's `run`, once its preconditions hold; it has not started in this iteration.
    Effect { preconditions: Vec<String> },
    /// An effect's `reconcile`: it started in this iteration and has no receipt.
    Reconcile,
}

/// Why the work is refused: a code and a reason, appended as `Refused`.
type Unfit = (&'static str, String);

/// Checks the work against the node as the log has it now: enabled (so everything it consumes is
/// current and fresh), outputs that fit, and a command that can run.
fn prepare(held: &Held, outputs: &Value) -> Result<Plan, Unfit> {
    check_outputs(outputs)?;
    enabled(held)?;
    let ir = &held.started.ir;
    let node = held.started.ir.nodes.get(&held.rec.node);
    // Only a work node's verify reads the outputs it judges, and only as data on its standard
    // input; every command's words are a function of the pinned IR and params, which no performer
    // chooses.
    let (words, step, judges) = match node.map(|n| &n.class) {
        Some(Class::Pure { run }) | Some(Class::Observe { run, .. }) => (run, Step::Command, false),
        Some(Class::Work { verify, .. }) => (verify, Step::Command, true),
        Some(Class::Effect { reconcile, .. }) if held.effect_started => (reconcile, Step::Reconcile, false),
        Some(Class::Effect { run, preconditions, .. }) if !run.is_empty() => {
            (run, Step::Effect { preconditions: preconditions.clone() }, false)
        }
        _ => return Err(("not_claimable", format!("node {} has no command the kernel runs", held.rec.node))),
    };
    let params = &held.started.params;
    // The words share one budget, so a command is never built past MAX_ARGV.
    let mut argv = Vec::with_capacity(words.len());
    let mut left = MAX_ARGV;
    for word in words {
        let filled = fill(ir, params, word, left).map_err(|e| ("template_invalid", e))?;
        left -= filled.len();
        argv.push(filled);
    }
    let Some(dir) = &ir.workdir else {
        return Err(("workdir_invalid", "the flow declares no workdir".to_string()));
    };
    let cwd = PathBuf::from(fill(ir, params, dir, MAX_ARGV).map_err(|e| ("workdir_invalid", e))?);
    if !cwd.is_absolute() || !cwd.is_dir() {
        return Err(("workdir_invalid", "the workdir is not an absolute path to a directory".to_string()));
    }
    let stdin = judges.then(|| serde_json::to_vec(outputs).unwrap_or_default());
    let snapshot = match node.and_then(|n| n.snapshot.as_deref()) {
        Some(rev) => {
            let commit = fill(ir, params, rev, MAX_ARGV).map_err(|e| ("snapshot_invalid", e))?;
            if !snapshot::is_object_id(&commit) {
                let detail = format!("the snapshot's commit {commit:?} is not a full object id");
                return Err(("snapshot_invalid", detail));
            }
            Some(commit)
        }
        None => None,
    };
    Ok(Plan { argv, cwd, step, stdin, snapshot })
}

/// The node is enabled now, so everything it consumes is current and fresh: write-time freshness.
fn enabled(held: &Held) -> Result<(), Unfit> {
    if !held.next.enabled.iter().any(|e| e.node == held.rec.node) {
        let state = state_name(held.next.pending.get(&held.rec.node).map(|s| s.state));
        return Err((
            "input_stale",
            format!("node {} is {state} now: something it consumes is not fresh", held.rec.node),
        ));
    }
    Ok(())
}

/// Refuses the verb the node's class does not take: a choice is chosen, the rest completed.
fn check_verb(held: &Held, choosing: bool) -> Result<(), Refusal> {
    let class = held.node()?.class.name();
    if (class == "choice") != choosing {
        let verb = if choosing { "complete" } else { "choose" };
        return Err(Refusal::WrongVerb { node: held.rec.node.clone(), class: class.to_string(), verb });
    }
    Ok(())
}

/// Outputs are a JSON object (or null), at most [`MAX_OUTPUTS`] bytes, shallow enough that the
/// `Completed` body carrying them reads back.
fn check_outputs(outputs: &Value) -> Result<(), Unfit> {
    let refuse = |detail: String| ("outputs_invalid", detail);
    if !(outputs.is_object() || outputs.is_null()) {
        return Err(refuse("outputs must be a JSON object".to_string()));
    }
    let bytes = serde_json::to_vec(outputs).map(|b| b.len()).unwrap_or(usize::MAX);
    if bytes > MAX_OUTPUTS {
        return Err(refuse(format!("outputs are {bytes} bytes, more than {MAX_OUTPUTS}")));
    }
    // `Completed` carries the outputs one level inside its body.
    let depth = nesting(outputs);
    if depth >= MAX_NESTING {
        return Err(refuse(format!("outputs nest {depth} levels deep, more than {}", MAX_NESTING - 1)));
    }
    Ok(())
}

/// Fills each `{{params.<name>}}` in `text` with the param's value: a string as it is, a number
/// or boolean as written, an absent or null optional param as nothing. Anything else in braces,
/// an undeclared param or a param that is an array or object is refused, and so is a result
/// over `budget` bytes, as soon as it is (one param at most past it, never a whole command).
fn fill(ir: &Ir, params: &Value, text: &str, budget: usize) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = Some(text);
    while let Some(text) = rest {
        let (literal, after) = match text.split_once("{{") {
            Some((literal, after)) => (literal, Some(after)),
            None => (text, None),
        };
        out.push_str(literal);
        rest = None;
        if let Some(after) = after {
            let (inner, tail) = after.split_once("}}").ok_or_else(|| "a {{ with no }}".to_string())?;
            let name = inner
                .strip_prefix("params.")
                .filter(|name| ir.params.contains_key(*name))
                .ok_or_else(|| format!("{{{{{inner}}}}} names no param of this flow"))?;
            match params.get(name) {
                None | Some(Value::Null) => {}
                Some(Value::String(s)) => out.push_str(s),
                Some(v @ (Value::Number(_) | Value::Bool(_))) => out.push_str(&v.to_string()),
                Some(_) => return Err(format!("param {name} is not a string, number or boolean")),
            }
            rest = Some(tail);
        }
        if out.len() > budget {
            return Err(format!("the command fills past {MAX_ARGV} bytes"));
        }
    }
    Ok(out)
}

/// Room a `Completed` line takes beyond the body `complete` measures before the run: the
/// envelope (`seq`, `prev_event`, `at`, `kind`) and the body's `ran_at`, set only at the append.
const RECEIPT_ROOM: usize = 256;

/// The receipt for the held claim.
fn receipt(held: &Held, session: &VerifiedSession, outputs: Value, verify: VerifyRecord) -> Completed {
    Completed {
        node: held.rec.node.clone(),
        iteration: held.rec.iteration.clone(),
        attempt: held.rec.attempt,
        inputs: held.rec.inputs.clone(),
        outputs,
        verify: Some(verify),
        performer: Some(performer(session)),
        tool_use_id: None,
    }
}

/// Refuses work whose receipt could never be appended: its body, with the widest exit status and
/// an empty `ran_at`, plus [`RECEIPT_ROOM`], must fit `log::MAX_LINE`. Checked before the command
/// runs, since nothing after the run could make the receipt smaller.
fn check_receipt(held: &Held, session: &VerifiedSession, outputs: &Value, argv: &[String]) -> Result<(), Unfit> {
    let verify =
        VerifyRecord { argv: argv.to_vec(), exit: i32::MIN, stdout_sha256: line_hash(b""), ran_at: String::new() };
    let body = serde_json::to_vec(&receipt(held, session, outputs.clone(), verify)).map(|b| b.len());
    let size = body.unwrap_or(usize::MAX).saturating_add(RECEIPT_ROOM);
    if size > MAX_LINE as usize {
        let detail = format!("the receipt would take about {size} bytes, more than an event line's {MAX_LINE}");
        return Err(("receipt_too_large", detail));
    }
    Ok(())
}

/// Appends `Refused` for the held claim; the token stays good.
fn refuse(
    guard: &mut WriteGuard,
    held: &Held,
    (code, reason): Unfit,
    stderr: &[u8],
) -> Result<Completion, KernelError> {
    let appended = refused(guard, held, code, &reason)?;
    Ok(Completion::Refused { appended, code, reason, stderr: String::from_utf8_lossy(stderr).into_owned() })
}

fn refused(guard: &mut WriteGuard, held: &Held, code: &'static str, reason: &str) -> Result<Appended, KernelError> {
    let refused = Refused {
        node: held.rec.node.clone(),
        iteration: held.rec.iteration.clone(),
        attempt: held.rec.attempt,
        code: code.to_string(),
        reason: reason.to_string(),
    };
    Ok(guard.append(&held.log.instance, held.log.seq(), refused.to_event()?)?)
}

/// Completes the claim `token` names: runs the node's command and appends the receipt, or a
/// refusal. See the module docs.
///
/// **Needs a guard host**: a write that evaluates a guard (a choice's edge, an effect's
/// preconditions, an `until` verdict) is refused `guard_host_missing` unless the process called
/// [`guard_entry`](super::guard_entry) first in `main` (see `guard`).
pub fn complete(
    ledger: &mut Ledger,
    session: &VerifiedSession,
    token: &str,
    outputs: Value,
    limits: &Limits,
) -> Result<Completion, KernelError> {
    limits.check().map_err(|detail| Refusal::InvalidLimits { detail })?;
    let (instance, sha) = parse_token(token)?;
    let mut guard = ledger.lock()?;
    let log = guard.log(&instance)?;
    let now = wall(&log)?;
    let mut held = hold(log, session, &instance, &sha, now)?;
    check_verb(&held, false)?;
    let plan = match prepare(&held, &outputs) {
        Ok(plan) => plan,
        Err(unfit) => return refuse(&mut guard, &held, unfit, &[]),
    };
    if let Err(unfit) = check_receipt(&held, session, &outputs, &plan.argv) {
        return refuse(&mut guard, &held, unfit, &[]);
    }
    if let Step::Effect { preconditions } = &plan.step {
        // The preconditions run with the lock released; then everything again under it.
        let input = guard_input(&held.log, &held.started, &held.rec.node)?;
        drop(guard);
        let judged = all_hold(preconditions, &input);
        pause!("preconditions_judged");
        guard = ledger.lock()?;
        let log = guard.log(&instance)?;
        let judged_at = wall(&log)?;
        held = hold(log, session, &instance, &sha, judged_at)?;
        // Another completion of this claim started the effect meanwhile: it is never run twice.
        if held.effect_started {
            let reason = "another completion started this effect; complete again to reconcile it";
            return refuse(&mut guard, &held, ("effect_started", reason.to_string()), &[]);
        }
        if let Err(unfit) = prepare(&held, &outputs) {
            return refuse(&mut guard, &held, unfit, &[]);
        }
        let guard_digest = match judged {
            Ok(digest) => digest,
            Err(unfit) => return refuse(&mut guard, &held, unfit, &[]),
        };
        let intent = EffectStarted {
            node: held.rec.node.clone(),
            iteration: held.rec.iteration.clone(),
            attempt: held.rec.attempt,
            inputs: held.rec.inputs.clone(),
            guard_digest,
        };
        guard.append(&instance, held.log.seq(), intent.to_event()?)?;
    }
    // A claim's cancel deadline ends its command too: the run is no longer than what is left.
    let limits = &cancel_capped(limits, held.rec.cancel_at);
    // The command runs with the flows lock released, so other writers and readers go on.
    // Every command but the project's own in a snapshot searches the system's PATH (`exec`).
    let feed = Feed { search: Search::Trusted, stdin: plan.stdin.as_deref() };
    drop(guard);
    let ran_at = clock::now();
    let ran = match &plan.snapshot {
        Some(commit) => snapshot::run_in(&plan.cwd, commit, &plan.argv, limits),
        None => exec::run(&plan.argv, &plan.cwd, limits, feed),
    };
    pause!("command_ran");

    // Everything again, under the lock, against the log as it is now and the clock read again.
    let mut guard = ledger.lock()?;
    let log = guard.log(&instance)?;
    let after = wall(&log)?;
    let held = hold(log, session, &instance, &sha, after)?;
    // The command is a function of the pinned IR and params, so only the rest can have changed.
    if let Err(unfit) = prepare(&held, &outputs) {
        return refuse(&mut guard, &held, unfit, &[]);
    }
    let (code, stdout, stderr) = match ran {
        Ran::Exited { code, stdout, stderr } => (code, stdout, stderr),
        Ran::Failed { code, detail, stderr } => {
            // Only the effect's own command failing to start withdraws its start; a reconcile that
            // cannot start says so under its own code, and the effect stays started.
            let code =
                if plan.step == Step::Reconcile && code == "spawn_failed" { "reconcile_spawn_failed" } else { code };
            return refuse(&mut guard, &held, (code, detail), &stderr);
        }
    };
    let Some(exit) = code else {
        return refuse(&mut guard, &held, ("verify_failed", "a signal ended the command".into()), &stderr);
    };
    let verify = VerifyRecord { argv: plan.argv, exit, stdout_sha256: line_hash(&stdout), ran_at: ran_at.to_string() };
    let unfit = match &held.node()?.class {
        Class::Observe { exit_map, .. } => (!exit_map.iter().any(|m| m.exit == exit))
            .then(|| ("exit_unmapped", format!("exit status {exit} is not in the node's exit_map"))),
        Class::Effect { .. } if plan.step == Step::Reconcile => (exit != 0).then(|| {
            let reason = format!("reconcile exited {exit}: the effect is not confirmed, and it is not run again");
            ("effect_unreconciled", reason)
        }),
        Class::Effect { .. } => {
            (exit != 0).then(|| ("effect_failed", format!("the effect exited {exit}; complete again to reconcile it")))
        }
        _ => (exit != 0).then(|| ("verify_failed", format!("the command exited {exit}"))),
    };
    if let Some(unfit) = unfit {
        return refuse(&mut guard, &held, unfit, &stderr);
    }
    let completed = receipt(&held, session, outputs, verify.clone());
    let appended = guard.append(&instance, held.log.seq(), completed.to_event()?)?;
    drop(guard);
    let verdicts = settled(ledger, session, &instance);
    Ok(Completion::Completed { appended, verify, stderr: String::from_utf8_lossy(&stderr).into_owned(), verdicts })
}

/// `limits`, with the timeout no longer than what is left before `cancel_at` (at least 1 ms, the
/// shortest a run may have; the check after the run refuses a claim past its deadline).
fn cancel_capped(limits: &Limits, cancel_at: Option<Timestamp>) -> Limits {
    let Some(at) = cancel_at else { return limits.clone() };
    let left = std::time::Duration::try_from(at.duration_since(clock::now())).unwrap_or_default();
    Limits { timeout: limits.timeout.min(left).max(std::time::Duration::from_millis(1)), ..limits.clone() }
}

/// Every precondition holds over `input`, evaluated in order, bounded: the digest of the input
/// they held over, or none when there are none. The first that does not hold is the refusal.
fn all_hold(preconditions: &[String], input: &Value) -> Result<Option<String>, Unfit> {
    if preconditions.is_empty() {
        return Ok(None);
    }
    for (i, code) in preconditions.iter().enumerate() {
        let Judged { holds, digest } = guard::judge(code, input, GUARD_TIMEOUT)?;
        if !holds {
            return Err(("precondition_false", format!("precondition {i} is false over the input {digest}")));
        }
    }
    Ok(Some(digest(input)))
}

/// Takes the edge `edge` of the choice node the claim `token` names. See the module docs.
///
/// **Needs a guard host**: a write that evaluates a guard (a choice's edge, an effect's
/// preconditions, an `until` verdict) is refused `guard_host_missing` unless the process called
/// [`guard_entry`](super::guard_entry) first in `main` (see `guard`).
pub fn choose(
    ledger: &mut Ledger,
    session: &VerifiedSession,
    token: &str,
    edge: &str,
) -> Result<Choosing, KernelError> {
    let (instance, sha) = parse_token(token)?;
    let mut guard = ledger.lock()?;
    let log = guard.log(&instance)?;
    let now = wall(&log)?;
    let held = hold(log, session, &instance, &sha, now)?;
    check_verb(&held, true)?;
    let reject = |guard: &mut WriteGuard, held: &Held, (code, reason): Unfit| {
        refused(guard, held, code, &reason).map(|appended| Choosing::Refused { appended, code, reason })
    };
    let code = match &held.node()?.class {
        Class::Choice { edges, .. } => edges.get(edge).cloned(),
        _ => None,
    };
    let Some(code) = code else {
        let reason = format!("node {} declares no such edge", held.rec.node);
        return reject(&mut guard, &held, ("undeclared_edge", reason));
    };
    // The guard runs with the lock released; then everything is checked under it, freshness
    // included. Its input is a function of the claim's inputs and the params, so a claim that
    // still holds read the same.
    let input = guard_input(&held.log, &held.started, &held.rec.node)?;
    drop(guard);
    let judged = guard::judge(&code, &input, GUARD_TIMEOUT);
    let mut guard = ledger.lock()?;
    let log = guard.log(&instance)?;
    let judged_at = wall(&log)?;
    let held = hold(log, session, &instance, &sha, judged_at)?;
    if let Err(unfit) = enabled(&held) {
        return reject(&mut guard, &held, unfit);
    }
    let Judged { holds, digest } = match judged {
        Ok(judged) => judged,
        Err(unfit) => return reject(&mut guard, &held, unfit),
    };
    if !holds {
        return reject(
            &mut guard,
            &held,
            ("guard_false", format!("the guard of edge {edge} is false over the input {digest}")),
        );
    }
    let chosen = Chosen {
        node: held.rec.node.clone(),
        iteration: held.rec.iteration.clone(),
        attempt: held.rec.attempt,
        edge: edge.to_string(),
        inputs: held.rec.inputs.clone(),
        tool_use_id: None,
        guard_digest: Some(digest.clone()),
    };
    let appended = guard.append(&instance, held.log.seq(), chosen.to_event()?)?;
    drop(guard);
    let verdicts = settled(ledger, session, &instance);
    Ok(Choosing::Chosen { appended, edge: edge.to_string(), guard_digest: digest, verdicts })
}

/// [`settle`] after a receipt is written: its errors become a verdict left unrecorded, since the
/// receipt stands either way.
fn settled(ledger: &mut Ledger, session: &VerifiedSession, instance: &InstanceId) -> Vec<Verdict> {
    settle(ledger, session, instance)
        .unwrap_or_else(|e| vec![Verdict::Unrecorded { repeat: None, code: e.code(), reason: e.to_string() }])
}

/// Writes the `until` verdict of each repeat that awaits one (see the module docs). The repeats
/// awaiting a verdict are read once, so a write with none costs one replay; each is then
/// evaluated with the lock released and appended by [`record`] only if it still applies. A
/// verdict that cannot be had, or whose input changed while it ran, is reported and left to the
/// next write, and so is a repeat that a verdict written here lets settle.
fn settle(ledger: &mut Ledger, session: &VerifiedSession, instance: &InstanceId) -> Result<Vec<Verdict>, KernelError> {
    let (started, pending) = {
        let guard = ledger.lock()?;
        let log = guard.log(instance)?;
        let started = pinned(&log)?;
        if started.ir.repeats.values().all(|r| r.until.is_none()) {
            return Ok(Vec::new());
        }
        check_owner(&log, &started, instance, session)?;
        let pending = awaiting(&log, &started)?;
        (started, pending)
    };
    let mut out = Vec::new();
    for next in pending {
        let judged = guard::judge(&next.until, &next.input, GUARD_TIMEOUT);
        // The clock is judged where the verdict is written: after the until ran, under the lock.
        let mut guard = ledger.lock()?;
        let log = guard.log(instance)?;
        // The instance may have changed owner while the until ran.
        check_owner(&log, &started, instance, session)?;
        wall(&log)?;
        let verdict = match judged {
            Err((code, reason)) => Verdict::Unrecorded { repeat: Some(next.repeat), code, reason },
            Ok(judged) => match record(&mut guard, &log, &started, &next, &judged)? {
                Some(appended) => {
                    let (repeat, iteration, holds) = (next.repeat, next.iteration, judged.holds);
                    Verdict::Recorded { repeat, iteration, holds, appended }
                }
                None => {
                    let reason = "the repeat's body changed while its until ran".to_string();
                    Verdict::Unrecorded { repeat: Some(next.repeat), code: "verdict_stale", reason }
                }
            },
        };
        out.push(verdict);
    }
    Ok(out)
}

/// Appends `judged`, the verdict on `awaited`, if the log as it is now still awaits a verdict for
/// that repeat and iteration over an input with the digest it was evaluated over.
pub(super) fn record(
    guard: &mut WriteGuard,
    log: &Log,
    started: &Started,
    awaited: &Awaiting,
    judged: &Judged,
) -> Result<Option<Appended>, KernelError> {
    let still = awaiting(log, started)?
        .into_iter()
        .any(|a| a.repeat == awaited.repeat && a.iteration == awaited.iteration && digest(&a.input) == judged.digest);
    if !still {
        return Ok(None);
    }
    let verdict = Guarded {
        repeat: awaited.repeat.clone(),
        iteration: awaited.iteration,
        holds: judged.holds,
        digest: Some(judged.digest.clone()),
    };
    Ok(Some(guard.append(&log.instance, log.seq(), verdict.to_event()?)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::compile;

    #[test]
    fn params_fill_a_command_and_nothing_else_does() {
        let flow = compile("flow: f\nversion: 1\nparams: [a, n?, o?]\nnodes:\n  x: {class: pure, run: [x]}\n").unwrap();
        let ir = flow.ir();
        let params = serde_json::json!({ "a": "A b", "n": 3, "o": [1] });
        let all = usize::MAX;
        assert_eq!(fill(ir, &params, "<{{params.a}}|{{params.n}}>", all).unwrap(), "<A b|3>");
        assert_eq!(fill(ir, &serde_json::json!({ "a": "x" }), "[{{params.n}}]", all).unwrap(), "[]");
        assert_eq!(fill(ir, &params, "no braces", all).unwrap(), "no braces");
        for bad in ["{{params.zz}}", "{{a}}", "{{params.a", "{{params.o}}"] {
            assert!(fill(ir, &params, bad, all).is_err(), "{bad}");
        }
        // The budget holds for literal text and for params, to the byte.
        assert_eq!(fill(ir, &params, "abc", 3).unwrap(), "abc");
        assert!(fill(ir, &params, "abcd", 3).is_err());
        assert_eq!(fill(ir, &params, "{{params.a}}", 3).unwrap(), "A b");
        assert!(fill(ir, &params, "x{{params.a}}", 3).is_err());
    }

    #[test]
    fn outputs_are_an_object_within_their_bounds() {
        let nested = |levels: usize| (1..levels).fold(serde_json::json!({}), |v, _| serde_json::json!({ "k": v }));
        assert!(check_outputs(&Value::Null).is_ok());
        assert!(check_outputs(&serde_json::json!({ "a": 1 })).is_ok());
        assert_eq!(check_outputs(&serde_json::json!([1])).unwrap_err().0, "outputs_invalid");
        assert!(check_outputs(&nested(MAX_NESTING - 1)).is_ok());
        assert_eq!(check_outputs(&nested(MAX_NESTING)).unwrap_err().0, "outputs_invalid");
        let fits = serde_json::json!({ "s": "x".repeat(MAX_OUTPUTS - 8) });
        assert_eq!(serde_json::to_vec(&fits).unwrap().len(), MAX_OUTPUTS);
        assert!(check_outputs(&fits).is_ok());
        let over = serde_json::json!({ "s": "x".repeat(MAX_OUTPUTS - 7) });
        assert_eq!(check_outputs(&over).unwrap_err().0, "outputs_invalid");
    }
}
