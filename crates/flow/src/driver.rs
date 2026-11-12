//! A flow's driver skill (flow kernel spec, "L2: procedure (skills)"; BRO-3636): one thin
//! `SKILL.md`, in the Agent Skills format, generated from the flow's compiled IR. It tells an
//! agent to call `flow next`, do each enabled node with the node's own skill (launching an
//! `agent:<name>` performer through the harness's own subagent), call `flow complete`, and stop
//! when `flow status` says `terminal` (`kernel::Status::terminal`). The kernel decides when to
//! stop: the skill's loop reads that field, or finds nothing enabled for the driver, and never
//! judges node states itself. Before it acts on an instance, the loop checks that `status.ir_hash`
//! is the hash the skill was generated from, so a resumed instance of another definition is never
//! driven by this one's nodes. A human node's `flow ask` is called for a filed ask too, since only
//! that verb reads the owner's answer from main. A repeat that awaits its `until` verdict is
//! retried through `flow claim`, which judges every pending verdict first (`kernel::write`), at
//! most [`VERDICT_RETRIES`] times.
//!
//! The loop, the verbs and what to do for each class of node are this module's fixed text. The
//! steps (which nodes, in which order, what each waits on and when it applies), each performer's
//! launch and the Codex brief come from the IR and the agent sources ([`crate::agents`]), so the
//! order cannot drift from the definition: [`generate`] is a function of the compiled flow and the
//! agents this crate was built with, and a committed skill is checked against it.
//!
//! **The order** ([`steps`]). A node is at the step after the latest step of everything it waits
//! on: the nodes it needs, the node its switch reads, and every node of a repeat it needs or whose
//! exhaustion it handles. A node it needs or switches on inside a repeat it is not in stands for
//! that repeat's whole body. Nodes at one step do not wait on each other and are listed by id. `flow
//! next` stays the authority on what is enabled now; the steps are the order it will enable them in.
//!
//! The skill is named `flow-<flow>-v<version>` ([`skill_name`]), with every character the Agent
//! Skills spec does not allow in a name made a dash. Its description is at most
//! [`MAX_DESCRIPTION`] characters.

use crate::agents::{self, LaunchSpec};
use crate::kernel::CompiledFlow;
use crate::kernel::ir::{Class, Ir, Need, Node};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// Why a flow has no driver skill.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DriverError {
    /// The flow's name and version make no skill name the Agent Skills spec allows.
    #[error("Flow {flow} makes no skill name: {detail}.")]
    Name { flow: String, detail: String },
    /// A performer names an agent this build has no source for.
    #[error("Node {node}'s performer is agent:{agent}, which has no source agents/{agent}.agent.yaml.")]
    UnknownAgent { node: String, agent: String },
}

impl DriverError {
    pub fn code(&self) -> &'static str {
        match self {
            DriverError::Name { .. } => "skill_name_invalid",
            DriverError::UnknownAgent { .. } => "unknown_agent",
        }
    }
}

/// `flow-<flow>-v<version>`, lower case, every other character a dash, no dash doubled or at
/// either end, at most 64 bytes.
pub fn skill_name(flow: &CompiledFlow) -> Result<String, DriverError> {
    let ir = flow.ir();
    let raw = format!("flow-{}-v{}", ir.flow, ir.version).to_ascii_lowercase();
    let mut name = String::new();
    for c in raw.chars() {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() { c } else { '-' };
        if !(c == '-' && name.ends_with('-')) {
            name.push(c);
        }
    }
    let name = name.trim_matches('-').to_string();
    if name.len() > agents::MAX_NAME {
        let detail = format!("{name} is longer than {} bytes", agents::MAX_NAME);
        return Err(DriverError::Name { flow: flow.name(), detail });
    }
    Ok(name)
}

