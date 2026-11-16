//! The stop gate's kernel half (flow kernel spec, "L4: gates"; BRO-3636): whether an instance has
//! work its owner can do now, the instances a session owns that are open, and the record of a stop
//! the gate could not refuse.
//!
//! # The predicate
//!
//! [`assess`] is the one place that decides it. The stop gate refuses a stop while one of the
//! session's instances has [`Work`], and `flow next` and `flow status` return the same [`Work`]
//! and [`Waiting`] lists, which the driver skill's step 4 reads to decide when nothing is its to do
//! (`driver`). So the gate and the skill never judge it apart. The work its owner can do now:
//!
//! - an enabled node, unless a former owner's claim holds it until its lease ends: claim it and
//!   finish it (`choose` for a choice node, `complete` for any other), or finish (or heartbeat)
//!   the claim it already holds;
//! - a repeat whose `until` verdict is pending: a claim judges it again;
//! - a human node whose ask is not filed: `flow ask` files it;
//! - a human node whose answer this clone sees on `origin/main`: `flow ask` takes it.
//!
//! A retired instance has none, moved to `retired/` or not yet. An instance that waits only on
//! someone else is not work, or a session could never end. Someone
//! else is the owner, whose answer to a filed ask has not landed (unanswered, or on an answers
//! branch only, `asks::seen`), or a former owner, whose claim still holds a node (`write`). A
//! terminal instance has no work, since nothing in it is ready, stale, waiting or awaiting a verdict.
//!
//! # The open instances
//!
//! [`open`] lists the instances the caller's verified session owns (its `SessionRef` and owner
//! marker together, as every write checks them) that are neither terminal nor retired, each with
//! its assessment. It reads under the shared lock and writes nothing. An instance whose log does
//! not read is listed as unread with its code, never left out: unread is not empty.
//!
//! # The loop bound
//!
//! A gate that refuses forever traps a session. [`Refusals`] counts, per session, how many stops in
//! a row the gate refused over each instance at one log position (its `seq`). The count starts
//! again whenever the instance's `seq` moves, and an instance the gate no longer refuses over
//! drops out. Once an instance has been refused [`MAX_REFUSALS`] times at one position, the next
//! stop over it goes through and the gate records `no_progress` ([`Refusals::tally`]).
//!
//! The counts are one JSON file per session under the ledger's own root, `stop-gate/<digest>.json`
//! (named by the digest of the session's `SessionRef` and owner marker, so a replacement starts
//! afresh), read and written through the root's descriptor, never through a link, as every file
//! of the ledger is. They are loaded, tallied and saved under the ledger's write lock, so the
//! lock's own checks bind them: a ledger of another host is `foreign_host`, a root moved or of
//! another epoch is refused, and two stops at once are both counted. The file records the epoch
//! it was taken in, and counts of another epoch are no counts. They move with the root, so
//! `operator::archive` takes them away and `create_fresh_ledger` starts with none. The file is
//! replaced whole (written aside, then renamed), so a crash leaves the old counts or the new
//! ones, and a stop with nothing to count writes nothing.
//!
//! # The record
//!
//! [`record`] appends `StopUnblockable` (harness, session, open instances, reason) to an instance,
//! as its owner, with the checks every write makes. It changes no node: replay lists it and
//! `status` reports it. A record whose instance has not moved since the same session's last record
//! for the same reason is not written again ([`Recorded::Already`]).

use super::adoption;
use super::asks::AnswerSeen;
use super::error::KernelError;
use super::events::{KernelEvent, StopUnblockable, Unblockable};
use super::ir::Iteration;
use super::replay::{Reason, State, pinned, read_both};
use super::write::{check_owner, live_claims, owner, wall};
use crate::at::Dir;
use crate::ids::InstanceId;
use crate::layout::STOP_GATE;
use crate::ledger::{Appended, Ledger, ReadGuard, WriteGuard, read_beside, write_beside};
use crate::log::Log;
use jiff::Timestamp;
use maestro_core::session::VerifiedSession;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Duration;

