//! Tiered delegation: the assessment of one unit of work and the rules of an agent graph.
//!
//! A port of the workspace's `scripts/delegation.py` (BRO-3118), held to it by
//! `scripts/parity-delegation.py` and `crates/core/tests/parity_delegation.rs`. The tier says
//! which model does the work, never what the work may reach. The assessment is a floor: an
//! earlier raise (`observed_tier`) and each failed verification move it up.
//!
//! Graphs arrive as loaded YAML (JSON values), because the Python reads whatever the file holds
//! and its messages name those values. This module does no I/O: on-disk existence and a repo's
//! policy come in as closures (the `plan` CLI binds them).

use crate::py;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};

/// The tier ladder, lowest first: the one place model names live. It equals the plugin's
/// `DEFAULT_TIER_LADDER` (maestro-paseo `shared/tiers.ts`), checked by the parity harness.
pub const LADDER: [(Tier, Rung); 4] = [
    (Tier::Mechanical, Rung { provider: "claude/claude-haiku-5-5", thinking: Thinking::Low, agent: "haiku" }),
    (Tier::Standard, Rung { provider: "claude/claude-sonnet-5-5", thinking: Thinking::Medium, agent: "sonnet" }),
    (Tier::Frontier, Rung { provider: "claude/claude-opus-5-5", thinking: Thinking::High, agent: "opus" }),
    (Tier::Apex, Rung { provider: "claude/claude-fable-5-1", thinking: Thinking::High, agent: "fable" }),
];
/// Apex only when a node asks for it with a reason.
pub const TOP_DEFAULT: Tier = Tier::Frontier;

pub const MECHANICAL_KINDS: [&str; 8] =
    ["rerun", "update-branch", "merge-on-green", "archive", "cleanup", "closeout", "relay", "resume"];
pub const WORK_KINDS: [&str; 3] = ["implement", "research", "docs"];
pub const FRONTIER_KINDS: [&str; 3] = ["design", "decompose", "review"];
/// Verification strength: a node may verify harder than assessed, never weaker.
pub const VERIFY: [(&str, u8); 4] = [("check", 0), ("rubric", 1), ("p20", 1), ("p20-full", 2)];
/// fleet_economy cadence modes that spend less.
pub const SAVING_MODES: [&str; 3] = ["conserve", "critical", "read_only"];
/// The P20 threshold: more than this many lines, or multi-file, or a public API.
pub const SUBSTANTIVE_LOC: i64 = 200;
/// More non-test files than this needs a spec even when LOC is unknown.
pub const SPEC_FILES: usize = 3;
/// Governance class beyond the policy's require_human rules: the controller and its contracts.
pub const GOVERNANCE_EXTRA: [&str; 7] = [
    ".control/",
    ".broomva/loops/",
    "metalayer.md",
    "references/orchestrator-tick.md",
    "references/delegation.md",
    "scripts/delegation.py",
    "scripts/fleet_economy.py",
];
pub const GOVERNANCE_ROOT: [&str; 2] = ["claude.md", "agents.md"];
/// Where a file counts as a spec, stated or found.
pub const SPEC_DIRS: [&str; 3] = ["docs/specs/", "docs/adrs/", "docs/plans/"];
const CODE_EXT: [&str; 13] =
    [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".py", ".rb", ".go", ".rs", ".java", ".kt", ".swift"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Mechanical,
    Standard,
    Frontier,
    Apex,
}

impl Tier {
    pub const ALL: [Tier; 4] = [Tier::Mechanical, Tier::Standard, Tier::Frontier, Tier::Apex];

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Mechanical => "mechanical",
            Tier::Standard => "standard",
            Tier::Frontier => "frontier",
            Tier::Apex => "apex",
        }
    }

    pub fn parse(s: &str) -> Option<Tier> {
        Tier::ALL.into_iter().find(|t| t.as_str() == s)
    }

    fn of(v: Option<&Value>) -> Option<Tier> {
        v.and_then(Value::as_str).and_then(Tier::parse)
    }

    pub fn rung(self) -> &'static Rung {
        &LADDER[self as usize].1
    }

    /// The model of this tier's provider (`claude-sonnet-5-5`).
    pub fn model(self) -> &'static str {
        self.rung().provider.split_once('/').map_or("", |(_, m)| m)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Thinking {
    Low,
    Medium,
    High,
    Xhigh,
}

impl Thinking {
    pub const ALL: [Thinking; 4] = [Thinking::Low, Thinking::Medium, Thinking::High, Thinking::Xhigh];

