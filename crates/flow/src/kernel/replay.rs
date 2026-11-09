//! `start`, and the read-only scheduler: `next` and `status` derive everything from the
//! instance's log. No current step is stored anywhere.
//!
//! Replay folds the log once, in order: each event is accepted only if it fits the state the
//! events before it built, and anything else is set aside as rejected and changes nothing. An
//! event's `at` matters only as the time a receipt was taken (for freshness at a read) and as the
//! time a verdict's iteration is checked to have settled. An observation was taken when its
//! command started, so its freshness counts from its `verify.ran_at`, or from the event's `at`
//! when that is earlier or `ran_at` is missing or unreadable. Replay never runs a command (a node's
//! `run`, `verify` and `reconcile` are data here), never evaluates a guard (an `until` verdict is
//! read from a `Guarded` event), never starts a process and writes nothing. The clock of a read is
//! the caller's `now`, so the same log and the same `now` give byte-identical JSON.
//!
//! # Acceptance, at an event's position
//!
//! - Every event naming a node must name a declared node and the node's current iteration: `{}`
//!   outside any repeat, `{r: i}` inside repeat `r` while it is at `i` (and not exhausted).
//! - `Completed` (any class but choice) and `Chosen` (choice only, a declared edge) count only if
//!   the node is enabled (every need has a receipt whose content is current, and its switch case
//!   was taken) and it is not already done at the event's own `at`. Only an observation may be
//!   recorded again while it is done (a re-run). A node whose receipt is stale at that `at`,
//!   because something upstream had expired by then, is not done, so a new receipt replaces it.
//! - `Claimed`, `Heartbeat`, `Refused`, `EffectStarted` and every rejected event change nothing and
//!   cost no evaluation; only an accepted receipt or verdict can move a repeat on.
//! - `Guarded` records a repeat's `until` verdict for its current iteration once that iteration
//!   has settled, with the digest of the body it judged; a second verdict on a body already judged
//!   is set aside. A false verdict moves the repeat on; a repeat with no `until` moves on as soon
//!   as an iteration settles.
//!
//! # State, at a clock
//!
//! - A receipt records the digests of the receipts it consumed (see `events`): each node it
//!   needs or switches on, and for a repeat it needs, the digest of the repeat's settled body. A
//!   node is done only while those digests are current and nothing upstream, transitively, is
//!   stale. Otherwise it is `stale`: redone when its own receipt is out of date, re-validated when
//!   the stale observation upstream is re-run with the same result.
//! - An observation older than `fresh_for`, or stamped after the read's clock, is stale. Its
//!   value is its exit status through `exit_map`; a receipt with no exit status, or one outside
//!   the map, fails the node.
//! - A repeat is done while a true verdict for its current iteration judged the body as it is
//!   now; a body re-run that changes it puts the repeat back to awaiting a verdict.
//! - A switch follows only the recorded value. A node whose case was not taken never runs, and
//!   neither does anything that needs it. A repeat whose whole body will never run is
//!   unreachable, not exhausted.
//! - A `human` node with every requirement met is `waiting` on the owner; any other is `ready`.
//!   Its reason names its ask once `asks` pinned it (`AskPinned`, `AskLinked`, `AskWithdrawn` are
//!   inert). Its receipt is `Answered` (the owner's answer, read by `asks`), never a `Completed`,
//!   and an `Answered` counts only for the ask linked at that node and iteration.
//!
//! # Contract for the writers (3b2)
//!
//! Acceptance checks order and branches; freshness only decides whether a node is already done.
//! A writer must check, at the moment it records, that nothing the node consumes is stale, and
//! record the `inputs` that `status` names. An `until` may read only the repeat's body nodes,
//! since a verdict's digest covers only the body: its input ([`awaiting`]) holds nothing else.
//! Claims, leases and cancel are the writers' to enforce. Retry is projected here, at the read's
//! clock: a node whose `retry.attempts` have all ended without a receipt (by lease, cancel,
//! changed inputs or a later claim) is `failed`, unless it is an effect started over its current
//! inputs, which only its reconcile may finish. The writers (`write`) keep this
//! contract: `claim`, `heartbeat`, `complete` and `choose`, and the `until` verdicts. What a guard
//! reads is built here, from the log ([`guard_input`], [`awaiting`]); replay itself never runs it.
//!
//! # Cost
//!
//! A read is bounded, not fast at every bound. The bounds are on nodes, fan-in and depth
//! (`ir::MAX_NODES`, `ir::MAX_FAN_IN`, `ir::MAX_DEPTH`), events (`log::MAX_EVENTS`), line and log
//! bytes (`log::MAX_LINE`, `log::MAX_LOG`), the definition's and the IR's bytes, and nesting
//! (`log::MAX_NESTING`).
//!
//! - **Real flows** (the verifier, a five-round three-stratum review, a 13-step tick) replay in
//!   milliseconds.
//! - **Synthetic pathological flows** at the caps cost seconds. The worst measured in release is
//!   about 3.6 s (fifteen roots of a dense observation graph under a repeat with no `until`), and
//!   1.33 s for long ids with 6 KB outputs. The `#[ignore]` measurements in
//!   `tests/kernel/schedule.rs` cover two shapes.
//! - **Known cost drivers, for a follow-up:** a core's `observed` set is rebuilt per core; a
//!   receipt with new content drops the whole cone downstream of its node; and a repeat with no
//!   `until` re-evaluates its cone through `settled` at every event.

