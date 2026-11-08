//! The intermediate representation a definition compiles to, and the rules every IR must pass.
//!
//! The IR is plain data: maps keyed by id, in a fixed order, so its JSON is a function of its
//! content and [`content_hash`] is stable. `Started` pins a copy of it in the instance's log, and
//! replay runs [`validate`] on that copy again before it trusts it.

use super::error::CompileError;
use super::jq;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The IR's own version. A change to its shape or meaning bumps it.
pub const IR_VERSION: u32 = 1;

/// The most iterations a repeat may declare. Replay walks a repeat's iterations, so a bound keeps
/// a replay's work bounded by the definition.
pub const MAX_REPEAT: u32 = 1000;

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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub class: Class,
    /// Everything that must hold before the node is ready. Fork ids are already expanded into
    /// their branches.
    pub needs: Vec<Need>,
    pub switch: Option<Case>,
    /// The node applies only when this optional param is supplied (present and not null).
    pub when_param: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum Input {
    Param { name: String },
    Node { node: String },
}

impl Input {
    /// The key a receipt records this input's digest under: `params.<name>` or the node id.
    pub fn key(&self) -> String {
        match self {
            Input::Param { name } => format!("params.{name}"),
            Input::Node { node } => node.clone(),
        }
    }
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

fn guard(at: String, code: &str) -> Result<(), CompileError> {
    jq::compile(code).map(|_| ()).map_err(|detail| CompileError::InvalidGuard { at, detail })
}

/// The rules every IR passes, at compile time and again on the copy a `Started` event pins.
pub fn validate(ir: &Ir) -> Result<(), CompileError> {
    let node = |from: &str, name: &str| ir.nodes.get(name).ok_or_else(|| unknown(from, name));
    let repeat = |from: &str, name: &str| ir.repeats.get(name).ok_or_else(|| unknown(from, name));
    let param = |from: &str, name: &str| ir.params.contains_key(name).then_some(()).ok_or_else(|| unknown(from, name));

    let mut membership = BTreeMap::new();
    for (rid, r) in &ir.repeats {
        let from = format!("repeat {rid}");
        if r.max == 0 || r.max > MAX_REPEAT {
            return Err(CompileError::UnboundedLoop {
                detail: format!("repeat {rid} needs a max from 1 to {MAX_REPEAT}"),
            });
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
        if let Some(until) = &r.until {
            guard(format!("repeat {rid} until"), until)?;
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
        if let Some(p) = &n.when_param {
            param(&from, p)?;
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
                for (edge, g) in edges {
                    guard(format!("node {id} edge {edge}"), g)?;
                }
            }
            Class::Effect { preconditions, reconcile, .. } => {
                if reconcile.is_empty() {
                    return Err(CompileError::EffectWithoutReconcile { node: id.clone() });
                }
                for (i, g) in preconditions.iter().enumerate() {
                    guard(format!("node {id} precondition {i}"), g)?;
                }
            }
            Class::Human { ask } => {
                if ask.trim().is_empty() {
                    return Err(CompileError::invalid(format!("human node {id} needs an ask")));
                }
            }
        }
    }
    acyclic(ir)
}

/// Refuses a cycle through `needs`, switches and repeat bodies: the only loops are repeats.
fn acyclic(ir: &Ir) -> Result<(), CompileError> {
    // Vertices: `n:<node>` and `r:<repeat>`. An edge points from a vertex to what it waits on.
    let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (id, n) in &ir.nodes {
        let set = deps.entry(format!("n:{id}")).or_default();
        for need in &n.needs {
            set.insert(match need {
                Need::Node { node } => format!("n:{node}"),
                Need::Repeat { repeat } | Need::Exhausted { repeat } => format!("r:{repeat}"),
            });
        }
        if let Some(case) = &n.switch {
            set.insert(format!("n:{}", case.on));
        }
    }
    for (rid, r) in &ir.repeats {
        let set = deps.entry(format!("r:{rid}")).or_default();
        set.extend(r.body.iter().map(|b| format!("n:{b}")));
    }
    // Kahn: peel vertices with no unfinished dependency; whatever is left lies on a cycle.
    let mut left = deps;
    loop {
        let free: Vec<String> =
            left.iter().filter(|(_, ds)| ds.iter().all(|d| !left.contains_key(d))).map(|(v, _)| v.clone()).collect();
        if free.is_empty() {
            break;
        }
        for v in free {
            left.remove(&v);
        }
    }
    let Some(v) = left.keys().next() else { return Ok(()) };
    let what = match v.strip_prefix("n:") {
        Some(id) => format!("node {id}"),
        None => format!("repeat {}", v.strip_prefix("r:").unwrap_or(v)),
    };
    Err(CompileError::UnboundedLoop {
        detail: format!("{what} waits on a cycle through needs (only a repeat may loop)"),
    })
}
