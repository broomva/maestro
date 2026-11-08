//! `start`, and the read-only scheduler: `next` and `status` derive everything from the
//! instance's log. No current step is stored anywhere.
//!
//! Replay folds the log once, in order. Each event is accepted only if it fits the state the
//! events before it built, with the event's own `at` as the clock; anything else is set aside as
//! rejected and changes nothing. Replay never runs a command (a node's `run`, `verify` and
//! `reconcile` are data here), never evaluates a guard (an `until` verdict is read from a
//! `Guarded` event), never starts a process and writes nothing. The clock of a read is the
//! caller's `now`, so the same log and the same `now` give byte-identical JSON.
//!
//! # Acceptance, at an event's position
//!
//! - Every event naming a node must name a declared node and the node's current iteration: `{}`
//!   outside any repeat, `{r: i}` inside repeat `r` while it runs at `i` (or finished at `i`).
//! - `Completed` (any class but choice) and `Chosen` (choice only, a declared edge) count only if
//!   the node is enabled: every need has a receipt that is still valid and its switch case was
//!   taken, and it is not already done. Only an observation may be recorded again while it is
//!   done (a re-run). Freshness is a rule of reads: a writer re-checks it before it records.
//! - `Claimed` must number its attempt above the last; `Heartbeat` must extend a lease it names.
//! - `Guarded` records a repeat's `until` verdict for its current iteration once that iteration
//!   has settled. A repeat moves on only from recorded verdicts (or at once, with no `until`).
//!
//! # State, at a clock
//!
//! - A receipt records the digests of the receipts it consumed (see `events`). If one changed,
//!   the receipt is stale and the node is not done until it is recorded again. A re-run with the
//!   same result keeps it.
//! - An observation older than `fresh_for` is stale: it still satisfies its dependents, but
//!   blocks every node downstream that is not done. Its value is its exit status through
//!   `exit_map`; a receipt with no exit status, or one outside the map, fails the node.
//! - A switch follows only the recorded value. A node whose case was not taken never runs, and
//!   neither does anything that needs it. A repeat whose whole body will never run is
//!   unreachable, not exhausted.
//! - A node with every requirement met is `waiting` while an attempt's lease holds (or the owner
//!   is asked, for a `human` node), `failed` once `retry.attempts` attempts ended without a
//!   receipt or the attempt numbers ran out, and `ready` otherwise. An attempt ends when its
//!   lease passes or, with `cancel`, `cancel.after` after its claim.

use super::compile::CompiledFlow;
use super::error::KernelError;
use super::events::{Chosen, Claimed, Completed, Guarded, Heartbeat, KernelEvent, Refused, Started, receipt_digest};
use super::ir::{self, Class, Ir, Iteration, Launch, Need, Wait};
use crate::ids::InstanceId;
use crate::ledger::{Appended, Ledger, ReadGuard, random_epoch};
use crate::log::{Event, Log};
use jiff::{SignedDuration, Timestamp};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// A node's state, as `status` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Done,
    Ready,
    Blocked,
    Stale,
    Waiting,
    Exhausted,
    Failed,
}

/// Why a node is not done or not ready.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Reason {
    /// A needed node has no valid receipt.
    Needs {
        node: String,
    },
    /// A needed node will never run.
    Unreachable {
        node: String,
    },
    /// This node's switch case was not taken.
    Switch {
        on: String,
        took: Option<String>,
        case: String,
    },
    RepeatRunning {
        repeat: String,
    },
    RepeatExhausted {
        repeat: String,
    },
    /// The repeat finished, so its `on_exhausted` route will not run.
    RepeatFinished {
        repeat: String,
    },
    /// No node of the repeat's body will ever run.
    RepeatUnreachable {
        repeat: String,
    },
    /// An observation upstream is stale.
    Stale {
        node: String,
    },
    /// This observation is older than its `fresh_for`.
    Expired {
        age_secs: i64,
        fresh_for_secs: u64,
    },
    /// A receipt this one consumed has changed since it was recorded.
    InputChanged {
        input: String,
    },
    /// This observation recorded no exit status, or one outside its `exit_map`.
    ExitUnmapped {
        exit: Option<i32>,
    },
    /// `retry.attempts` attempts ended without a receipt.
    AttemptsUsed {
        attempts: u32,
    },
    /// The attempt numbers ran out.
    AttemptsExhausted,
    /// An attempt holds a lease.
    Claimed {
        attempt: u32,
        lease_until: String,
    },
    /// The claimed attempt waits on something outside, as the node declares.
    External {
        waits_for: String,
        heartbeat_secs: u64,
    },
    /// The owner's answer to this ask, through the asks ledger.
    Owner {
        ask: String,
    },
}