use super::compile::CompiledFlow;
use super::error::KernelError;
use super::events::{
    Answered, AskLinked, AskPinned, AskWithdrawn, Chosen, Claimed, Completed, EffectStarted, Guarded, Heartbeat,
    KernelEvent, Refused, Started, digest, receipt_digest,
};
use super::ir::{self, Class, Ir, Iteration, Launch, Need, Wait};
use super::write;
use crate::ids::InstanceId;
use crate::ledger::{Appended, Ledger, ReadGuard, random_epoch};
use crate::log::{Event, Log};
use jiff::{SignedDuration, Timestamp};
use maestro_core::session::{SessionRef, VerifiedSession};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

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
    /// A needed node has no receipt whose content is current.
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
    /// This observation is older than its `fresh_for`, or stamped after the read's clock.
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
    /// The node's `retry.attempts` have all ended without a receipt: it takes no more claims.
    AttemptsExhausted {
        attempts: u32,
    },
    /// The owner's answer to this ask, through the asks ledger. `ask` is the question the
    /// definition gives; once `asks::ask` has pinned the ask, `ask_id` and `ledger` name it, and
    /// `linked` says it was confirmed in the ledger (filed).
    Owner {
        ask: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        ask_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ledger: Option<String>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        linked: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NodeStatus {
    pub state: State,
    pub class: String,
    pub iteration: Iteration,
    /// What must change before the node can be done, or who it waits on.
    pub unmet: Vec<Reason>,
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
    /// The session that owns the instance (`Started`), so a waiting human node's question can be
    /// relayed toward it; `None` in a log written before owners were recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<SessionRef>,
    /// The last event replayed.
    pub seq: u64,
    pub nodes: BTreeMap<String, NodeStatus>,
    pub repeats: BTreeMap<String, RepeatStatus>,
    pub rejected: Vec<Rejected>,
}

/// A node to do now, and the hint for launching its performer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Enabled {
    pub node: String,
    pub class: String,
    pub iteration: Iteration,
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
    /// The instance's owner, as [`Status::owner`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<SessionRef>,
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

/// Starts an instance of `flow` with `params`: appends `Started`, which pins the IR and records
/// `session` as the instance's owner. A session with no owner marker starts nothing.
pub fn start(
    ledger: &mut Ledger,
    session: &VerifiedSession,
    flow: &CompiledFlow,
    params: Value,
) -> Result<Begun, KernelError> {
    let owner = Some(write::owner(session)?);
    check_params(flow.ir(), &params)?;
    let instance = InstanceId::new(format!("fi_{}", random_epoch()?))?;
    let started = Started { flow: flow.name(), ir_hash: flow.hash().to_string(), ir: flow.ir().clone(), params, owner };
    let event = started.to_event()?;
    let appended = ledger.lock()?.append(&instance, 0, event)?;
    Ok(Begun { instance, appended })
}

fn check_params(ir: &Ir, params: &Value) -> Result<(), KernelError> {
    let refuse = |detail: String| KernelError::InvalidParams { flow: format!("{}@{}", ir.flow, ir.version), detail };
    let Value::Object(given) = params else {
        return Err(refuse("params must be a JSON object".into()));
    };
    let bytes = serde_json::to_vec(params).map(|b| b.len()).unwrap_or(usize::MAX);
    if bytes > ir::MAX_PARAMS {
        return Err(refuse(format!("params are {bytes} bytes, more than {}", ir::MAX_PARAMS)));
    }
    // `Started` carries the params one level inside its body, whose nesting the ledger bounds.
    let depth = crate::log::nesting(params);
    if depth >= crate::log::MAX_NESTING {
        return Err(refuse(format!("params nest {depth} levels deep, more than {}", crate::log::MAX_NESTING - 1)));
    }
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
    Ok(fold.read(&log, &started, now).0)
}

/// The nodes to do now and what blocks the others. Reads the log under the shared lock and
/// writes nothing.
pub fn next(guard: &ReadGuard, instance: &InstanceId, now: Timestamp) -> Result<Next, KernelError> {
    let log = guard.log(instance)?;
    let started = pinned(&log)?;
    next_of(&log, &started, now)
}

/// [`next`] over a log already read and pinned: the writers' view of what is enabled now.
pub(crate) fn next_of(log: &Log, started: &Started, now: Timestamp) -> Result<Next, KernelError> {
    let fold = Fold::replay(log, started)?;
    let (status, stale) = fold.read(log, started, now);
    let mut enabled = Vec::new();
    let mut pending = BTreeMap::new();
    for (id, s) in status.nodes {
        // A stale receipt is redone only when nothing upstream of it is stale.
        let rerun = s.state == State::Stale && stale.get(&id).is_some_and(|set| set.iter().all(|s| *s == id));
        if s.state == State::Ready || rerun {
            let class = started.ir.nodes.get(&id).map(|n| &n.class);
            enabled.push(Enabled {
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
        owner: status.owner,
        seq: status.seq,
        enabled,
        pending,
    })
}

/// A repeat whose current iteration has settled and has no verdict, and the input its `until`
/// reads: the params and the receipts of its body at that iteration, and nothing else.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Awaiting {
    pub repeat: String,
    pub iteration: u32,
    pub until: String,
    pub input: Value,
}

/// Every repeat with an `until` that awaits its verdict now (`RepeatStatus::awaiting_verdict`).
pub(crate) fn awaiting(log: &Log, started: &Started) -> Result<Vec<Awaiting>, KernelError> {
    let fold = Fold::replay(log, started)?;
    let ir: &Ir = &started.ir;
    // Cores are clock-free: settling and receipts do not depend on the clock.
    let mut eval = Eval::new(&fold, Timestamp::UNIX_EPOCH);
    let mut out = Vec::new();
    for (rid, r) in &ir.repeats {
        let Some(until) = &r.until else { continue };
        let Loop::Running(i) = eval.phase(rid) else { continue };
        if !eval.settled(rid, i) {
            continue;
        }
        let read: Vec<(&str, Rc<Core>)> = r.body.iter().map(|b| (b.as_str(), eval.node(b, i))).collect();
        let input = document(log, started, read)?;
        out.push(Awaiting { repeat: rid.clone(), iteration: i, until: until.clone(), input });
    }
    Ok(out)
}

/// The input a guard on `node` reads at its current iteration: the params, and the receipts it
/// consumes now (each node it needs or switches on, and a repeat it needs through its body at the
/// iteration judged), which are exactly the receipts behind the `inputs` `next` names for it. Its
/// content is a function of those digests and the params, so a writer holding a claim bound to
/// the same inputs reads the same document.
pub(crate) fn guard_input(log: &Log, started: &Started, node: &str) -> Result<Value, KernelError> {
    let fold = Fold::replay(log, started)?;
    let ir: &Ir = &started.ir;
    let Some((id, _)) = ir.nodes.get_key_value(node) else {
        return Err(malformed(log, log.seq(), format!("the flow declares no node {node}")));
    };
    let id = id.as_str();
    let i = fold.current(id).unwrap_or(0);
    let mine = fold.member.get(id).copied();
    let mut eval = Eval::new(&fold, Timestamp::UNIX_EPOCH);
    let core = eval.node(id, i);
    let mut read = Vec::new();
    for m in core.consumed.keys().copied() {
        if ir.nodes.contains_key(m) {
            if let Dep::Eval(c) = eval.dep(m, mine, i) {
                read.push((m, c));
            }
        } else if let Loop::Done(j) = eval.phase(m) {
            for b in eval.body(m) {
                read.push((b.as_str(), eval.node(b, j)));
            }
        }
    }
    document(log, started, read)
}

/// `{"params": ..., "nodes": {"<id>": {"outputs": ..., "value": ...}}}` for each node read that has
/// a receipt; a node without one (it will never run) is left out. A node read here has a receipt
/// only if its content is current: what a guard reads is consumed, or a settled body.
fn document(log: &Log, started: &Started, read: Vec<(&str, Rc<Core>)>) -> Result<Value, KernelError> {
    let mut nodes = serde_json::Map::new();
    for (id, core) in read {
        let Some(seq) = core.seq else { continue };
        let index = usize::try_from(seq.saturating_sub(1)).unwrap_or(usize::MAX);
        let e = log.events.get(index).filter(|e| e.seq == seq).ok_or_else(|| malformed(log, seq, "no such event"))?;
        let outputs = e.body.get("outputs").cloned().unwrap_or(Value::Null);
        nodes.insert(id.to_string(), serde_json::json!({ "outputs": outputs, "value": core.value }));
    }
    Ok(serde_json::json!({ "params": started.params, "nodes": nodes }))
}

fn malformed(log: &Log, seq: u64, detail: impl Into<String>) -> KernelError {
    KernelError::Malformed { instance: log.instance.to_string(), seq, detail: detail.into() }
}

fn body<T: DeserializeOwned>(log: &Log, e: &Event) -> Result<T, KernelError> {
    serde_json::from_value(e.body.clone())
        .map_err(|err| malformed(log, e.seq, format!("a {} body that does not parse ({err})", e.kind)))
}

pub(crate) fn secs(s: u64) -> SignedDuration {
    SignedDuration::from_secs(i64::try_from(s).unwrap_or(i64::MAX))
}

/// The `Started` event, checked: the first event, an IR of this kernel's version that validates,
/// matches its hash and is the flow it names.
pub(crate) fn pinned(log: &Log) -> Result<Started, KernelError> {
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

/// A node at an iteration, inside replay: its id, borrowed from the IR, and its iteration index,
/// 0 outside every repeat. A node belongs to at most one repeat, so the index is the iteration.
type Key<'a> = (&'a str, u32);

/// An accepted receipt.
#[derive(Debug, Clone)]
struct Receipt {
    /// Its event's position in the log, where a guard's input reads its outputs.
    seq: u64,
    at: Timestamp,
    /// The observed value or the chosen edge.
    value: Option<String>,
    /// Why this receipt fails its node, for an observation whose exit status maps to nothing.
    failure: Option<Reason>,
    inputs: BTreeMap<String, String>,
    /// [`receipt_digest`] of its outputs and value, computed once when it is accepted.
    digest: Rc<str>,
}

impl Receipt {
    fn new(
        seq: u64,
        at: Timestamp,
        outputs: &Value,
        value: Option<String>,
        failure: Option<Reason>,
        inputs: BTreeMap<String, String>,
    ) -> Self {
        let digest = Rc::from(receipt_digest(outputs, value.as_deref()));
        Receipt { seq, at, value, failure, inputs, digest }
    }
}

/// A repeat's place in the fold: the iteration it is at, or the one it ran out at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    At(u32),
    Exhausted(u32),
}

/// A repeat as seen at a clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Loop {
    Running(u32),
    Done(u32),
    Exhausted(u32),
    Unreachable,
}