/// The key under which `flow next` and `flow status` return an instance's [`Work`], beside
/// `next` or `status`; the driver skill's step 4 reads it.
pub const WORK: &str = "work";

/// The key under which they return what the instance waits on ([`Waiting`]).
pub const WAITING: &str = "waiting";

/// How many times in a row the gate refuses one session at one instance's log position before it
/// lets the next stop through and records `no_progress` (the owner's bound, BRO-3636).
pub const MAX_REFUSALS: u32 = 3;

/// The most instances one `StopUnblockable` names, so its line stays within the log's bound.
pub const MAX_NAMED: usize = 64;

/// Something the instance's owner can do now (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Work {
    /// An enabled node, of `class` (its IR class's name). `held` when its owner holds a live claim
    /// on it: finish that claim, with `choose` for a choice node and `complete` for any other.
    Node { node: String, class: String, iteration: Iteration, held: bool },
    /// A repeat awaiting its `until` verdict: a claim of `node`, its body's first node, judges it.
    Verdict { repeat: String, iteration: u32, node: String },
    /// A human node whose ask is not filed.
    Ask { node: String, iteration: Iteration },
    /// A human node whose answer this clone sees on `origin/main`: `flow ask` takes it.
    Answer { node: String, iteration: Iteration, ask_id: String, ledger: String },
}

/// Someone else the instance waits on (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Waiting {
    /// The owner's answer to a filed ask; `landing` names the answers branch it waits on, if any.
    Owner {
        node: String,
        iteration: Iteration,
        ask_id: String,
        ledger: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        landing: Option<String>,
    },
    /// A former owner's claim holds an enabled node until `lease_until`.
    Lease { node: String, iteration: Iteration, lease_until: String },
}

/// One instance, judged for its owner at a clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Assessment {
    pub instance: String,
    pub flow: String,
    pub ir_hash: String,
    pub seq: u64,
    pub terminal: bool,
    pub work: Vec<Work>,
    pub waiting: Vec<Waiting>,
}

impl Assessment {
    /// The gate refuses a stop over this instance: its owner has work it can do now.
    pub fn refuses_stop(&self) -> bool {
        !self.work.is_empty()
    }

    /// What the agent does next, one line per item, each naming the verb.
    pub fn next_steps(&self) -> Vec<String> {
        let i = &self.instance;
        self.work
            .iter()
            .map(|w| match w {
                Work::Node { node, class, held: false, .. } => format!(
                    "node {node} is enabled: claim it with `maestro flow claim --instance {i} --node-id {node}` and {} (`maestro flow next --instance {i} --json` says how)",
                    finish(class, "the claim token it returns")
                ),
                Work::Node { node, class, held: true, .. } => format!(
                    "you hold a live claim on node {node}: {}, or heartbeat it while the work runs",
                    finish(class, "its token")
                ),
                Work::Verdict { repeat, node, .. } => format!(
                    "repeat {repeat} awaits its until verdict: retry it with `maestro flow claim --instance {i} --node-id {node}`"
                ),
                Work::Ask { node, .. } => format!(
                    "human node {node} has no ask filed: file it with `maestro flow ask --instance {i} --node-id {node} --ledger <ledger> --ask '<json>'`"
                ),
                Work::Answer { node, ask_id, .. } => format!(
                    "the owner's answer to ask {ask_id} is on main: take it with `maestro flow ask --instance {i} --node-id {node}`"
                ),
            })
            .collect()
    }
}

/// How a claim on a node of `class` is finished: a choice node takes one of its edges with
/// `choose` (`complete` refuses it, `wrong_verb`); every other class is completed.
fn finish(class: &str, token: &str) -> String {
    if class == "choice" {
        format!("choose one of its edges with `maestro flow choose --claim-token <{token}> --edge <edge>`")
    } else {
        format!("complete it with `maestro flow complete --claim-token <{token}>`")
    }
}

