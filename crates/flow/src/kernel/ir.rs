//! The intermediate representation a definition compiles to, and the rules every IR must pass.
//!
//! The IR is plain data: maps keyed by id, in a fixed order, so its JSON is a function of its
//! content and [`content_hash`] is stable. `Started` pins a copy of it in the instance's log, and
//! replay runs [`validate`] on that copy again before it trusts it. `validate` holds the rules of
//! the IR version it is given and refuses any other version, so a later version's stricter rules
//! never reject an instance started under an earlier one. Guards are checked by the compiler, not
//! here: replay never touches jq.

use super::error::CompileError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The IR's own version. A change to its shape or meaning bumps it.
pub const IR_VERSION: u32 = 1;

/// The most iterations a repeat may declare. Replay walks a repeat's iterations, so a bound keeps
/// a replay's work bounded by the definition.
pub const MAX_REPEAT: u32 = 1000;

// The size bounds below are set from the flows the kernel is for, with room to spare: the
// verifier (4 nodes, depth 4, fan-in 2), a three-stratum review over five rounds (about a dozen
// nodes) and the fleet tick (13 steps, fan-in at most 13). Replay's cost per event is bounded by
// what one node reads, and its total by that times the events (`log::MAX_EVENTS`).

/// The most nodes a definition may declare: about twice the largest real flow. With
/// [`MAX_FAN_IN`] it bounds the work of re-evaluating a whole flow, which a log that keeps
/// changing an observation everything depends on asks for at every other event.
pub const MAX_NODES: usize = 64;

/// The most distinct nodes one node's evaluation may read: its needs, its switch, and the whole
/// body of every repeat it waits on (needing it, or needing a node in its body from outside),
/// each counted once. Above the tick's 13.
pub const MAX_FAN_IN: usize = 16;

/// The longest chain of dependencies (needs, switches, repeat bodies) a definition may have,
/// counted in nodes and repeats. Replay follows a chain by recursion, so this bounds its stack;
/// a 13-step tick in sequence is 13 deep.
pub const MAX_DEPTH: usize = 64;

/// The most bytes a compiled IR may serialize to. `Started` carries the whole IR and the params
/// ([`MAX_PARAMS`]) in one event line, which `log::MAX_LINE` (1 MiB) bounds, so anything that
/// compiles can start.
pub const MAX_IR: usize = 256 << 10;

/// The most bytes of params `start` takes, serialized.
pub const MAX_PARAMS: usize = 64 << 10;

/// A repeat's iteration index for each repeat enclosing a node; `{}` outside any repeat.
pub type Iteration = BTreeMap<String, u32>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ir {
    pub ir_version: u32,
    pub flow: String,
    pub version: u32,
    /// Param name to whether it is required.
    pub params: BTreeMap<String, bool>,
    pub nodes: BTreeMap<String, Node>,
    pub repeats: BTreeMap<String, Repeat>,
    /// The directory the flow's commands run in, `{{params.<name>}}` filled in when they run. A
    /// flow that declares none has its commands refused `workdir_invalid`. Left out of the JSON
    /// when absent, so the IR of a flow without one hashes as it did before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub class: Class,
    /// Everything that must hold before the node is ready. Fork ids are already expanded into
    /// their branches.
    pub needs: Vec<Need>,
    pub switch: Option<Case>,
    /// Attempts allowed before the node fails; unlimited when absent.
    pub retry: Option<u32>,
    /// Seconds an attempt may run, heartbeats or not, before it counts as ended.
    pub cancel_after: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "on", rename_all = "snake_case")]