    pub fn as_str(self) -> &'static str {
        match self {
            Thinking::Low => "low",
            Thinking::Medium => "medium",
            Thinking::High => "high",
            Thinking::Xhigh => "xhigh",
        }
    }

    pub fn parse(s: &str) -> Option<Thinking> {
        Thinking::ALL.into_iter().find(|t| t.as_str() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rung {
    pub provider: &'static str,
    pub thinking: Thinking,
    pub agent: &'static str,
}

/// `tier` moved `steps` rungs up, capped at `top`, never below `tier`.
pub fn bump(tier: Tier, steps: i64, top: Tier) -> Tier {
    let up = (tier as i64 + steps.max(0)).min(top as i64);
    Tier::ALL[(tier as i64).max(up) as usize]
}

/// fleet_economy writes `read_only`; prose says `read-only`. Both are one mode.
pub fn norm_mode(mode: &str) -> String {
    mode.replace('-', "_")
}

/// The tier a model id implies: haiku mechanical, sonnet standard, opus frontier, fable apex.
/// Anything else (codex, antigravity) is none. The plugin's `tierOfModel`.
pub fn tier_of_model(model: &str) -> Option<Tier> {
    let id = model.to_lowercase();
    [("haiku", Tier::Mechanical), ("sonnet", Tier::Standard), ("opus", Tier::Frontier), ("fable", Tier::Apex)]
        .into_iter()
        .find(|(k, _)| id.contains(k))
        .map(|(_, t)| t)
}

/// The model of a `provider/model` selection, or none for a bare provider. The plugin's `modelOfProvider`.
pub fn model_of_provider(provider: &str) -> Option<&str> {
    let text = provider.trim();
    let slash = text.find('/')?;
    (slash < text.len() - 1).then(|| &text[slash + 1..])
}

/// One policy rule against a lowercased path. A rule with `*` or `?` is a glob over the whole
/// path whose wildcards stay inside one segment; any other rule is a substring.
pub fn rule_matches(rule: &str, path: &str) -> bool {
    let r = rule.to_lowercase();
    if r.contains('*') || r.contains('?') {
        let pat: Vec<char> = r.chars().collect();
        let text: Vec<char> = path.chars().collect();
        glob(&pat, &text)
    } else {
        path.contains(&r)
    }
}

/// `re.fullmatch` of `*` -> `[^/]*`, `?` -> `[^/]`, every other character literal.
fn glob(pat: &[char], text: &[char]) -> bool {
    match pat.split_first() {
        None => text.is_empty(),
        Some(('*', rest)) => {
            let mut i = 0;
            loop {
                if glob(rest, &text[i..]) {
                    return true;
                }
                if i == text.len() || text[i] == '/' {
                    return false;
                }
                i += 1;
            }
        }
        Some(('?', rest)) => text.first().is_some_and(|c| *c != '/') && glob(rest, &text[1..]),
        Some((c, rest)) => text.first() == Some(c) && glob(rest, &text[1..]),
    }
}

/// A path whose change can move a gate, a controller or a contract. Case-insensitive.
pub fn is_governance(path: &str, human_paths: &[String]) -> bool {
    let p = path.strip_prefix("./").unwrap_or(path).to_lowercase();
    if GOVERNANCE_ROOT.contains(&p.as_str()) {
        return true;
    }
    human_paths.iter().map(String::as_str).chain(GOVERNANCE_EXTRA).any(|s| rule_matches(s, &p))
}

/// A test file, by the fleet's conventions. `.spec.` counts only on code.
pub fn is_test_path(path: &str) -> bool {
    let low = format!("/{}", path.to_lowercase());
    let base = low.rsplit('/').next().unwrap_or("");
    if ["/tests/", "/test/", "/__tests__/", "/spec/"].iter().any(|d| low.contains(d)) {
        return true;
    }
    base.starts_with("test_")
        || base.contains(".test.")
        || base.contains("_test.")
        || ((base.contains("_spec.") || base.contains(".spec.")) && CODE_EXT.iter().any(|e| base.ends_with(e)))
}

pub fn is_spec_path(path: &str) -> bool {
    SPEC_DIRS.iter().any(|d| path.starts_with(d))
}

/// Why the Python would stop. `Value` is a `ValueError` the graph validator reports as a node
/// error; `Crash` is one it lets through (a traceback, exit 1), with the Python exception's kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    Value(String),
    Crash { kind: &'static str, msg: String },
}

impl Fault {
    fn type_error(msg: String) -> Fault {
        Fault::Crash { kind: "TypeError", msg }
    }

    fn attribute_error(msg: String) -> Fault {
        Fault::Crash { kind: "AttributeError", msg }
    }
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::Value(m) => f.write_str(m),
            Fault::Crash { kind, msg } => write!(f, "{kind}: {msg}"),
        }
    }
}

/// `int(x)` on a loaded value.
fn int_of(v: &Value) -> Result<i64, Fault> {
    match v {
        Value::Bool(b) => Ok(*b as i64),
        Value::Number(n) => {
            n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc() as i64)).ok_or_else(|| {
                Fault::Crash { kind: "OverflowError", msg: format!("cannot convert float {} to integer", py::repr(v)) }
            })
        }
        Value::String(s) => {
            // Python's int(): optional sign, ASCII digits, single underscores between digits.
            let t = s.trim();
            let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
            let ok = digits.starts_with(|c: char| c.is_ascii_digit())
                && digits.ends_with(|c: char| c.is_ascii_digit())
                && digits.chars().all(|c| c.is_ascii_digit() || c == '_')
                && !digits.contains("__");
            if ok {
                // Python's int is unbounded; an i64 is not, so a longer literal stops here.
                t.replace('_', "")
                    .parse()
                    .map_err(|_| Fault::Crash { kind: "OverflowError", msg: format!("integer {s} out of range") })
            } else {
                Err(Fault::Value(format!("invalid literal for int() with base 10: {}", py::repr_str(s))))
            }
        }
        other => Err(Fault::type_error(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            type_name(other)
        ))),
    }
}