/// Judges one instance for its owner at `now` (see the module docs). `seen` says where a filed
/// ask's answer stands (`asks::seen` in production). Reads `log` only.
pub(crate) fn assess(
    log: &Log,
    started: &super::events::Started,
    now: Timestamp,
    seen: &dyn Fn(&str, &str) -> AnswerSeen,
) -> Result<Assessment, KernelError> {
    let (status, next) = read_both(log, started, now)?;
    let owners = adoption::owners(log, started)?;
    let since = owners.since;
    let live = live_claims(log, now)?;
    let mut work = Vec::new();
    let mut waiting = Vec::new();
    // A retired instance takes no write, so nothing in it is anyone's to do, moved or not.
    let retired = owners.retired.is_some();
    for e in &next.enabled {
        let held = live.iter().find(|c| c.node == e.node && c.iteration == e.iteration && c.inputs == e.inputs);
        match held {
            // A claim the former owner took holds the node for nobody until its lease ends.
            Some(c) if c.seq < since => waiting.push(Waiting::Lease {
                node: e.node.clone(),
                iteration: e.iteration.clone(),
                lease_until: c.lease_until.to_string(),
            }),
            held => work.push(Work::Node {
                node: e.node.clone(),
                class: e.class.clone(),
                iteration: e.iteration.clone(),
                held: held.is_some(),
            }),
        }
    }
    for (rid, r) in &status.repeats {
        let first = started.ir.repeats.get(rid).and_then(|r| r.body.first());
        if let (true, Some(node)) = (r.awaiting_verdict, first) {
            work.push(Work::Verdict { repeat: rid.clone(), iteration: r.iteration, node: node.clone() });
        }
    }
    for (id, s) in status.nodes.iter().filter(|(_, s)| s.state == State::Waiting) {
        for reason in &s.unmet {
            let iteration = s.iteration.clone();
            match reason {
                Reason::Unfiled { .. } => work.push(Work::Ask { node: id.clone(), iteration }),
                Reason::Owner { ask_id, ledger, .. } | Reason::AnswerWaitingToLand { ask_id, ledger, .. } => {
                    let (node, ask_id, ledger) = (id.clone(), ask_id.clone(), ledger.clone());
                    match seen(&ledger, &ask_id) {
                        AnswerSeen::Landed => work.push(Work::Answer { node, iteration, ask_id, ledger }),
                        AnswerSeen::Landing(at) => {
                            waiting.push(Waiting::Owner { node, iteration, ask_id, ledger, landing: Some(at) })
                        }
                        AnswerSeen::Open => {
                            waiting.push(Waiting::Owner { node, iteration, ask_id, ledger, landing: None })
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if retired {
        work.clear();
        waiting.clear();
    }
    Ok(Assessment {
        instance: status.instance,
        flow: status.flow,
        ir_hash: status.ir_hash,
        seq: status.seq,
        terminal: status.terminal,
        work,
        waiting,
    })
}

/// [`assess`] of one instance, read under the shared lock. Writes nothing.
pub fn assess_instance(
    guard: &ReadGuard,
    instance: &InstanceId,
    now: Timestamp,
    seen: &dyn Fn(&str, &str) -> AnswerSeen,
) -> Result<Assessment, KernelError> {
    let log = guard.log(instance)?;
    let started = pinned(&log)?;
    assess(&log, &started, now, seen)
}

/// An instance [`open`] could not read: its code and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Unread {
    pub instance: String,
    pub code: String,
    pub reason: String,
}

/// The open instances a session owns (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Open {
    pub session: String,
    pub instances: Vec<Assessment>,
    /// Instances whose log did not read: whose they are is unknown.
    pub unread: Vec<Unread>,
}

impl Open {
    /// The instances the gate refuses a stop over.
    pub fn refusing(&self) -> impl Iterator<Item = &Assessment> {
        self.instances.iter().filter(|a| a.refuses_stop())
    }
}

/// The instances `session` owns that are neither terminal nor retired, each judged at `now`.
/// Reads under the shared lock and writes nothing. A session with no owner marker owns nothing
/// and is refused `no_owner_marker`, as every write refuses it.
pub fn open(
    guard: &ReadGuard,
    session: &VerifiedSession,
    now: Timestamp,
    seen: &dyn Fn(&str, &str) -> AnswerSeen,
) -> Result<Open, KernelError> {
    let me = owner(session)?;
    let mut out = Open { session: session.session().to_string(), instances: Vec::new(), unread: Vec::new() };
    for id in guard.instances()? {
        let judged = || -> Result<Option<Assessment>, KernelError> {
            let log = guard.log(&id)?;
            let started = pinned(&log)?;
            let owners = adoption::owners(&log, &started)?;
            if owners.owner.as_ref() != Some(&me) || owners.retired.is_some() {
                return Ok(None);
            }
            Ok(Some(assess(&log, &started, now, seen)?).filter(|a| !a.terminal))
        };
        match judged() {
            Ok(Some(a)) => out.instances.push(a),
            Ok(None) => {}
            Err(e) => {
                out.unread.push(Unread { instance: id.to_string(), code: e.code().to_string(), reason: e.to_string() })
            }
        }
    }
    Ok(out)
}

/// What [`record`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum Recorded {
    Appended(Appended),
    /// The instance has not moved since this session's last record for this reason, at `seq`.
    Already {
        seq: u64,
    },
}

/// Appends `StopUnblockable` to `instance` as its owner (see the module docs): refused as every
/// write is (`not_owner`, `no_owner_marker`, `retired`, `clock_behind`). `open_instances` is cut
/// to [`MAX_NAMED`].
pub fn record(
    ledger: &mut Ledger,
    session: &VerifiedSession,
    instance: &InstanceId,
    harness: &str,
    reason: Unblockable,
    open_instances: &[String],
) -> Result<Recorded, KernelError> {
    let mut guard = ledger.lock()?;
    let log = guard.log(instance)?;
    let started = pinned(&log)?;
    check_owner(&log, &started, instance, session)?;
    wall(&log)?;
    if let Some(last) = log.events.last().filter(|e| e.kind == StopUnblockable::KIND) {
        let prior: Option<StopUnblockable> = serde_json::from_value(last.body.clone()).ok();
        if prior.is_some_and(|p| p.session == *session.session() && p.reason == reason) {
            return Ok(Recorded::Already { seq: last.seq });
        }
    }
    let event = StopUnblockable {
        harness: harness.to_string(),
        session: session.session().clone(),
        open_instances: open_instances.iter().take(MAX_NAMED).cloned().collect(),
        reason,
    };
    Ok(Recorded::Appended(guard.append(instance, log.seq(), event.to_event()?)?))
}

/// The block reason for `refusing`: what the agent does next, at most `max` instances named.
pub fn block_reason(refusing: &[&Assessment], max: usize) -> String {
    let mut out = String::from("Maestro's stop gate: a flow instance you own is open and has work you can do now.");
    for a in refusing.iter().take(max) {
        let _ = write!(out, " Instance {} ({}):", a.instance, a.flow);
        for step in a.next_steps() {
            let _ = write!(out, " {step};");
        }
        out.pop();
        out.push('.');
    }
    if refusing.len() > max {
        let _ = write!(out, " And {} more: `maestro flow open --json` lists them.", refusing.len() - max);
    }
    out.push_str(
        " Do it before you stop. If something outside your reach blocks it, say what blocks it in your reply.",
    );
    out
}

/// How long the gate waits for the ledger's write lock to count a stop before it fails open. The
/// lock is held for a load, a tally and a save, as briefly as any append.
pub const REFUSALS_WAIT: Duration = Duration::from_secs(2);

/// The most bytes of a refusal-count file the gate reads.
pub const MAX_REFUSALS_FILE: u64 = 1 << 20;

/// How many stops in a row the gate refused over one instance at one log position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Count {
    pub seq: u64,
    pub refusals: u32,
}

/// What [`Refusals::tally`] decided for the instances the gate would refuse over.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tally {
    /// Refused again: the stop is refused over these.
    pub refused: Vec<String>,
    /// Refused [`MAX_REFUSALS`] times at this position already: the stop goes through over these.
    pub spent: Vec<String>,
}

/// One session's refusal counts in one ledger, by instance (see the module docs). Loaded and saved
/// under that ledger's write lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusals {
    name: String,
    pub counts: BTreeMap<String, Count>,
    /// Whether it held no counts when it was loaded: then an empty tally writes nothing.
    was_empty: bool,
}