impl Reason {
    fn is_wait(&self) -> bool {
        matches!(self, Reason::Claimed { .. } | Reason::External { .. } | Reason::Owner { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NodeStatus {
    pub state: State,
    pub class: String,
    pub iteration: Iteration,
    /// What must change before the node can be done.
    pub unmet: Vec<Reason>,
    /// Who or what the node is waiting on.
    pub waits: Vec<Reason>,
    /// The digests a receipt for this node recorded now would carry as `inputs`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatState {
    Running,
    Done,
    Exhausted,
    Unreachable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepeatStatus {
    pub state: RepeatState,
    pub iteration: u32,
    /// The iteration has settled and waits for its `until` verdict (a `Guarded` event).
    pub awaiting_verdict: bool,
}

/// An event replay set aside: it did not fit the state at its position, so it changed nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rejected {
    pub seq: u64,
    pub kind: String,
    /// The node or repeat it named.
    pub subject: String,
    pub reason: String,
}

/// Every node's state, derived from the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    pub instance: String,
    pub flow: String,
    pub ir_hash: String,
    /// The last event replayed.
    pub seq: u64,
    pub nodes: BTreeMap<String, NodeStatus>,
    pub repeats: BTreeMap<String, RepeatStatus>,
    pub rejected: Vec<Rejected>,
}

/// A node that may be claimed now, and the hint for launching its performer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Enabled {
    pub node: String,
    pub class: String,
    pub iteration: Iteration,
    /// The attempt number the next claim gets.
    pub attempt: u32,
    /// The digests its receipt must record as `inputs`.
    pub inputs: BTreeMap<String, String>,
    pub launch: Launch,
    pub wait: Option<Wait>,
}

/// What may happen next: the enabled nodes, and for every other node not done, why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Next {
    pub instance: String,
    pub flow: String,
    pub ir_hash: String,
    pub seq: u64,
    pub enabled: Vec<Enabled>,
    pub pending: BTreeMap<String, NodeStatus>,
}

/// A started instance.
#[derive(Debug, Clone, PartialEq)]
pub struct Begun {
    pub instance: InstanceId,
    pub appended: Appended,
}

/// Starts an instance of `flow` with `params`: appends `Started`, which pins the IR.
pub fn start(ledger: &mut Ledger, flow: &CompiledFlow, params: Value) -> Result<Begun, KernelError> {
    check_params(flow.ir(), &params)?;
    let instance = InstanceId::new(format!("fi_{}", random_epoch()?))?;
    let started = Started { flow: flow.name(), ir_hash: flow.hash().to_string(), ir: flow.ir().clone(), params };
    let event = started.to_event()?;
    let appended = ledger.lock()?.append(&instance, 0, event)?;
    Ok(Begun { instance, appended })
}

fn check_params(ir: &Ir, params: &Value) -> Result<(), KernelError> {
    let refuse = |detail: String| KernelError::InvalidParams { flow: format!("{}@{}", ir.flow, ir.version), detail };
    let Value::Object(given) = params else {
        return Err(refuse("params must be a JSON object".into()));
    };
    if let Some(name) = ir.params.iter().find(|(name, required)| **required && !supplied(params, name)).map(|p| p.0) {
        return Err(refuse(format!("the required param {name} is missing")));
    }
    if let Some(name) = given.keys().find(|k| !ir.params.contains_key(*k)) {
        return Err(refuse(format!("{name} is not a param of this flow")));
    }
    Ok(())
}

/// Present and not null.
fn supplied(params: &Value, name: &str) -> bool {
    params.get(name).is_some_and(|v| !v.is_null())
}

/// Every node's state. Reads the log under the shared lock and writes nothing.
pub fn status(guard: &ReadGuard, instance: &InstanceId, now: Timestamp) -> Result<Status, KernelError> {
    let log = guard.log(instance)?;
    let started = pinned(&log)?;
    let fold = Fold::replay(&log, &started)?;
    Ok(fold.status(&log, &started, now))
}

/// The enabled nodes and what blocks the others. Reads the log under the shared lock and writes
/// nothing.
pub fn next(guard: &ReadGuard, instance: &InstanceId, now: Timestamp) -> Result<Next, KernelError> {
    let log = guard.log(instance)?;
    let started = pinned(&log)?;
    let fold = Fold::replay(&log, &started)?;
    let status = fold.status(&log, &started, now);
    let mut eval = Eval::new(&fold, now);
    let mut enabled = Vec::new();
    let mut pending = BTreeMap::new();
    for (id, s) in status.nodes {
        let e = eval.node(&id, &s.iteration);
        // A stale receipt is redone once its requirements hold and nothing upstream is stale.
        let rerun = s.state == State::Stale && e.enabled && e.stale.iter().all(|s| *s == id);
        let key = (id.clone(), s.iteration.clone());
        if let (true, Some(attempt)) = (s.state == State::Ready || rerun, fold.next_attempt(&key)) {
            let class = started.ir.nodes.get(&id).map(|n| &n.class);
            enabled.push(Enabled {
                attempt,
                inputs: s.inputs,
                launch: class.and_then(Class::launch).cloned().unwrap_or_default(),
                wait: class.and_then(Class::wait).cloned(),
                node: id,
                class: s.class,
                iteration: s.iteration,
            });
        } else if s.state != State::Done {
            pending.insert(id, s);
        }
    }
    Ok(Next {
        instance: status.instance,
        flow: status.flow,
        ir_hash: status.ir_hash,
        seq: status.seq,
        enabled,
        pending,
    })
}

fn malformed(log: &Log, seq: u64, detail: impl Into<String>) -> KernelError {
    KernelError::Malformed { instance: log.instance.to_string(), seq, detail: detail.into() }
}

fn body<T: DeserializeOwned>(log: &Log, e: &Event) -> Result<T, KernelError> {
    serde_json::from_value(e.body.clone())
        .map_err(|err| malformed(log, e.seq, format!("a {} body that does not parse ({err})", e.kind)))
}

fn timestamp(log: &Log, seq: u64, text: &str) -> Result<Timestamp, KernelError> {
    text.parse().map_err(|_| malformed(log, seq, format!("{text} is not a timestamp")))
}

fn secs(s: u64) -> SignedDuration {
    SignedDuration::from_secs(i64::try_from(s).unwrap_or(i64::MAX))
}

/// The `Started` event, checked: the first event, an IR of this kernel's version that validates,
/// matches its hash and is the flow it names.
fn pinned(log: &Log) -> Result<Started, KernelError> {
    let Some(first) = log.events.first() else {
        return Err(malformed(log, 0, "the log is empty"));
    };
    if first.kind != Started::KIND {
        return Err(malformed(log, first.seq, format!("the first event is {}, not Started", first.kind)));
    }
    let started: Started = body(log, first)?;
    ir::validate(&started.ir)
        .map_err(|e| malformed(log, first.seq, format!("the pinned IR does not validate ({e})")))?;
    if ir::content_hash(&started.ir)? != started.ir_hash {
        return Err(malformed(log, first.seq, "the pinned IR does not match its hash"));
    }
    if started.flow != format!("{}@{}", started.ir.flow, started.ir.version) {
        return Err(malformed(log, first.seq, format!("Started names {}, not its pinned IR", started.flow)));
    }
    Ok(started)
}

type Key = (String, Iteration);

/// An accepted receipt.
#[derive(Debug, Clone)]
struct Receipt {
    at: Timestamp,
    outputs: Value,
    /// The observed value or the chosen edge.
    value: Option<String>,
    /// Why this receipt fails its node, for an observation whose exit status maps to nothing.
    failure: Option<Reason>,
    inputs: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct Attempt {
    claimed_at: Timestamp,
    lease_until: Timestamp,
    lease_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Running(u32),
    Done(u32),
    Exhausted(u32),
}

/// A repeat's phase as seen at a clock: the folded phase, or unreachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Loop {
    Running(u32),
    Done(u32),
    Exhausted(u32),
    Unreachable,
}

/// The common head of every event that names a node.
#[derive(Deserialize)]
struct Subject {
    node: String,
    iteration: Iteration,
}

/// What the accepted events built, in log order.
struct Fold<'a> {
    ir: &'a Ir,
    member: BTreeMap<&'a str, &'a str>,
    receipts: BTreeMap<Key, Receipt>,
    attempts: BTreeMap<Key, BTreeMap<u32, Attempt>>,
    phases: BTreeMap<&'a str, Phase>,
    verdicts: BTreeMap<(String, u32), bool>,
    rejected: Vec<Rejected>,
}

