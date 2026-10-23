//! `maestro delegation` and `maestro economy`: the CLIs of the workspace's `scripts/delegation.py`
//! and `scripts/fleet_economy.py`, over the ported `maestro_core::{delegation, economy}`.
//!
//! Output, messages and exit codes are the Python's, so the two can be run side by side
//! (`scripts/parity-delegation.py`). This module holds the I/O the core does not: loading a
//! graph, binding its repos to directories, reading each repo's policy and checking files exist.

use clap::{Args, Subcommand};
use maestro_core::delegation::{self as d, BriefNode, Disk, Fault, Ref, Thinking, Tier};
use maestro_core::{economy as e, py};
use serde_json::{Map, Value, json};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::Path;
use std::process::ExitCode;

#[derive(Subcommand)]
pub enum DelegationCommand {
    /// Tier, verifier and needs for one unit of work.
    Assess(AssessArgs),
    /// Validate an agent graph; print each node's tier and Maestro fields.
    Plan {
        graph: String,
        #[command(flatten)]
        bind: BindArgs,
    },
    /// The delegation block for one node of a valid graph.
    Brief {
        graph: String,
        node: String,
        #[command(flatten)]
        bind: BindArgs,
    },
    /// The tiers and the models they map to.
    Ladder,
}

#[derive(Args)]
pub struct BindArgs {
    /// A graph with no `repos`: the repo whose .control/policy.yaml names governance paths (default: cwd).
    #[arg(long)]
    root: Option<String>,
    /// Where a repo the graph names is checked out here (overrides its `repos` path).
    #[arg(long = "repo", value_name = "NAME=PATH")]
    repos: Vec<String>,
    /// Skip, and name, nodes whose repo is not checked out here; default is an error.
    #[arg(long)]
    skip_absent_repos: bool,
}

#[derive(Args)]
pub struct AssessArgs {
    /// The repo whose .control/policy.yaml names governance paths (default: cwd).
    #[arg(long)]
    root: Option<String>,
    /// Classify with the built-in governance paths only (a repo with no policy file).
    #[arg(long)]
    no_policy: bool,
    #[arg(long, default_value = "implement")]
    kind: String,
    #[arg(long, num_args = 0..)]
    paths: Vec<String>,
    /// Omit when unknown; unknown counts as 0 for the LOC rules.
    #[arg(long, allow_negative_numbers = true)]
    est_loc: Option<i64>,
    #[arg(long, allow_negative_numbers = true)]
    files: Option<i64>,
    #[arg(long, default_value_t = 1, allow_negative_numbers = true)]
    repos: i64,
    #[arg(long)]
    public_api: bool,
    #[arg(long)]
    one_way_door: bool,
    #[arg(long)]
    spec: Option<String>,
    #[arg(long, default_value_t = 0, allow_negative_numbers = true)]
    failed_attempts: i64,
    #[arg(long)]
    observed_tier: Option<String>,
    /// capacity.cadence.mode from the snapshot (read_only or read-only).
    #[arg(long)]
    mode: Option<String>,
    #[arg(long)]
    allow_apex: bool,
    /// The done check; with --brief, print the delegation block instead of JSON.
    #[arg(long)]
    done: Option<String>,
    #[arg(long)]
    brief: bool,
    #[arg(long)]
    ticket: Option<String>,
}

#[derive(Subcommand)]
pub enum EconomyCommand {
    /// The snapshot's capacity section: forecast, cadence mode and launch gates.
    Capacity {
        /// `provider_manager.py --json usage` output; `-` reads stdin.
        #[arg(long)]
        usage: String,
        /// The sample cache (JSON lines), already holding the current sample. Default: none.
        #[arg(long)]
        samples: Option<String>,
        /// The time to forecast from (ISO 8601). Default: now.
        #[arg(long)]
        now: Option<String>,
    },
    /// Maestro's economy by tier, and what to re-plan.
    Tiers {
        #[arg(long)]
        path: Option<String>,
    },
}

fn fail(msg: impl AsRef<str>, code: u8) -> ExitCode {
    eprintln!("{}", msg.as_ref());
    ExitCode::from(code)
}

fn cwd() -> String {
    std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| ".".into())
}