/// What a node waits on before it can run: its needs, its switch's subject, and the body of every
/// repeat it needs. A node it needs or switches on inside a repeat it is not in stands for that
/// repeat's whole body, as the kernel reads it (`ir::validate`'s fan-in).
fn waits_on<'a>(ir: &'a Ir, id: &str, node: &'a Node) -> Vec<&'a str> {
    let body = |r: &'a str| ir.repeats.get(r).into_iter().flat_map(|r| r.body.iter().map(String::as_str));
    let repeat_of =
        |n: &str| ir.repeats.iter().find(|(_, r)| r.body.iter().any(|b| b == n)).map(|(rid, _)| rid.as_str());
    let mut on = Vec::new();
    for need in &node.needs {
        match need {
            Need::Node { node } => on.push(node.as_str()),
            Need::Repeat { repeat } | Need::Exhausted { repeat } => on.extend(body(repeat)),
        }
    }
    on.extend(node.switch.as_ref().map(|c| c.on.as_str()));
    let mut out = Vec::new();
    for d in on {
        match repeat_of(d) {
            Some(r) if repeat_of(id) != Some(r) => out.extend(body(r)),
            _ => out.push(d),
        }
    }
    out
}

/// The nodes by step (see the module docs): each step's ids in order, the first step first.
pub fn steps(flow: &CompiledFlow) -> Vec<Vec<String>> {
    fn at<'a>(ir: &'a Ir, id: &'a str, memo: &mut BTreeMap<&'a str, usize>) -> usize {
        if let Some(&s) = memo.get(id) {
            return s;
        }
        // A compiled IR has no cycle and no chain past `ir::MAX_DEPTH`, so this ends.
        let Some(node) = ir.nodes.get(id) else { return 0 };
        let step = waits_on(ir, id, node).into_iter().map(|d| at(ir, d, memo) + 1).max().unwrap_or(0);
        memo.insert(id, step);
        step
    }
    let ir = flow.ir();
    let mut memo = BTreeMap::new();
    let mut out: Vec<Vec<String>> = Vec::new();
    for id in ir.nodes.keys() {
        let step = at(ir, id, &mut memo);
        if out.len() <= step {
            out.resize(step + 1, Vec::new());
        }
        out[step].push(id.clone());
    }
    out.retain(|ids| !ids.is_empty());
    out
}

fn code(s: &str) -> String {
    if s.contains('`') { format!("`` {s} ``") } else { format!("`{s}`") }
}

fn codes<'a>(items: impl IntoIterator<Item = &'a str>) -> String {
    let items: Vec<String> = items.into_iter().map(code).collect();
    items.join(", ")
}

/// The agents the flow's performers name, by name.
fn agents_of(flow: &CompiledFlow) -> Result<BTreeMap<String, &'static agents::Agent>, DriverError> {
    let mut out = BTreeMap::new();
    for (id, node) in &flow.ir().nodes {
        let Some(launch) = node.class.launch() else { continue };
        match agents::launch_spec(launch) {
            Some(LaunchSpec::Agent(spec)) => {
                if let Some(agent) = agents::find(&spec.agent) {
                    out.insert(spec.agent, agent);
                }
            }
            Some(LaunchSpec::UnknownAgent { agent, .. }) => {
                return Err(DriverError::UnknownAgent { node: id.clone(), agent });
            }
            None => {}
        }
    }
    Ok(out)
}