impl<'a> Fold<'a> {
    fn replay(log: &Log, started: &'a Started) -> Result<Fold<'a>, KernelError> {
        let ir = &started.ir;
        let member =
            ir.repeats.iter().flat_map(|(rid, r)| r.body.iter().map(move |b| (b.as_str(), rid.as_str()))).collect();
        let mut fold = Fold {
            ir,
            member,
            receipts: BTreeMap::new(),
            attempts: BTreeMap::new(),
            phases: ir.repeats.keys().map(|r| (r.as_str(), Phase::Running(0))).collect(),
            verdicts: BTreeMap::new(),
            rejected: Vec::new(),
        };
        for e in log.events.iter().skip(1) {
            let at = timestamp(log, e.seq, &e.at)?;
            if let Err(reason) = fold.accept(log, e, at)? {
                let subject = e.body.get("node").or_else(|| e.body.get("repeat"));
                let subject = subject.and_then(Value::as_str).unwrap_or_default().to_string();
                fold.rejected.push(Rejected { seq: e.seq, kind: e.kind.clone(), subject, reason: reason.into() });
                continue;
            }
            fold.advance(at);
        }
        Ok(fold)
    }

    /// The number the next claim on `key` gets; `None` once the numbers ran out.
    fn next_attempt(&self, key: &Key) -> Option<u32> {
        match self.attempts.get(key).and_then(|a| a.keys().next_back()) {
            None => Some(1),
            Some(last) => last.checked_add(1),
        }
    }

    /// The iteration a node's events must carry now; `None` once its repeat is exhausted.
    fn current(&self, node: &str) -> Option<Iteration> {
        match self.member.get(node) {
            None => Some(Iteration::new()),
            Some(rid) => match self.phases.get(rid)? {
                Phase::Running(i) | Phase::Done(i) => Some(Iteration::from([(rid.to_string(), *i)])),
                Phase::Exhausted(_) => None,
            },
        }
    }

    /// Applies one event if it fits; the inner error is why it was set aside.
    fn accept(&mut self, log: &Log, e: &Event, at: Timestamp) -> Result<Result<(), &'static str>, KernelError> {
        if e.kind == Guarded::KIND {
            let g: Guarded = body(log, e)?;
            return Ok(self.guarded(g, at));
        }
        if ![Completed::KIND, Chosen::KIND, Claimed::KIND, Heartbeat::KIND, Refused::KIND].contains(&e.kind.as_str()) {
            return Err(malformed(log, e.seq, format!("{} is not a kernel event", e.kind)));
        }
        let subject: Subject = body(log, e)?;
        let Some(node) = self.ir.nodes.get(&subject.node) else { return Ok(Err("unknown_node")) };
        if self.current(&subject.node).as_ref() != Some(&subject.iteration) {
            return Ok(Err("not_current_iteration"));
        }
        let key = (subject.node, subject.iteration);
        let receipt = match e.kind.as_str() {
            Completed::KIND => {
                let c: Completed = body(log, e)?;
                let (value, failure) = match &node.class {
                    Class::Choice { .. } => return Ok(Err("wrong_class")),
                    Class::Observe { exit_map, .. } => {
                        let exit = c.verify.as_ref().map(|v| v.exit);
                        match exit.and_then(|x| exit_map.iter().find(|m| m.exit == x)) {
                            Some(m) => (Some(m.value.clone()), None),
                            None => (None, Some(Reason::ExitUnmapped { exit })),
                        }
                    }
                    _ => (None, None),
                };
                Receipt { at, outputs: c.outputs, value, failure, inputs: c.inputs }
            }
            Chosen::KIND => {
                let c: Chosen = body(log, e)?;
                let Class::Choice { edges, .. } = &node.class else { return Ok(Err("wrong_class")) };
                if !edges.contains_key(&c.edge) {
                    return Ok(Err("undeclared_edge"));
                }
                Receipt { at, outputs: Value::Null, value: Some(c.edge), failure: None, inputs: c.inputs }
            }
            Claimed::KIND => {
                let c: Claimed = body(log, e)?;
                let lease_until = timestamp(log, e.seq, &c.lease_until)?;
                let attempts = self.attempts.entry(key).or_default();
                if attempts.keys().next_back().is_some_and(|last| c.attempt <= *last) {
                    return Ok(Err("attempt_not_after_last"));
                }
                attempts.insert(c.attempt, Attempt { claimed_at: at, lease_until, lease_text: c.lease_until });
                return Ok(Ok(()));
            }
            Heartbeat::KIND => {
                let h: Heartbeat = body(log, e)?;
                let lease_until = timestamp(log, e.seq, &h.lease_until)?;
                let Some(a) = self.attempts.get_mut(&key).and_then(|m| m.get_mut(&h.attempt)) else {
                    return Ok(Err("no_such_attempt"));
                };
                if lease_until <= a.lease_until {
                    return Ok(Err("lease_not_extended"));
                }
                a.lease_until = lease_until;
                a.lease_text = h.lease_until;
                return Ok(Ok(()));
            }
            _ => {
                let _: Refused = body(log, e)?;
                return Ok(Ok(()));
            }
        };
        let now = Eval::new(self, at).node(&key.0, &key.1);
        if !now.enabled {
            return Ok(Err("not_enabled"));
        }
        if now.state == State::Done && !matches!(node.class, Class::Observe { .. }) {
            return Ok(Err("already_done"));
        }
        self.receipts.insert(key, receipt);
        Ok(Ok(()))
    }

