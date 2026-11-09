//! The flow kernel's verbs as tools (BRO-3634): thin adapters over [`maestro_flow::kernel`]. The
//! CLI's `maestro flow` subcommands call these same tools, so their JSON in and out is identical.
//!
//! - **Verified writers only.** `start`, `claim`, `heartbeat`, `complete` and `choose` act for the
//!   caller's verified session (`VerifiedSession`, through the same caller check every agent
//!   request uses). A caller that is not verified is refused `caller_unverified`, and one whose
//!   claim does not verify `caller_refused`; both write nothing. `next` and `status` are reads:
//!   anyone may call them, and they write nothing.
//! - **Refusals are results.** Every kernel refusal comes back as `{"ok": false, "code", "reason",
//!   "recorded"}` with the kernel's stable code. `recorded` is true when the kernel appended a
//!   `Refused` event (a completion or choice whose work did not pass keeps its token). Only
//!   arguments that do not parse are protocol errors.
//! - **Only the verbs.** No tool appends a raw event: the storage layer's `WriteGuard::append`
//!   and `NewEvent` are not reachable from here.
//! - **One ledger per workspace.** The flows root is `.broomva/flows` in the workspace the path
//!   registry names, as every other reader resolves it ([`flows_root`]). A workspace without a
//!   `.broomva` folder, or one that is a linked git worktree (its `.git` is a file), is refused,
//!   so no ledger is made in a stray folder or a checkout beside the real one. Only `start` may
//!   make the ledger (its genesis); every other verb finds none and says `no_ledger`.
//! - **One host.** The ledger is bound to the machine's host name (its first label, lower case),
//!   which every process on the machine reads alike ([`host_id`]).

use maestro_core::session::{SessionRef, VerifiedSession};
use maestro_flow::kernel::{self, Choosing, Completion, KernelError, Limits, Verdict};
use maestro_flow::{FlowError, HostId, InstanceId, Ledger, Options, Reader};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const START: &str = "maestro_flow_start";
pub const NEXT: &str = "maestro_flow_next";
pub const CLAIM: &str = "maestro_flow_claim";
pub const COMPLETE: &str = "maestro_flow_complete";
pub const CHOOSE: &str = "maestro_flow_choose";
pub const STATUS: &str = "maestro_flow_status";
pub const HEARTBEAT: &str = "maestro_flow_heartbeat";

/// The flows root under the workspace: the ADR's `.broomva/flows`.
pub const FLOWS: &str = ".broomva/flows";

/// The longest `node_id` or `edge` a tool takes. Ids are names in a definition; nothing longer is
/// passed on to the kernel or echoed back in a refusal.
pub const MAX_NAME: usize = 1024;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowStartArgs {
    /// Your session as `surface:environment/id`, e.g. `t3:local/6ff25f7c`.
    pub session: String,
    /// The flow definition, as YAML text.
    pub definition: String,
    /// The flow's params, a JSON object.
    pub params: Option<Value>,
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
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowHeartbeatArgs {
    pub session: String,
    /// The token `maestro_flow_claim` returned.
    pub claim_token: String,
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
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowChooseArgs {
    pub session: String,
    /// The token `maestro_flow_claim` returned for the choice node.
    pub claim_token: String,
    /// The edge to take, one the choice node declares.
    pub edge: String,
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

/// This machine's host id for the ledger: the host name's first label, lower case, with any
/// character a host id does not allow made a dash.
pub fn host_id() -> Result<HostId, Value> {
    let uname = rustix::system::uname();
    let name = uname.nodename().to_string_lossy();
    let label: String = name
        .split('.')
        .next()
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c.to_ascii_lowercase() } else { '-' })
        .collect();
    HostId::new(label.trim_start_matches('-')).map_err(|e| refusal("host_unknown", e, false))
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

/// Runs one flow tool.
pub(crate) fn call(
    workspace: &Path,
    hosts: &maestro_hosts::SurfaceRegistry,
    name: &str,
    args: Value,
) -> Result<Value, McpError> {
    let clean = crate::strip_auth_fields(args.clone());
    match name {
        NEXT | STATUS => {
            let a: FlowInstanceArgs = parse(clean)?;
            Ok(read(workspace, name, &a.instance).unwrap_or_else(|v| v))
        }
        START => {
            let a: FlowStartArgs = parse(clean)?;
            let session = crate::session_of(&a.session)?;
            Ok(write(workspace, hosts, &session, &args, true, |w| {
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
            let a: FlowClaimArgs = parse(clean)?;
            let session = crate::session_of(&a.session)?;
            Ok(write(workspace, hosts, &session, &args, false, |w| {
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
            let a: FlowHeartbeatArgs = parse(clean)?;
            let session = crate::session_of(&a.session)?;
            Ok(write(workspace, hosts, &session, &args, false, |w| {
                let beat = kernel::heartbeat(&mut w.ledger()?, &w.me, &a.claim_token)?;
                Ok(json!({ "ok": true, "lease_until": beat.lease_until.to_string(), "seq": beat.appended.event.seq }))
            }))
        }
        COMPLETE => {
            let a: FlowCompleteArgs = parse(clean)?;
            let session = crate::session_of(&a.session)?;
            Ok(write(workspace, hosts, &session, &args, false, |w| {
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
            let a: FlowChooseArgs = parse(clean)?;
            let session = crate::session_of(&a.session)?;
            Ok(write(workspace, hosts, &session, &args, false, |w| {
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
        other => Err(McpError::invalid_params(format!("unknown tool `{other}`"), None)),
    }
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
fn read(workspace: &Path, name: &str, instance: &str) -> Result<Value, Value> {
    let root = flows_root(workspace)?;
    let run = || -> Result<Value, KernelError> {
        let instance = InstanceId::new(instance)?;
        let guard = Reader::new(&root, Options::default())?.lock()?;
        let now = maestro_flow::clock::now();
        Ok(if name == NEXT {
            json!({ "ok": true, "next": kernel::next(&guard, &instance, now)? })
        } else {
            json!({ "ok": true, "status": kernel::status(&guard, &instance, now)? })
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
    hosts: &maestro_hosts::SurfaceRegistry,
    session: &SessionRef,
    args: &Value,
    may_create: bool,
    verb: impl FnOnce(&Writer) -> Result<Value, KernelError>,
) -> Value {
    let me = match crate::verified_caller(hosts, session, args) {
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