/// Python's type name of a loaded value.
pub fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// `int(sig.get(k) or default)`.
fn int_or(sig: &Map<String, Value>, k: &str, default: i64) -> Result<i64, Fault> {
    match sig.get(k) {
        v if py::truthy(v) => int_of(v.expect("truthy is present")),
        _ => Ok(default),
    }
}

/// Iterating a loaded value as Python would (`for x in v`): a list's items, a string's
/// characters, a mapping's keys. Anything else is a `TypeError`.
pub fn iter_of(v: &Value) -> Result<Vec<Value>, Fault> {
    match v {
        Value::Array(a) => Ok(a.clone()),
        Value::String(s) => Ok(s.chars().map(|c| Value::String(c.to_string())).collect()),
        Value::Object(o) => Ok(o.keys().map(|k| Value::String(k.clone())).collect()),
        other => Err(Fault::type_error(format!("'{}' object is not iterable", type_name(other)))),
    }
}

/// `v or []`, iterated.
fn iter_or_empty(v: Option<&Value>) -> Result<Vec<Value>, Fault> {
    if py::truthy(v) { iter_of(v.expect("truthy is present")) } else { Ok(Vec::new()) }
}

/// What a unit of work needs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Assessment {
    pub complexity: &'static str,
    pub author: Option<Tier>,
    pub executor: Tier,
    pub thinking: Thinking,
    pub verify: &'static str,
    pub needs: Vec<&'static str>,
    pub reasons: Vec<String>,
}

/// What a unit of work needs, from signals a brief can state before any work is done.
///
/// `sig` keys as the Python takes them: kind, paths, est_loc, files, repos, public_api,
/// one_way_door, spec, failed_attempts, observed_tier, allow_apex, mode.
pub fn assess(sig: &Map<String, Value>, human_paths: &[String]) -> Result<Assessment, Fault> {
    let kind_v = py::or(sig.get("kind"), None);
    let kind = match kind_v {
        None => "implement".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => py::str_of(other),
    };
    let known = |k: &str| MECHANICAL_KINDS.contains(&k) || WORK_KINDS.contains(&k) || FRONTIER_KINDS.contains(&k);
    if !matches!(kind_v, None | Some(Value::String(_))) || !known(&kind) {
        let mut all: Vec<&str> = MECHANICAL_KINDS.iter().chain(&WORK_KINDS).chain(&FRONTIER_KINDS).copied().collect();
        all.sort_unstable();
        let shown = kind_v.map_or_else(|| py::repr_str(&kind), py::repr);
        let list = all.iter().map(|k| py::repr_str(k)).collect::<Vec<_>>().join(", ");
        return Err(Fault::Value(format!("unknown kind {shown}; one of [{list}]")));
    }
    let kind = kind.as_str();
    let paths: Vec<String> = iter_or_empty(sig.get("paths"))?.iter().map(py::str_of).collect();
    let loc = int_or(sig, "est_loc", 0)?;
    let files = int_or(sig, "files", 0)?.max(paths.len() as i64);
    let code_files = paths.iter().filter(|p| !is_test_path(p)).count();
    let repos = int_or(sig, "repos", 1)?;
    let gov: Vec<&String> = paths.iter().filter(|p| is_governance(p, human_paths)).collect();
    let one_way = py::truthy(sig.get("one_way_door"));
    let public_api = py::truthy(sig.get("public_api"));
    let spec = py::truthy(sig.get("spec"));
    let mut reasons: Vec<String> = Vec::new();
    let mut needs = Vec::new();
    let mut author = None;
    let mut verify = "check";
    let mut thinking: Option<Thinking> = None;
    let mut executor;
    let complexity;

    if MECHANICAL_KINDS.contains(&kind) && paths.is_empty() && loc == 0 && gov.is_empty() {
        executor = Tier::Mechanical;
        complexity = "trivial";
        reasons.push(format!("{kind} names no code: a deterministic check verifies it"));
    } else if FRONTIER_KINDS.contains(&kind) {
        executor = Tier::Frontier;
        complexity = if !gov.is_empty() || one_way { "governance" } else { "substantive" };
        if complexity == "governance" {
            thinking = Some(Thinking::Xhigh);
        }
        verify = if kind == "review" { "check" } else { "rubric" };
        reasons.push(format!(
            "{kind} is frontier work{}",
            if kind == "review" {
                ": a reviewer is pinned, never downgraded"
            } else {
                ": the spec-contract rubric, graded by a different model"
            }
        ));
    } else {
        if MECHANICAL_KINDS.contains(&kind) {
            reasons.push(format!("{kind} names code or governance paths: treated as implement"));
        }
        let big = loc > SUBSTANTIVE_LOC;
        let substantive = big || files > 1 || public_api;
        if !gov.is_empty() || one_way {
            executor = Tier::Frontier;
            complexity = "governance";
            verify = "p20-full";
            thinking = Some(Thinking::Xhigh);
            reasons.push(if gov.is_empty() {
                "one-way door: reversal is expensive".to_string()
            } else {
                format!(
                    "governance-class paths: {}",
                    gov.iter().take(4).map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                )
            });
        } else if substantive {
            executor = Tier::Standard;
            complexity = "substantive";
            verify = "p20";
            reasons.push(format!("substantive (>{SUBSTANTIVE_LOC} LOC, multi-file or public API): P20 verifies"));
            if !spec && (big || public_api || code_files > SPEC_FILES) {
                author = Some(Tier::Frontier);
                needs.push("spec");
                reasons.push(
                    "no spec for work this size: a frontier node writes it first, the executor works from it".into(),
                );
            }
        } else {
            executor = Tier::Standard;
            complexity = "bounded";
            reasons.push("bounded change: its done check verifies it".into());
        }
        if repos > 1 {
            author = Some(Tier::Frontier);
            needs.push("decompose");
            reasons.push(format!("{repos} repos: a frontier plan splits it into one node per repo"));
        }
    }

    if let Some(seen) = Tier::of(sig.get("observed_tier"))
        && seen > executor
    {
        reasons.push(format!("an earlier run raised to {}: that is the floor now", seen.as_str()));
        executor = seen;
    }
    let failed = int_or(sig, "failed_attempts", 0)?;
    if failed != 0 {
        let top = if py::truthy(sig.get("allow_apex")) { Tier::Apex } else { TOP_DEFAULT };
        let raised = bump(executor, failed, top);
        reasons.push(format!(
            "failed verification {failed}x, one rung each: {} -> {}",
            executor.as_str(),
            raised.as_str()
        ));
        executor = raised;
    }
    let mut thinking = thinking.unwrap_or(executor.rung().thinking);
    let mode = match sig.get("mode") {
        Some(Value::String(m)) => Some(norm_mode(m)),
        _ => None,
    };
    if let Some(mode) = mode
        && SAVING_MODES.contains(&mode.as_str())
        && complexity != "governance"
        && kind != "review"
        && thinking > Thinking::Low
    {
        let lower = Thinking::ALL[thinking as usize - 1];
        reasons.push(format!("capacity {mode}: thinking {} -> {}; the tier stays", thinking.as_str(), lower.as_str()));
        thinking = lower;
    }
    Ok(Assessment { complexity, author, executor, thinking, verify, needs, reasons })
}