    fn guarded(&mut self, g: Guarded, at: Timestamp) -> Result<(), &'static str> {
        let Some(r) = self.ir.repeats.get(&g.repeat) else { return Err("unknown_repeat") };
        if r.until.is_none() {
            return Err("no_until");
        }
        if self.phases.get(g.repeat.as_str()) != Some(&Phase::Running(g.iteration)) {
            return Err("not_current_iteration");
        }
        if !Eval::new(self, at).settled(&g.repeat, g.iteration) {
            return Err("not_settled");
        }
        self.verdicts.insert((g.repeat, g.iteration), g.holds);
        Ok(())
    }

    /// Moves every repeat whose current iteration settled to its next phase, from the recorded
    /// verdicts, until none moves.
    fn advance(&mut self, at: Timestamp) {
        let ir: &'a Ir = self.ir;
        loop {
            let mut moves = Vec::new();
            let mut eval = Eval::new(self, at);
            for (rid, r) in &ir.repeats {
                let Some(Phase::Running(i)) = self.phases.get(rid.as_str()).copied() else { continue };
                if !eval.settled(rid, i) {
                    continue;
                }
                let holds = match r.until {
                    Some(_) => match self.verdicts.get(&(rid.clone(), i)) {
                        Some(holds) => *holds,
                        None => continue,
                    },
                    None => false,
                };
                let next = if holds {
                    Phase::Done(i)
                } else if i.saturating_add(1) >= r.max {
                    Phase::Exhausted(i)
                } else {
                    Phase::Running(i + 1)
                };
                moves.push((rid.as_str(), next));
            }
            if moves.is_empty() {
                return;
            }
            self.phases.extend(moves);
        }
    }

    fn status(&self, log: &Log, started: &Started, now: Timestamp) -> Status {
        let mut eval = Eval::new(self, now);
        let nodes = self.ir.nodes.keys().map(|id| (id.clone(), eval.status(id))).collect();
        let repeats = self
            .ir
            .repeats
            .iter()
            .map(|(rid, r)| {
                let (state, iteration) = match eval.phase(rid) {
                    Loop::Running(i) => (RepeatState::Running, i),
                    Loop::Done(i) => (RepeatState::Done, i),
                    Loop::Exhausted(i) => (RepeatState::Exhausted, i),
                    Loop::Unreachable => (RepeatState::Unreachable, 0),
                };
                let awaiting_verdict = state == RepeatState::Running
                    && r.until.is_some()
                    && eval.settled(rid, iteration)
                    && !self.verdicts.contains_key(&(rid.clone(), iteration));
                (rid.clone(), RepeatStatus { state, iteration, awaiting_verdict })
            })
            .collect();
        Status {
            instance: log.instance.to_string(),
            flow: started.flow.clone(),
            ir_hash: started.ir_hash.clone(),
            seq: log.seq(),
            nodes,
            repeats,
            rejected: self.rejected.clone(),
        }
    }
}