pub enum Need {
    /// The node is done (in the matching iteration, see `replay`).
    Node { node: String },
    /// The repeat finished because its `until` held.
    Repeat { repeat: String },
    /// The repeat ran out of iterations: the `on_exhausted` route.
    Exhausted { repeat: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Case {
    pub on: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Class {
    /// A command the kernel runs; its output is a function of its inputs.
    Pure { run: Vec<String> },
    /// A command the kernel runs to observe the world. Stale after `fresh_for` seconds, or when a
    /// declared input changed. `exit_map` names its exit statuses.
    Observe { run: Vec<String>, fresh_for: Option<u64>, inputs: Vec<Input>, exit_map: Vec<Exit> },
    /// Done by a performer, accepted when `verify` (run by the kernel) passes.
    Work { verify: Vec<String>, launch: Launch, wait: Option<Wait> },
    /// A performer proposes one of `edges` (name to jq guard).
    Choice { edges: BTreeMap<String, String>, launch: Launch },
    /// An action on the world. Never replayed; `reconcile` is how the kernel learns its outcome.
    Effect { preconditions: Vec<String>, reconcile: Vec<String>, launch: Launch, wait: Option<Wait> },
    /// Waits for the owner's answer to `ask`, through the asks ledger.
    Human { ask: String },
}

impl Class {
    pub fn name(&self) -> &'static str {
        match self {
            Class::Pure { .. } => "pure",
            Class::Observe { .. } => "observe",
            Class::Work { .. } => "work",
            Class::Choice { .. } => "choice",
            Class::Effect { .. } => "effect",
            Class::Human { .. } => "human",
        }
    }

    pub fn launch(&self) -> Option<&Launch> {
        match self {
            Class::Work { launch, .. } | Class::Choice { launch, .. } | Class::Effect { launch, .. } => Some(launch),
            _ => None,
        }
    }

    pub fn wait(&self) -> Option<&Wait> {
        match self {
            Class::Work { wait, .. } | Class::Effect { wait, .. } => wait.as_ref(),
            _ => None,
        }
    }
}

/// One `exit_map` entry: an exit status and the value it names. Listed by exit status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exit {
    pub exit: i32,
    pub value: String,
}

/// A declared input of an observation: a param (fixed for the instance, so the observation
/// never goes stale by it) or a node it also needs (whose receipt digest it consumes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum Input {
    Param { name: String },
    Node { node: String },
}

/// Who performs a node and how to start them: the hint `next` returns. The kernel launches nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    pub performer: Option<String>,
    pub skill: Option<String>,
    pub routing: BTreeMap<String, String>,
}

/// A declared external wait: what the node waits on, and how often its claim must heartbeat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wait {
    pub external: String,
    pub heartbeat: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Repeat {
    pub body: Vec<String>,
    /// 0 means the definition gave none, which [`validate`] refuses.
    pub max: u32,
    pub until: Option<String>,
    pub on_exhausted: Option<String>,
}

/// `sha256:<hex>` of the IR's JSON.
pub fn content_hash(ir: &Ir) -> Result<String, CompileError> {
    let bytes = serde_json::to_vec(ir).map_err(|e| CompileError::invalid(format!("the IR does not serialize: {e}")))?;
    Ok(crate::log::line_hash(&bytes))
}

fn unknown(from: impl Into<String>, name: &str) -> CompileError {
    CompileError::UnknownReference { from: from.into(), name: name.to_string() }
}