/// How to start a tier: Maestro's `tier`, the Paseo `provider` and `thinking`, the Agent tool alias.
pub fn contract(tier: Tier, thinking: Option<Thinking>) -> Value {
    let r = tier.rung();
    json!({"tier": tier, "provider": r.provider, "thinking": thinking.unwrap_or(r.thinking), "agent": r.agent})
}

/// A place in a graph: a repo name and a normalised path relative to it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ref {
    pub repo: String,
    pub path: String,
}

impl Ref {
    pub fn show(&self, here: &str) -> String {
        if self.repo == here { self.path.clone() } else { format!("{}:{}", self.repo, self.path) }
    }
}

/// `name:path` (a repo the graph names) or a bare path in `here`. Absolute and `..` paths are
/// refused, and so is a path inside another bound repo (`dirs`, name -> realpath, in the graph's order).
pub fn ref_of(raw: &Value, here: &str, names: &[String], dirs: Option<&[(String, String)]>) -> Result<Ref, Fault> {
    let raw = py::str_of(raw);
    let (mut repo, mut path) = (here.to_string(), raw.as_str());
    if let Some((head, tail)) = raw.split_once(':') {
        if !names.iter().any(|n| n == head) {
            let mut sorted: Vec<&str> = names.iter().map(String::as_str).collect();
            sorted.sort_unstable();
            return Err(Fault::Value(format!(
                "{}: {} is not a repo this graph names ({})",
                py::repr_str(&raw),
                py::repr_str(head),
                sorted.join(", ")
            )));
        }
        repo = head.to_string();
        path = tail;
    }
    let norm = py::normpath(path);
    if norm.starts_with('/') || norm == ".." || norm.starts_with("../") {
        return Err(Fault::Value(format!("{}: paths stay inside their repo", py::repr_str(&raw))));
    }
    if let Some(dirs) = dirs
        && let Some((_, mine)) = dirs.iter().find(|(n, _)| *n == repo)
    {
        let full = py::join(mine, &norm);
        for (other, d) in dirs {
            if *other != repo
                && d.starts_with(&format!("{mine}/"))
                && (full == *d || full.starts_with(&format!("{d}/")))
            {
                let rel = if full == *d { ".".to_string() } else { py::normpath(&full[d.len() + 1..]) };
                return Err(Fault::Value(format!(
                    "{} is inside repo {other}: write it as {other}:{rel}",
                    py::repr_str(&raw)
                )));
            }
        }
    }
    Ok(Ref { repo, path: norm })
}

/// A validated node, as `plan --json` prints it (less `input_refs`).
#[derive(Debug, Clone)]
pub struct PlanNode {
    pub id: Value,
    pub repo: String,
    pub kind: String,
    pub tier: Tier,
    pub spawn_default: Value,
    pub verify: Value,
    pub thinking: Value,
    pub inputs: Vec<String>,
    pub input_refs: Vec<Ref>,
    pub depends_on: Vec<Value>,
    pub done: Value,
    pub assessment: Assessment,
}