/// A node evaluated at one iteration and one clock.
#[derive(Debug, Clone)]
struct NodeEval {
    state: State,
    reasons: Vec<Reason>,
    /// It will never run in this iteration.
    dead: bool,
    /// Every requirement is met: a receipt would be accepted.
    enabled: bool,
    /// Its receipt satisfies a dependent (done, or an observation only past its `fresh_for`).
    satisfies: bool,
    /// Stale observations at or upstream of this node.
    stale: BTreeSet<String>,
    /// The digest of its receipt, for a dependent's inputs.
    digest: Option<String>,
    value: Option<String>,
    /// The digests a receipt for this node must record.
    consumed: BTreeMap<String, String>,
}

impl NodeEval {
    fn new(state: State, reasons: Vec<Reason>) -> Self {
        NodeEval {
            state,
            reasons,
            dead: false,
            enabled: false,
            satisfies: false,
            stale: BTreeSet::new(),
            digest: None,
            value: None,
            consumed: BTreeMap::new(),
        }
    }
}

/// What a dependency looks like from a node's iteration.
enum Dep {
    Eval(Box<NodeEval>),
    Waits(Reason),
    Dead(Reason),
}

/// Evaluates the folded state at one clock. Nothing here changes the fold.
struct Eval<'f, 'a> {
    fold: &'f Fold<'a>,
    now: Timestamp,
    memo: BTreeMap<Key, NodeEval>,
}