/// The rules every IR of [`IR_VERSION`] passes, at compile time and again on the copy a `Started`
/// event pins.
pub fn validate(ir: &Ir) -> Result<(), CompileError> {
    if ir.ir_version != IR_VERSION {
        return Err(CompileError::invalid(format!("IR version {} is not {IR_VERSION}", ir.ir_version)));
    }
    if ir.nodes.len() > MAX_NODES {
        return Err(CompileError::invalid(format!("{} nodes is more than {MAX_NODES}", ir.nodes.len())));
    }
    let node = |from: &str, name: &str| ir.nodes.get(name).ok_or_else(|| unknown(from, name));
    let repeat = |from: &str, name: &str| ir.repeats.get(name).ok_or_else(|| unknown(from, name));
    let param = |from: &str, name: &str| ir.params.contains_key(name).then_some(()).ok_or_else(|| unknown(from, name));

    let body = |r: &str| ir.repeats.get(r).map(|r| r.body.iter().map(String::as_str)).into_iter().flatten();
    let member: BTreeMap<&str, &str> =
        ir.repeats.iter().flat_map(|(rid, r)| r.body.iter().map(move |b| (b.as_str(), rid.as_str()))).collect();
    for (id, n) in &ir.nodes {
        let mut read: BTreeSet<&str> = BTreeSet::new();
        let on = n.needs.iter().map(|need| match need {
            Need::Node { node } => node.as_str(),
            Need::Repeat { repeat } | Need::Exhausted { repeat } => repeat.as_str(),
        });
        for m in on.chain(n.switch.as_ref().map(|c| c.on.as_str())) {
            match member.get(m) {
                Some(r) if member.get(id.as_str()) != Some(r) => {
                    read.insert(m);
                    read.extend(body(r));
                }
                _ if ir.repeats.contains_key(m) => read.extend(body(m)),
                _ => {
                    read.insert(m);
                }
            }
        }
        let reads = read.len();
        if reads > MAX_FAN_IN {
            return Err(CompileError::invalid(format!("node {id} reads {reads} nodes, more than {MAX_FAN_IN}")));
        }
    }

    let mut membership = BTreeMap::new();
    for (rid, r) in &ir.repeats {
        let from = format!("repeat {rid}");
        if r.max == 0 || r.max > MAX_REPEAT {
            return Err(CompileError::UnboundedLoop {
                detail: format!("repeat {rid} needs a max from 1 to {MAX_REPEAT}"),
            });
        }
        if r.body.is_empty() {
            return Err(CompileError::invalid(format!("repeat {rid} has an empty body")));
        }
        for b in &r.body {
            node(&from, b)?;
            if let Some(other) = membership.insert(b.as_str(), rid.as_str()) {
                return Err(CompileError::invalid(format!("node {b} is in the bodies of repeats {other} and {rid}")));
            }
        }
        if let Some(target) = &r.on_exhausted {
            node(&from, target)?;
            if r.body.contains(target) {
                return Err(CompileError::invalid(format!(
                    "repeat {rid} routes exhaustion to {target}, inside its own body"
                )));
            }
        }
    }

    for (id, n) in &ir.nodes {
        let from = format!("node {id}");
        for need in &n.needs {
            match need {
                Need::Node { node: m } => {
                    node(&from, m)?;
                }
                Need::Repeat { repeat: r } | Need::Exhausted { repeat: r } => {
                    repeat(&from, r)?;
                }
            }
        }
        if n.retry == Some(0) {
            return Err(CompileError::invalid(format!("node {id} allows 0 attempts")));
        }
        if let Some(case) = &n.switch {
            let declared = match &node(&from, &case.on)?.class {
                Class::Choice { edges, .. } => edges.contains_key(&case.value),
                Class::Observe { exit_map, .. } => exit_map.iter().any(|e| e.value == case.value),
                _ => false,
            };
            if !declared {
                return Err(CompileError::UndeclaredEdge {
                    node: id.clone(),
                    on: case.on.clone(),
                    value: case.value.clone(),
                });
            }
        }
        let command = |what: &str, argv: &[String]| {
            if argv.is_empty() {
                Err(CompileError::invalid(format!("node {id} needs a non-empty {what} command")))
            } else {
                Ok(())
            }
        };
        match &n.class {
            Class::Pure { run } => command("run", run)?,
            Class::Observe { run, fresh_for, inputs, exit_map } => {
                command("run", run)?;
                if fresh_for.is_none() && inputs.is_empty() {
                    return Err(CompileError::invalid(format!("observe node {id} needs fresh_for or inputs")));
                }
                if exit_map.is_empty() {
                    return Err(CompileError::invalid(format!("observe node {id} needs an exit_map")));
                }
                for input in inputs {
                    match input {
                        Input::Param { name } => param(&from, name)?,
                        Input::Node { node: m } => {
                            node(&from, m)?;
                            if !n.needs.contains(&Need::Node { node: m.clone() }) {
                                return Err(CompileError::invalid(format!(
                                    "observe node {id} reads {m} without needing it"
                                )));
                            }
                        }
                    }
                }
            }
            Class::Work { verify, .. } => command("verify", verify)?,
            Class::Choice { edges, .. } => {
                if edges.is_empty() {
                    return Err(CompileError::invalid(format!("choice node {id} declares no edges")));
                }
            }
            Class::Effect { reconcile, .. } => {
                if reconcile.is_empty() {
                    return Err(CompileError::EffectWithoutReconcile { node: id.clone() });
                }
            }
            Class::Human { ask } => {
                if ask.trim().is_empty() {
                    return Err(CompileError::invalid(format!("human node {id} needs an ask")));
                }
            }
        }
    }
    topology(ir)
}