fn node_section(out: &mut String, ir: &Ir, id: &str, node: &Node) {
    let _ = writeln!(out, "#### {} ({})\n", code(id), node.class.name());
    let waits: BTreeSet<String> = node
        .needs
        .iter()
        .map(|need| match need {
            Need::Node { node } => code(node),
            Need::Repeat { repeat } => format!("repeat {} to finish", code(repeat)),
            Need::Exhausted { repeat } => format!("repeat {} to run out (its on_exhausted route)", code(repeat)),
        })
        .collect();
    let mut waits: Vec<String> = waits.into_iter().collect();
    if let Some(case) = &node.switch
        && !node.needs.contains(&Need::Node { node: case.on.clone() })
    {
        waits.push(format!("{} (its switch reads it)", code(&case.on)));
    }
    let waits = if waits.is_empty() { "nothing".to_string() } else { waits.join(", ") };
    let _ = writeln!(out, "- Waits on: {waits}.");
    if let Some(case) = &node.switch {
        let _ = writeln!(
            out,
            "- Applies only when {} is {}; otherwise it never runs, nor does anything that waits on it.",
            code(&case.on),
            code(&case.value)
        );
    }
    if let Some((rid, r)) = ir.repeats.iter().find(|(_, r)| r.body.iter().any(|b| b == id)) {
        let _ =
            writeln!(out, "- In repeat {}: it runs once per iteration, at most {} (see Repeats).", code(rid), r.max);
    }
    if let Some(attempts) = node.retry {
        let _ = writeln!(out, "- At most {attempts} attempt(s); then it is failed.");
    }
    if let Some(secs) = node.cancel_after {
        let _ = writeln!(out, "- An attempt ends {secs} s after its claim, heartbeats or not.");
    }
    if let Some(wait) = node.class.wait() {
        let _ = writeln!(
            out,
            "- Waits on outside work ({}): heartbeat its claim at least every {} s.",
            wait.external, wait.heartbeat
        );
    }
    let performer = node.class.launch().and_then(|l| {
        let skill = l.skill.as_deref().map(|s| format!(", with the skill {}", code(s))).unwrap_or_default();
        match (&l.performer, &l.skill) {
            (Some(p), _) => Some(format!("- Performer: {}{skill}.", code(p))),
            (None, Some(s)) => Some(format!("- Skill: {}.", code(s))),
            (None, None) => None,
        }
    });
    if let Some(line) = performer {
        let _ = writeln!(out, "{line}");
    }
    let agent = node
        .class
        .launch()
        .and_then(|l| l.performer.as_deref())
        .and_then(agents::performer_agent)
        .map(|name| format!("launch the {} agent (see Agents) with the node's task", code(name)));
    let doer = agent.unwrap_or_else(|| "do the work, with its skill if it names one".to_string());
    let todo = match &node.class {
        Class::Pure { .. } => {
            let place = match &node.snapshot {
                Some(commit) => {
                    format!(" in a snapshot of the commit {} names, never in the working tree", code(commit))
                }
                None => String::new(),
            };
            format!(
                "claim it, then complete it with no outputs. The kernel runs its command{place}. A refusal `verify_failed` means the command failed, and a pure command fails the same way again: stop and report it."
            )
        }
        Class::Observe { fresh_for, exit_map, .. } => {
            let values: Vec<String> =
                exit_map.iter().map(|e| format!("exit {} is {}", e.exit, code(&e.value))).collect();
            let fresh = match fresh_for {
                Some(secs) => format!(
                    " It is fresh for {secs} s; once stale, `flow next` enables it again, and everything after it waits for the new value."
                ),
                None => String::new(),
            };
            format!(
                "claim it, then complete it with no outputs. The kernel runs its command and records its value: {}; any other exit fails it.{fresh}",
                values.join(", ")
            )
        }
        Class::Work { .. } => format!(
            "claim it; {doer}; then complete it with what that returns as its outputs. The kernel runs the node's verify on them. A refused completion keeps the claim while its lease holds: repair from the refusal's reason and stderr, and complete again with the same claim token."
        ),
        Class::Choice { edges, .. } => format!(
            "claim it; {doer}, to pick one of its edges ({}); then choose that edge. The kernel takes an edge only when its guard holds.",
            codes(edges.keys().map(String::as_str))
        ),
        Class::Effect { .. } => format!(
            "claim it; {doer} if it has a performer; then complete it. The kernel checks its preconditions, runs it at most once in an iteration and, after a run with no receipt, runs its reconcile instead."
        ),
        Class::Human { ask } => format!(
            "nothing to claim. File its ask with `flow ask` (the first call takes `--ledger` and `--ask`), then call `flow ask` again until it says answered. The owner answers {}; never answer it yourself.",
            code(ask.trim())
        ),
    };
    let _ = writeln!(out, "- Do: {todo}\n");
}