impl<'f, 'a> Eval<'f, 'a> {
    fn new(fold: &'f Fold<'a>, now: Timestamp) -> Self {
        Eval { fold, now, memo: BTreeMap::new() }
    }

    fn phase(&mut self, rid: &str) -> Loop {
        let fold = self.fold;
        match fold.phases.get(rid).copied() {
            Some(Phase::Done(i)) => Loop::Done(i),
            Some(Phase::Exhausted(i)) => Loop::Exhausted(i),
            Some(Phase::Running(i)) => {
                let body = fold.ir.repeats.get(rid).map(|r| r.body.as_slice()).unwrap_or_default();
                let at = Iteration::from([(rid.to_string(), i)]);
                if body.iter().all(|b| self.node(b, &at).dead) { Loop::Unreachable } else { Loop::Running(i) }
            }
            None => Loop::Unreachable,
        }
    }

    /// Iteration `i` of `rid` has settled: every body node is satisfied or will never run, and
    /// not all of them will never run.
    fn settled(&mut self, rid: &str, i: u32) -> bool {
        let fold = self.fold;
        let body = fold.ir.repeats.get(rid).map(|r| r.body.as_slice()).unwrap_or_default();
        let at = Iteration::from([(rid.to_string(), i)]);
        let evals: Vec<NodeEval> = body.iter().map(|b| self.node(b, &at)).collect();
        evals.iter().all(|e| e.dead || e.satisfies) && !evals.iter().all(|e| e.dead)
    }