#[derive(Serialize, Deserialize)]
struct RefusalsFile {
    session: String,
    /// The ledger's epoch the counts were taken in: counts of another epoch are no counts.
    epoch: String,
    counts: BTreeMap<String, Count>,
}

/// The file name of `session`'s counts: the digest of its `SessionRef` and owner marker.
fn file_name(session: &VerifiedSession) -> Result<String, KernelError> {
    let me = owner(session)?;
    let key = crate::log::line_hash(format!("{}\n{}", me.session, me.marker_sha256).as_bytes());
    Ok(format!("{}.json", key.trim_start_matches("sha256:").get(..32).unwrap_or_default()))
}

/// The counts in the file `name` under `root`, if they were taken in `epoch`; none otherwise.
fn counts_in(root: &Dir, epoch: &str, name: &str) -> Result<BTreeMap<String, Count>, KernelError> {
    let Some(bytes) = read_beside(root, STOP_GATE, name, MAX_REFUSALS_FILE, "refusal-count file")? else {
        return Ok(BTreeMap::new());
    };
    let parsed: RefusalsFile = serde_json::from_slice(&bytes)
        .map_err(|e| KernelError::Encode(format!("{STOP_GATE}/{name} does not parse ({e})")))?;
    Ok(if parsed.epoch == epoch { parsed.counts } else { BTreeMap::new() })
}

