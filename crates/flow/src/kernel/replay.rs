//! `start`, and the read-only scheduler: `next` and `status` derive everything from the
//! instance's log. No current step is stored anywhere.
//!
//! Replay reads the events and evaluates the pinned IR over them. It never runs a command (a
//! node's `run`, `verify` or `reconcile` is data here), never starts a process and writes
//! nothing; the only code it evaluates is a repeat's `until`, a jq guard compiled without any
//! filter that reads the clock, the environment or input streams, or prints. The clock is the
//! caller's `now`, so the same log and the same `now` give byte-identical JSON.
//!
//! # Semantics
//!
//! - A node is done when the log holds its receipt for the node's current iteration: a
//!   `Completed` event, or for a `choice` node a `Chosen` event. The last receipt wins.
//! - A node outside every repeat has iteration `{}`. A node in repeat `r`'s body is evaluated at
//!   `{r: i}`, `i` being the repeat's current iteration, and only receipts carrying exactly that
//!   iteration count. A receipt from iteration `i` never satisfies anything in `i + 1`.
//! - A repeat's iteration `i` is settled when every body node is done, stale or will not run.
//!   Then the repeat is done if its `until` holds over that iteration, exhausted if `i + 1` is its
//!   `max`, and otherwise iteration `i + 1` begins. A node outside the repeat that needs a body
//!   node sees the body node's final iteration once the repeat is done, and never runs if the
//!   repeat is exhausted; the `on_exhausted` node runs only then.
//! - An `observe` receipt is stale when it is older than `fresh_for` (by the event's `at`) or
//!   when a declared input's digest changed. A stale observation blocks every node downstream of
//!   it that is not done. Its value is its exit status through `exit_map`; an exit status
//!   outside the map fails the node.
//! - A `switch` follows only the recorded value: the chosen edge, or the observed value. A node
//!   whose case was not taken, or whose `when_param` was not supplied, stays blocked and never
//!   runs, and so does everything that needs it.
//! - A node with every requirement met is `waiting` while an attempt's lease holds (or the owner
//!   is asked, for a `human` node), `failed` once `retry.attempts` attempts ended without a
//!   receipt, and `ready` otherwise. An attempt ends when its lease passes or, with `cancel`,
//!   `cancel.after` after its claim.