fn expanduser(p: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match p.strip_prefix('~') {
        Some("") => home,
        Some(rest) if rest.starts_with('/') => format!("{home}{rest}"),
        _ => p.to_string(),
    }
}

/// `os.path.realpath`, non-strict: symlinks resolved where they exist, the rest kept.
pub fn realpath(filename: &str) -> String {
    let mut rest: Vec<Option<String>> = filename.split('/').rev().map(|s| Some(s.to_string())).collect();
    let mut part_count = rest.len();
    let mut path = if filename.starts_with('/') { "/".to_string() } else { cwd() };
    let mut seen: HashMap<String, Option<String>> = HashMap::new();
    while part_count > 0 {
        let Some(name) = rest.pop() else { break };
        let Some(name) = name else {
            if let Some(Some(link)) = rest.pop() {
                seen.insert(link, Some(path.clone()));
            }
            continue;
        };
        part_count -= 1;
        if name.is_empty() || name == "." {
            continue;
        }
        if name == ".." {
            path = match path.rfind('/') {
                Some(0) | None => "/".into(),
                Some(i) => path[..i].to_string(),
            };
            continue;
        }
        let newpath = if path == "/" { format!("/{name}") } else { format!("{path}/{name}") };
        let Ok(meta) = std::fs::symlink_metadata(&newpath) else {
            path = newpath;
            continue;
        };
        if !meta.file_type().is_symlink() {
            path = newpath;
            continue;
        }
        if let Some(cached) = seen.get(&newpath) {
            match cached {
                Some(p) => {
                    path = p.clone();
                    continue;
                }
                None => {
                    path = newpath;
                    continue;
                }
            }
        }
        let Ok(target) = std::fs::read_link(&newpath) else {
            path = newpath;
            continue;
        };
        let target = target.to_string_lossy().into_owned();
        if target.starts_with('/') {
            path = "/".into();
        }
        seen.insert(newpath.clone(), None);
        rest.push(Some(newpath));
        rest.push(None);
        let parts: Vec<Option<String>> = target.split('/').rev().map(|s| Some(s.to_string())).collect();
        part_count += parts.len();
        rest.extend(parts);
    }
    path
}

/// The graph, and its `repos` names in file order (JSON values sort their keys; the Python's
/// dict keeps file order, and its messages and nested-repo checks follow it).
fn load_graph(path: &str) -> Result<(Value, Vec<String>), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let v: Value = if path.ends_with(".json") {
        serde_json::from_str(&text).map_err(|e| e.to_string())?
    } else {
        // PyYAML applies merge keys (`<<: *anchor`); serde_yaml only on request.
        let mut y: serde_yaml::Value = serde_yaml::from_str(&text).map_err(|e| e.to_string())?;
        y.apply_merge().map_err(|e| e.to_string())?;
        serde_json::to_value(y).map_err(|e| e.to_string())?
    };
    let ordered: serde_yaml::Value = serde_yaml::from_str(&text).map_err(|e| e.to_string())?;
    let order = match ordered.get("repos") {
        Some(serde_yaml::Value::Mapping(m)) => {
            m.keys().map(|k| serde_json::to_value(k).map(|k| py::str_of(&k)).unwrap_or_default()).collect()
        }
        _ => vec![],
    };
    Ok((if py::truthy(Some(&v)) { v } else { json!({}) }, order))
}

/// A repo's `.control/policy.yaml` require_human paths. An unreadable policy is `Fault::Value`
/// with the Python's message shape.
fn policy_of(root: &str) -> Result<Vec<String>, Fault> {
    let path = py::join(&py::join(root, ".control"), "policy.yaml");
    let text = std::fs::read_to_string(&path).map_err(|e| {
        Fault::Value(match e.kind() {
            std::io::ErrorKind::NotFound => {
                format!("{path}: [Errno 2] No such file or directory: {}", py::repr_str(&path))
            }
            _ => format!("{path}: {e}"),
        })
    })?;
    let data: Value = serde_yaml::from_str(&text).map_err(|e| Fault::Value(format!("{path}: {e}")))?;
    d::require_human_paths(&data).map_err(|f| match f {
        Fault::Value(m) => Fault::Value(format!("{path}: {m}")),
        crash => crash,
    })
}

struct Repo {
    dir: String,
    policy: Value,
}

