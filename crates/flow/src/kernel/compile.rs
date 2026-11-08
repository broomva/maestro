//! Compiling a YAML definition to the IR: parse, refuse what phase 1 does not support, refuse
//! repeated or malformed ids and fields outside a node's class, expand forks, then
//! [`ir::validate`] and check every guard ([`jq::check`]).

use super::definition::{ClassDef, Definition, NodeDef};
use super::error::CompileError;
use super::ir::{self, Case, Class, Exit, IR_VERSION, Input, Ir, Launch, Need, Node, Repeat, Wait};
use super::jq;
use std::collections::{BTreeMap, BTreeSet};

/// A definition that compiled: its IR and the IR's content hash. Only [`compile`] makes one.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledFlow {
    ir: Ir,
    hash: String,
}

impl CompiledFlow {
    pub fn ir(&self) -> &Ir {
        &self.ir
    }

    /// `sha256:<hex>` of the IR's JSON.
    pub fn hash(&self) -> &str {
        &self.hash
    }

    /// `<flow>@<version>`, as `start` names it.
    pub fn name(&self) -> String {
        format!("{}@{}", self.ir.flow, self.ir.version)
    }
}

/// The class-specific fields each class accepts. `needs`, `switch`, `retry` and `cancel` apply to
/// every class.
const CLASS_FIELDS: &[(ClassDef, &[&str])] = &[
    (ClassDef::Pure, &["run"]),
    (ClassDef::Observe, &["run", "fresh_for", "inputs", "exit_map"]),
    (ClassDef::Work, &["verify", "performer", "skill", "routing", "wait"]),
    (ClassDef::Choice, &["edges", "performer", "skill", "routing"]),
    (ClassDef::Effect, &["preconditions", "reconcile", "performer", "skill", "routing", "wait"]),
    (ClassDef::Human, &["ask"]),
];

fn unique<'a>(kind: &'static str, ids: impl IntoIterator<Item = &'a str>) -> Result<(), CompileError> {
    let mut seen = BTreeSet::new();
    for id in ids {
        if !seen.insert(id) {
            return Err(CompileError::DuplicateId { kind, id: id.to_string() });
        }
    }
    Ok(())
}

/// Ids and param names are `[A-Za-z0-9_-]`, so none reads as `params.<name>` or a path.
fn plain(kind: &str, id: &str) -> Result<(), CompileError> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        return Err(CompileError::invalid(format!(
            "the {kind} id {id:?} must be letters, digits, underscores and dashes"
        )));
    }
    Ok(())
}