/// Refuses a cycle through `needs`, switches and repeat bodies (the only loops are repeats) and
/// a chain longer than [`MAX_DEPTH`], in one topological pass.
fn topology(ir: &Ir) -> Result<(), CompileError> {
    let member: BTreeMap<&str, &str> =
        ir.repeats.iter().flat_map(|(rid, r)| r.body.iter().map(move |b| (b.as_str(), rid.as_str()))).collect();
    // Vertices: `n:<node>` and `r:<repeat>`. An edge points from a vertex to what it waits on;
    // waiting on a body node from outside its repeat waits on the whole repeat too.
    let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (id, n) in &ir.nodes {
        let set = deps.entry(format!("n:{id}")).or_default();
        let on = n.needs.iter().filter_map(|need| match need {
            Need::Node { node } => Some(node.as_str()),
            Need::Repeat { repeat } | Need::Exhausted { repeat } => {
                set.insert(format!("r:{repeat}"));
                None
            }
        });
        let on: Vec<&str> = on.chain(n.switch.as_ref().map(|c| c.on.as_str())).collect();
        for m in on {
            set.insert(format!("n:{m}"));
            if let Some(r) = member.get(m)
                && member.get(id.as_str()) != Some(r)
            {
                set.insert(format!("r:{r}"));
            }
        }
    }
    for (rid, r) in &ir.repeats {
        deps.entry(format!("r:{rid}")).or_default().extend(r.body.iter().map(|b| format!("n:{b}")));
    }
    let mut waiting: BTreeMap<&str, usize> = deps.iter().map(|(v, ds)| (v.as_str(), ds.len())).collect();
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (v, ds) in &deps {
        for d in ds {
            dependents.entry(d.as_str()).or_default().push(v.as_str());
        }
    }
    let mut depth: BTreeMap<&str, usize> = BTreeMap::new();
    let mut free: Vec<&str> = waiting.iter().filter(|(_, n)| **n == 0).map(|(v, _)| *v).collect();
    let mut finished = 0;
    while let Some(v) = free.pop() {
        finished += 1;
        let d = depth.get(v).copied().unwrap_or(0) + 1;
        if d > MAX_DEPTH {
            return Err(CompileError::invalid(format!("a chain of dependencies is deeper than {MAX_DEPTH}")));
        }
        for w in dependents.get(v).map(Vec::as_slice).unwrap_or_default() {
            let at = depth.entry(w).or_default();
            *at = (*at).max(d);
            if let Some(n) = waiting.get_mut(w) {
                *n -= 1;
                if *n == 0 {
                    free.push(w);
                }
            }
        }
    }
    if finished == deps.len() {
        return Ok(());
    }
    let v = waiting.iter().find(|(_, n)| **n > 0).map_or("", |(v, _)| *v);
    let what = match v.strip_prefix("n:") {
        Some(id) => format!("node {id}"),
        None => format!("repeat {}", v.strip_prefix("r:").unwrap_or(v)),
    };
    Err(CompileError::UnboundedLoop {
        detail: format!("{what} waits on a cycle through needs (only a repeat may loop)"),
    })
}