/// name -> {dir, policy} for every repo a graph names, in the graph's order.
/// `Fault::Value` is the Python's `ValueError` (exit 2); `Fault::Crash` its traceback (exit 1).
fn bind_repos(
    graph: &Value,
    order: &[String],
    root: &str,
    bindings: &[(String, String)],
) -> Result<Vec<(String, Repo)>, Fault> {
    let named = py::truthy(graph.get("repos"));
    let home = py::or(graph.get("repo"), None).map_or_else(String::new, py::str_of);
    // `graph.get("repos") or {}`, then `.items()`: a truthy non-mapping is the Python's AttributeError.
    let declared: Vec<(String, Value)> = match py::or(graph.get("repos"), None) {
        Some(Value::Object(o)) => order.iter().filter_map(|k| o.get(k).map(|v| (k.clone(), v.clone()))).collect(),
        Some(other) => {
            return Err(Fault::Crash {
                kind: "AttributeError",
                msg: format!("'{}' object has no attribute 'items'", d::type_name(other)),
            });
        }
        None => vec![(home, json!(root))],
    };
    let mut out: Vec<(String, Repo)> = Vec::new();
    for (name, v) in declared {
        if named && (name.is_empty() || name.contains(':')) {
            return Err(Fault::Value(format!("repo name {}: a name is non-empty and has no ':'", py::repr_str(&name))));
        }
        let (mut path, policy) = match &v {
            Value::Object(o) => (o.get("path").cloned(), o.get("policy").cloned().unwrap_or(Value::Null)),
            other => (Some(other.clone()), Value::Null),
        };
        if let Some((_, p)) = bindings.iter().rev().find(|(n, _)| *n == name) {
            path = Some(json!(p));
        }
        let Some(path) = path.filter(|p| py::truthy(Some(p))) else {
            return Err(Fault::Value(format!("repo {} has no path", py::repr_str(&name))));
        };
        out.push((name, Repo { dir: realpath(&expanduser(&py::str_of(&path))), policy }));
    }
    let mut unknown: Vec<&str> =
        bindings.iter().map(|(n, _)| n.as_str()).filter(|n| !out.iter().any(|(o, _)| o == n)).collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        unknown.dedup();
        return Err(Fault::Value(format!("--repo names {}, which the graph does not", unknown.join(", "))));
    }
    let mut seen: HashMap<&str, &str> = HashMap::new();
    for (name, r) in &out {
        if let Some(first) = seen.get(r.dir.as_str()) {
            return Err(Fault::Value(format!(
                "repos {first} and {name} are one directory ({}): a place has one name",
                r.dir
            )));
        }
        seen.insert(&r.dir, name);
    }
    Ok(out)
}

/// The Python's traceback exit for a fault it does not catch.
fn crash(f: &Fault) -> ExitCode {
    fail(format!("Traceback (most recent call last):\n  {f}"), 1)
}

