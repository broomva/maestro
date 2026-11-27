//! Compiling a YAML definition to the IR: parse, refuse what phase 1 does not support, refuse
//! repeated or malformed ids and fields outside a node's class, expand forks, then
//! [`ir::validate`] and check every guard ([`jq::check`]).

use super::definition::{ClassDef, Definition, NodeDef};
use super::error::CompileError;
use super::ir::{self, Case, Class, Exit, Input, Ir, Launch, Need, Node, Repeat, RouteRef, Wait};
use super::jq;
use std::collections::{BTreeMap, BTreeSet};

/// The most bytes of YAML a definition may have. Parsing is bounded by it, and with
/// [`ir::MAX_IR`] it keeps a flow small enough to start.
pub const MAX_SOURCE: usize = 256 << 10;

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
    (ClassDef::Pure, &["run", "snapshot"]),
    (ClassDef::Observe, &["run", "fresh_for", "inputs", "exit_map"]),
    (ClassDef::Work, &["verify", "performer", "skill", "routing", "wait"]),
    (ClassDef::Choice, &["edges", "performer", "skill", "routing"]),
    (ClassDef::Effect, &["run", "preconditions", "reconcile", "performer", "skill", "routing", "wait"]),
    (ClassDef::Human, &["ask", "required_body_fields"]),
    (ClassDef::Subflow, &["uses", "with"]),
    (
        ClassDef::Map,
        &[
            "over",
            "item_key",
            "max_items",
            "max_concurrency_param",
            "ready_when",
            "reserve_providers",
            "provider_pool",
            "on_item_failure",
            "item_retry",
            "uses",
            "with",
        ],
    ),
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
            Class::Map { over, item_key, .. } => {
                check(format!("node {id} over"), over)?;
                check(format!("node {id} item_key"), item_key)?;
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
    compile_with_path(yaml, None)
}

pub fn compile_with_path(yaml: &str, path: Option<&std::path::Path>) -> Result<CompiledFlow, CompileError> {
    let mut visiting = BTreeSet::new();
    compile_inner(yaml, path, 0, &mut visiting)
}

fn compile_inner(
    yaml: &str,
    path: Option<&std::path::Path>,
    depth: u32,
    visiting: &mut BTreeSet<String>,
) -> Result<CompiledFlow, CompileError> {
    if depth > ir::MAX_SUBFLOW_DEPTH {
        return Err(CompileError::invalid(format!(
            "subflow nesting depth {depth} exceeds MAX_SUBFLOW_DEPTH ({})",
            ir::MAX_SUBFLOW_DEPTH
        )));
    }
    if yaml.len() > MAX_SOURCE {
        return Err(CompileError::invalid(format!("the definition is {} bytes, more than {MAX_SOURCE}", yaml.len())));
    }
    let def: Definition = serde_yaml::from_str(yaml).map_err(|e| CompileError::invalid(e.to_string()))?;
    let flow_key = format!("{}@{}", def.flow, def.version);
    if !visiting.insert(flow_key.clone()) {
        return Err(CompileError::UnboundedLoop {
            detail: format!("cyclic subflow reference through {flow_key}"),
        });
    }
    let result = compile_def(def, path, depth, visiting);
    visiting.remove(&flow_key);
    result
}

fn resolve_subflow_source(
    uses: &str,
    inline: &BTreeMap<String, String>,
    parent_path: Option<&std::path::Path>,
) -> Result<(String, Option<std::path::PathBuf>), CompileError> {
    if let Some(src) = inline.get(uses) {
        return Ok((src.clone(), parent_path.map(std::path::Path::to_path_buf)));
    }
    let Some((name, _ver)) = ir::parse_pinned_uses(uses) else {
        return Err(CompileError::invalid(format!(
            "subflow reference {uses:?} must be version-pinned as <flow>@<version>"
        )));
    };
    let Some(parent_dir) = parent_path.and_then(|p| p.parent()) else {
        return Err(CompileError::UnknownReference { from: "subflow uses".into(), name: uses.to_string() });
    };
    let candidates = [
        parent_dir.join(format!("{name}.yaml")),
        parent_dir.join("subflows").join(format!("{name}.yaml")),
    ];
    for cand in candidates {
        if cand.is_file() {
            let text = std::fs::read_to_string(&cand)
                .map_err(|e| CompileError::invalid(format!("failed to read subflow {}: {e}", cand.display())))?;
            return Ok((text, Some(cand)));
        }
    }
    Err(CompileError::UnknownReference { from: "subflow uses".into(), name: uses.to_string() })
}

