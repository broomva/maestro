//! The writers that take a node from enabled to done: [`claim`], [`heartbeat`] and [`complete`].
//! They fulfil the contract `replay` leaves to them (see its module docs).
//!
//! # Ownership
//!
//! An instance is owned by the session that started it: its `SessionRef` and the digest of its
//! owner marker, together, recorded in `Started`. Every write verb takes the caller's
//! [`VerifiedSession`] and refuses unless both match: a session with no marker writes nothing
//! (`no_owner_marker`), another session writes nothing (`not_owner`), and neither does a
//! replacement of the owner, which keeps the `SessionRef` with a new marker. A replacement may
//! start an instance of its own. There is no transfer or adoption.
//!
//! # Claims, tokens and leases
//!
//! - `claim` takes an enabled node of a kernel-run class (pure, observe, work) at its current
//!   iteration and appends `Claimed`: the attempt, a lease, the input digests `next` names for the
//!   node, and the sha256 of a new token. The token goes back to the caller once and is never
//!   stored, nor returned by `next`, `status` or replay.
//! - The token is a handle, not a capability: every use checks ownership first, so a token in
//!   another session's hands grants nothing, nor in a replacement's (the same `SessionRef` with a
//!   new marker). A resumed or restarted session keeps its marker (it survives turns, provider
//!   switches and restarts), so it may heartbeat and complete its own claim until the lease
//!   ends: the lease is the bound on a restart.
//! - A claim holds its node while it is the node's latest claim, its lease has not ended, it is
//!   not spent, its iteration is current and the receipts it was bound to are still the node's
//!   inputs. While one holds, the node cannot be claimed again (`already_claimed`).
//! - `heartbeat` moves the lease end to now plus the node's lease. Once the lease ends, the
//!   token is void: `complete` and `heartbeat` refuse `lease_expired`, the node is released, and
//!   work goes on only through a new claim, the next attempt.
//! - Attempts count claims per node and iteration. A refusal leaves the attempt as it is; the next
//!   claim takes the next number, with checked arithmetic (`attempt_overflow`).
//!
//! # Complete
//!
//! `complete` checks the claim, then that the node is still enabled now (everything it consumes
//! current and fresh) and that the outputs fit, then runs the node's own command: a pure node's
//! `run`, a work node's `verify`, an observe node's `run` (its value is its exit status through
//! `exit_map`; any other status fails closed). The flows lock is not held while the command runs:
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

use super::error::{KernelError, Refusal};
use super::events::{
    Claimed, Completed, Heartbeat, KernelEvent, Owner, Performer, Refused, Started, VerifyRecord, marker_digest,
};
use super::exec::{self, Limits, Ran};
use super::ir::{Class, Ir, Iteration, Node};
use super::replay::{Next, State, next_of, pinned, secs};
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
    },
    Refused {
        appended: Appended,
        code: &'static str,
        reason: String,
        stderr: String,
    },
}

/// The owner a session would be: its `SessionRef` and the digest of its marker.
pub(crate) fn owner(session: &VerifiedSession) -> Result<Owner, Refusal> {
    let marker = session.owner_marker().filter(|m| !m.is_empty()).ok_or(Refusal::NoOwnerMarker)?;
    Ok(Owner { session: session.session().clone(), marker_sha256: marker_digest(marker) })
}

/// Refuses any session but the instance's owner, with the same marker.
fn check_owner(started: &Started, instance: &InstanceId, session: &VerifiedSession) -> Result<(), Refusal> {
    let me = owner(session)?;
    let Some(owner) = &started.owner else {
        return Err(Refusal::NoOwner { instance: instance.to_string() });
    };
    if *owner != me {
        return Err(Refusal::NotOwner { instance: instance.to_string() });
    }
    Ok(())
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
    node: String,
    iteration: Iteration,
    attempt: u32,
    lease_until: Timestamp,
    inputs: BTreeMap<String, String>,
    spent: bool,
}