/// What an accepted event did: nothing a node's state depends on, or something that may.
enum Change {
    Inert,
    State,
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
    receipts: BTreeMap<Key<'a>, Receipt>,
    phases: BTreeMap<&'a str, Phase>,
    /// True verdicts, by repeat and iteration, with the digest of the body they judged.
    verdicts: BTreeMap<(&'a str, u32), Rc<str>>,
    rejected: Vec<Rejected>,
    /// Node cores (evaluations without the clock), kept across events: an accepted receipt with
    /// new content drops those downstream of its node, a repeat that moves or is judged drops
    /// them all.
    cache: RefCell<BTreeMap<Key<'a>, Rc<Core<'a>>>>,
    /// Body digests kept across events, dropped with the evaluation of any body node.
    digests: RefCell<BTreeMap<(&'a str, u32), Rc<str>>>,
    /// The nodes whose evaluation reads each node: its dependents, and for a body node, every
    /// node that waits on its repeat.
    down: BTreeMap<&'a str, BTreeSet<&'a str>>,
    /// How many times `advance` ran and how many evaluators were built: the work a replay did,
    /// for the tests that bound it.
    advances: usize,
    evaluations: Cell<usize>,
    /// How many node cores were computed (not taken from the cache): the bound tests' ceiling.
    cores: Cell<usize>,
    /// The claims of each node with `retry`, by iteration, in log order (the retry projection).
    attempts: BTreeMap<Key<'a>, Vec<Attempt>>,
    /// The effects started and not withdrawn by a refusal that their command never started
    /// (`spawn_failed`): by node and iteration, each attempt's inputs.
    started_effects: BTreeMap<Key<'a>, BTreeMap<u32, BTreeMap<String, String>>>,
    /// Each human node's ask at an iteration: the pin that stands, and whether it was linked.
    asks: BTreeMap<Key<'a>, AskRec>,
}

/// A human node's ask as the log has it: pinned (`AskPinned`), and linked once confirmed
/// (`AskLinked`). A withdrawal (`AskWithdrawn`) of an unlinked pin removes it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AskRec {
    pub ask_id: String,
    pub ledger: String,
    pub body: Value,
    pub body_hash: String,
    pub linked: bool,
}

/// The ask of `node` at its current iteration, as replay accepted it.
pub(crate) fn ask_of(log: &Log, started: &Started, node: &str) -> Result<Option<AskRec>, KernelError> {
    let fold = Fold::replay(log, started)?;
    let Some(i) = fold.current(node) else { return Ok(None) };
    Ok(started.ir.nodes.get_key_value(node).and_then(|(id, _)| fold.asks.get(&(id.as_str(), i)).cloned()))
}

/// A claim, for the retry projection: it ends at its lease's end (which is never past its cancel
/// deadline), when the inputs it was bound to are no longer the node's, or when a later claim
/// takes the node, unless a receipt spent it.
#[derive(Debug, Clone)]
struct Attempt {
    attempt: u32,
    lease_until: Timestamp,
    inputs: BTreeMap<String, String>,
    spent: bool,
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
            phases: ir.repeats.keys().map(|r| (r.as_str(), Phase::At(0))).collect(),
            verdicts: BTreeMap::new(),
            rejected: Vec::new(),
            cache: RefCell::new(BTreeMap::new()),
            digests: RefCell::new(BTreeMap::new()),
            down: downstream(ir),
            advances: 0,
            evaluations: Cell::new(0),
            cores: Cell::new(0),
            attempts: BTreeMap::new(),
            started_effects: BTreeMap::new(),
            asks: BTreeMap::new(),
        };
        for e in log.events.iter().skip(1) {
            let at: Timestamp =
                e.at.parse().map_err(|_| malformed(log, e.seq, format!("{} is not a timestamp", e.at)))?;
            match fold.accept(log, e, at)? {
                Ok(Change::Inert) => {}
                Ok(Change::State) => fold.advance(at),
                Err(reason) => {
                    let subject = e.body.get("node").or_else(|| e.body.get("repeat"));
                    let subject = subject.and_then(Value::as_str).unwrap_or_default().to_string();
                    fold.rejected.push(Rejected { seq: e.seq, kind: e.kind.clone(), subject, reason: reason.into() });
                }
            }
        }
        Ok(fold)
    }

    /// Drops every kept evaluation and body digest: a repeat moved or was judged.
    fn forget(&self) {
        self.cache.borrow_mut().clear();
        self.digests.borrow_mut().clear();
    }

    /// Drops the kept evaluations of `node` and of every node downstream of it.
    fn invalidate(&self, node: &'a str) {
        let mut cache = self.cache.borrow_mut();
        let mut digests = self.digests.borrow_mut();
        let mut todo = vec![node];
        let mut seen = BTreeSet::new();
        while let Some(n) = todo.pop() {
            if seen.insert(n) {
                let kept: Vec<Key<'a>> = cache.range((n, 0)..=(n, u32::MAX)).map(|(k, _)| *k).collect();
                for k in kept {
                    cache.remove(&k);
                }
                if let Some(r) = self.member.get(n) {
                    digests.retain(|(rid, _), _| rid != r);
                }
                todo.extend(self.down.get(n).into_iter().flatten().copied());
            }
        }
    }

    /// The iteration index a node's events must carry now; `None` once its repeat is exhausted.
    fn current(&self, node: &str) -> Option<u32> {
        match self.member.get(node) {
            None => Some(0),
            Some(rid) => match self.phases.get(rid)? {
                Phase::At(i) => Some(*i),
                Phase::Exhausted(_) => None,
            },
        }
    }

    /// The public form of a node's iteration index: `{repeat: i}`, or `{}` outside every repeat.
    fn iteration(&self, node: &str, i: u32) -> Iteration {
        self.member.get(node).map(|rid| Iteration::from([(rid.to_string(), i)])).unwrap_or_default()
    }

    /// Applies one event if it fits; the inner error is why it was set aside.
    fn accept(&mut self, log: &Log, e: &Event, at: Timestamp) -> Result<Result<Change, &'static str>, KernelError> {
        if e.kind == Guarded::KIND {
            let g: Guarded = body(log, e)?;
            return Ok(self.guarded(g, at).map(|()| Change::State));
        }
        let kinds = [
            Completed::KIND,
            Chosen::KIND,
            Claimed::KIND,
            Heartbeat::KIND,
            Refused::KIND,
            EffectStarted::KIND,
            AskPinned::KIND,
            AskLinked::KIND,
            AskWithdrawn::KIND,
            Answered::KIND,
        ];
        if !kinds.contains(&e.kind.as_str()) {
            return Err(malformed(log, e.seq, format!("{} is not a kernel event", e.kind)));
        }
        let subject: Subject = body(log, e)?;
        let ir: &'a Ir = self.ir;
        let Some((id, node)) = ir.nodes.get_key_value(&subject.node) else { return Ok(Err("unknown_node")) };
        let id = id.as_str();
        let Some(i) = self.current(id).filter(|i| self.iteration(id, *i) == subject.iteration) else {
            return Ok(Err("not_current_iteration"));
        };
        let receipt = match e.kind.as_str() {
            Completed::KIND => {
                let c: Completed = body(log, e)?;
                let (value, failure) = match &node.class {
                    // A human node's receipt is the owner's answer (`Answered`), never a completion.
                    Class::Choice { .. } | Class::Human { .. } => return Ok(Err("wrong_class")),
                    Class::Observe { exit_map, .. } => {
                        let exit = c.verify.as_ref().map(|v| v.exit);
                        match exit.and_then(|x| exit_map.iter().find(|m| m.exit == x)) {
                            Some(m) => (Some(m.value.clone()), None),
                            None => (None, Some(Reason::ExitUnmapped { exit })),
                        }
                    }
                    _ => (None, None),
                };
                // An observation is as old as its run, never younger than its append.
                let taken = match &node.class {
                    Class::Observe { .. } => {
                        let ran = c.verify.as_ref().and_then(|v| v.ran_at.parse::<Timestamp>().ok());
                        ran.map_or(at, |ran| ran.min(at))
                    }
                    _ => at,
                };
                (Receipt::new(e.seq, taken, &c.outputs, value, failure, c.inputs), c.attempt)
            }
            Chosen::KIND => {
                let c: Chosen = body(log, e)?;
                let Class::Choice { edges, .. } = &node.class else { return Ok(Err("wrong_class")) };
                if !edges.contains_key(&c.edge) {
                    return Ok(Err("undeclared_edge"));
                }
                (Receipt::new(e.seq, at, &Value::Null, Some(c.edge), None, c.inputs), c.attempt)
            }
            Claimed::KIND => {
                let c: Claimed = body(log, e)?;
                if node.retry.is_some() {
                    let stamp = |text: &str| {
                        text.parse::<Timestamp>()
                            .map_err(|_| malformed(log, e.seq, format!("{text} is not a timestamp")))
                    };
                    let claims = self.attempts.entry((id, i)).or_default();
                    if claims.last().is_none_or(|last| c.attempt > last.attempt) {
                        let lease_until = stamp(&c.lease_until)?;
                        claims.push(Attempt { attempt: c.attempt, lease_until, inputs: c.inputs, spent: false });
                    }
                }
                return Ok(Ok(Change::Inert));
            }
            Heartbeat::KIND => {
                let h: Heartbeat = body(log, e)?;
                if let Some(last) = self.attempts.get_mut(&(id, i)).and_then(|c| c.last_mut())
                    && last.attempt == h.attempt
                {
                    let at: Timestamp = h
                        .lease_until
                        .parse()
                        .map_err(|_| malformed(log, e.seq, format!("{} is not a timestamp", h.lease_until)))?;
                    last.lease_until = at;
                }
                return Ok(Ok(Change::Inert));
            }
            EffectStarted::KIND => {
                let s: EffectStarted = body(log, e)?;
                self.started_effects.entry((id, i)).or_default().insert(s.attempt, s.inputs);
                return Ok(Ok(Change::Inert));
            }
            Answered::KIND => {
                let a: Answered = body(log, e)?;
                if !matches!(node.class, Class::Human { .. }) {
                    return Ok(Err("wrong_class"));
                }
                // Only an answer to the linked ask counts.
                if !self.asks.get(&(id, i)).is_some_and(|r| r.linked && r.ask_id == a.ask_id) {
                    return Ok(Err("not_linked"));
                }
                (Receipt::new(e.seq, at, &a.outputs, None, None, a.inputs), 0)
            }
            AskPinned::KIND => {
                let p: AskPinned = body(log, e)?;
                if !matches!(node.class, Class::Human { .. }) {
                    return Ok(Err("wrong_class"));
                }
                // A pin stands until it is withdrawn; a linked one is never replaced.
                if self.asks.contains_key(&(id, i)) {
                    return Ok(Err("already_pinned"));
                }
                let rec =
                    AskRec { ask_id: p.ask_id, ledger: p.ledger, body: p.body, body_hash: p.body_hash, linked: false };
                self.asks.insert((id, i), rec);
                return Ok(Ok(Change::Inert));
            }
            AskLinked::KIND => {
                let l: AskLinked = body(log, e)?;
                return Ok(match self.asks.get_mut(&(id, i)) {
                    Some(r) if !r.linked && r.ask_id == l.ask_id && r.body_hash == l.body_hash => {
                        r.linked = true;
                        Ok(Change::Inert)
                    }
                    _ => Err("not_pinned"),
                });
            }
            AskWithdrawn::KIND => {
                let w: AskWithdrawn = body(log, e)?;
                if !self.asks.get(&(id, i)).is_some_and(|r| !r.linked && r.ask_id == w.ask_id) {
                    return Ok(Err("not_pinned"));
                }
                self.asks.remove(&(id, i));
                return Ok(Ok(Change::Inert));
            }
            _ => {
                let r: Refused = body(log, e)?;
                // The effect's command never started (`exec`), so its start no longer stands.
                if r.code == "spawn_failed"
                    && let Some(started) = self.started_effects.get_mut(&(id, i))
                {
                    started.remove(&r.attempt);
                }
                return Ok(Ok(Change::Inert));
            }
        };
        let (receipt, attempt) = receipt;
        let key = (id, i);
        let mut eval = Eval::new(self, at);
        let core = eval.node(id, i);
        if !core.enabled {
            return Ok(Err("not_enabled"));
        }
        if eval.seen(key, &core).state == State::Done && !matches!(node.class, Class::Observe { .. }) {
            return Ok(Err("already_done"));
        }
        // A receipt with the content of the one it replaces (a re-run with the same result)
        // changes no core: only its time, which the clock reads.
        let same = self.receipts.get(&key).is_some_and(|old| {
            old.digest == receipt.digest && old.failure == receipt.failure && old.inputs == receipt.inputs
        });
        if !same {
            self.invalidate(id);
        }
        self.receipts.insert(key, receipt);
        if let Some(last) = self.attempts.get_mut(&key).and_then(|c| c.last_mut())
            && last.attempt == attempt
        {
            last.spent = true;
        }
        Ok(Ok(Change::State))
    }

    /// The node's `retry.attempts`, when they have all ended without a receipt at `now`, in its
    /// current iteration. An effect started over its current `inputs` is never exhausted: only
    /// its reconcile runs, and a reconcile takes no attempt. (A done node is never exhausted, so
    /// a start a receipt followed needs no clearing.)
    fn exhausted(&self, id: &'a str, now: Timestamp, inputs: &BTreeMap<String, String>) -> Option<u32> {
        let attempts = self.ir.nodes.get(id)?.retry?;
        let i = self.current(id)?;
        if self.started_effects.get(&(id, i)).is_some_and(|s| s.values().any(|started| started == inputs)) {
            return None;
        }
        let claims = self.attempts.get(&(id, i))?;
        let ended = claims
            .iter()
            .enumerate()
            .filter(|(k, c)| !c.spent && (k + 1 < claims.len() || now >= c.lease_until || c.inputs != *inputs))
            .count();
        (ended >= usize::try_from(attempts).unwrap_or(usize::MAX)).then_some(attempts)
    }

    fn guarded(&mut self, g: Guarded, at: Timestamp) -> Result<(), &'static str> {
        let ir: &'a Ir = self.ir;
        let Some((rid, r)) = ir.repeats.get_key_value(&g.repeat) else { return Err("unknown_repeat") };
        let rid = rid.as_str();
        if r.until.is_none() {
            return Err("no_until");
        }
        if self.phases.get(rid) != Some(&Phase::At(g.iteration)) {
            return Err("not_current_iteration");
        }
        let mut eval = Eval::new(self, at);
        if !eval.settled(rid, g.iteration) {
            return Err("not_settled");
        }
        let judged = eval.body_digest(rid, g.iteration);
        if self.verdicts.get(&(rid, g.iteration)) == Some(&judged) {
            return Err("already_judged");
        }
        if g.holds {
            self.forget();
            self.verdicts.insert((rid, g.iteration), judged);
        } else {
            let next = self.after(r.max, g.iteration);
            self.forget();
            self.phases.insert(rid, next);
        }
        Ok(())
    }

    /// Where a repeat goes after iteration `i` ends without its `until` holding.
    fn after(&self, max: u32, i: u32) -> Phase {
        if i.saturating_add(1) >= max { Phase::Exhausted(i) } else { Phase::At(i + 1) }
    }

    /// Moves every repeat with no `until` whose current iteration settled on, until none moves.
    fn advance(&mut self, at: Timestamp) {
        self.advances += 1;
        let ir: &'a Ir = self.ir;
        loop {
            let mut moves = Vec::new();
            let mut eval = Eval::new(self, at);
            for (rid, r) in ir.repeats.iter().filter(|(_, r)| r.until.is_none()) {
                if let Some(Phase::At(i)) = self.phases.get(rid.as_str()).copied()
                    && eval.settled(rid, i)
                {
                    moves.push((rid.as_str(), self.after(r.max, i)));
                }
            }
            if moves.is_empty() {
                return;
            }
            self.forget();
            self.phases.extend(moves);
        }
    }

    /// Every node's state at `now`, and each node's stale set (for `next`'s re-run rule).
    fn read(&self, log: &Log, started: &Started, now: Timestamp) -> (Status, BTreeMap<String, BTreeSet<String>>) {
        let ir: &'a Ir = self.ir;
        let mut eval = Eval::new(self, now);
        let mut nodes = BTreeMap::new();
        let mut stale = BTreeMap::new();
        for id in ir.nodes.keys() {
            let (mut status, set) = eval.status(id);
            if let Some(attempts) = self.exhausted(id, now, &status.inputs)
                && !matches!(status.state, State::Done | State::Exhausted)
            {
                status.state = State::Failed;
                status.unmet.push(Reason::AttemptsExhausted { attempts });
            }
            nodes.insert(id.clone(), status);
            stale.insert(id.clone(), set);
        }
        let repeats = ir
            .repeats
            .keys()
            .map(|rid| {
                let (state, iteration) = match eval.phase(rid) {
                    Loop::Running(i) => (RepeatState::Running, i),
                    Loop::Done(i) => (RepeatState::Done, i),
                    Loop::Exhausted(i) => (RepeatState::Exhausted, i),
                    Loop::Unreachable => (RepeatState::Unreachable, 0),
                };
                // A repeat with no `until` never rests settled: it moves on as it settles.
                let awaiting_verdict = state == RepeatState::Running && eval.settled(rid, iteration);
                (rid.clone(), RepeatStatus { state, iteration, awaiting_verdict })
            })
            .collect();
        let status = Status {
            instance: log.instance.to_string(),
            flow: started.flow.clone(),
            ir_hash: started.ir_hash.clone(),
            owner: started.owner.as_ref().map(|o| o.session.clone()),
            seq: log.seq(),
            nodes,
            repeats,
            rejected: self.rejected.clone(),
        };
        (status, stale)
    }
}