    /// A node's reported state: at its repeat's current iteration, or exhausted with it.
    fn status(&mut self, id: &str) -> NodeStatus {
        let class = self.fold.ir.nodes.get(id).map_or("", |n| n.class.name()).to_string();
        let (iteration, eval) = match self.fold.member.get(id).copied() {
            None => (Iteration::new(), self.node(id, &Iteration::new())),
            Some(rid) => {
                let (i, exhausted) = match self.fold.phases.get(rid).copied() {
                    Some(Phase::Running(i) | Phase::Done(i)) => (i, false),
                    Some(Phase::Exhausted(i)) => (i, true),
                    None => (0, false),
                };
                let iteration = Iteration::from([(rid.to_string(), i)]);
                let eval = if exhausted {
                    NodeEval::new(State::Exhausted, vec![Reason::RepeatExhausted { repeat: rid.to_string() }])
                } else {
                    self.node(id, &iteration)
                };
                (iteration, eval)
            }
        };
        let (waits, unmet) = eval.reasons.into_iter().partition(Reason::is_wait);
        NodeStatus { state: eval.state, class, iteration, unmet, waits, inputs: eval.consumed }
    }

    /// A dependency `m` seen from iteration `iter`.
    fn dep(&mut self, m: &str, iter: &Iteration) -> Dep {
        let Some(rid) = self.fold.member.get(m).copied() else {
            return Dep::Eval(Box::new(self.node(m, &Iteration::new())));
        };
        if let Some(i) = iter.get(rid) {
            return Dep::Eval(Box::new(self.node(m, &Iteration::from([(rid.to_string(), *i)]))));
        }
        match self.phase(rid) {
            Loop::Running(_) => Dep::Waits(Reason::RepeatRunning { repeat: rid.to_string() }),
            Loop::Exhausted(_) => Dep::Dead(Reason::RepeatExhausted { repeat: rid.to_string() }),
            Loop::Unreachable => Dep::Dead(Reason::RepeatUnreachable { repeat: rid.to_string() }),
            Loop::Done(i) => Dep::Eval(Box::new(self.node(m, &Iteration::from([(rid.to_string(), i)])))),
        }
    }

    fn node(&mut self, id: &str, iter: &Iteration) -> NodeEval {
        let key = (id.to_string(), iter.clone());
        if let Some(e) = self.memo.get(&key) {
            return e.clone();
        }
        let out = self.eval_node(id, &key);
        self.memo.insert(key, out.clone());
        out
    }

    fn eval_node(&mut self, id: &str, key: &Key) -> NodeEval {
        let fold = self.fold;
        let Some(n) = fold.ir.nodes.get(id) else {
            return NodeEval { dead: true, ..NodeEval::new(State::Blocked, Vec::new()) };
        };
        let mut out = NodeEval::new(State::Blocked, Vec::new());
        let iter = &key.1;
        for need in &n.needs {
            match need {
                Need::Node { node: m } => {
                    self.consume(m, iter, &mut out);
                }
                Need::Repeat { repeat: r } => match self.phase(r) {
                    Loop::Done(i) => {
                        let at = Iteration::from([(r.clone(), i)]);
                        let body = fold.ir.repeats.get(r).map(|r| r.body.as_slice()).unwrap_or_default();
                        for b in body {
                            let stale = self.node(b, &at).stale;
                            out.stale.extend(stale);
                        }
                    }
                    Loop::Running(_) => out.reasons.push(Reason::RepeatRunning { repeat: r.clone() }),
                    Loop::Exhausted(_) => kill(&mut out, Reason::RepeatExhausted { repeat: r.clone() }),
                    Loop::Unreachable => kill(&mut out, Reason::RepeatUnreachable { repeat: r.clone() }),
                },
                Need::Exhausted { repeat: r } => match self.phase(r) {
                    Loop::Exhausted(_) => {}
                    Loop::Running(_) => out.reasons.push(Reason::RepeatRunning { repeat: r.clone() }),
                    Loop::Done(_) => kill(&mut out, Reason::RepeatFinished { repeat: r.clone() }),
                    Loop::Unreachable => kill(&mut out, Reason::RepeatUnreachable { repeat: r.clone() }),
                },
            }
        }
        if let Some(case) = &n.switch
            && let Some(e) = self.consume(&case.on, iter, &mut out)
            && e.satisfies
            && e.value.as_deref() != Some(case.value.as_str())
        {
            kill(&mut out, Reason::Switch { on: case.on.clone(), took: e.value.clone(), case: case.value.clone() });
        }
        out.enabled = !out.dead && out.reasons.is_empty();
        if out.dead || !out.reasons.is_empty() {
            return out;
        }
        if let Some(r) = fold.receipts.get(key) {
            return self.receipt(n, id, r, out);
        }
        if !out.stale.is_empty() {
            out.reasons = out.stale.iter().map(|s| Reason::Stale { node: s.clone() }).collect();
            return out;
        }
        if let Class::Human { ask } = &n.class {
            out.state = State::Waiting;
            out.reasons.push(Reason::Owner { ask: ask.clone() });
            return out;
        }
        self.attempts(n, key, out)
    }