/// Checks every guard in the IR without running it.
fn check_guards(ir: &Ir) -> Result<(), CompileError> {
    let check = |at: String, code: &str| jq::check(code).map_err(|detail| CompileError::InvalidGuard { at, detail });
    for (rid, r) in &ir.repeats {
        if let Some(until) = &r.until {
            check(format!("repeat {rid} until"), until)?;
        }
    }
    for (id, n) in &ir.nodes {
        match &n.class {
            Class::Choice { edges, .. } => {
                for (edge, g) in edges {
                    check(format!("node {id} edge {edge}"), g)?;
                }
            }
            Class::Effect { preconditions, .. } => {
                for (i, g) in preconditions.iter().enumerate() {
                    check(format!("node {id} precondition {i}"), g)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whole seconds in a duration such as `10m`, `1h 30m` or `PT90S`; at least one second.
fn seconds(at: &str, text: &str) -> Result<u64, CompileError> {
    let d: jiff::SignedDuration =
        text.parse().map_err(|e| CompileError::invalid(format!("{at} is not a duration ({e})")))?;
    match u64::try_from(d.as_secs()) {
        Ok(s) if s > 0 => Ok(s),
        _ => Err(CompileError::invalid(format!("{at} must be at least one second"))),
    }
}

/// Compiles a YAML definition to its IR.
pub fn compile(yaml: &str) -> Result<CompiledFlow, CompileError> {
    let def: Definition = serde_yaml::from_str(yaml).map_err(|e| CompileError::invalid(e.to_string()))?;
    if def.uses.is_some() {
        return Err(CompileError::Unsupported { feature: "A trait (uses at the top level)".into() });
    }
    if def.templates.is_some() {
        return Err(CompileError::Unsupported { feature: "A template".into() });
    }
    if let Some((id, _)) = def.nodes.iter().find(|(_, n)| n.uses.is_some()) {
        return Err(CompileError::Unsupported { feature: format!("A subflow (uses on node {id})") });
    }
    let ids = def.nodes.iter().map(|(id, _)| id).chain(def.forks.iter().map(|(id, _)| id));
    unique("node", ids.chain(def.repeats.iter().map(|(id, _)| id)).map(String::as_str))?;
    let params: Vec<(&str, bool)> =
        def.params.iter().map(|p| p.strip_suffix('?').map_or((p.as_str(), true), |name| (name, false))).collect();
    unique("param", params.iter().map(|(name, _)| *name))?;
    let all = def.nodes.iter().map(|(id, _)| ("node", id)).chain(def.forks.iter().map(|(id, _)| ("fork", id)));
    for (kind, id) in all.chain(def.repeats.iter().map(|(id, _)| ("repeat", id))) {
        plain(kind, id)?;
    }
    for (name, _) in &params {
        plain("param", name)?;
    }

    let node_ids: BTreeSet<&str> = def.nodes.iter().map(|(id, _)| id.as_str()).collect();
    let forks: BTreeMap<&str, &[String]> = def.forks.iter().map(|(id, f)| (id.as_str(), f.all.as_slice())).collect();
    for (fid, f) in &def.forks {
        if f.all.is_empty() {
            return Err(CompileError::invalid(format!("fork {fid} has no branches")));
        }
        if let Some(b) = f.all.iter().find(|b| !node_ids.contains(b.as_str())) {
            return Err(CompileError::UnknownReference { from: format!("fork {fid}"), name: b.clone() });
        }
    }
    let repeat_ids: BTreeSet<&str> = def.repeats.iter().map(|(id, _)| id.as_str()).collect();
    let need = |name: &String| -> Vec<Need> {
        if let Some(branches) = forks.get(name.as_str()) {
            // fork: all. Needing the fork is needing every branch: the join.
            branches.iter().map(|b| Need::Node { node: b.clone() }).collect()
        } else if repeat_ids.contains(name.as_str()) {
            vec![Need::Repeat { repeat: name.clone() }]
        } else {
            vec![Need::Node { node: name.clone() }]
        }
    };

    let mut nodes = BTreeMap::new();
    for (id, n) in def.nodes {
        let allowed = CLASS_FIELDS.iter().find(|(c, _)| *c == n.class).map_or(&[][..], |(_, f)| *f);
        if let Some(field) = n.class_fields().into_iter().find(|f| !allowed.contains(f)) {
            return Err(CompileError::invalid(format!(
                "{field} does not apply to node {id}, a {} node",
                n.class.name()
            )));
        }
        let node = build_node(&id, n, &need)?;
        nodes.insert(id, node);
    }

    let mut repeats = BTreeMap::new();
    for (rid, r) in def.repeats {
        if let Some(target) = &r.on_exhausted
            && let Some(node) = nodes.get_mut(target)
        {
            node.needs.push(Need::Exhausted { repeat: rid.clone() });
        }
        let repeat = Repeat { body: r.body, max: r.max.unwrap_or(0), until: r.until, on_exhausted: r.on_exhausted };
        repeats.insert(rid, repeat);
    }

    let ir = Ir {
        ir_version: IR_VERSION,
        flow: def.flow,
        version: def.version,
        params: params.into_iter().map(|(name, required)| (name.to_string(), required)).collect(),
        nodes,
        repeats,
    };
    ir::validate(&ir)?;
    check_guards(&ir)?;
    let hash = ir::content_hash(&ir)?;
    Ok(CompiledFlow { ir, hash })
}

fn build_node(id: &str, n: NodeDef, need: &dyn Fn(&String) -> Vec<Need>) -> Result<Node, CompileError> {
    let launch = Launch { performer: n.performer, skill: n.skill, routing: n.routing.unwrap_or_default() };
    let wait = match n.wait {
        Some(w) => {
            Some(Wait { heartbeat: seconds(&format!("node {id} wait.heartbeat"), &w.heartbeat)?, external: w.external })
        }
        None => None,
    };
    let class = match n.class {
        ClassDef::Pure => Class::Pure { run: n.run.unwrap_or_default() },
        ClassDef::Observe => Class::Observe {
            run: n.run.unwrap_or_default(),
            fresh_for: n.fresh_for.map(|f| seconds(&format!("node {id} fresh_for"), &f)).transpose()?,
            inputs: n
                .inputs
                .unwrap_or_default()
                .into_iter()
                .map(|i| match i.strip_prefix("params.") {
                    Some(name) => Input::Param { name: name.to_string() },
                    None => Input::Node { node: i },
                })
                .collect(),
            exit_map: {
                let exits = n.exit_map.unwrap_or_default();
                let codes: Vec<String> = exits.iter().map(|(code, _)| code.to_string()).collect();
                unique("exit_map", codes.iter().map(String::as_str))?;
                let mut exits: Vec<Exit> = exits.into_iter().map(|(exit, value)| Exit { exit, value }).collect();
                exits.sort_by_key(|e| e.exit);
                exits
            },
        },
        ClassDef::Work => Class::Work { verify: n.verify.unwrap_or_default(), launch, wait },
        ClassDef::Choice => {
            let edges = n.edges.unwrap_or_default();
            unique("edge", edges.iter().map(|(e, _)| e.as_str()))?;
            Class::Choice { edges: edges.into_iter().map(|(e, d)| (e, d.guard)).collect(), launch }
        }
        ClassDef::Effect => Class::Effect {
            preconditions: n.preconditions.unwrap_or_default(),
            reconcile: n.reconcile.unwrap_or_default(),
            launch,
            wait,
        },
        ClassDef::Human => Class::Human { ask: n.ask.unwrap_or_default() },
    };
    Ok(Node {
        class,
        needs: n.needs.iter().flat_map(need).collect(),
        switch: n.switch.map(|s| Case { on: s.on, value: s.case }),
        retry: n.retry.map(|r| r.attempts),
        cancel_after: n.cancel.map(|c| seconds(&format!("node {id} cancel.after"), &c.after)).transpose()?,
    })
}