/// For each node, the nodes whose evaluation reads it (see `Fold::down`).
fn downstream(ir: &Ir) -> BTreeMap<&str, BTreeSet<&str>> {
    let member: BTreeMap<&str, &str> =
        ir.repeats.iter().flat_map(|(rid, r)| r.body.iter().map(move |b| (b.as_str(), rid.as_str()))).collect();
    let body = |r: &str| ir.repeats.get(r).map(|r| r.body.as_slice()).unwrap_or_default();
    let mut down: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (id, n) in &ir.nodes {
        let mut reads: Vec<&str> = n.switch.iter().map(|c| c.on.as_str()).collect();
        for need in &n.needs {
            match need {
                Need::Node { node } => reads.push(node),
                Need::Repeat { repeat } | Need::Exhausted { repeat } => {
                    reads.extend(body(repeat).iter().map(String::as_str))
                }
            }
        }
        for m in reads.clone() {
            if let Some(r) = member.get(m)
                && member.get(id.as_str()) != Some(r)
            {
                reads.extend(body(r).iter().map(String::as_str));
            }
        }
        for m in reads {
            down.entry(m).or_default().insert(id.as_str());
        }
    }
    down
}

/// A node evaluated at one iteration, without the clock: what its requirements and its receipt
/// say. It is a function of the folded state alone, so it is kept across events; freshness is
/// applied at a clock by [`Eval::seen`].
#[derive(Debug, Clone)]
struct Core<'a> {
    /// Unmet requirements, the receipt's failure, or the inputs that changed since it.
    reasons: Vec<Reason>,
    form: Form,
    /// It will never run in this iteration.
    dead: bool,
    /// Every requirement is met: a receipt would be accepted.
    enabled: bool,
    /// Its receipt's content is current: its digest is what a dependent consumes.
    satisfies: bool,
    /// The digest of its receipt, for a dependent's inputs.
    digest: Option<Rc<str>>,
    /// Its receipt's position in the log.
    seq: Option<u64>,
    value: Option<String>,
    /// The digests a receipt for this node must record.
    consumed: BTreeMap<&'a str, Rc<str>>,
    /// The observation receipts with `fresh_for` at or upstream of this node: where the clock
    /// can make it stale.
    observed: BTreeSet<Key<'a>>,
}