fn plan_or_brief(graph_path: &str, node: Option<&str>, bind: &BindArgs, json_out: bool) -> ExitCode {
    let (graph, order) = match load_graph(graph_path) {
        Ok(g) => g,
        Err(e) => return fail(format!("Traceback (most recent call last):\n  {e}"), 1),
    };
    if py::truthy(graph.get("repos")) && bind.root.is_some() {
        return fail("this graph names its repos: bind them with --repo NAME=PATH, not --root", 2);
    }
    let root = bind.root.clone().unwrap_or_else(cwd);
    if let Some(bad) = bind.repos.iter().find(|x| x.split_once('=').is_none_or(|(n, p)| n.is_empty() || p.is_empty())) {
        return fail(format!("  [error] --repo takes NAME=PATH, not {}", py::repr_str(bad)), 2);
    }
    let bindings: Vec<(String, String)> =
        bind.repos.iter().filter_map(|x| x.split_once('=').map(|(n, p)| (n.to_string(), p.to_string()))).collect();
    let repos = match bind_repos(&graph, &order, &root, &bindings) {
        Ok(r) => r,
        Err(Fault::Value(e)) => return fail(format!("  [error] {e}"), 2),
        Err(f) => return crash(&f),
    };
    let absent: HashSet<String> =
        repos.iter().filter(|(_, r)| !Path::new(&r.dir).is_dir()).map(|(n, _)| n.clone()).collect();
    if !absent.is_empty() && !bind.skip_absent_repos {
        let mut names: Vec<&String> = absent.iter().collect();
        names.sort();
        for name in names {
            let dir = &repos.iter().find(|(n, _)| n == name).expect("bound").1.dir;
            eprintln!(
                "  [error] repo {name} ({dir}) is not here: bind it with --repo {name}=PATH, or pass --skip-absent-repos"
            );
        }
        return ExitCode::from(1);
    }
    let home = py::or(graph.get("repo"), None).map_or_else(String::new, py::str_of);
    let mut skipped: Vec<String> = Vec::new();
    if let Ok(nodes) = d::iter_of(graph.get("nodes").unwrap_or(&json!([]))) {
        for n in &nodes {
            let Some(id) = n.get("id").filter(|v| py::truthy(Some(v))) else { continue };
            if !n.is_object() {
                continue;
            }
            let here = py::or(n.get("repo"), None).map_or_else(|| home.clone(), py::str_of);
            if absent.contains(&here) {
                skipped.push(py::str_of(id));
            }
        }
    }
    skipped.sort();
    skipped.dedup();
    for nid in &skipped {
        eprintln!("  [skip] {nid}: its repo is not checked out here; structure checked only (--skip-absent-repos)");
    }
    let dirs: Vec<(String, String)> = repos.iter().map(|(n, r)| (n.clone(), r.dir.clone())).collect();
    let policies: RefCell<HashMap<String, Vec<String>>> = RefCell::new(HashMap::new());
    let policy_for = |name: &str| -> Result<Vec<String>, Fault> {
        if let Some(p) = policies.borrow().get(name) {
            return Ok(p.clone());
        }
        let r = &repos.iter().find(|(n, _)| n == name).expect("a named repo").1;
        let file = py::join(&py::join(&r.dir, ".control"), "policy.yaml");
        let has_file = Path::new(&file).is_file();
        let got = if r.policy == json!("none") {
            if has_file {
                return Err(Fault::Value(format!(
                    "repo {name} declares policy: none, but {} has .control/policy.yaml",
                    r.dir
                )));
            }
            vec![]
        } else {
            policy_of(&r.dir).map_err(|f| match f {
                Fault::Value(e) => {
                    let hint = if has_file {
                        String::new()
                    } else {
                        format!(" (a repo with no policy file declares `policy: none` in `repos.{name}`)")
                    };
                    Fault::Value(format!("{e}{hint}"))
                }
                crash => crash,
            })?
        };
        policies.borrow_mut().insert(name.to_string(), got.clone());
        Ok(got)
    };
    let exists = |r: &Ref| -> bool {
        if absent.contains(&r.repo) {
            return true; // unknown here: not checked out, so never reported missing
        }
        dirs.iter().find(|(n, _)| *n == r.repo).is_some_and(|(_, dir)| Path::new(&py::join(dir, &r.path)).is_file())
    };
    let disk = Disk { exists: &exists, policy_for: &policy_for };
    let skip: HashSet<String> = skipped.into_iter().collect();
    let (errors, nodes) = match d::validate(&graph, &[], Some(&disk), Some(&dirs), &skip) {
        Ok(r) => r,
        Err(f) => return crash(&f),
    };
    if !errors.is_empty() {
        for err in errors {
            eprintln!("  [error] {err}");
        }
        return ExitCode::from(1);
    }
    if let Some(want) = node {
        // `n["id"] == o.node`: only a string id names a node from the command line.
        let Some(n) = nodes.iter().find(|n| n.id.as_str() == Some(want)) else {
            return fail(format!("no node {}", py::repr_str(want)), 2);
        };
        let ticket = graph.get("ticket").filter(|t| py::truthy(Some(t))).map(py::str_of);
        let spawn = n.spawn_default.as_str().and_then(Tier::parse).unwrap_or(n.tier);
        let brief = BriefNode {
            tier: n.tier,
            spawn_default: spawn,
            thinking: n.thinking.as_str().unwrap_or(""),
            verify: n.verify.as_str().unwrap_or(""),
            done: &py::str_of(&n.done),
            inputs: n.inputs.iter().cloned().zip(n.input_refs.iter().cloned().map(Some)).collect(),
            repo: Some(&n.repo),
            repo_dirs: &dirs,
        };
        println!("{}", d::brief_block(&brief, ticket.as_deref()));
        return ExitCode::SUCCESS;
    }
    if json_out {
        let all: Vec<Value> = nodes.iter().map(d::PlanNode::to_json).collect();
        println!("{}", serde_json::to_string_pretty(&all).expect("JSON"));
        return ExitCode::SUCCESS;
    }
    for n in &nodes {
        let thinking = n.thinking.as_str().and_then(Thinking::parse).unwrap_or(n.tier.rung().thinking);
        // `','.join(depends_on)` takes strings only.
        if let Some(bad) = n.depends_on.iter().find(|d| !d.is_string()) {
            return crash(&Fault::Crash {
                kind: "TypeError",
                msg: format!("sequence item 0: expected str instance, {} found", d::type_name(bad)),
            });
        }
        let after: Vec<String> = n.depends_on.iter().map(py::str_of).collect();
        let after = after.join(",");
        // A number right-aligns in a format width, a string left-aligns.
        let id = match &n.id {
            Value::String(s) => format!("{s:16}"),
            other => format!("{:>16}", py::str_of(other)),
        };
        println!(
            "{id} {:12} {:10} {:28} thinking={:6} verify={:8} after={}",
            n.kind,
            n.tier.as_str(),
            n.tier.rung().provider,
            thinking.as_str(),
            py::str_of(&n.verify),
            if after.is_empty() { "-" } else { &after }
        );
    }
    ExitCode::SUCCESS
}