/// Every claim in an instance's log, folded in log order.
struct Claims {
    /// By token digest.
    recs: BTreeMap<String, ClaimRec>,
    /// The latest claim's token digest for each node and iteration.
    latest: BTreeMap<(String, Iteration), String>,
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
fn wall(log: &Log) -> Result<Timestamp, KernelError> {
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
    /// the lease of the latest claim of its node and iteration, and a `Completed` spends that
    /// claim, each only when it is for the claim's attempt.
    fn fold(log: &Log) -> Result<Claims, KernelError> {
        let mut claims = Claims { recs: BTreeMap::new(), latest: BTreeMap::new() };
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
                    let rec = ClaimRec {
                        node: c.node,
                        iteration: c.iteration,
                        attempt: c.attempt,
                        lease_until,
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
                    if let Some(rec) = claims.latest_mut(&(c.node, c.iteration))
                        && rec.attempt == c.attempt
                    {
                        rec.spent = true;
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
pub fn claim(
    ledger: &mut Ledger,
    session: &VerifiedSession,
    instance: &InstanceId,
    node: &str,
) -> Result<Claim, KernelError> {
    let mut guard = ledger.lock()?;
    let log = guard.log(instance)?;
    let started = pinned(&log)?;
    check_owner(&started, instance, session)?;
    let now = wall(&log)?;
    let Some(n) = started.ir.nodes.get(node) else {
        return Err(Refusal::UnknownNode { node: node.to_string() }.into());
    };
    if !matches!(n.class, Class::Pure { .. } | Class::Observe { .. } | Class::Work { .. }) {
        return Err(Refusal::NotClaimable { node: node.to_string(), class: n.class.name().to_string() }.into());
    }
    let next = next_of(&log, &started, now)?;
    let Some(enabled) = next.enabled.iter().find(|e| e.node == node) else {
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
    let lease_until = until(now, lease(n))?;
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
    check_owner(&started, instance, session)?;
    let claims = Claims::fold(&log)?;
    let Some(rec) = claims.recs.get(sha).cloned() else {
        return Err(Refusal::UnknownToken { instance: instance.to_string() }.into());
    };
    if rec.spent {
        return Err(Refusal::TokenSpent { node: rec.node }.into());
    }
    let latest = claims.latest.get(&(rec.node.clone(), rec.iteration.clone())).map(String::as_str);
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
    Ok(Held { log, started, rec, next })
}

/// Extends the lease of the claim `token` names. See the module docs.
pub fn heartbeat(ledger: &mut Ledger, session: &VerifiedSession, token: &str) -> Result<Beat, KernelError> {
    let (instance, sha) = parse_token(token)?;
    let mut guard = ledger.lock()?;
    let log = guard.log(&instance)?;
    let now = wall(&log)?;
    let held = hold(log, session, &instance, &sha, now)?;
    let lease_until = until(now, lease(held.node()?))?;
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
}

/// Why the work is refused: a code and a reason, appended as `Refused`.
type Unfit = (&'static str, String);

/// Checks the work against the node as the log has it now: enabled (so everything it consumes is
/// current and fresh), outputs that fit, and a command that can run.
fn prepare(held: &Held, outputs: &Value) -> Result<Plan, Unfit> {
    check_outputs(outputs)?;
    if !held.next.enabled.iter().any(|e| e.node == held.rec.node) {
        let state = state_name(held.next.pending.get(&held.rec.node).map(|s| s.state));
        return Err((
            "input_stale",
            format!("node {} is {state} now: something it consumes is not fresh", held.rec.node),
        ));
    }
    let ir = &held.started.ir;
    let words = match held.started.ir.nodes.get(&held.rec.node).map(|n| &n.class) {
        Some(Class::Pure { run }) | Some(Class::Observe { run, .. }) => run,
        Some(Class::Work { verify, .. }) => verify,
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
    Ok(Plan { argv, cwd })
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
    let refused = Refused {
        node: held.rec.node.clone(),
        iteration: held.rec.iteration.clone(),
        attempt: held.rec.attempt,
        code: code.to_string(),
        reason: reason.clone(),
    };
    let appended = guard.append(&held.log.instance, held.log.seq(), refused.to_event()?)?;
    Ok(Completion::Refused { appended, code, reason, stderr: String::from_utf8_lossy(stderr).into_owned() })
}

/// Completes the claim `token` names: runs the node's command and appends the receipt, or a
/// refusal. See the module docs.
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
    let held = hold(log, session, &instance, &sha, now)?;
    let plan = match prepare(&held, &outputs) {
        Ok(plan) => plan,
        Err(unfit) => return refuse(&mut guard, &held, unfit, &[]),
    };
    if let Err(unfit) = check_receipt(&held, session, &outputs, &plan.argv) {
        return refuse(&mut guard, &held, unfit, &[]);
    }
    // The command runs with the flows lock released, so other writers and readers go on.
    drop(guard);
    let ran_at = clock::now();
    let ran = exec::run(&plan.argv, &plan.cwd, limits);

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
        Ran::Failed { code, detail, stderr } => return refuse(&mut guard, &held, (code, detail), &stderr),
    };
    let Some(exit) = code else {
        return refuse(&mut guard, &held, ("verify_failed", "a signal ended the command".into()), &stderr);
    };
    let verify = VerifyRecord { argv: plan.argv, exit, stdout_sha256: line_hash(&stdout), ran_at: ran_at.to_string() };
    let unfit = match &held.node()?.class {
        Class::Observe { exit_map, .. } => (!exit_map.iter().any(|m| m.exit == exit))
            .then(|| ("exit_unmapped", format!("exit status {exit} is not in the node's exit_map"))),
        _ => (exit != 0).then(|| ("verify_failed", format!("the command exited {exit}"))),
    };
    if let Some(unfit) = unfit {
        return refuse(&mut guard, &held, unfit, &stderr);
    }
    let completed = receipt(&held, session, outputs, verify.clone());
    let appended = guard.append(&instance, held.log.seq(), completed.to_event()?)?;
    Ok(Completion::Completed { appended, verify, stderr: String::from_utf8_lossy(&stderr).into_owned() })
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