/// What a node's core is, before the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// A requirement is unmet, or it will never run.
    Blocked,
    /// Its receipt fails it.
    Failed,
    /// It has a receipt.
    Recorded,
    /// Every requirement is met and it has no receipt.
    Open,
}

impl Core<'_> {
    fn new() -> Self {
        Core {
            reasons: Vec::new(),
            form: Form::Blocked,
            dead: false,
            enabled: false,
            satisfies: false,
            digest: None,
            seq: None,
            value: None,
            consumed: BTreeMap::new(),
            observed: BTreeSet::new(),
        }
    }
}

/// A node's state at a clock.
struct Seen {
    state: State,
    reasons: Vec<Reason>,
    /// The stale observations at or upstream of it.
    stale: BTreeSet<String>,
}

/// What a dependency looks like from a node's iteration.
enum Dep<'a> {
    Eval(Rc<Core<'a>>),
    Waits(Reason),
    Dead(Reason),
}

#[cfg(test)]
thread_local! {
    /// Set by the differential tests: evaluate everything afresh, keeping nothing across events.
    static NO_CACHE: Cell<bool> = const { Cell::new(false) };
}

/// Whether evaluations are kept across events (always, outside the differential tests).
fn keep() -> bool {
    #[cfg(test)]
    if NO_CACHE.with(Cell::get) {
        return false;
    }
    true
}