impl PlanNode {
    /// The node as `plan --json` prints it, with its start contract.
    pub fn to_json(&self) -> Value {
        let mut c = contract(self.tier, None);
        if py::truthy(Some(&self.thinking)) {
            c["thinking"] = self.thinking.clone(); // `thinking or LADDER[tier]`, unchecked, as the Python
        }
        json!({
            "id": self.id, "repo": self.repo, "kind": self.kind, "tier": self.tier,
            "spawn_default": self.spawn_default, "verify": self.verify, "thinking": self.thinking,
            "inputs": self.inputs, "depends_on": self.depends_on, "done": self.done,
            "assessment": self.assessment, "contract": c,
        })
    }
}

/// What `validate` needs from the disk; none checks nothing on disk and uses `human_paths`.
pub struct Disk<'a> {
    /// Whether a Ref exists.
    pub exists: &'a dyn Fn(&Ref) -> bool,
    /// A repo's require_human paths. Its error is reported against the node.
    pub policy_for: &'a dyn Fn(&str) -> Result<Vec<String>, Fault>,
}

/// A loaded value as a map key: `1` and `"1"` are different keys, as in Python.
fn key(v: &Value) -> String {
    format!("{}:{}", type_name(v), py::repr(v))
}

/// Errors in an agent graph (empty when it may be dispatched), and the resolved nodes.
/// `Err` is a fault the Python does not catch.
pub fn validate(
    graph: &Value,
    human_paths: &[String],
    disk: Option<&Disk>,
    dirs: Option<&[(String, String)]>,
    skip: &HashSet<String>,
) -> Result<(Vec<String>, Vec<PlanNode>), Fault> {
    let mut errors: Vec<String> = Vec::new();
    let mut out = Vec::new();
    let get = |k: &str| graph.get(k);
    let raw = iter_or_empty(get("nodes"))?;
    if raw.is_empty() {
        return Ok((vec!["graph has no nodes".into()], vec![]));
    }
    let home = py::or(get("repo"), None).map_or_else(String::new, py::str_of);
    // `set((graph.get("repos") or {}).keys())`: repos is a mapping or nothing.
    let mut names: Vec<String> = match py::or(get("repos"), None) {
        Some(Value::Object(o)) => o.keys().cloned().collect(),
        Some(other) => {
            return Err(Fault::attribute_error(format!("'{}' object has no attribute 'keys'", type_name(other))));
        }
        None => vec![],
    };
    if names.is_empty() {
        names.push(home.clone());
    }
    if !names.contains(&home) {
        let mut sorted = names.clone();
        sorted.sort_unstable();
        return Ok((
            vec![format!("the graph's repo {} is not in its repos ({})", py::repr_str(&home), sorted.join(", "))],
            vec![],
        ));
    }
    // Insertion-ordered, a duplicate id keeping its first position and its last value (a dict).
    let mut order: Vec<(String, Value)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for n in &raw {
        if !n.is_object() {
            errors.push(format!("a node is not a mapping: {}", py::repr(n)));
            continue;
        }
        let nid = match n.get("id") {
            v if py::truthy(v) => v.expect("truthy is present").clone(),
            _ => {
                errors.push("a node has no id".into());
                continue;
            }
        };
        let k = key(&nid);
        if let Some(&i) = index.get(&k) {
            errors.push(format!("{}: duplicate id", py::str_of(&nid)));
            order[i].1 = n.clone();
        } else {
            index.insert(k, order.len());
            order.push((py::str_of(&nid), n.clone()));
        }
    }
    let node = |i: usize| &order[i].1;
    let deps_of = |i: usize| iter_or_empty(node(i).get("depends_on"));
    for (i, (nid, _)) in order.iter().enumerate() {
        for d in deps_of(i)? {
            if !index.contains_key(&key(&d)) {
                errors.push(format!("{nid}: depends on unknown node {}", py::repr(&d)));
            }
        }
    }
    if has_cycle(&order, &index)? {
        errors.push("depends_on has a cycle".into());
    }

    struct Resolved {
        i: usize,
        here: String,
        inputs: Vec<Ref>,
        produces: Vec<Ref>,
        spec: Option<Ref>,
        sig: Map<String, Value>,
    }
    let mut resolved: Vec<Resolved> = Vec::new();
    for (i, (nid, n)) in order.iter().enumerate() {
        let here = py::or(n.get("repo"), None).map_or_else(|| home.clone(), py::str_of);
        if !names.contains(&here) {
            errors.push(format!("{nid}: repo {} is not one the graph names", py::repr_str(&here)));
            continue;
        }
        let sig_v = py::or(n.get("signals"), None);
        let sig = match sig_v {
            None => Some(Map::new()),
            Some(Value::Object(o)) => Some(o.clone()),
            Some(_) => None,
        };
        let lists = [
            ("inputs", n.get("inputs")),
            ("produces", n.get("produces")),
            ("depends_on", n.get("depends_on")),
            ("signals.paths", sig.as_ref().and_then(|s| s.get("paths"))),
        ];
        let bad: Vec<&str> =
            lists.iter().filter(|(_, v)| v.is_some_and(|v| !v.is_null() && !v.is_array())).map(|(k, _)| *k).collect();
        let Some(mut sig) = sig.filter(|_| bad.is_empty()) else {
            errors.push(if bad.is_empty() {
                format!("{nid}: signals must be a mapping")
            } else {
                format!("{nid}: {} must be a list", bad.join(", "))
            });
            continue;
        };
        let attempt = (|| -> Result<Resolved, Fault> {
            let spec = match sig.get("spec") {
                v if py::truthy(v) => Some(ref_of(v.expect("truthy is present"), &here, &names, dirs)?),
                _ => None,
            };
            let own = iter_or_empty(sig.get("paths"))?
                .iter()
                .map(|x| ref_of(x, &here, &names, dirs))
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(f) = own.iter().find(|r| r.repo != here) {
                return Err(Fault::Value(format!(
                    "{} is not in this node's repo {}: a node's paths are its own repo's; split the node",
                    f.show(&here),
                    py::repr_str(&here)
                )));
            }
            sig.insert("paths".into(), Value::Array(own.iter().map(|r| Value::String(r.path.clone())).collect()));
            let refs = |k: &str| -> Result<Vec<Ref>, Fault> {
                iter_or_empty(n.get(k))?.iter().map(|x| ref_of(x, &here, &names, dirs)).collect()
            };
            Ok(Resolved {
                i,
                here: here.clone(),
                inputs: refs("inputs")?,
                produces: refs("produces")?,
                spec,
                sig: sig.clone(),
            })
        })();
        match attempt {
            Ok(r) => resolved.push(r),
            Err(Fault::Value(e)) => errors.push(format!("{nid}: {e}")),
            Err(crash) => return Err(crash),
        }
    }
    let mut produced_by: HashMap<Ref, usize> = HashMap::new();
    for r in &resolved {
        for p in &r.produces {
            if let Some(&other) = produced_by.get(p) {
                errors.push(format!("{}: {} is also produced by {}", order[r.i].0, p.show(&home), order[other].0));
            }
            produced_by.insert(p.clone(), r.i);
        }
    }
    let upstream = |start: usize| -> Result<HashSet<usize>, Fault> {
        let mut seen = HashSet::new();
        let mut stack = vec![start];
        while let Some(i) = stack.pop() {
            for d in deps_of(i)? {
                if let Some(&j) = index.get(&key(&d))
                    && seen.insert(j)
                {
                    stack.push(j);
                }
            }
        }
        Ok(seen)
    };

    for r in resolved {
        let (nid, n) = (&order[r.i].0, &order[r.i].1);
        let here = r.here.as_str();
        let done_blank = |n: &Value| -> Result<bool, Fault> {
            match py::or(n.get("done"), None) {
                None => Ok(true),
                Some(Value::String(s)) => Ok(s.trim().is_empty()),
                Some(other) => {
                    Err(Fault::attribute_error(format!("'{}' object has no attribute 'strip'", type_name(other))))
                }
            }
        };
        if skip.contains(nid) {
            if done_blank(n)? {
                errors.push(format!("{nid}: no done check"));
            }
            continue;
        }
        let mut sig = r.sig;
        let kind = py::or(py::or(n.get("kind"), sig.get("kind")), None).cloned().unwrap_or(json!("implement"));
        sig.insert("kind".into(), kind.clone());
        let mut spec = r.spec;
        if let Some(s) = &spec {
            if !is_spec_path(&s.path) {
                errors.push(format!("{nid}: spec {} is not under {}", s.show(here), SPEC_DIRS.join(", ")));
            } else if disk.is_some_and(|d| !(d.exists)(s)) {
                errors.push(format!("{nid}: spec {} does not exist", s.show(here)));
            }
        } else {
            let ups = upstream(r.i)?;
            spec = r
                .inputs
                .iter()
                .find(|x| {
                    is_spec_path(&x.path)
                        && match produced_by.get(x) {
                            Some(&p) => ups.contains(&p) && node(p).get("kind") == Some(&json!("design")),
                            None => disk.is_some_and(|d| (d.exists)(x)),
                        }
                })
                .cloned();
        }
        sig.insert("spec".into(), spec.as_ref().map_or(Value::Null, |s| Value::String(s.path.clone())));
        let a = match disk
            .map_or_else(|| Ok(human_paths.to_vec()), |d| (d.policy_for)(here))
            .and_then(|h| assess(&sig, &h))
        {
            Ok(a) => a,
            Err(Fault::Value(e)) => {
                errors.push(format!("{nid}: {e}"));
                continue;
            }
            Err(crash) => return Err(crash),
        };
        let tier_v = py::or(n.get("tier"), None).cloned().unwrap_or(json!(a.executor));
        let Some(tier) = Tier::of(Some(&tier_v)) else {
            errors.push(format!("{nid}: unknown tier {}", py::repr(&tier_v)));
            continue;
        };
        let kind_s = py::str_of(&kind);
        if kind == json!("review") && tier < Tier::Frontier {
            errors.push(format!("{nid}: a reviewer runs at frontier or above, never {}", tier.as_str()));
        } else if tier < a.executor {
            errors.push(format!(
                "{nid}: tier {} is below its floor {} ({})",
                tier.as_str(),
                a.executor.as_str(),
                a.reasons.join("; ")
            ));
        }
        let has_reason = py::truthy(n.get("reason"));
        if tier > a.executor && !has_reason {
            errors.push(format!(
                "{nid}: tier {} is above its floor {}: say why in `reason`",
                tier.as_str(),
                a.executor.as_str()
            ));
        }
        if tier == Tier::Apex && !has_reason {
            errors.push(format!("{nid}: apex needs a `reason`"));
        }
        if a.needs.contains(&"spec") {
            errors.push(format!(
                "{nid}: substantive with no spec: add an upstream design node that produces one under {}, and list it in inputs",
                SPEC_DIRS[0]
            ));
        }
        if a.needs.contains(&"decompose") {
            let repos = sig.get("repos").map_or_else(|| "None".to_string(), py::str_of);
            errors.push(format!("{nid}: spans {repos} repos: split it into one node per repo"));
        }
        if done_blank(n)? {
            errors.push(format!("{nid}: no done check"));
        }
        let verify_v = py::or(n.get("verify"), None).cloned().unwrap_or(json!(a.verify));
        let strength = |v: &Value| VERIFY.iter().find(|(k, _)| Some(*k) == v.as_str()).map(|(_, s)| *s);
        match strength(&verify_v) {
            None => errors.push(format!("{nid}: unknown verify {}", py::repr(&verify_v))),
            Some(s) if s < strength(&json!(a.verify)).expect("assess names a known verify") => {
                errors.push(format!("{nid}: verify {} is weaker than required {}", py::str_of(&verify_v), a.verify))
            }
            Some(_) => {}
        }
        let spawn = py::or(n.get("spawn_default"), None).cloned().unwrap_or(json!(tier));
        if Tier::of(Some(&spawn)).is_none() {
            errors.push(format!("{nid}: unknown spawn_default {}", py::repr(&spawn)));
        }
        // A raised tier thinks at least as hard as the work was assessed to need.
        let default_thinking = if tier == a.executor { a.thinking } else { a.thinking.max(tier.rung().thinking) };
        let thinking_v = py::or(n.get("thinking"), None).cloned().unwrap_or(json!(default_thinking));
        match thinking_v.as_str().and_then(Thinking::parse) {
            None => errors.push(format!(
                "{nid}: unknown thinking {}; one of {}",
                py::repr(&thinking_v),
                Thinking::ALL.map(Thinking::as_str).join(", ")
            )),
            Some(t) if (a.complexity == "governance" || kind_s == "review") && t < a.thinking => errors.push(format!(
                "{nid}: thinking {} is below {}, which governance and reviews keep",
                t.as_str(),
                a.thinking.as_str()
            )),
            Some(_) => {}
        }
        out.push(PlanNode {
            id: n["id"].clone(),
            repo: here.to_string(),
            kind: kind_s,
            tier,
            spawn_default: spawn,
            verify: verify_v,
            thinking: thinking_v,
            inputs: r.inputs.iter().map(|x| x.show(here)).collect(),
            input_refs: r.inputs,
            depends_on: iter_or_empty(n.get("depends_on"))?,
            done: n.get("done").cloned().unwrap_or(Value::Null),
            assessment: a,
        });
    }
    Ok((errors, out))
}

