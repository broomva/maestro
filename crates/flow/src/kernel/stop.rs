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
//!   complete it, or complete (or heartbeat) the claim it already holds;
//! - a repeat whose `until` verdict is pending: a claim judges it again;
//! - a human node whose ask is not filed: `flow ask` files it;
//! - a human node whose answer this clone sees on `origin/main`: `flow ask` takes it.
//!
//! An instance that waits only on someone else is not work, or a session could never end. Someone
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
//! The counts are one JSON file per session, `flows.stop-gate/<digest>.json` beside the flows root
//! ([`REFUSALS_DIR`]), where the ADR keeps the ledger's coordination files: they count against
//! that ledger's instance logs, and so share its host and its lifetime. Not inside the root, whose
//! instance folders hold only their hash-chained logs. One file per session (named by the digest of
//! its `SessionRef` and owner marker, so a replacement starts afresh) means no two sessions write
//! one file, and a session's own stops run one at a time. The file is replaced whole (written
//! aside, then renamed), so a crash leaves the old counts or the new ones.
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
use crate::ids::InstanceId;
use crate::ledger::{Appended, Ledger, ReadGuard};
use crate::log::Log;
use jiff::Timestamp;
use maestro_core::session::VerifiedSession;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

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
    /// An enabled node. `held` when its owner holds a live claim on it: complete that claim.
    Node { node: String, iteration: Iteration, held: bool },
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
                Work::Node { node, held: false, .. } => format!(
                    "node {node} is enabled: claim it with `maestro flow claim --instance {i} --node-id {node}` and complete it (`maestro flow next --instance {i} --json` says how)"
                ),
                Work::Node { node, held: true, .. } => format!(
                    "you hold a live claim on node {node}: finish it with `maestro flow complete --claim-token <its token>`, or heartbeat it while the work runs"
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

/// Judges one instance for its owner at `now` (see the module docs). `seen` says where a filed
/// ask's answer stands (`asks::seen` in production). Reads `log` only.
pub(crate) fn assess(
    log: &Log,
    started: &super::events::Started,
    now: Timestamp,
    seen: &dyn Fn(&str, &str) -> AnswerSeen,
) -> Result<Assessment, KernelError> {
    let (status, next) = read_both(log, started, now)?;
    let since = adoption::owners(log, started)?.since;
    let live = live_claims(log, now)?;
    let mut work = Vec::new();
    let mut waiting = Vec::new();
    for e in &next.enabled {
        let held = live.iter().find(|c| c.node == e.node && c.iteration == e.iteration && c.inputs == e.inputs);
        match held {
            // A claim the former owner took holds the node for nobody until its lease ends.
            Some(c) if c.seq < since => waiting.push(Waiting::Lease {
                node: e.node.clone(),
                iteration: e.iteration.clone(),
                lease_until: c.lease_until.to_string(),
            }),
            held => {
                work.push(Work::Node { node: e.node.clone(), iteration: e.iteration.clone(), held: held.is_some() })
            }
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

/// The folder beside the flows root that holds the gate's refusal counts (see the module docs).
pub const REFUSALS_DIR: &str = "flows.stop-gate";

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

/// One session's refusal counts, by instance (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusals {
    path: PathBuf,
    pub counts: BTreeMap<String, Count>,
}

#[derive(Serialize, Deserialize)]
struct RefusalsFile {
    session: String,
    counts: BTreeMap<String, Count>,
}

impl Refusals {
    /// Where `session`'s counts live for the ledger at `root`.
    pub fn path(root: &Path, session: &VerifiedSession) -> Result<PathBuf, KernelError> {
        let me = owner(session)?;
        let key = crate::log::line_hash(format!("{}\n{}", me.session, me.marker_sha256).as_bytes());
        let hex = key.trim_start_matches("sha256:").get(..32).unwrap_or_default().to_string();
        let dir = root.parent().map(|p| p.join(REFUSALS_DIR)).unwrap_or_else(|| PathBuf::from(REFUSALS_DIR));
        Ok(dir.join(format!("{hex}.json")))
    }

    /// `session`'s counts, or none yet. A file that does not read is an error, never no counts.
    pub fn load(root: &Path, session: &VerifiedSession) -> Result<Refusals, KernelError> {
        use std::io::Read as _;
        let path = Refusals::path(root, session)?;
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Refusals { path, counts: BTreeMap::new() });
            }
            Err(e) => return Err(crate::FlowError::io(&path)(e).into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_REFUSALS_FILE + 1).read_to_end(&mut bytes).map_err(crate::FlowError::io(&path))?;
        if bytes.len() as u64 > MAX_REFUSALS_FILE {
            let size = bytes.len() as u64;
            return Err(
                crate::FlowError::TooLarge { what: "refusal-count file", size, limit: MAX_REFUSALS_FILE }.into()
            );
        }
        let parsed: RefusalsFile = serde_json::from_slice(&bytes)
            .map_err(|e| KernelError::Encode(format!("{} does not parse ({e})", path.display())))?;
        Ok(Refusals { path, counts: parsed.counts })
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

    /// Writes the counts, whole: aside, then renamed over the old file.
    pub fn save(&self, session: &VerifiedSession) -> Result<(), KernelError> {
        let io = |e| crate::FlowError::io(&self.path)(e);
        let dir = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(io)?;
        let body = RefusalsFile { session: session.session().to_string(), counts: self.counts.clone() };
        let bytes = serde_json::to_vec(&body).map_err(|e| KernelError::Encode(e.to_string()))?;
        let aside = self.path.with_extension(format!("json.{}", crate::ledger::random_epoch()?));
        std::fs::write(&aside, bytes).map_err(io)?;
        std::fs::rename(&aside, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&aside);
            io(e)
        })?;
        Ok(())
    }
}