fn compile_def(
    def: Definition,
    path: Option<&std::path::Path>,
    depth: u32,
    visiting: &mut BTreeSet<String>,
) -> Result<CompiledFlow, CompileError> {
    let external = load_agents(def.agents.as_deref(), path)?;
    if def.uses.is_some() {
        return Err(CompileError::Unsupported { feature: "A trait (uses at the top level)".into() });
    }
    if def.templates.is_some() {
        return Err(CompileError::Unsupported { feature: "A template".into() });
    }
    if let Some((id, _)) =
        def.nodes.iter().find(|(_, n)| n.uses.is_some() && !matches!(n.class, ClassDef::Subflow | ClassDef::Map))
    {
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

    let inline_subflows = def.subflows;
    let mut subflows: BTreeMap<String, Ir> = BTreeMap::new();
    let mut nodes = BTreeMap::new();
    for (id, n) in def.nodes {
        let allowed = CLASS_FIELDS.iter().find(|(c, _)| *c == n.class).map_or(&[][..], |(_, f)| *f);
        if let Some(field) = n.class_fields().into_iter().find(|f| !allowed.contains(f)) {
            return Err(CompileError::invalid(format!(
                "{field} does not apply to node {id}, a {} node",
                n.class.name()
            )));
        }
        let mut subflow_ir_hash = String::new();
        if matches!(n.class, ClassDef::Subflow | ClassDef::Map) {
            let uses_str = n
                .uses
                .as_ref()
                .and_then(|v| v.as_str())
                .ok_or_else(|| CompileError::invalid(format!("node {id} requires a string uses: <flow>@<version>")))?;
            let (name, ver) = ir::parse_pinned_uses(uses_str).ok_or_else(|| {
                CompileError::invalid(format!(
                    "node {id} uses {uses_str:?}, which must be version-pinned as <flow>@<version>"
                ))
            })?;
            if let Some(existing) = subflows.get(uses_str) {
                subflow_ir_hash = ir::content_hash(existing)?;
            } else {
                let (src, child_path) = resolve_subflow_source(uses_str, &inline_subflows, path)?;
                let compiled_child = compile_inner(&src, child_path.as_deref(), depth + 1, visiting)?;
                if compiled_child.ir().flow != name || compiled_child.ir().version != ver {
                    return Err(CompileError::invalid(format!(
                        "node {id} pins {uses_str} but resolved subflow is {}@{}",
                        compiled_child.ir().flow,
                        compiled_child.ir().version
                    )));
                }
                subflow_ir_hash = compiled_child.hash().to_string();
                subflows.insert(uses_str.to_string(), compiled_child.ir().clone());
            }
        }
        let mut node = build_node(&id, n, &need, subflow_ir_hash)?;
        let launch = match &mut node.class {
            Class::Work { launch, .. } | Class::Choice { launch, .. } | Class::Effect { launch, .. } => Some(launch),
            _ => None,
        };
        if let Some(launch) = launch
            && let Some(name) = launch.performer.as_deref().and_then(crate::agents::performer_agent)
        {
            let agent = external
                .iter()
                .find(|a| a.name == name)
                .or_else(|| crate::agents::find(name))
                .ok_or_else(|| CompileError::UnknownAgent { node: id.clone(), agent: name.to_string() })?;
            launch.pinned_agent = Some(agent.launch(launch.skill.as_deref()));
        }
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

    let mut ir = Ir {
        ir_version: 0,
        flow: def.flow,
        version: def.version,
        params: params.into_iter().map(|(name, required)| (name.to_string(), required)).collect(),
        types: def.types,
        nodes,
        repeats,
        workdir: def.workdir,
        receipt_stdin: def.receipt_stdin,
        subflows,
    };
    ir.ir_version = ir::version_for(&ir);
    ir::validate(&ir)?;
    check_guards(&ir)?;
    check_no_outputs_placeholders(&ir)?;

    let bytes = serde_json::to_vec(&ir).map(|b| b.len()).unwrap_or(usize::MAX);
    if bytes > ir::MAX_IR {
        return Err(CompileError::invalid(format!("the IR is {bytes} bytes, more than {}", ir::MAX_IR)));
    }
    let hash = ir::content_hash(&ir)?;
    Ok(CompiledFlow { ir, hash })
}

/// Parses `routing.provider` when it reads `different_from(a, params.b, ...)` (BRO-3828); any other
/// provider text is a hint and parses to nothing. Each argument is `params.<name>` or a node id:
/// `ir::validate` checks that the param is declared and the node is a work node this one needs.
fn different_from(id: &str, provider: &str) -> Result<Vec<RouteRef>, CompileError> {
    let Some(rest) = provider.trim().strip_prefix("different_from(") else { return Ok(Vec::new()) };
    let Some(args) = rest.strip_suffix(')') else {
        return Err(CompileError::invalid(format!("node {id} routing.provider {provider:?} does not close")));
    };
    let mut refs = Vec::new();
    for arg in args.split(',').map(str::trim) {
        let r = match arg.strip_prefix("params.") {
            Some(name) if is_word(name) => RouteRef::Param { name: name.to_string() },
            None if is_word(arg) => RouteRef::Node { node: arg.to_string() },
            _ => {
                return Err(CompileError::invalid(format!(
                    "node {id} routing.provider names {arg:?}, which is neither a node id nor params.<name>"
                )));
            }
        };
        refs.push(r);
    }
    Ok(refs)
}

fn is_word(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn build_node(
    id: &str,
    n: NodeDef,
    need: &dyn Fn(&String) -> Vec<Need>,
    subflow_ir_hash: String,
) -> Result<Node, CompileError> {
    let routing = n.routing.unwrap_or_default();
    let different_from = match routing.get("provider") {
        Some(provider) => different_from(id, provider)?,
        None => Vec::new(),
    };
    let launch = Launch { pinned_agent: None, performer: n.performer, skill: n.skill, routing, different_from };
    let wait = match n.wait {
        Some(w) => {
            Some(Wait { heartbeat: seconds(&format!("node {id} wait.heartbeat"), &w.heartbeat)?, external: w.external })
        }
        None => None,
    };
    let snapshot = n.snapshot;
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
            // Every effect a definition declares runs something: an empty run is refused here,
            // and an IR with one is version 3 (`ir::version_for`).
            run: match n.run {
                Some(run) if !run.is_empty() => run,
                _ => return Err(CompileError::invalid(format!("effect node {id} needs a non-empty run command"))),
            },
            preconditions: n.preconditions.unwrap_or_default(),
            reconcile: n.reconcile.unwrap_or_default(),
            launch,
            wait,
        },
        ClassDef::Human => {
            let ask = n.ask.unwrap_or_default();
            let fill_ask = ask.contains("{{");
            Class::Human { ask, fill_ask, required_body_fields: n.required_body_fields.unwrap_or_default() }
        }
        ClassDef::Subflow => Class::Subflow {
            uses: n
                .uses
                .and_then(|v| v.as_str().map(ToOwned::to_owned))
                .unwrap_or_default(),
            subflow_ir_hash,
            with: n.with.unwrap_or_default(),
        },
        ClassDef::Map => Class::Map {
            over: n.over.unwrap_or_default(),
            item_key: n.item_key.unwrap_or_default(),
            max_items: n.max_items.unwrap_or(0),
            max_concurrency_param: n.max_concurrency_param,
            ready_when: n.ready_when,
            reserve_providers: n.reserve_providers,
            provider_pool: n.provider_pool.unwrap_or_default(),
            on_item_failure: n.on_item_failure.unwrap_or_default(),
            item_retry_max: n.item_retry.map(|r| r.max).unwrap_or(0),
            uses: n
                .uses
                .and_then(|v| v.as_str().map(ToOwned::to_owned))
                .unwrap_or_default(),
            subflow_ir_hash,
            with: n.with.unwrap_or_default(),
        },
    };
    Ok(Node {
        class,
        // A node needed twice (named again, or through a fork) is needed once.
        needs: n.needs.iter().flat_map(need).fold(Vec::new(), |mut needs, x| {
            if !needs.contains(&x) {
                needs.push(x);
            }
            needs
        }),
        switch: n.switch.map(|s| Case { on: s.on, value: s.case }),
        retry: n.retry.map(|r| r.attempts),
        cancel_after: n.cancel.map(|c| seconds(&format!("node {id} cancel.after"), &c.after)).transpose()?,
        snapshot,
    })
}

fn load_agents(root: Option<&str>, path: Option<&std::path::Path>) -> Result<Vec<crate::agents::Agent>, CompileError> {
    let Some(root) = root else {
        return Ok(Vec::new());
    };
    let path = path.filter(|p| p.is_absolute()).ok_or(CompileError::AgentsRootNeedsPath)?;
    let definition = path.canonicalize().map_err(|e| CompileError::invalid(e.to_string()))?;
    let dir = definition.parent().ok_or_else(|| CompileError::invalid("definition has no parent"))?;
    maestro_store::tree::check_root_owned(std::path::Path::new(maestro_store::tree::git()))
        .map_err(CompileError::invalid)?;
    let output = std::process::Command::new(maestro_store::tree::git())
        .env_clear()
        .env("PATH", maestro_store::tree::git_path())
        .env("xcrun_nocache", "1")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .output()
        .map_err(|e| CompileError::invalid(e.to_string()))?;
    if !output.status.success() {
        return Err(CompileError::invalid("definition is not in a git repository"));
    }
    let repo = std::path::PathBuf::from(
        String::from_utf8(output.stdout).map_err(|e| CompileError::invalid(e.to_string()))?.trim(),
    )
    .canonicalize()
    .map_err(|e| CompileError::invalid(e.to_string()))?;
    let relative = std::path::Path::new(root);
    if relative.is_absolute() {
        return Err(CompileError::invalid("agents root must be relative"));
    }
    let root = dir.join(relative).canonicalize().map_err(|e| CompileError::invalid(e.to_string()))?;
    if !root.starts_with(&repo) {
        return Err(CompileError::invalid("agents root escapes repository"));
    }
    use rustix::fs::{FileType, Mode, OFlags};
    let walk = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut dir_fd = rustix::fs::open(&repo, walk, Mode::empty()).map_err(|e| CompileError::invalid(e.to_string()))?;
    for component in root.strip_prefix(&repo).map_err(|e| CompileError::invalid(e.to_string()))?.components() {
        dir_fd = rustix::fs::openat(&dir_fd, component.as_os_str(), walk, Mode::empty())
            .map_err(|e| CompileError::invalid(e.to_string()))?;
    }
    let mut result = Vec::new();
    let entries = rustix::fs::Dir::read_from(&dir_fd).map_err(|e| CompileError::invalid(e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| CompileError::invalid(e.to_string()))?;
        let file = entry.file_name().to_string_lossy().into_owned();
        if !file.ends_with(".agent.yaml") {
            continue;
        }
        let file_path = root.join(&file).canonicalize().map_err(|e| CompileError::invalid(e.to_string()))?;
        if !file_path.starts_with(&repo) {
            return Err(CompileError::invalid("agent source escapes repository"));
        }
        use std::io::Read;
        let mut bytes = Vec::new();
        let fd = rustix::fs::openat(
            &dir_fd,
            file.as_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| CompileError::invalid(e.to_string()))?;
        let stat = rustix::fs::fstat(&fd).map_err(|e| CompileError::invalid(e.to_string()))?;
        if FileType::from_raw_mode(stat.st_mode as _) != FileType::RegularFile {
            return Err(CompileError::invalid("agent source is not a regular file"));
        }
        std::fs::File::from(fd)
            .take(crate::agents::MAX_SOURCE as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| CompileError::invalid(e.to_string()))?;
        if bytes.len() > crate::agents::MAX_SOURCE {
            return Err(CompileError::invalid("agent source exceeds maximum size"));
        }
        let text = String::from_utf8(bytes).map_err(|e| CompileError::invalid(e.to_string()))?;
        let mut agent = crate::agents::parse(&file, &text).map_err(|e| CompileError::invalid(e.to_string()))?;
        agent.source = file_path
            .strip_prefix(&repo)
            .map_err(|e| CompileError::invalid(e.to_string()))?
            .to_string_lossy()
            .into_owned();
        if crate::agents::find(&agent.name).is_some() {
            return Err(CompileError::AgentNameConflict { name: agent.name });
        }
        result.push(agent);
    }
    Ok(result)
}

/// Refuses an `{{outputs.<name>}}` placeholder in any command or the workdir (BRO-3636): the
/// kernel fills placeholders from the params alone, and a work node's verify reads the outputs it
/// judges as JSON on its standard input, never in its argv, where a performer's text could be
/// read as code. Compile time only: an IR pinned before this rule replays and runs as it did (its
/// placeholder is refused `template_invalid` when the command is built).
fn check_no_outputs_placeholders(ir: &Ir) -> Result<(), CompileError> {
    let mut words: Vec<(String, &String)> = ir.workdir.iter().map(|w| ("the workdir".to_string(), w)).collect();
    for (id, n) in &ir.nodes {
        let commands: Vec<&Vec<String>> = match &n.class {
            Class::Pure { run } | Class::Observe { run, .. } => vec![run],
            Class::Work { verify, .. } => vec![verify],
            Class::Effect { run, reconcile, .. } => vec![run, reconcile],
            Class::Choice { .. } | Class::Human { .. } | Class::Subflow { .. } | Class::Map { .. } => vec![],
        };
        words.extend(commands.into_iter().flatten().chain(&n.snapshot).map(|w| (format!("node {id}"), w)));
    }
    if let Some((at, _)) = words.iter().find(|(_, w)| w.contains("{{outputs.")) {
        return Err(CompileError::invalid(format!(
            "{at} names {{{{outputs...}}}}: a work node's verify reads its outputs as JSON on standard input"
        )));
    }
    Ok(())
}