fn agent_section(out: &mut String, agent: &agents::Agent) {
    let spec = agent.launch(None);
    let _ = writeln!(out, "### {}\n", code(&agent.name));
    let _ = writeln!(out, "{}\n", agent.description.trim());
    let _ = writeln!(out, "Its source is `{}`. Launch it through your harness:\n", agent.source);
    let subagent = |n: &agents::NativeLaunch| match (&n.subagent_type, &n.agent_name) {
        (Some(t), _) => format!(" with `subagent_type: \"{t}\"`"),
        (None, Some(a)) => format!(" with `agent_name: \"{a}\"` and the node's task as `prompt`"),
        (None, None) => String::new(),
    };
    let (c, g, x) = (&spec.claude, &spec.gemini, &spec.grok);
    let _ = writeln!(out, "- Claude Code: the `{}` tool{} (`{}`).", c.tool, subagent(c), c.file);
    let _ = writeln!(out, "- Gemini CLI: the `{}` tool{} (`{}`).", g.tool, subagent(g), g.file);
    let _ = writeln!(out, "- Grok: the `{}` tool{} (`{}`).", x.tool, subagent(x), x.file);
    let _ = writeln!(
        out,
        "- Codex: the `{}` tool with these arguments, the node's task appended to `message` (`task_name` only where your `spawn_agent` takes one):\n",
        spec.codex.tool
    );
    let brief = serde_json::json!({
        "task_name": spec.codex.task_name,
        "model": spec.codex.model,
        "message": spec.codex.message,
    });
    let brief = serde_json::to_string_pretty(&brief).unwrap_or_default();
    let _ = writeln!(out, "```json\n{brief}\n```\n");
}

/// The most claims the driver skill makes to retry one repeat's `until` verdict before it stops
/// and reports. An `until` is jq over the same input each try, so an error repeats; only a
/// timeout under load (the kernel's 1 s guard timeout) can pass on a later one, and three tries
/// bound that wait to a few seconds of evaluation.
pub const VERDICT_RETRIES: u32 = 3;

/// The most characters a skill's description may have (the Agent Skills spec).
pub const MAX_DESCRIPTION: usize = 1024;

/// The skill's description, naming the flow; a flow name that would carry it past
/// [`MAX_DESCRIPTION`] is named by the skill's own name instead.
fn description(flow_name: &str, skill: &str) -> String {
    let say = |name: &str| {
        format!(
            "Drives an instance of the Maestro flow {name} through the flow kernel until its status is terminal: flow next, each enabled node with its own skill (agent performers through the harness's own subagents), flow complete. Generated from the flow's compiled IR; use it to start or resume a {name} instance."
        )
    };
    let full = say(flow_name);
    if full.chars().count() > MAX_DESCRIPTION { say(skill) } else { full }
}