/// Depth-first, in node order, as the Python's `_cycle`.
fn has_cycle(order: &[(String, Value)], index: &HashMap<String, usize>) -> Result<bool, Fault> {
    let mut state = vec![0u8; order.len()];
    fn visit(
        i: usize,
        order: &[(String, Value)],
        index: &HashMap<String, usize>,
        state: &mut [u8],
    ) -> Result<bool, Fault> {
        match state[i] {
            1 => return Ok(true),
            2 => return Ok(false),
            _ => {}
        }
        state[i] = 1;
        for d in iter_or_empty(order[i].1.get("depends_on"))? {
            if let Some(&j) = index.get(&key(&d))
                && visit(j, order, index, state)?
            {
                return Ok(true);
            }
        }
        state[i] = 2;
        Ok(false)
    }
    for i in 0..order.len() {
        if visit(i, order, index, &mut state)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// What `brief_block` prints for one node: its tier, children's tier, thinking, verify, done, inputs.
pub struct BriefNode<'a> {
    pub tier: Tier,
    pub spawn_default: Tier,
    pub thinking: &'a str,
    pub verify: &'a str,
    pub done: &'a str,
    /// Each input as shown, with its Ref when the graph resolved one.
    pub inputs: Vec<(String, Option<Ref>)>,
    pub repo: Option<&'a str>,
    /// Repo name -> directory, to say where a foreign input lives.
    pub repo_dirs: &'a [(String, String)],
}

/// The contract every delegated brief carries. A child copies it into the briefs it writes.
pub fn brief_block(node: &BriefNode, ticket: Option<&str>) -> String {
    let (tier, spawn) = (node.tier, node.spawn_default);
    let mut lines = vec![
        format!("## Delegation contract{}", ticket.map_or_else(String::new, |t| format!(" ({t})"))),
        format!(
            "- Your tier: **{}** ({}, thinking {}). It is fixed for this run.",
            tier.as_str(),
            tier.model(),
            node.thinking
        ),
    ];
    if !node.inputs.is_empty() {
        let shown: Vec<String> = if node.inputs.iter().any(|(_, r)| r.is_some()) {
            node.inputs
                .iter()
                .filter_map(|(p, r)| {
                    let r = r.as_ref()?;
                    let foreign = node.repo != Some(r.repo.as_str());
                    let at = node.repo_dirs.iter().find(|(n, _)| *n == r.repo).filter(|_| foreign);
                    Some(format!("`{p}`{}", at.map_or_else(String::new, |(n, d)| format!(" (repo `{n}` at `{d}`)"))))
                })
                .collect()
        } else {
            node.inputs.iter().map(|(p, _)| format!("`{p}`")).collect()
        };
        lines.push(format!("- Read first, and work from it: {}.", shown.join(", ")));
    }
    lines.extend([
        format!(
            "- Done means: `{}`. Verification: **{}**{}",
            node.done,
            node.verify,
            if node.verify != "check" { ", by a different model than you." } else { ", the check's exit code." }
        ),
        format!(
            "- Agents you start default to **{}**, with the model passed explicitly (Agent tool `model: {}`; Paseo \
             `model` and `thinkingOptionId`; Maestro `--tier`); mechanical steps go to mechanical (`model: haiku`). \
             Copy this block into every brief you write, with that child's own tier.",
            spawn.as_str(),
            spawn.rung().agent
        ),
        "- Raise when the work outgrows the assessment: a design call your inputs do not settle, a governance path, \
         a one-way door, a failed verification. Start a higher-tier agent for the part that outgrew you and give it \
         what you learned; you keep the rest. Say it in your final message, one line per raise: `TIER-RAISE: <from> \
         -> <to>: <why>`. Raises are measured from transcripts, and the orchestrator re-plans on them; an unstated \
         raise reads as a mis-assessment nobody explained."
            .into(),
        "- Reviewers are pinned: a P20 reviewer, and an Explore run that serves a review, runs at frontier or \
         cross-vendor. Never downgrade a reviewer."
            .into(),
        "- Your tier changes nothing about what you may touch: scope, merge rule and gates are this brief's.".into(),
    ]);
    lines.join("\n")
}

/// The `path_touched` of every require_human rule in a loaded `.control/policy.yaml`.
/// `Fault::Value` means the policy is unreadable (classification must not go on without it);
/// `Fault::Crash` is the Python's uncaught error on rules that are not iterable.
pub fn require_human_paths(policy: &Value) -> Result<Vec<String>, Fault> {
    let empty = json!({});
    let data = py::or(Some(policy), Some(&empty)).expect("or has a fallback");
    let Value::Object(data) = data else {
        return Err(Fault::Value(format!("'{}' object has no attribute 'get'", type_name(data))));
    };
    let am = match py::or(data.get("auto_merge"), None) {
        None => return Ok(vec![]),
        Some(Value::Object(o)) => o,
        Some(other) => return Err(Fault::Value(format!("'{}' object has no attribute 'get'", type_name(other)))),
    };
    Ok(iter_or_empty(am.get("rules"))?
        .iter()
        .filter(|r| {
            r.is_object() && r.get("action") == Some(&json!("require_human")) && py::truthy(r.get("path_touched"))
        })
        .map(|r| py::str_of(&r["path_touched"]))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(v: Value) -> Map<String, Value> {
        v.as_object().expect("an object").clone()
    }

    #[test]
    fn bump_is_capped_and_never_lowers() {
        assert_eq!(bump(Tier::Standard, 1, TOP_DEFAULT), Tier::Frontier);
        assert_eq!(bump(Tier::Standard, 5, TOP_DEFAULT), Tier::Frontier);
        assert_eq!(bump(Tier::Standard, 5, Tier::Apex), Tier::Apex);
        assert_eq!(bump(Tier::Apex, 1, TOP_DEFAULT), Tier::Apex);
        assert_eq!(bump(Tier::Standard, -3, TOP_DEFAULT), Tier::Standard);
    }

    #[test]
    fn globs_stay_inside_one_segment() {
        assert!(rule_matches("scripts/*-hook.sh", "scripts/a-hook.sh"));
        assert!(!rule_matches("scripts/*-hook.sh", "scripts/x/a-hook.sh"));
        assert!(rule_matches("a?c", "abc"));
        assert!(!rule_matches("a?c", "a/c"));
        assert!(rule_matches(".control/", "x/.control/policy.yaml"));
    }

    #[test]
    fn a_failed_verification_moves_one_rung() {
        let a = assess(&sig(json!({"kind": "implement", "paths": ["a.rs"], "failed_attempts": 1})), &[]).unwrap();
        assert_eq!((a.executor, a.complexity), (Tier::Frontier, "bounded"));
    }

    #[test]
    fn plugin_tiers_agree() {
        assert_eq!(tier_of_model("claude-opus-5-5-20261001"), Some(Tier::Frontier));
        assert_eq!(tier_of_model("gpt-5.6-terra"), None);
        assert_eq!(model_of_provider("claude/claude-sonnet-5-5"), Some("claude-sonnet-5-5"));
        assert_eq!(model_of_provider("claude/"), None);
        assert_eq!(model_of_provider("claude"), None);
    }
}