    /// Requires `m`, recording why not or the digest a receipt consumes from it.
    fn consume(&mut self, m: &str, iter: &Iteration, out: &mut NodeEval) -> Option<NodeEval> {
        match self.dep(m, iter) {
            Dep::Eval(e) => {
                out.stale.extend(e.stale.iter().cloned());
                if e.dead {
                    kill(out, Reason::Unreachable { node: m.to_string() });
                } else if !e.satisfies {
                    push(out, Reason::Needs { node: m.to_string() });
                } else if let Some(d) = &e.digest {
                    out.consumed.insert(m.to_string(), d.clone());
                }
                Some(*e)
            }
            Dep::Waits(reason) => {
                push(out, reason);
                None
            }
            Dep::Dead(reason) => {
                kill(out, reason);
                None
            }
        }
    }

    /// The node's state from its accepted receipt.
    fn receipt(&self, n: &ir::Node, id: &str, r: &Receipt, mut out: NodeEval) -> NodeEval {
        if let Some(failure) = &r.failure {
            out.state = State::Failed;
            out.reasons.push(failure.clone());
            return out;
        }
        for (input, d) in &out.consumed {
            if r.inputs.get(input) != Some(d) {
                out.reasons.push(Reason::InputChanged { input: input.clone() });
            }
        }
        let changed = !out.reasons.is_empty();
        if let Class::Observe { fresh_for: Some(f), .. } = &n.class {
            let age = self.now.duration_since(r.at);
            if age > secs(*f) {
                out.reasons.push(Reason::Expired { age_secs: age.as_secs(), fresh_for_secs: *f });
                out.stale.insert(id.to_string());
            }
        }
        out.state = if out.reasons.is_empty() { State::Done } else { State::Stale };
        out.satisfies = !changed;
        out.value = r.value.clone();
        out.digest = Some(receipt_digest(&r.outputs, r.value.as_deref()));
        out
    }

    /// The state of a node with every requirement met and no receipt: waiting, failed or ready.
    fn attempts(&self, n: &ir::Node, key: &Key, mut out: NodeEval) -> NodeEval {
        let attempts = self.fold.attempts.get(key);
        let live = attempts.and_then(|a| {
            a.iter().rev().find(|(_, a)| {
                let cancelled = n.cancel_after.is_some_and(|c| self.now.duration_since(a.claimed_at) >= secs(c));
                self.now < a.lease_until && !cancelled
            })
        });
        if let Some((attempt, a)) = live {
            out.state = State::Waiting;
            out.reasons.push(Reason::Claimed { attempt: *attempt, lease_until: a.lease_text.clone() });
            if let Some(w) = n.class.wait() {
                out.reasons.push(Reason::External { waits_for: w.external.clone(), heartbeat_secs: w.heartbeat });
            }
            return out;
        }
        let ended = u32::try_from(attempts.map_or(0, BTreeMap::len)).unwrap_or(u32::MAX);
        if self.fold.next_attempt(key).is_none() {
            out.state = State::Failed;
            out.reasons.push(Reason::AttemptsExhausted);
        } else if let Some(max) = n.retry
            && ended >= max
        {
            out.state = State::Failed;
            out.reasons.push(Reason::AttemptsUsed { attempts: ended });
        } else {
            out.state = State::Ready;
        }
        out
    }
}

/// Records why a node is not ready, once (a node may need and switch on the same node).
fn push(out: &mut NodeEval, reason: Reason) {
    if !out.reasons.contains(&reason) {
        out.reasons.push(reason);
    }
}

/// Marks a node as never running, for `reason`.
fn kill(out: &mut NodeEval, reason: Reason) {
    out.dead = true;
    push(out, reason);
}