/// The driver skill of `flow`: its `SKILL.md`, frontmatter included.
pub fn generate(flow: &CompiledFlow) -> Result<String, DriverError> {
    let ir = flow.ir();
    // A flow's name is the definition's text: a control character in it would end a line of
    // the frontmatter or a heading, so each is shown as a space.
    let flow_name: String = flow.name().chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let (name, hash) = (skill_name(flow)?, flow.hash());
    let agents = agents_of(flow)?;
    let description = description(&flow_name, &name);
    let mut out = String::new();
    let _ = writeln!(out, "---");
    let _ = writeln!(out, "# Generated by maestro flow driver-skill from {flow_name} ({hash}). Do not edit it.");
    let _ = writeln!(out, "name: {}", serde_json::Value::from(name.as_str()));
    let _ = writeln!(out, "description: {}", serde_json::Value::from(description));
    let _ = writeln!(out, "---\n");
    let _ = writeln!(out, "# Drive {flow_name}\n");
    let _ = writeln!(
        out,
        "Generated by `maestro flow driver-skill` from the compiled IR of {flow_name} (`{hash}`). Do not edit it: change the definition and generate it again. It says only in what order to drive the flow. The kernel decides what may run, and each node's own skill says how to do it well.\n"
    );
    let _ = writeln!(
        out,
        "Every verb is a `maestro flow` subcommand with `--json`, and the MCP tool `maestro_flow_<verb>` takes the same arguments as JSON. A write acts for your verified session, named by `--session` (or `MAESTRO_SESSION`) on the CLI and `session` over MCP. The CLI reads your claim from the environment: a Direct launch token (`MAESTRO_DIRECT_TOKEN` with `MAESTRO_CALLER_CWD`) or a bridge credential (`MAESTRO_BRIDGE_CREDENTIAL`). Over MCP, pass it in each write's arguments: `cwd` and `token` for a Direct session, `credential` for a T3 or Paseo session, and `node` for the tailnet node your session runs on.\n"
    );

    let _ = writeln!(out, "## Start or resume\n");
    let required: Vec<&str> = ir.params.iter().filter(|(_, r)| **r).map(|(p, _)| p.as_str()).collect();
    let optional: Vec<&str> = ir.params.iter().filter(|(_, r)| !**r).map(|(p, _)| p.as_str()).collect();
    let params = match (required.is_empty(), optional.is_empty()) {
        (true, true) => "none".to_string(),
        (false, true) => format!("{} (required)", codes(required)),
        (true, false) => format!("{} (optional)", codes(optional)),
        (false, false) => format!("{} (required); {} (optional)", codes(required), codes(optional)),
    };
    let _ = writeln!(out, "- Params: {params}.");
    let _ = writeln!(
        out,
        "- Start on the CLI: `maestro flow start --definition <path to the {flow_name} definition file> --params '<json>'`. `--definition` is a file path (`-` reads the YAML from stdin)."
    );
    let _ = writeln!(
        out,
        "- Start over MCP: `maestro_flow_start` with `definition` holding the definition's YAML text itself, never a path, and `params` as a JSON object."
    );
    let _ = writeln!(
        out,
        "- Either way, the `ir_hash` it returns must be `{hash}`; another hash is another definition, which this skill does not drive."
    );
    let _ = writeln!(out, "- Resume: keep the instance id `flow start` returned; every verb below takes it.\n");

    let _ = writeln!(out, "## The loop\n");
    let _ = writeln!(
        out,
        "Whether to stop is decided by the kernel (`terminal`, in step 1 and step 4), never by reading node states yourself.\n"
    );
    let _ = writeln!(
        out,
        "1. Run `maestro flow status --instance <I> --json` (`maestro_flow_status`). First, `status.ir_hash` must be `{hash}`: an instance whose hash is another runs another definition, so stop and report it, and apply none of this skill's nodes to it. Then, if `status.terminal` is `true`, stop: report each node's state and the accepting spine, `flow spine --instance <I>` (`maestro_flow_spine`). The flow is done; a node blocked by a branch not taken never runs, and that is not a failure."
    );
    let _ = writeln!(
        out,
        "2. Run `maestro flow next --instance <I> --json` (`maestro_flow_next`). Do each node in `next.enabled` as The nodes below says, in any order: `flow claim --instance <I> --node-id <N>` (`maestro_flow_claim`) gives a `claim_token`; finish with `flow complete --claim-token <T>` (`maestro_flow_complete`, `--outputs '<json>'` for a performer's work) or, for a choice, `flow choose --claim-token <T> --edge <E>` (`maestro_flow_choose`). Heartbeat a long claim with `flow heartbeat --claim-token <T>` (`maestro_flow_heartbeat`). An enabled node's `launch_spec` of `kind: agent` says how your harness launches its agent; one of `kind: unknown_agent` cannot be launched, so stop and report it."
    );
    let _ = writeln!(
        out,
        "3. For each `relay` entry `flow next` returned, run `flow ask --instance <I> --node-id <N>` (`maestro_flow_ask`). An `unfiled` entry needs `--ledger` and `--ask` the first time, to file its ask. An `owner` or `answer_waiting_to_land` entry needs nothing more: only `flow ask` reads main, so it is what takes an answer that has landed there. `state: answered` means the node is done; `state: waiting` means the owner has not answered on main yet."
    );
    let _ = writeln!(out, "4. Then, in this order:");
    let _ = writeln!(out, "   - If `next.terminal` is `true`, the flow is done: stop and report, as step 1 says.");
    let _ = writeln!(
        out,
        "   - If a repeat in `status.repeats` has `awaiting_verdict: true`, its `until` could not be judged when its body settled (an error or a timeout). Retry it with `flow claim --instance <I> --node-id <the first node of its body, as Repeats lists it>`: a claim judges every pending verdict before anything else. A `claim_token` means the verdict moved the repeat on: do that node as step 2 says, then go back to 1. A refusal: go back to 1. After {VERDICT_RETRIES} such claims with the repeat still awaiting its verdict at the same iteration, stop and report: its `until` fails over that input every time. ({VERDICT_RETRIES} is this skill's bound, `maestro_flow::driver::VERDICT_RETRIES`: an `until` is jq over the same input each time, so an error repeats, and only a timeout under load, at the kernel's 1 s guard timeout, can pass on a later try.)"
    );
    let _ = writeln!(
        out,
        "   - If `next.enabled` was empty and step 3 filed no ask and took no answer, nothing is yours to do now: the instance waits on someone else (the owner's answer to a `relay` ask, or another claim's lease). Stop and report what `relay` and `next.pending` name; the instance stays open."
    );
    let _ = writeln!(out, "   - Otherwise, go back to 1.\n");
    let _ = writeln!(
        out,
        "A refusal is a result, not an error: read its `code`. After `lease_expired` or `cancelled`, claim the node again; after `not_enabled`, go back to 1. A pure node's `verify_failed` means its command failed, and a pure command fails the same way again: stop and report it, never retry it. A human node's answer comes from the owner through the asks ledger, never from you.\n"
    );

    let _ = writeln!(out, "## The nodes\n");
    let _ = writeln!(
        out,
        "In the order the flow enables them. A node comes after everything it waits on; the nodes of one step do not wait on each other.\n"
    );
    for (i, ids) in steps(flow).iter().enumerate() {
        let _ = writeln!(out, "### Step {}: {}\n", i + 1, codes(ids.iter().map(String::as_str)));
        for id in ids {
            if let Some(node) = ir.nodes.get(id) {
                node_section(&mut out, ir, id, node);
            }
        }
    }

    if !ir.repeats.is_empty() {
        let _ = writeln!(out, "## Repeats\n");
        for (rid, r) in &ir.repeats {
            let until = match &r.until {
                Some(guard) => {
                    format!(", until {} holds (the kernel judges it when an iteration settles)", code(guard))
                }
                None => String::new(),
            };
            let exhausted = match &r.on_exhausted {
                Some(node) => format!("; when it runs out, {} runs", code(node)),
                None => String::new(),
            };
            let body = codes(r.body.iter().map(String::as_str));
            let _ = writeln!(out, "- {}: {body}, at most {} iterations{until}{exhausted}.", code(rid), r.max);
        }
        let _ = writeln!(out);
    }

    if !agents.is_empty() {
        let _ = writeln!(out, "## Agents\n");
        let params =
            if ir.params.is_empty() { "none".to_string() } else { codes(ir.params.keys().map(String::as_str)) };
        let _ = writeln!(
            out,
            "Give the agent the node's task: the flow ({flow_name}), the instance, the node and the instance's params ({params}). It returns the node's outputs; you claim and complete the node, never the agent.\n"
        );
        for agent in agents.values() {
            agent_section(&mut out, agent);
        }
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    Ok(out)
}