fn assess(a: &AssessArgs) -> ExitCode {
    if let Some(t) = &a.observed_tier
        && Tier::parse(t).is_none()
    {
        return fail(format!("argument --observed-tier: invalid choice: {}", py::repr_str(t)), 2);
    }
    let mode = a.mode.as_deref().map(d::norm_mode);
    if let Some(m) = &mode
        && m != "normal"
        && !d::SAVING_MODES.contains(&m.as_str())
    {
        return fail(format!("argument --mode: invalid choice: {}", py::repr_str(m)), 2);
    }
    let root = a.root.clone().unwrap_or_else(cwd);
    let human = if a.no_policy {
        vec![]
    } else {
        match policy_of(&root) {
            Ok(h) => h,
            Err(crash @ Fault::Crash { .. }) => return self::crash(&crash),
            Err(e) => {
                return fail(
                    format!("policy unreadable, refusing to classify (pass --no-policy for a repo without one): {e}"),
                    3,
                );
            }
        }
    };
    let spec = a.spec.as_deref().map(py::normpath);
    if let Some(s) = &spec
        && !(d::is_spec_path(s) && Path::new(&py::join(&root, s)).is_file())
    {
        return fail(
            format!(
                "--spec {} must be an existing file under {} of {root}",
                a.spec.as_deref().unwrap_or(""),
                d::SPEC_DIRS.join(", ")
            ),
            2,
        );
    }
    let sig = json!({
        "kind": a.kind, "paths": a.paths, "est_loc": a.est_loc, "files": a.files, "repos": a.repos,
        "public_api": a.public_api, "one_way_door": a.one_way_door, "spec": spec,
        "failed_attempts": a.failed_attempts, "observed_tier": a.observed_tier, "mode": mode,
        "allow_apex": a.allow_apex,
    });
    let r = match d::assess(sig.as_object().expect("an object"), &human) {
        Ok(r) => r,
        Err(f) => return fail(format!("{f}"), 2),
    };
    if a.brief {
        let done = a.done.as_deref().unwrap_or("");
        if done.trim().is_empty() {
            return fail("--brief needs --done: a brief without a done check is not delegable", 2);
        }
        if !r.needs.is_empty() {
            return fail(
                format!("not delegable as one node: needs {} first ({})", r.needs.join(", "), r.reasons.join("; ")),
                1,
            );
        }
        let brief = BriefNode {
            tier: r.executor,
            spawn_default: r.executor,
            thinking: r.thinking.as_str(),
            verify: r.verify,
            done,
            inputs: spec.iter().map(|s| (s.clone(), None)).collect(),
            repo: None,
            repo_dirs: &[],
        };
        println!("{}", d::brief_block(&brief, a.ticket.as_deref()));
        return ExitCode::SUCCESS;
    }
    let mut out = serde_json::to_value(&r).expect("JSON");
    out["contract"] = d::contract(r.executor, Some(r.thinking));
    if let Some(author) = r.author {
        out["author_contract"] = d::contract(author, None);
    }
    println!("{}", serde_json::to_string_pretty(&out).expect("JSON"));
    ExitCode::SUCCESS
}