/// Evaluates the folded state. Nothing here changes the fold. Recursion follows `needs`, whose
/// depth `ir::validate` bounds; the clock (`now`) is read only by [`Eval::seen`].
struct Eval<'f, 'a> {
    fold: &'f Fold<'a>,
    now: Timestamp,
    memo: BTreeMap<Key<'a>, Rc<Core<'a>>>,
}

impl<'f, 'a> Eval<'f, 'a> {
    fn new(fold: &'f Fold<'a>, now: Timestamp) -> Self {
        fold.evaluations.set(fold.evaluations.get() + 1);
        Eval { fold, now, memo: BTreeMap::new() }
    }

    fn body(&self, rid: &str) -> &'a [String] {
        let ir: &'a Ir = self.fold.ir;
        ir.repeats.get(rid).map(|r| r.body.as_slice()).unwrap_or_default()
    }

    /// The digest of a repeat's body at iteration `i`: each body node's receipt digest, or none.
    fn body_digest(&mut self, rid: &'a str, i: u32) -> Rc<str> {
        if keep()
            && let Some(d) = self.fold.digests.borrow().get(&(rid, i))
        {
            return d.clone();
        }
        let body: BTreeMap<&str, Option<String>> = self
            .body(rid)
            .iter()
            .map(|b| {
                let e = self.node(b, i);
                (b.as_str(), e.digest.as_deref().filter(|_| e.satisfies).map(str::to_string))
            })
            .collect();
        let d: Rc<str> = Rc::from(digest(&serde_json::json!(body)));
        if keep() {
            self.fold.digests.borrow_mut().insert((rid, i), d.clone());
        }
        d
    }

    fn phase(&mut self, rid: &'a str) -> Loop {
        match self.fold.phases.get(rid).copied() {
            Some(Phase::Exhausted(i)) => Loop::Exhausted(i),
            Some(Phase::At(i)) => {
                if let Some(judged) = self.fold.verdicts.get(&(rid, i))
                    && *judged == self.body_digest(rid, i)
                {
                    return Loop::Done(i);
                }
                if self.body(rid).iter().all(|b| self.node(b, i).dead) { Loop::Unreachable } else { Loop::Running(i) }
            }
            None => Loop::Unreachable,
        }
    }

    /// Iteration `i` of `rid` has settled: every body node's receipt content is current or the
    /// node will never run, and not all of them will never run.
    fn settled(&mut self, rid: &'a str, i: u32) -> bool {
        let evals: Vec<Rc<Core<'a>>> = self.body(rid).iter().map(|b| self.node(b, i)).collect();
        evals.iter().all(|e| e.dead || e.satisfies) && !evals.iter().all(|e| e.dead)
    }

    /// A node's reported state at this evaluator's clock, and its stale set: at its repeat's
    /// current iteration, or exhausted with it.
    fn status(&mut self, id: &'a str) -> (NodeStatus, BTreeSet<String>) {
        let class = self.fold.ir.nodes.get(id).map_or("", |n| n.class.name()).to_string();
        let i = match self.fold.member.get(id).copied() {
            None => 0,
            Some(rid) => match self.fold.phases.get(rid).copied() {
                Some(Phase::Exhausted(i)) => {
                    let unmet = vec![Reason::RepeatExhausted { repeat: rid.to_string() }];
                    let iteration = self.fold.iteration(id, i);
                    let status =
                        NodeStatus { state: State::Exhausted, class, iteration, unmet, inputs: BTreeMap::new() };
                    return (status, BTreeSet::new());
                }
                Some(Phase::At(i)) => i,
                None => 0,
            },
        };
        let core = self.node(id, i);
        let seen = self.seen((id, i), &core);
        let inputs = core.consumed.iter().map(|(k, d)| (k.to_string(), d.to_string())).collect();
        let iteration = self.fold.iteration(id, i);
        (NodeStatus { state: seen.state, class, iteration, unmet: seen.reasons, inputs }, seen.stale)
    }

    /// When the observation receipt `key` is stale at this clock, its age.
    fn expired(&self, key: Key<'a>) -> Option<SignedDuration> {
        let r = self.fold.receipts.get(&key)?;
        let Some(Class::Observe { fresh_for: Some(f), .. }) = self.fold.ir.nodes.get(key.0).map(|n| &n.class) else {
            return None;
        };
        let age = self.now.duration_since(r.at);
        (age > secs(*f) || age.is_negative()).then_some(age)
    }

    /// A node's state at this evaluator's clock: its core, with freshness applied. A node is done
    /// only while nothing upstream, or itself, is stale.
    fn seen(&self, key: Key<'a>, c: &Core<'a>) -> Seen {
        let id = key.0;
        let stale: BTreeSet<String> =
            c.observed.iter().filter(|k| self.expired(**k).is_some()).map(|k| k.0.to_string()).collect();
        let mut reasons = c.reasons.clone();
        let state = match c.form {
            Form::Blocked => State::Blocked,
            Form::Failed => State::Failed,
            Form::Recorded => {
                if let Some(age) = self.expired(key) {
                    let fresh_for_secs = match self.fold.ir.nodes.get(id).map(|n| &n.class) {
                        Some(Class::Observe { fresh_for: Some(f), .. }) => *f,
                        _ => 0,
                    };
                    reasons.push(Reason::Expired { age_secs: age.as_secs(), fresh_for_secs });
                }
                reasons.extend(stale.iter().filter(|s| *s != id).map(|s| Reason::Stale { node: s.clone() }));
                if reasons.is_empty() { State::Done } else { State::Stale }
            }
            Form::Open if !stale.is_empty() => {
                reasons = stale.iter().map(|s| Reason::Stale { node: s.clone() }).collect();
                State::Blocked
            }
            Form::Open => match self.fold.ir.nodes.get(id).map(|n| &n.class) {
                Some(Class::Human { ask }) => {
                    let rec = self.fold.asks.get(&key);
                    reasons.push(Reason::Owner {
                        ask: ask.clone(),
                        ask_id: rec.map(|r| r.ask_id.clone()),
                        ledger: rec.map(|r| r.ledger.clone()),
                        linked: rec.is_some_and(|r| r.linked),
                    });
                    State::Waiting
                }
                _ => State::Ready,
            },
        };
        Seen { state, reasons, stale }
    }

    /// A dependency `m` seen from a node in repeat `mine` (or none) at iteration index `i`.
    fn dep(&mut self, m: &'a str, mine: Option<&'a str>, i: u32) -> Dep<'a> {
        let Some(rid) = self.fold.member.get(m).copied() else {
            return Dep::Eval(self.node(m, 0));
        };
        if mine == Some(rid) {
            return Dep::Eval(self.node(m, i));
        }
        match self.phase(rid) {
            Loop::Running(_) => Dep::Waits(Reason::RepeatRunning { repeat: rid.to_string() }),
            Loop::Exhausted(_) => Dep::Dead(Reason::RepeatExhausted { repeat: rid.to_string() }),
            Loop::Unreachable => Dep::Dead(Reason::RepeatUnreachable { repeat: rid.to_string() }),
            Loop::Done(j) => Dep::Eval(self.node(m, j)),
        }
    }

    fn node(&mut self, id: &'a str, i: u32) -> Rc<Core<'a>> {
        let key = (id, i);
        if let Some(e) = self.memo.get(&key) {
            return e.clone();
        }
        let kept = if keep() { self.fold.cache.borrow().get(&key).cloned() } else { None };
        let out = match kept {
            Some(e) => e,
            None => {
                self.fold.cores.set(self.fold.cores.get() + 1);
                let out = Rc::new(self.eval_node(id, i));
                if keep() {
                    self.fold.cache.borrow_mut().insert(key, out.clone());
                }
                out
            }
        };
        self.memo.insert(key, out.clone());
        out
    }

    fn eval_node(&mut self, id: &'a str, i: u32) -> Core<'a> {
        let fold = self.fold;
        let ir: &'a Ir = fold.ir;
        let Some(n) = ir.nodes.get(id) else {
            return Core { dead: true, ..Core::new() };
        };
        let mine = fold.member.get(id).copied();
        let mut out = Core::new();
        for need in &n.needs {
            match need {
                Need::Node { node: m } => {
                    self.consume(m, mine, i, &mut out);
                }
                Need::Repeat { repeat: r } => match self.phase(r) {
                    Loop::Done(j) => {
                        for b in self.body(r) {
                            let e = self.node(b, j);
                            out.observed.extend(e.observed.iter().copied());
                        }
                        let judged = self.body_digest(r, j);
                        out.consumed.insert(r.as_str(), judged);
                    }
                    Loop::Running(_) => push(&mut out, Reason::RepeatRunning { repeat: r.clone() }),
                    Loop::Exhausted(_) => kill(&mut out, Reason::RepeatExhausted { repeat: r.clone() }),
                    Loop::Unreachable => kill(&mut out, Reason::RepeatUnreachable { repeat: r.clone() }),
                },
                Need::Exhausted { repeat: r } => match self.phase(r) {
                    Loop::Exhausted(_) => {}
                    Loop::Running(_) => push(&mut out, Reason::RepeatRunning { repeat: r.clone() }),
                    Loop::Done(_) => kill(&mut out, Reason::RepeatFinished { repeat: r.clone() }),
                    Loop::Unreachable => kill(&mut out, Reason::RepeatUnreachable { repeat: r.clone() }),
                },
            }
        }
        // A switch on a node it also needs reads it once: the need recorded why not, if not.
        let needed = |on: &str| n.needs.iter().any(|x| matches!(x, Need::Node { node } if node == on));
        if let Some(case) = &n.switch
            && let Some(e) = match needed(&case.on) {
                true => match self.dep(&case.on, mine, i) {
                    Dep::Eval(e) => Some(e),
                    _ => None,
                },
                false => self.consume(&case.on, mine, i, &mut out),
            }
            && e.satisfies
            && e.value.as_deref() != Some(case.value.as_str())
        {
            kill(&mut out, Reason::Switch { on: case.on.clone(), took: e.value.clone(), case: case.value.clone() });
        }
        out.enabled = !out.dead && out.reasons.is_empty();
        if !out.enabled {
            return out;
        }
        let Some(r) = fold.receipts.get(&(id, i)) else {
            out.form = Form::Open;
            return out;
        };
        if let Some(failure) = &r.failure {
            out.form = Form::Failed;
            out.reasons.push(failure.clone());
            return out;
        }
        for (input, d) in &out.consumed {
            if r.inputs.get(*input).map(String::as_str) != Some(&**d) {
                out.reasons.push(Reason::InputChanged { input: input.to_string() });
            }
        }
        out.satisfies = out.reasons.is_empty();
        out.form = Form::Recorded;
        out.value = r.value.clone();
        out.digest = Some(r.digest.clone());
        out.seq = Some(r.seq);
        if let Class::Observe { fresh_for: Some(_), .. } = &n.class {
            out.observed.insert((id, i));
        }
        out
    }

    /// Requires `m`, recording why not or the digest a receipt consumes from it.
    fn consume(&mut self, m: &'a str, mine: Option<&'a str>, i: u32, out: &mut Core<'a>) -> Option<Rc<Core<'a>>> {
        match self.dep(m, mine, i) {
            Dep::Eval(e) => {
                out.observed.extend(e.observed.iter().copied());
                // A node is needed once (compile drops a repeated need), so its reason is too.
                if e.dead {
                    out.dead = true;
                    out.reasons.push(Reason::Unreachable { node: m.to_string() });
                } else if !e.satisfies {
                    out.reasons.push(Reason::Needs { node: m.to_string() });
                } else if let Some(d) = &e.digest {
                    out.consumed.insert(m, d.clone());
                }
                Some(e)
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
}

/// Records why a node is not ready, once: a node may wait on one repeat for several reasons (two
/// of its body nodes, or the repeat itself and a body node), and a switch is a reason of its own.
fn push(out: &mut Core, reason: Reason) {
    if !out.reasons.contains(&reason) {
        out.reasons.push(reason);
    }
}

/// Marks a node as never running, for `reason`, once.
fn kill(out: &mut Core, reason: Reason) {
    out.dead = true;
    push(out, reason);
}

#[cfg(test)]
#[path = "replay_gaps.rs"]
mod gaps;

#[cfg(test)]
#[path = "replay_oracle.rs"]
mod oracle;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::compile;

    fn event(seq: u64, kind: &str, body: Value) -> Event {
        Event { seq, prev_event: None, at: "2026-11-08T00:00:00Z".into(), kind: kind.into(), body }
    }

    #[test]
    fn inert_and_rejected_events_never_advance_or_evaluate() {
        let flow = compile("flow: f\nversion: 1\nnodes:\n  a: {class: pure, run: [x]}\n  b: {class: pure, run: [x]}\nrepeats:\n  r: {max: 2, body: [b]}\n").unwrap();
        let started = Started {
            flow: flow.name(),
            ir_hash: flow.hash().into(),
            ir: flow.ir().clone(),
            params: Value::Null,
            owner: None,
        };
        let mut events = vec![event(1, "Started", serde_json::to_value(&started).unwrap())];
        let at = |node: &str| serde_json::json!({ "node": node, "iteration": {}, "attempt": 1, "lease_until": "2026-11-08T00:00:00Z", "token_sha256": "x", "code": "x", "reason": "x" });
        for (i, kind) in ["Claimed", "Heartbeat", "Refused"].iter().cycle().take(300).enumerate() {
            events.push(event(i as u64 + 2, kind, at("a")));
        }
        // Rejected before any evaluation: an undeclared node, another iteration, the wrong class.
        events.push(event(302, "Completed", at("ghost")));
        events.push(event(303, "Completed", serde_json::json!({ "node": "b", "iteration": {}, "attempt": 1 })));
        events.push(event(
            304,
            "Chosen",
            serde_json::json!({ "node": "a", "iteration": {}, "attempt": 1, "edge": "x" }),
        ));
        let log = Log { instance: InstanceId::new("fi_x").unwrap(), events, last_hash: None };
        let fold = Fold::replay(&log, &started).unwrap();
        assert_eq!((fold.advances, fold.evaluations.get()), (0, 0));
        assert_eq!(fold.rejected.len(), 3);

        // An accepted receipt is evaluated once and advances once.
        let mut log = log;
        log.events.push(event(305, "Completed", serde_json::json!({ "node": "a", "iteration": {}, "attempt": 1 })));
        let fold = Fold::replay(&log, &started).unwrap();
        assert_eq!(fold.advances, 1);
        assert_eq!(fold.rejected.len(), 3);
    }

    #[test]
    fn a_re_run_with_the_same_result_redoes_no_core() {
        // A bounded cost, counted rather than timed: re-running an observation everything depends
        // on with the same result, then asking for the last node, computes each core once.
        let flow = compile("flow: f\nversion: 1\nnodes:\n  head: {class: observe, run: [x], fresh_for: 10m, exit_map: {0: a, 1: b}}\n  a: {class: pure, run: [x], needs: [head]}\n  b: {class: pure, run: [x], needs: [a]}\n").unwrap();
        let started = Started {
            flow: flow.name(),
            ir_hash: flow.hash().into(),
            ir: flow.ir().clone(),
            params: Value::Null,
            owner: None,
        };
        let mut events = vec![event(1, "Started", serde_json::to_value(&started).unwrap())];
        let head = |exit: i32| serde_json::json!({ "node": "head", "iteration": {}, "attempt": 1, "verify": { "argv": ["x"], "exit": exit, "stdout_sha256": "s", "ran_at": "x" } });
        let b = serde_json::json!({ "node": "b", "iteration": {}, "attempt": 1 });
        for k in 0..100u64 {
            let e = if k % 2 == 0 { event(k + 2, "Completed", head(0)) } else { event(k + 2, "Completed", b.clone()) };
            events.push(e);
        }
        let log = Log { instance: InstanceId::new("fi_x").unwrap(), events, last_hash: None };
        let fold = Fold::replay(&log, &started).unwrap();
        // head before its first receipt and after it, then a and b: four, however many re-runs.
        assert_eq!(fold.cores.get(), 4, "cores computed");

        // A new result each time redoes them, and no more than them, each time.
        let mut log = log;
        for (k, e) in log.events.iter_mut().skip(1).enumerate() {
            if k % 2 == 0 {
                e.body = head((k / 2 % 2) as i32);
            }
        }
        let fold = Fold::replay(&log, &started).unwrap();
        assert!(fold.cores.get() <= 1 + 3 * 50, "{} cores", fold.cores.get());
    }
}