impl Refusals {
    /// `session`'s counts in the ledger `guard` holds exclusive, or none yet: one load, its tally
    /// and its save are one step under that lock, so two stops never both read one count. A file
    /// that does not read is an error, never no counts; counts taken in another epoch are none.
    pub fn load(guard: &WriteGuard<'_>, session: &VerifiedSession) -> Result<Refusals, KernelError> {
        let name = file_name(session)?;
        let counts = counts_in(guard.root(), guard.epoch(), &name)?;
        Ok(Refusals { name, was_empty: counts.is_empty(), counts })
    }

    /// Whether `session` has counts in the ledger `guard` reads (shared lock): a stop with nothing
    /// to refuse writes only to clear them.
    pub fn any(guard: &ReadGuard, session: &VerifiedSession) -> Result<bool, KernelError> {
        let name = file_name(session)?;
        Ok(!counts_in(guard.root(), guard.epoch(), &name)?.is_empty())
    }

    /// Counts one more stop over `refusing`, the instances the gate would refuse over now: each
    /// is refused again unless it was refused [`MAX_REFUSALS`] times at its current `seq` already.
    /// A count at another `seq` starts again, and an instance not in `refusing` drops out.
    pub fn tally(&mut self, refusing: &[&Assessment]) -> Tally {
        let mut tally = Tally::default();
        let mut counts = BTreeMap::new();
        for a in refusing {
            let prior = self.counts.get(&a.instance).filter(|c| c.seq == a.seq).map_or(0, |c| c.refusals);
            let refusals = if prior >= MAX_REFUSALS {
                tally.spent.push(a.instance.clone());
                prior
            } else {
                tally.refused.push(a.instance.clone());
                prior + 1
            };
            counts.insert(a.instance.clone(), Count { seq: a.seq, refusals });
        }
        self.counts = counts;
        tally
    }

    /// Writes the counts, whole, through the root `guard` holds: aside, then renamed over the old
    /// file, never through a link. Counts that were empty and stay empty write nothing.
    pub fn save(&self, guard: &WriteGuard<'_>, session: &VerifiedSession) -> Result<(), KernelError> {
        if self.was_empty && self.counts.is_empty() {
            return Ok(());
        }
        let body = RefusalsFile {
            session: session.session().to_string(),
            epoch: guard.epoch().to_string(),
            counts: self.counts.clone(),
        };
        let bytes = serde_json::to_vec(&body).map_err(|e| KernelError::Encode(e.to_string()))?;
        Ok(write_beside(guard.root(), STOP_GATE, &self.name, &bytes)?)
    }
}