use super::compile::CompiledFlow;
use super::error::KernelError;
use super::events::{Chosen, Claimed, Completed, Heartbeat, KernelEvent, Refused, Started, digest};
use super::ir::{self, Class, Input, Ir, Iteration, Launch, Need, Wait};
use super::jq;
use crate::ids::InstanceId;
use crate::ledger::{Appended, Ledger, ReadGuard, random_epoch};
use crate::log::{Event, Log};
use jiff::{SignedDuration, Timestamp};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
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
    /// A needed node is not done.
    Needs {
        node: String,
    },
    /// A needed node will never run (its case was not taken, its param was not supplied, or
    /// something it needs will never run).
    Unreachable {
        node: String,
    },
    /// This node's switch case was not taken.
    Switch {
        on: String,
        took: Option<String>,
        case: String,
    },
    /// This node's `when_param` was not supplied.
    ParamAbsent {
        param: String,
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
    /// An observation upstream is stale.
    Stale {
        node: String,
    },
    /// This observation is older than its `fresh_for`.
    Expired {
        age_secs: i64,
        fresh_for_secs: u64,
    },
    /// A declared input of this observation changed since it was recorded.
    InputChanged {
        input: String,
    },
    /// This observation's exit status is not in its `exit_map`.
    ExitUnmapped {
        exit: Option<i32>,
    },
    /// The recorded edge is not one this choice node declares.
    EdgeUndeclared {
        edge: String,
    },
    /// `retry.attempts` attempts ended without a receipt.
    AttemptsUsed {
        attempts: u32,
    },
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatState {
    Running,
    Done,
    Exhausted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepeatStatus {
    pub state: RepeatState,
    pub iteration: u32,
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
}

/// A node that may be claimed now, and the hint for launching its performer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Enabled {
    pub node: String,
    pub class: String,
    pub iteration: Iteration,
    /// The attempt number the next claim gets.
    pub attempt: u32,
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

fn supplied(params: &Value, name: &str) -> bool {
    params.get(name).is_some_and(|v| !v.is_null())
}

/// Every node's state. Reads the log under the shared lock and writes nothing.
pub fn status(guard: &ReadGuard, instance: &InstanceId, now: Timestamp) -> Result<Status, KernelError> {
    Ok(replay(&guard.log(instance)?, now)?.status)
}

/// The enabled nodes and what blocks the others. Reads the log under the shared lock and writes
/// nothing.
pub fn next(guard: &ReadGuard, instance: &InstanceId, now: Timestamp) -> Result<Next, KernelError> {
    let replayed = replay(&guard.log(instance)?, now)?;
    let Replayed { status, started, attempts } = replayed;
    let mut enabled = Vec::new();
    let mut pending = BTreeMap::new();
    for (id, s) in status.nodes {
        match s.state {
            State::Ready => {
                let class = started.ir.nodes.get(&id).map(|n| &n.class);
                let key = (id.clone(), s.iteration.clone());
                enabled.push(Enabled {
                    attempt: attempts.get(&key).and_then(|a| a.keys().next_back().copied()).unwrap_or(0) + 1,
                    launch: class.and_then(Class::launch).cloned().unwrap_or_default(),
                    wait: class.and_then(Class::wait).cloned(),
                    node: id,
                    class: s.class,
                    iteration: s.iteration,
                });
            }
            State::Done => {}
            _ => {
                pending.insert(id, s);
            }
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

/// One attempt at a node: when it was claimed and how long its lease holds.
#[derive(Debug, Clone)]
struct Attempt {
    claimed_at: Timestamp,
    lease_until: Timestamp,
    lease_text: String,
}

type Key = (String, Iteration);

/// What the events after `Started` record, by (node, iteration).
#[derive(Debug, Default)]
struct Facts {
    completed: BTreeMap<Key, (Timestamp, Completed)>,
    chosen: BTreeMap<Key, Chosen>,
    attempts: BTreeMap<Key, BTreeMap<u32, Attempt>>,
}

/// The result of a replay.
struct Replayed {
    status: Status,
    started: Started,
    attempts: BTreeMap<Key, BTreeMap<u32, Attempt>>,
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

/// Replays a verified log. Pure: the result is a function of the log and `now`.
fn replay(log: &Log, now: Timestamp) -> Result<Replayed, KernelError> {
    let Some((first, rest)) = log.events.split_first() else {
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

    let mut facts = Facts::default();
    let known = |node: &str, seq: u64| {
        if started.ir.nodes.contains_key(node) {
            Ok(())
        } else {
            Err(malformed(log, seq, format!("an event names node {node}, which the flow does not declare")))
        }
    };
    for e in rest {
        let at = timestamp(log, e.seq, &e.at)?;
        match e.kind.as_str() {
            Completed::KIND => {
                let c: Completed = body(log, e)?;
                known(&c.node, e.seq)?;
                facts.completed.insert((c.node.clone(), c.iteration.clone()), (at, c));
            }
            Chosen::KIND => {
                let c: Chosen = body(log, e)?;
                known(&c.node, e.seq)?;
                facts.chosen.insert((c.node.clone(), c.iteration.clone()), c);
            }
            Claimed::KIND => {
                let c: Claimed = body(log, e)?;
                known(&c.node, e.seq)?;
                let lease_until = timestamp(log, e.seq, &c.lease_until)?;
                let attempt = Attempt { claimed_at: at, lease_until, lease_text: c.lease_until };
                facts.attempts.entry((c.node, c.iteration)).or_default().insert(c.attempt, attempt);
            }
            Heartbeat::KIND => {
                let h: Heartbeat = body(log, e)?;
                known(&h.node, e.seq)?;
                let lease_until = timestamp(log, e.seq, &h.lease_until)?;
                if let Some(a) = facts.attempts.get_mut(&(h.node, h.iteration)).and_then(|m| m.get_mut(&h.attempt))
                    && lease_until > a.lease_until
                {
                    a.lease_until = lease_until;
                    a.lease_text = h.lease_until;
                }
            }
            Refused::KIND => {
                let r: Refused = body(log, e)?;
                known(&r.node, e.seq)?;
            }
            other => return Err(malformed(log, e.seq, format!("{other} is not a kernel event"))),
        }
    }

    let mut eval = Eval::new(&started, &facts, now);
    let mut nodes = BTreeMap::new();
    for id in started.ir.nodes.keys() {
        nodes.insert(id.clone(), eval.status(id));
    }
    let mut repeats = BTreeMap::new();
    for rid in started.ir.repeats.keys() {
        let (state, iteration) = match eval.repeat(rid) {
            Loop::Running(i) => (RepeatState::Running, i),
            Loop::Done(i) => (RepeatState::Done, i),
            Loop::Exhausted(i) => (RepeatState::Exhausted, i),
        };
        repeats.insert(rid.clone(), RepeatStatus { state, iteration });
    }
    let status = Status {
        instance: log.instance.to_string(),
        flow: started.flow.clone(),
        ir_hash: started.ir_hash.clone(),
        seq: log.seq(),
        nodes,
        repeats,
    };
    Ok(Replayed { status, started, attempts: facts.attempts })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Loop {
    Running(u32),
    Done(u32),
    Exhausted(u32),
}

/// A node evaluated at one iteration.
#[derive(Debug, Clone)]
struct NodeEval {
    state: State,
    reasons: Vec<Reason>,
    /// It will never run in this iteration.
    dead: bool,
    /// Stale observations at or upstream of this node.
    stale: BTreeSet<String>,
    /// The observed value or the chosen edge, once done.
    value: Option<String>,
    outputs: Value,
}

impl NodeEval {
    /// Done or stale: the node has its receipt.
    fn recorded(&self) -> bool {
        matches!(self.state, State::Done | State::Stale)
    }
}

/// What a dependency looks like from a node's iteration.
enum Dep {
    Eval(NodeEval),
    Running(String),
    Exhausted(String),
}

struct Eval<'a> {
    ir: &'a Ir,
    params: &'a Value,
    facts: &'a Facts,
    now: Timestamp,
    member: BTreeMap<&'a str, &'a str>,
    nodes: BTreeMap<Key, NodeEval>,
    loops: BTreeMap<String, Loop>,
}

impl<'a> Eval<'a> {
    fn new(started: &'a Started, facts: &'a Facts, now: Timestamp) -> Self {
        let ir = &started.ir;
        let member =
            ir.repeats.iter().flat_map(|(rid, r)| r.body.iter().map(move |b| (b.as_str(), rid.as_str()))).collect();
        Eval { ir, params: &started.params, facts, now, member, nodes: BTreeMap::new(), loops: BTreeMap::new() }
    }

    /// A node's reported state: at its repeat's current iteration, or exhausted with it.
    fn status(&mut self, id: &str) -> NodeStatus {
        let class = self.ir.nodes.get(id).map_or("", |n| n.class.name()).to_string();
        let (iteration, eval) = match self.member.get(id).copied() {
            None => (Iteration::new(), None),
            Some(rid) => {
                let (i, exhausted) = match self.repeat(rid) {
                    Loop::Running(i) | Loop::Done(i) => (i, false),
                    Loop::Exhausted(i) => (i, true),
                };
                let iteration = Iteration::from([(rid.to_string(), i)]);
                let eval = exhausted.then(|| NodeEval {
                    state: State::Exhausted,
                    reasons: vec![Reason::RepeatExhausted { repeat: rid.to_string() }],
                    dead: true,
                    stale: BTreeSet::new(),
                    value: None,
                    outputs: Value::Null,
                });
                (iteration, eval)
            }
        };
        let eval = match eval {
            Some(e) => e,
            None => self.node(id, &iteration),
        };
        let (waits, unmet) = eval.reasons.into_iter().partition(Reason::is_wait);
        NodeStatus { state: eval.state, class, iteration, unmet, waits }
    }

    /// A dependency `m` seen from iteration `iter`.
    fn dep(&mut self, m: &str, iter: &Iteration) -> Dep {
        match self.member.get(m).copied() {
            None => Dep::Eval(self.node(m, &Iteration::new())),
            Some(rid) => {
                if let Some(i) = iter.get(rid) {
                    let at = Iteration::from([(rid.to_string(), *i)]);
                    return Dep::Eval(self.node(m, &at));
                }
                match self.repeat(rid) {
                    Loop::Running(_) => Dep::Running(rid.to_string()),
                    Loop::Exhausted(_) => Dep::Exhausted(rid.to_string()),
                    Loop::Done(i) => Dep::Eval(self.node(m, &Iteration::from([(rid.to_string(), i)]))),
                }
            }
        }
    }

    fn repeat(&mut self, rid: &str) -> Loop {
        if let Some(l) = self.loops.get(rid) {
            return *l;
        }
        let out = self.walk_repeat(rid);
        self.loops.insert(rid.to_string(), out);
        out
    }

    fn walk_repeat(&mut self, rid: &str) -> Loop {
        let Some(r) = self.ir.repeats.get(rid) else { return Loop::Running(0) };
        let until = r.until.as_deref().map(jq::compile);
        let mut i = 0;
        loop {
            let at = Iteration::from([(rid.to_string(), i)]);
            let mut state = serde_json::Map::new();
            for b in &r.body {
                let e = self.node(b, &at);
                if !(e.dead || e.recorded()) {
                    return Loop::Running(i);
                }
                if e.recorded() {
                    state.insert(b.clone(), json!({ "outputs": e.outputs, "value": e.value }));
                }
            }
            let input = json!({ "params": self.params, "iteration": i, "nodes": state });
            if let Some(Ok(filter)) = &until
                && jq::holds(filter, &input)
            {
                return Loop::Done(i);
            }
            if i + 1 >= r.max {
                return Loop::Exhausted(i);
            }
            i += 1;
        }
    }

    fn node(&mut self, id: &str, iter: &Iteration) -> NodeEval {
        let key = (id.to_string(), iter.clone());
        if let Some(e) = self.nodes.get(&key) {
            return e.clone();
        }
        let out = self.eval_node(id, iter, &key);
        self.nodes.insert(key, out.clone());
        out
    }

    fn eval_node(&mut self, id: &str, iter: &Iteration, key: &Key) -> NodeEval {
        let ir = self.ir;
        let Some(n) = ir.nodes.get(id) else {
            return blocked(vec![Reason::Unreachable { node: id.to_string() }], true, BTreeSet::new());
        };
        let mut reasons = Vec::new();
        let mut dead = false;
        let mut stale = BTreeSet::new();
        let mut needs_node =
            |this: &mut Self, m: &str, reasons: &mut Vec<Reason>, dead: &mut bool| match this.dep(m, iter) {
                Dep::Eval(e) => {
                    stale.extend(e.stale.iter().cloned());
                    if e.dead {
                        *dead = true;
                        reasons.push(Reason::Unreachable { node: m.to_string() });
                    } else if !e.recorded() {
                        reasons.push(Reason::Needs { node: m.to_string() });
                    }
                    Some(e)
                }
                Dep::Running(r) => {
                    reasons.push(Reason::RepeatRunning { repeat: r });
                    None
                }
                Dep::Exhausted(r) => {
                    *dead = true;
                    reasons.push(Reason::RepeatExhausted { repeat: r });
                    None
                }
            };
        for need in &n.needs {
            match need {
                Need::Node { node: m } => {
                    needs_node(self, m, &mut reasons, &mut dead);
                }
                Need::Repeat { repeat: r } => match self.repeat(r) {
                    Loop::Done(_) => {}
                    Loop::Running(_) => reasons.push(Reason::RepeatRunning { repeat: r.clone() }),
                    Loop::Exhausted(_) => {
                        dead = true;
                        reasons.push(Reason::RepeatExhausted { repeat: r.clone() });
                    }
                },
                Need::Exhausted { repeat: r } => match self.repeat(r) {
                    Loop::Exhausted(_) => {}
                    Loop::Running(_) => reasons.push(Reason::RepeatRunning { repeat: r.clone() }),
                    Loop::Done(_) => {
                        dead = true;
                        reasons.push(Reason::RepeatFinished { repeat: r.clone() });
                    }
                },
            }
        }
        if let Some(case) = &n.switch
            && let Some(e) = needs_node(self, &case.on, &mut reasons, &mut dead)
            && e.recorded()
            && e.value.as_deref() != Some(case.value.as_str())
        {
            dead = true;
            reasons.push(Reason::Switch { on: case.on.clone(), took: e.value.clone(), case: case.value.clone() });
        }
        if let Some(p) = &n.when_param
            && !supplied(self.params, p)
        {
            dead = true;
            reasons.push(Reason::ParamAbsent { param: p.clone() });
        }

        // The node's own receipt, if any.
        if let Some(done) = self.receipt(n, id, key, stale.clone()) {
            return done;
        }
        if dead || !reasons.is_empty() {
            return blocked(reasons, dead, stale);
        }
        if !stale.is_empty() {
            let reasons = stale.iter().map(|s| Reason::Stale { node: s.clone() }).collect();
            return blocked(reasons, false, stale);
        }
        if let Class::Human { ask } = &n.class {
            return NodeEval {
                state: State::Waiting,
                reasons: vec![Reason::Owner { ask: ask.clone() }],
                ..fresh(stale)
            };
        }
        let attempts = self.facts.attempts.get(key);
        let live = attempts.and_then(|a| {
            a.iter().rev().find(|(_, a)| {
                let cancelled = n.cancel_after.is_some_and(|c| self.now.duration_since(a.claimed_at) >= secs(c));
                self.now < a.lease_until && !cancelled
            })
        });
        if let Some((attempt, a)) = live {
            let mut reasons = vec![Reason::Claimed { attempt: *attempt, lease_until: a.lease_text.clone() }];
            if let Some(w) = n.class.wait() {
                reasons.push(Reason::External { waits_for: w.external.clone(), heartbeat_secs: w.heartbeat });
            }
            return NodeEval { state: State::Waiting, reasons, ..fresh(stale) };
        }
        let ended = attempts.map_or(0, |a| a.len() as u32);
        if let Some(max) = n.retry
            && ended >= max
        {
            return NodeEval {
                state: State::Failed,
                reasons: vec![Reason::AttemptsUsed { attempts: ended }],
                ..fresh(stale)
            };
        }
        NodeEval { state: State::Ready, ..fresh(stale) }
    }

    /// The node's state from its receipt in this iteration, if the log holds one.
    fn receipt(&self, n: &ir::Node, id: &str, key: &Key, mut stale: BTreeSet<String>) -> Option<NodeEval> {
        let done = |value: Option<String>, outputs: Value, stale: BTreeSet<String>| NodeEval {
            state: State::Done,
            value,
            outputs,
            ..fresh(stale)
        };
        let failed = |reason: Reason, stale: BTreeSet<String>| NodeEval {
            state: State::Failed,
            reasons: vec![reason],
            ..fresh(stale)
        };
        if let Class::Choice { edges, .. } = &n.class {
            let chosen = self.facts.chosen.get(key)?;
            if !edges.contains_key(&chosen.edge) {
                return Some(failed(Reason::EdgeUndeclared { edge: chosen.edge.clone() }, stale));
            }
            return Some(done(Some(chosen.edge.clone()), Value::Null, stale));
        }
        let (at, receipt) = self.facts.completed.get(key)?;
        let Class::Observe { fresh_for, inputs, exit_map, .. } = &n.class else {
            return Some(done(None, receipt.outputs.clone(), stale));
        };
        let exit = receipt.verify.as_ref().map(|v| v.exit);
        let Some(value) = exit.and_then(|x| exit_map.iter().find(|e| e.exit == x)).map(|e| e.value.clone()) else {
            return Some(failed(Reason::ExitUnmapped { exit }, stale));
        };
        let mut reasons = Vec::new();
        let age = self.now.duration_since(*at);
        if let Some(f) = fresh_for
            && age > secs(*f)
        {
            reasons.push(Reason::Expired { age_secs: age.as_secs(), fresh_for_secs: *f });
        }
        for input in inputs {
            if receipt.inputs.get(&input.key()) != self.input_digest(input, key).as_ref() {
                reasons.push(Reason::InputChanged { input: input.key() });
            }
        }
        if reasons.is_empty() {
            return Some(done(Some(value), receipt.outputs.clone(), stale));
        }
        stale.insert(id.to_string());
        Some(NodeEval {
            state: State::Stale,
            reasons,
            value: Some(value),
            outputs: receipt.outputs.clone(),
            ..fresh(stale)
        })
    }

    /// The digest an input has now: a param's value, or a node's latest outputs in the matching
    /// iteration. `None` when the node has no receipt there.
    fn input_digest(&self, input: &Input, key: &Key) -> Option<String> {
        match input {
            Input::Param { name } => Some(digest(self.params.get(name).unwrap_or(&Value::Null))),
            Input::Node { node } => {
                let iter = match self.member.get(node.as_str()) {
                    Some(rid) => key.1.get(*rid).map(|i| Iteration::from([(rid.to_string(), *i)]))?,
                    None => Iteration::new(),
                };
                self.facts.completed.get(&(node.clone(), iter)).map(|(_, c)| digest(&c.outputs))
            }
        }
    }
}

fn fresh(stale: BTreeSet<String>) -> NodeEval {
    NodeEval { state: State::Ready, reasons: Vec::new(), dead: false, stale, value: None, outputs: Value::Null }
}

fn blocked(reasons: Vec<Reason>, dead: bool, stale: BTreeSet<String>) -> NodeEval {
    NodeEval { state: State::Blocked, reasons, dead, ..fresh(stale) }
}

fn secs(s: u64) -> SignedDuration {
    SignedDuration::from_secs(i64::try_from(s).unwrap_or(i64::MAX))
}
