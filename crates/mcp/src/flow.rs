//! The flow kernel's verbs as tools (BRO-3634): thin adapters over [`maestro_flow::kernel`]. The
//! CLI's `maestro flow` subcommands call these same tools, so their JSON in and out is identical.
//!
//! - **Verified writers only.** `start`, `claim`, `heartbeat`, `complete`, `choose`, `ask`,
//!   `propose_adoption` and `propose_retirement` act for the caller's verified session (`VerifiedSession`, through the same caller check every agent
//!   request uses). A caller that is not verified is refused `caller_unverified`, and one whose
//!   claim does not verify `caller_refused`; both write nothing. `next` and `status` are reads:
//!   anyone may call them, and they write nothing.
//! - **Refusals are results.** Every kernel refusal comes back as `{"ok": false, "code", "reason",
//!   "recorded"}` with the kernel's stable code. `recorded` is true when the kernel appended a
//!   `Refused` event (a completion or choice whose work did not pass keeps its token), an
//!   `AskWithdrawn` (an ask the writer of record refused, `ask_not_filed`), or a
//!   `ProposalWithdrawn` (`proposal_not_filed`). Only arguments that do not parse are protocol
//!   errors.
//! - **Human steps (BRO-3635).** `ask` files a waiting human node's ask through the asks ledger's
//!   writer of record and, called again, records the owner's answer once it has landed on main
//!   (`maestro_flow::kernel::asks`). `next` and `status` add `relay`: each waiting human node's
//!   question, its ask once filed, and for a T3 owner prompt links to its thread ([`relay`]).
//! - **Adoption and retirement (BRO-3635).** `propose_adoption` and `propose_retirement` put a
//!   change of owner, or a retirement, to the owner as a Maestro ask filed the same way, and carry
//!   it out only once the owner's approval has landed on main (`maestro_flow::kernel::adoption`).
//!   They are owner decisions the kernel applies: the tools take no answer and no ask id, and
//!   their names only propose. `next` and `status` list each standing proposal and its state.
//! - **Only the verbs.** No tool appends a raw event: the storage layer's `WriteGuard::append`
//!   and `NewEvent` are not reachable from here.
//! - **One ledger per workspace.** The flows root is `.broomva/flows` in the workspace the path
//!   registry names, as every other reader resolves it ([`flows_root`]). A workspace without a
//!   `.broomva` folder, or one that is a linked git worktree (its `.git` is a file), is refused,
//!   so no ledger is made in a stray folder or a checkout beside the real one. Only `start` may
//!   make the ledger (its genesis); every other verb finds none and says `no_ledger`.
//! - **One host.** The ledger is bound to the machine's host name (its first label, lower case),
//!   which every process on the machine reads alike ([`host_id`]).

use maestro_core::session::{SessionRef, Surface, VerifiedSession};
use maestro_flow::kernel::events::Owner;
use maestro_flow::kernel::{
    self, Asked, Choosing, Completion, KernelError, Limits, NodeStatus, Proposed, Reason, Verdict,
};
use maestro_flow::{FlowError, HostId, InstanceId, Ledger, Options, Reader};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const START: &str = "maestro_flow_start";
pub const NEXT: &str = "maestro_flow_next";
pub const CLAIM: &str = "maestro_flow_claim";
pub const COMPLETE: &str = "maestro_flow_complete";
pub const CHOOSE: &str = "maestro_flow_choose";
pub const STATUS: &str = "maestro_flow_status";
pub const HEARTBEAT: &str = "maestro_flow_heartbeat";
pub const ASK: &str = "maestro_flow_ask";
pub const PROPOSE_ADOPTION: &str = "maestro_flow_propose_adoption";
pub const PROPOSE_RETIREMENT: &str = "maestro_flow_propose_retirement";

/// The flows root under the workspace: the ADR's `.broomva/flows`.
pub const FLOWS: &str = ".broomva/flows";