pub fn ladder() -> Value {
    let m: Map<String, Value> = d::LADDER
        .iter()
        .map(|(t, r)| {
            (t.as_str().to_string(), json!({"provider": r.provider, "thinking": r.thinking, "agent": r.agent}))
        })
        .collect();
    Value::Object(m)
}

pub fn delegation(cmd: &DelegationCommand, json_out: bool) -> ExitCode {
    match cmd {
        DelegationCommand::Ladder => {
            println!("{}", serde_json::to_string_pretty(&ladder()).expect("JSON"));
            ExitCode::SUCCESS
        }
        DelegationCommand::Assess(a) => assess(a),
        DelegationCommand::Plan { graph, bind } => plan_or_brief(graph, None, bind, json_out),
        DelegationCommand::Brief { graph, node, bind } => plan_or_brief(graph, Some(node), bind, json_out),
    }
}

fn read_input(path: &str) -> Result<String, String> {
    if path == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).map_err(|e| e.to_string())?;
        Ok(s)
    } else {
        std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
    }
}

pub fn economy(cmd: &EconomyCommand, json_out: bool) -> ExitCode {
    match cmd {
        EconomyCommand::Capacity { usage, samples, now } => {
            let run = || -> Result<Value, String> {
                let rows: Value = serde_json::from_str(&read_input(usage)?).map_err(|e| e.to_string())?;
                let rows = rows.as_array().cloned().unwrap_or_default();
                let samples: Vec<Value> = match samples {
                    Some(p) => read_input(p)?.lines().filter_map(|l| serde_json::from_str(l).ok()).collect(),
                    None => vec![],
                };
                let now = match now {
                    Some(n) => e::parse_ts(Some(&json!(n)))?.ok_or("--now is empty")?,
                    None => jiff::Timestamp::now().as_microsecond(),
                };
                e::capacity(&rows, &samples, now)
            };
            match run() {
                Ok(v) => {
                    println!("{}", serde_json::to_string_pretty(&v).expect("JSON"));
                    ExitCode::SUCCESS
                }
                Err(err) => fail(format!("maestro: {err}"), 1),
            }
        }
        EconomyCommand::Tiers { path } => {
            let path = path.clone().unwrap_or_else(|| expanduser("~/.config/maestro-paseo/economy.json"));
            let data: Value = match std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
            {
                Ok(d) => d,
                Err(err) => return fail(format!("no economy by tier yet ({err})"), 1),
            };
            let (rows, signals) = e::tier_signals(&data);
            if json_out {
                let out = json!({"generatedAt": data.get("generatedAt"), "rows": rows, "replan": signals});
                println!("{}", serde_json::to_string_pretty(&out).expect("JSON"));
                return ExitCode::SUCCESS;
            }
            for r in &rows {
                println!(
                    "{:10} runs {} approved {} first-pass {}",
                    py::str_of(&r["tier"]),
                    py::str_of(&r["runs"]),
                    py::str_of(&r["approved"]),
                    py::str_of(&r["first_pass"])
                );
            }
            for s in signals {
                println!("re-plan: {s}");
            }
            ExitCode::SUCCESS
        }
    }
}

/// One parity case: a ported function on JSON arguments. Used by `scripts/parity-delegation.py`
/// and by `crates/core/tests/parity_delegation.rs` (which evaluates the same cases in-process).
pub fn eval_stdin() -> ExitCode {
    let mut s = String::new();
    if let Err(err) = std::io::stdin().read_to_string(&mut s) {
        return fail(err.to_string(), 2);
    }
    let cases: Vec<Value> = match serde_json::from_str(&s) {
        Ok(c) => c,
        Err(err) => return fail(err.to_string(), 2),
    };
    let out: Vec<Value> = cases.iter().map(maestro_core::parity::eval).collect();
    println!("{}", serde_json::to_string(&out).expect("JSON"));
    ExitCode::SUCCESS
}