/// The longest `node_id` or `edge` a tool takes. Ids are names in a definition; nothing longer is
/// passed on to the kernel or echoed back in a refusal.
pub const MAX_NAME: usize = 1024;

// Every write tool's schema carries the caller's claim (`cwd`, `token`, `node`, `credential`):
// a schema-enforcing client sends it in the call's arguments, the only place a shared daemon
// reads it (`crate::Ambient`). The caller check reads these fields from the arguments.

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowStartArgs {
    /// Your session as `surface:environment/id`, e.g. `t3:local/6ff25f7c`.
    pub session: String,
    /// The flow definition, as YAML text.
    pub definition: String,
    /// The flow's params, a JSON object.
    pub params: Option<Value>,
    /// Your session's folder, an absolute path: with `token` or `credential`, your claim.
    pub cwd: Option<String>,
    /// A Direct session's launch token.
    pub token: Option<String>,
    /// The tailnet node your session runs on.
    pub node: Option<String>,
    /// A T3 or Paseo session's signed bridge credential, a JSON object.
    pub credential: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowInstanceArgs {
    /// The instance id `maestro_flow_start` returned, e.g. `fi_0123abcd...`.
    pub instance: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowClaimArgs {
    pub session: String,
    pub instance: String,
    /// The flow node to claim, one `maestro_flow_next` lists as enabled.
    pub node_id: String,
    /// Your session's folder, an absolute path: with `token` or `credential`, your claim.
    pub cwd: Option<String>,
    /// A Direct session's launch token.
    pub token: Option<String>,
    /// The tailnet node your session runs on.
    pub node: Option<String>,
    /// A T3 or Paseo session's signed bridge credential, a JSON object.
    pub credential: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowHeartbeatArgs {
    pub session: String,
    /// The token `maestro_flow_claim` returned.
    pub claim_token: String,
    /// Your session's folder, an absolute path: with `token` or `credential`, your claim.
    pub cwd: Option<String>,
    /// A Direct session's launch token.
    pub token: Option<String>,
    /// The tailnet node your session runs on.
    pub node: Option<String>,
    /// A T3 or Paseo session's signed bridge credential, a JSON object.
    pub credential: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowCompleteArgs {
    pub session: String,
    /// The token `maestro_flow_claim` returned.
    pub claim_token: String,
    /// The node's outputs, a JSON object; the kernel records them with the receipt.
    pub outputs: Option<Value>,
    /// How long the node's command may run, in seconds (default 300, at most 3600).
    pub timeout_secs: Option<u64>,
    /// Your session's folder, an absolute path: with `token` or `credential`, your claim.
    pub cwd: Option<String>,
    /// A Direct session's launch token.
    pub token: Option<String>,
    /// The tailnet node your session runs on.
    pub node: Option<String>,
    /// A T3 or Paseo session's signed bridge credential, a JSON object.
    pub credential: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowChooseArgs {
    pub session: String,
    /// The token `maestro_flow_claim` returned for the choice node.
    pub claim_token: String,
    /// The edge to take, one the choice node declares.
    pub edge: String,
    /// Your session's folder, an absolute path: with `token` or `credential`, your claim.
    pub cwd: Option<String>,
    /// A Direct session's launch token.
    pub token: Option<String>,
    /// The tailnet node your session runs on.
    pub node: Option<String>,
    /// A T3 or Paseo session's signed bridge credential, a JSON object.
    pub credential: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowAskArgs {
    pub session: String,
    pub instance: String,
    /// The human node, one `maestro_flow_next` lists as waiting on the owner.
    pub node_id: String,
    /// The asks ledger to file into, a file name in the asks folder (`*.yaml`). Needed on the first
    /// call only; later calls use the ask already pinned.
    pub ledger: Option<String>,
    /// The ask's fields besides `id`, `ask` and `asked_at`, which the kernel writes: what the
    /// ledger's validator requires (`class`, `gates`, `exhausted`, `blocking`, ...). Needed on the
    /// first call only.
    pub ask: Option<serde_json::Map<String, Value>>,
    /// Your session's folder, an absolute path: with `token` or `credential`, your claim.
    pub cwd: Option<String>,
    /// A Direct session's launch token.
    pub token: Option<String>,
    /// The tailnet node your session runs on.
    pub node: Option<String>,
    /// A T3 or Paseo session's signed bridge credential, a JSON object.
    pub credential: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowProposeArgs {
    pub session: String,
    /// The instance, as `maestro_flow_status` names it.
    pub instance: String,
    /// The asks ledger to file the proposal into, a file name in the asks folder (`*.yaml`). Needed
    /// on the first call only, and again to propose anew after the owner declined.
    pub ledger: Option<String>,
    /// The ask's fields besides those the kernel writes (`id`, `asked_at`, `headline`, `ask`,
    /// `options`, `default`, `preanswerable`): what the ledger's validator requires (`class`,
    /// `gates`, `exhausted`, `blocking`, `stakes`, `why_you`, ...). Needed with `ledger`.
    pub ask: Option<serde_json::Map<String, Value>>,
    /// Your session's folder, an absolute path: with `token` or `credential`, your claim.
    pub cwd: Option<String>,
    /// A Direct session's launch token.
    pub token: Option<String>,
    /// The tailnet node your session runs on.
    pub node: Option<String>,
    /// A T3 or Paseo session's signed bridge credential, a JSON object.
    pub credential: Option<Value>,
}

/// A refusal as a result: the stable code, the reason, and whether the kernel recorded it.
fn refusal(code: &str, reason: impl std::fmt::Display, recorded: bool) -> Value {
    json!({ "ok": false, "code": code, "reason": reason.to_string(), "recorded": recorded })
}

fn refused(e: &KernelError) -> Value {
    refusal(e.code(), e, false)
}

/// Where the workspace's flow ledger lives, or why it does not live here (see the module docs).
pub fn flows_root(workspace: &Path) -> Result<PathBuf, Value> {
    let broomva = workspace.join(".broomva");
    if !std::fs::symlink_metadata(&broomva).is_ok_and(|m| m.is_dir()) {
        let reason = format!("{} has no .broomva folder, so no flow ledger lives there", workspace.display());
        return Err(refusal("workspace_unready", reason, false));
    }
    if std::fs::symlink_metadata(workspace.join(".git")).is_ok_and(|m| !m.is_dir()) {
        let reason = format!(
            "{} is a linked git worktree; the flow ledger lives in the workspace's main checkout",
            workspace.display()
        );
        return Err(refusal("workspace_is_worktree", reason, false));
    }
    Ok(workspace.join(FLOWS))
}

/// This machine's host id for the ledger: [`host_label`] of its host name.
pub fn host_id() -> Result<HostId, Value> {
    let uname = rustix::system::uname();
    HostId::new(host_label(&uname.nodename().to_string_lossy())).map_err(|e| refusal("host_unknown", e, false))
}

/// A host name's first DNS label, lower case, with any character a host id does not allow made a
/// dash. The network adds the rest (`Carloss-MBP.lan` on one, `Carloss-MBP.local` on another), so
/// it is dropped, or every write would be refused `foreign_host` after a move between networks.
pub fn host_label(name: &str) -> String {
    let label: String = name
        .split('.')
        .next()
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c.to_ascii_lowercase() } else { '-' })
        .collect();
    label.trim_start_matches('-').to_string()
}

fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, McpError> {
    serde_json::from_value(args).map_err(|e| McpError::invalid_params(e.to_string(), None))
}

/// A name the kernel looks up, bounded before it reaches the kernel.
fn bounded<'a>(what: &str, value: &'a str) -> Result<&'a str, Value> {
    if value.len() > MAX_NAME {
        return Err(refusal("name_too_long", format!("{what} is {} bytes, more than {MAX_NAME}", value.len()), false));
    }
    Ok(value)
}

/// Runs one flow tool. `paths` names the workspace (whose `.broomva/flows` is the ledger) and,
/// for `ask`, where its asks ledgers and their lock are.
pub(crate) fn call(
    paths: &maestro_store::Paths,
    hosts: &maestro_hosts::SurfaceRegistry,
    ambient: crate::Ambient,
    name: &str,
    args: Value,
) -> Result<Value, McpError> {
    let workspace = paths.workspace.as_path();
    // A read takes no claim, so a claim passed to one is dropped; a write declares its claim fields.
    match name {
        NEXT | STATUS => {
            let a: FlowInstanceArgs = parse(crate::strip_auth_fields(args))?;
            Ok(read(paths, name, &a.instance).unwrap_or_else(|v| v))
        }
        START => {
            let a: FlowStartArgs = parse(args.clone())?;
            let session = crate::session_of(&a.session)?;
            Ok(write(workspace, (hosts, ambient), &session, &args, true, |w| {
                // The definition compiles before the ledger is opened, or made.
                let flow = kernel::compile(&a.definition)?;
                let params = a.params.clone().unwrap_or_else(|| json!({}));
                let begun = kernel::start(&mut w.ledger()?, &w.me, &flow, params)?;
                Ok(json!({
                    "ok": true,
                    "instance": begun.instance.as_str(),
                    "flow": flow.name(),
                    "ir_hash": flow.hash(),
                    "seq": begun.appended.event.seq,
                }))
            }))
        }
        CLAIM => {
            let a: FlowClaimArgs = parse(args.clone())?;
            let session = crate::session_of(&a.session)?;
            Ok(write(workspace, (hosts, ambient), &session, &args, false, |w| {
                let node = match bounded("node_id", &a.node_id) {
                    Ok(node) => node,
                    Err(v) => return Ok(v),
                };
                let instance = InstanceId::new(&a.instance)?;
                let c = kernel::claim(&mut w.ledger()?, &w.me, &instance, node)?;
                Ok(json!({
                    "ok": true,
                    "claim_token": c.token,
                    "instance": c.instance.as_str(),
                    "node_id": c.node,
                    "iteration": c.iteration,
                    "attempt": c.attempt,
                    "lease_until": c.lease_until.to_string(),
                    "inputs": c.inputs,
                    "seq": c.appended.event.seq,
                }))
            }))
        }
        HEARTBEAT => {
            let a: FlowHeartbeatArgs = parse(args.clone())?;
            let session = crate::session_of(&a.session)?;
            Ok(write(workspace, (hosts, ambient), &session, &args, false, |w| {
                let beat = kernel::heartbeat(&mut w.ledger()?, &w.me, &a.claim_token)?;
                Ok(json!({ "ok": true, "lease_until": beat.lease_until.to_string(), "seq": beat.appended.event.seq }))
            }))
        }
        COMPLETE => {
            let a: FlowCompleteArgs = parse(args.clone())?;
            let session = crate::session_of(&a.session)?;
            Ok(write(workspace, (hosts, ambient), &session, &args, false, |w| {
                let mut limits = Limits::default();
                if let Some(secs) = a.timeout_secs {
                    limits.timeout = std::time::Duration::from_secs(secs);
                }
                let outputs = a.outputs.clone().unwrap_or(Value::Null);
                Ok(match kernel::complete(&mut w.ledger()?, &w.me, &a.claim_token, outputs, &limits)? {
                    Completion::Completed { appended, verify, stderr, verdicts } => json!({
                        "ok": true,
                        "seq": appended.event.seq,
                        "verify": verify,
                        "stderr": stderr,
                        "verdicts": verdicts.iter().map(verdict).collect::<Vec<_>>(),
                    }),
                    Completion::Refused { appended, code, reason, stderr } => {
                        let mut v = refusal(code, reason, true);
                        v["seq"] = json!(appended.event.seq);
                        v["stderr"] = json!(stderr);
                        v
                    }
                })
            }))
        }
        CHOOSE => {
            let a: FlowChooseArgs = parse(args.clone())?;
            let session = crate::session_of(&a.session)?;
            Ok(write(workspace, (hosts, ambient), &session, &args, false, |w| {
                let edge = match bounded("edge", &a.edge) {
                    Ok(edge) => edge,
                    Err(v) => return Ok(v),
                };
                Ok(match kernel::choose(&mut w.ledger()?, &w.me, &a.claim_token, edge)? {
                    Choosing::Chosen { appended, edge, guard_digest, verdicts } => json!({
                        "ok": true,
                        "seq": appended.event.seq,
                        "edge": edge,
                        "guard_digest": guard_digest,
                        "verdicts": verdicts.iter().map(verdict).collect::<Vec<_>>(),
                    }),
                    Choosing::Refused { appended, code, reason } => {
                        let mut v = refusal(code, reason, true);
                        v["seq"] = json!(appended.event.seq);
                        v
                    }
                })
            }))
        }
        ASK => {
            let a: FlowAskArgs = parse(args.clone())?;
            let session = crate::session_of(&a.session)?;
            Ok(write(workspace, (hosts, ambient), &session, &args, false, |w| {
                let node = match bounded("node_id", &a.node_id) {
                    Ok(node) => node,
                    Err(v) => return Ok(v),
                };
                let instance = InstanceId::new(&a.instance)?;
                let filing = match (&a.ledger, &a.ask) {
                    (Some(ledger), Some(body)) => Some(kernel::Filing { ledger: ledger.clone(), body: body.clone() }),
                    _ => None,
                };
                Ok(match kernel::ask(&mut w.ledger()?, paths, &w.me, &instance, node, filing.as_ref())? {
                    Asked::Waiting { ask_id, ledger, appended, landing } => json!({
                        "ok": true,
                        "state": "waiting",
                        "ask_id": ask_id,
                        "ledger": ledger,
                        "seq": appended.map(|a| a.event.seq),
                        "answer_waiting_to_land": landing,
                    }),
                    Asked::Answered { ask_id, ledger, outputs, source, appended } => json!({
                        "ok": true,
                        "state": "answered",
                        "ask_id": ask_id,
                        "ledger": ledger,
                        "outputs": outputs,
                        "source": source,
                        "seq": appended.event.seq,
                    }),
                    Asked::Withdrawn { ask_id, ledger, code, reason, appended } => {
                        let mut v = refusal(code, reason, true);
                        v["ask_id"] = json!(ask_id);
                        v["ledger"] = json!(ledger);
                        v["seq"] = json!(appended.event.seq);
                        v
                    }
                })
            }))
        }
        PROPOSE_ADOPTION | PROPOSE_RETIREMENT => {
            let a: FlowProposeArgs = parse(args.clone())?;
            let session = crate::session_of(&a.session)?;
            Ok(write(workspace, (hosts, ambient), &session, &args, false, |w| {
                let instance = InstanceId::new(&a.instance)?;
                let filing = match (&a.ledger, &a.ask) {
                    (Some(ledger), Some(body)) => Some(kernel::Filing { ledger: ledger.clone(), body: body.clone() }),
                    _ => None,
                };
                let propose =
                    if name == PROPOSE_ADOPTION { kernel::propose_adoption } else { kernel::propose_retirement };
                Ok(proposed(propose(&mut w.ledger()?, paths, &w.me, &instance, filing.as_ref())?))
            }))
        }
        other => Err(McpError::invalid_params(format!("unknown tool `{other}`"), None)),
    }
}

fn owner(o: &Owner) -> Value {
    json!({ "session": o.session.to_string(), "marker_sha256": o.marker_sha256 })
}

/// Where a proposal stands, as the tools return it. `state` is `waiting`, `declined`, `adopted`
/// or `retired`; a withdrawn proposal is a recorded refusal.
fn proposed(p: Proposed) -> Value {
    match p {
        Proposed::Waiting { proposal, ask_id, ledger, appended, landing } => json!({
            "ok": true,
            "state": "waiting",
            "proposal": proposal,
            "ask_id": ask_id,
            "ledger": ledger,
            "seq": appended.map(|a| a.event.seq),
            "answer_waiting_to_land": landing,
        }),
        Proposed::Withdrawn { proposal, ask_id, ledger, code, reason, appended } => {
            let mut v = refusal(code, reason, true);
            v["proposal"] = json!(proposal);
            v["ask_id"] = json!(ask_id);
            v["ledger"] = json!(ledger);
            v["seq"] = json!(appended.event.seq);
            v
        }
        Proposed::Declined { proposal, ask_id, ledger, answer_option, source } => json!({
            "ok": true,
            "state": "declined",
            "proposal": proposal,
            "ask_id": ask_id,
            "ledger": ledger,
            "answer_option": answer_option,
            "source": source,
        }),
        Proposed::Adopted { ask_id, ledger, from, to, source, appended } => json!({
            "ok": true,
            "state": "adopted",
            "proposal": "adoption",
            "ask_id": ask_id,
            "ledger": ledger,
            "from": owner(&from),
            "to": owner(&to),
            "source": source,
            "seq": appended.event.seq,
        }),
        Proposed::Retired { ask_id, ledger, source, appended } => json!({
            "ok": true,
            "state": "retired",
            "proposal": "retirement",
            "ask_id": ask_id,
            "ledger": ledger,
            "source": source,
            "seq": appended.map(|a| a.event.seq),
        }),
    }
}

/// A waiting human node's question, relayed toward the session that drives the flow: data the
/// reads return, never a message Maestro sends (ADR decisions 4 and 8). Each entry names the node,
/// the definition's question and, once filed, the ask's id and ledger. When the instance's owner
/// is a T3 thread and the ask is filed, `t3` holds prompt links (`maestro_view::handoff`) that
/// open that thread with a draft telling its agent to take the answer with `maestro_flow_ask`.
/// The draft carries no answer: the kernel reads the answer from main only.
pub fn relay(instance: &str, owner: Option<&SessionRef>, nodes: &BTreeMap<String, NodeStatus>) -> Vec<Value> {
    let thread = owner
        .filter(|o| o.surface == Surface::T3)
        .and_then(|o| o.environment.as_ref().map(|env| format!("{env}/{}", o.id)))
        .and_then(|t| maestro_view::handoff::AnswerThread::parse(&t).ok());
    let mut out = Vec::new();
    for (node, s) in nodes {
        for reason in &s.unmet {
            let (state, ask, ask_id, ledger) = match reason {
                Reason::Unfiled { ask, ask_id, ledger } => ("unfiled", ask, ask_id.as_deref(), ledger.as_deref()),
                Reason::Owner { ask, ask_id, ledger } => ("owner", ask, Some(ask_id.as_str()), Some(ledger.as_str())),
                Reason::AnswerWaitingToLand { ask, ask_id, ledger, .. } => {
                    ("answer_waiting_to_land", ask, Some(ask_id.as_str()), Some(ledger.as_str()))
                }
                _ => continue,
            };
            let mut entry = json!({
                "node_id": node,
                "iteration": s.iteration,
                "state": state,
                "question": ask,
                "ask_id": ask_id,
                "ledger": ledger,
            });
            if let (Some(thread), Some(id), Some(ledger), true) = (&thread, ask_id, ledger, state != "unfiled") {
                let prompt = format!(
                    "Maestro ask {id} in {ledger} is for flow instance {instance}, node {node}. Once its answer has landed on main, take it with maestro_flow_ask (instance {instance}, node_id {node}); the kernel reads it from main, not from this message."
                );
                // Every value placed in the draft is printable ASCII, so none can start a line or
                // hide a character in it (T3 rewrites U+2028 and drops invisible characters), and
                // the draft fits a T3 link.
                let plain =
                    [id, ledger, instance, node.as_str()].iter().all(|v| v.bytes().all(|b| b.is_ascii_graphic()));
                if plain && maestro_view::handoff::fits(&prompt) {
                    entry["t3"] = json!({ "app": thread.app_link(&prompt), "web": thread.web_link(&prompt) });
                }
            }
            out.push(entry);
        }
    }
    out
}

fn verdict(v: &Verdict) -> Value {
    match v {
        Verdict::Recorded { repeat, iteration, holds, appended } => json!({
            "recorded": true, "repeat": repeat, "iteration": iteration, "holds": holds, "seq": appended.event.seq,
        }),
        Verdict::Unrecorded { repeat, code, reason } => {
            json!({ "recorded": false, "repeat": repeat, "code": code, "reason": reason })
        }
    }
}

/// `next` or `status`: under the shared lock, writing nothing. A ledger that is not there is
/// `no_ledger`, never an empty flow.
fn read(paths: &maestro_store::Paths, name: &str, instance: &str) -> Result<Value, Value> {
    let root = flows_root(&paths.workspace)?;
    let run = || -> Result<Value, KernelError> {
        let instance = InstanceId::new(instance)?;
        let guard = Reader::new(&root, Options::default())?.lock()?;
        let now = maestro_flow::clock::now();
        Ok(if name == NEXT {
            let mut next = kernel::next(&guard, &instance, now)?;
            for s in next.pending.values_mut() {
                kernel::landing(paths, &mut s.unmet);
            }
            kernel::adoption::landing(paths, &mut next.proposals);
            let relay = relay(&next.instance, next.owner.as_ref(), &next.pending);
            json!({ "ok": true, "next": next, "relay": relay })
        } else {
            let mut status = kernel::status(&guard, &instance, now)?;
            for s in status.nodes.values_mut() {
                kernel::landing(paths, &mut s.unmet);
            }
            kernel::adoption::landing(paths, &mut status.proposals);
            let relay = relay(&status.instance, status.owner.as_ref(), &status.nodes);
            json!({ "ok": true, "status": status, "relay": relay })
        })
    };
    run().map_err(|e| refused(&e))
}

/// A write verb's caller, verified, and where it writes.
struct Writer {
    me: VerifiedSession,
    root: PathBuf,
    host: HostId,
    may_create: bool,
}

impl Writer {
    /// The ledger, opened once the verb's own arguments are checked. Only `start` may make it.
    fn ledger(&self) -> Result<Ledger, KernelError> {
        if !self.may_create && std::fs::symlink_metadata(&self.root).is_err() {
            return Err(FlowError::NoLedger { root: self.root.clone() }.into());
        }
        Ok(Ledger::open(&self.root, &self.host, Options::default())?)
    }
}

/// A write verb: the caller verified first, then the flows root and host resolved, then the verb,
/// which checks its own arguments before it opens the ledger. Every refusal is a result.
fn write(
    workspace: &Path,
    caller: (&maestro_hosts::SurfaceRegistry, crate::Ambient),
    session: &SessionRef,
    args: &Value,
    may_create: bool,
    verb: impl FnOnce(&Writer) -> Result<Value, KernelError>,
) -> Value {
    let me = match crate::verified_caller(caller.0, session, args, caller.1) {
        Ok(Some(me)) => me,
        Ok(None) => {
            let reason = format!(
                "Session {session} is not verified, so it writes no flow. A flow write needs a launch token or a bridge credential."
            );
            return refusal("caller_unverified", reason, false);
        }
        Err(e) => {
            let mut v = refusal("caller_refused", &e.message, false);
            v["identity"] = e.data.as_ref().and_then(|d| d.get("reason")).cloned().unwrap_or(Value::Null);
            return v;
        }
    };
    let root = match flows_root(workspace) {
        Ok(root) => root,
        Err(v) => return v,
    };
    let host = match host_id() {
        Ok(host) => host,
        Err(v) => return v,
    };
    verb(&Writer { me, root, host, may_create }).unwrap_or_else(|e| refused(&e))
}
