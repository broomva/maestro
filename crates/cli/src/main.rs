//! `maestro`: the governance metalayer's command line.
//!
//! Every read command goes through the same dispatch as the MCP tools, so a
//! person, an agent on the CLI and an agent over MCP see identical data.
//! `--json` prints that data verbatim; without it the output is a compact
//! human view.

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use maestro_core::authority::{Authority, INVARIANTS, OwnerVerb, UNDO_MS};
use maestro_core::session::{Harness, Node, NodeId, Surface, harness_capabilities, surface_capabilities};
use maestro_store::{Paths, Store, TreeSource};
use serde_json::{Value, json};
use std::io::Read as _;
use std::path::PathBuf;

mod port;

#[derive(Parser)]
#[command(name = "maestro", version, about = "Governance and orchestration metalayer for agent fleets")]
struct Cli {
    /// Workspace checkout holding .broomva/ (legacy .control/). Default: the path registry's
    /// workspace root (BROOMVA_WORKSPACE, then MAESTRO_WORKSPACE, then BSTACK_WORKSPACE, then ~/broomva).
    #[arg(long, global = true)]
    workspace: Option<PathBuf>,
    /// Read the workspace at a git revision (`origin/main`, the home rule) or `worktree`.
    #[arg(long, global = true, env = "MAESTRO_TREE", default_value = "origin/main")]
    tree: String,
    /// Print machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Counts across the control plane.
    Snapshot,
    /// Holds on the owner: the Needs you list.
    Needs {
        #[arg(long)]
        project: Option<String>,
    },
    /// Asks from the workspace's asks folder (.broomva/asks, legacy .control/asks).
    Asks {
        #[command(subcommand)]
        command: Option<AsksCommand>,
        /// open, answered, archived or all.
        #[arg(long, default_value = "open")]
        state: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        blocking: bool,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Work items by canonical state.
    Work {
        /// Queued, Running, Stuck, "Needs you", Done or Standing.
        #[arg(long)]
        state: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Standing loops, their last tick and drift.
    Loops {
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        drifting: bool,
    },
    /// Live initiatives.
    Initiatives {
        #[arg(long)]
        all: bool,
    },
    /// Latest fleet-reconcile report per scope.
    Fleet,
    /// Render the governance context packet for a session.
    Context {
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        initiative: Option<String>,
        #[arg(long, value_enum, default_value_t = Format::Markdown)]
        format: Format,
        /// Hook event name for `--format hook`.
        #[arg(long, default_value = "SessionStart")]
        event: String,
        /// Read the harness hook payload from stdin and take `cwd` from it.
        #[arg(long)]
        stdin: bool,
        #[arg(long, default_value_t = 8)]
        max_needs: usize,
    },
    /// Serve the MCP server on stdio.
    Mcp,
    /// Serve the MCP server over Streamable HTTP (run once per machine, as `maestrod`).
    Serve {
        /// Streamable HTTP at /mcp (the only transport `serve` offers; stdio is `maestro mcp`).
        #[arg(long)]
        http: bool,
        /// Listen here instead of 127.0.0.1:7171. Only a loopback or tailnet address is accepted.
        #[arg(long)]
        bind: Option<std::net::SocketAddr>,
    },
    /// What each surface may do, and the standing invariants.
    Authority,
    /// This node, and the known surfaces and harnesses with their capabilities.
    ///
    /// A capability says what the surface or harness can do, not what Maestro does with it:
    /// Maestro launches and sends nothing until its host adapters land (BRO-3276).
    Hosts,
    /// Agent requests: file one ask (`maestro ask file`). Writes go to the working tree.
    Ask {
        #[command(subcommand)]
        command: AskCommand,
    },
    /// Queue work for the owner to dispatch; recorded and held (nothing starts).
    Queue {
        #[command(flatten)]
        session: SessionArg,
        #[arg(long)]
        title: String,
        /// What the work is and how its done is checked.
        #[arg(long)]
        brief: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        initiative: Option<String>,
    },
    /// Request one exact action the owner must allow; recorded and held (nothing runs).
    #[command(name = "action-request")]
    ActionRequest {
        #[command(flatten)]
        session: SessionArg,
        #[arg(long)]
        action: String,
        #[arg(long)]
        reason: String,
    },
    /// Record that this session's work is finished, with its receipt. Closes nothing by itself (A1).
    Close {
        #[command(flatten)]
        session: SessionArg,
        /// The evidence: PRs, merge SHAs, checks run.
        #[arg(long)]
        receipt: String,
        #[arg(long)]
        summary: Option<String>,
    },
    /// Hand the work back to the owner.
    Handback {
        #[command(flatten)]
        session: SessionArg,
        #[arg(long)]
        summary: String,
        #[arg(long)]
        next: Option<String>,
    },
    /// Escalate to the owner: something needs their attention now.
    Escalate {
        #[command(flatten)]
        session: SessionArg,
        #[arg(long)]
        reason: String,
    },
    /// Self-checks: is the vendored path reader the one bstack generates from its registry?
    Doctor {
        /// The bstack checkout to compare with, read at --tree. Default: <workspace>/bstack.
        #[arg(long)]
        bstack: Option<PathBuf>,
    },
    /// Tiers, assessments and agent-graph rules (the workspace's delegation.py, ported).
    Delegation {
        #[command(subcommand)]
        command: port::DelegationCommand,
    },
    /// Capacity forecast, cadence mode and launch gates (the workspace's fleet_economy.py, ported).
    Economy {
        #[command(subcommand)]
        command: port::EconomyCommand,
    },
    /// Evaluate parity cases (JSON on stdin) with the ported functions: scripts/parity-delegation.py.
    #[command(hide = true)]
    ParityEval,
    /// A flow's steps, through the flow kernel. Each prints the JSON its maestro_flow tool returns,
    /// and exits 1 on a refusal. Writes need a verified session:
    /// a Direct launch token (MAESTRO_DIRECT_TOKEN) or a bridge credential (MAESTRO_BRIDGE_CREDENTIAL).
    Flow {
        #[command(subcommand)]
        command: FlowCommand,
    },
    /// Dogfood receipts (BRO-3636): the verifier pilot's check, which the flow kernel runs as the
    /// verify of verify@1's exercise node. Reads only, and needs no workspace.
    Dogfood {
        #[command(subcommand)]
        command: DogfoodCommand,
    },
    /// Specialist agents (BRO-3636): agents/<name>.agent.yaml, rendered for each harness. Needs no
    /// workspace.
    Agents {
        #[command(subcommand)]
        command: AgentsCommand,
    },
}

#[derive(Subcommand)]
enum AgentsCommand {
    /// Write .claude/agents/<name>.md, .gemini/agents/<name>.md and .grok/agents/<name>.md from
    /// every agents/<name>.agent.yaml under --root, and remove the renders whose source is gone.
    /// With --check, write nothing and exit 1 naming the first render that drifts from its source.
    Render {
        /// The folder holding agents/ and the render folders.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        check: bool,
    },
}

#[derive(Subcommand)]
enum DogfoodCommand {
    /// Check a dogfood-receipt@1 file. Reads the outputs a performer completed with on stdin,
    /// `{"receipt": "<path>", "receipt_sha256": "sha256:<hex>"}` (a relative path is taken from
    /// the working directory), as the flow kernel hands them to the verify. Exits 0 only when the
    /// receipt hashes to receipt_sha256, is well formed, is for --head and --surface, says pass,
    /// and its evidence file hashes to the digest it records. Each failure has its own exit
    /// status, 3 to 14 (see maestro_flow::dogfood). Prints one JSON line; writes nothing.
    VerifyReceipt {
        /// The commit the receipt must be for.
        #[arg(long)]
        head: String,
        /// The surface the receipt must be for.
        #[arg(long)]
        surface: String,
    },
}

#[derive(Subcommand)]
enum FlowCommand {
    /// Start an instance of a flow, owned by this session.
    Start {
        #[command(flatten)]
        session: SessionArg,
        /// The definition's YAML file; `-` reads it from stdin.
        #[arg(long)]
        definition: String,
        /// The flow's params as a JSON object.
        #[arg(long)]
        params: Option<String>,
    },
    /// The nodes that can be claimed now, and why the others cannot. Writes nothing.
    Next {
        #[arg(long)]
        instance: String,
    },
    /// Claim an enabled node.
    Claim {
        #[command(flatten)]
        session: SessionArg,
        #[arg(long)]
        instance: String,
        #[arg(long)]
        node_id: String,
    },
    /// Finish a claim: the kernel runs the node's command and records the receipt if it passes.
    Complete {
        #[command(flatten)]
        session: SessionArg,
        #[arg(long)]
        claim_token: String,
        /// The node's outputs as a JSON object.
        #[arg(long)]
        outputs: Option<String>,
        /// How long the node's command may run, in seconds.
        #[arg(long)]
        timeout_secs: Option<u64>,
    },
    /// Take a declared edge of a claimed choice node, if its guard holds.
    Choose {
        #[command(flatten)]
        session: SessionArg,
        #[arg(long)]
        claim_token: String,
        #[arg(long)]
        edge: String,
    },
    /// Every node's state. Writes nothing.
    Status {
        #[arg(long)]
        instance: String,
    },
    /// The accepting spine: one record per accepted node and iteration, with its edge and verify
    /// exit, in log order. With --against, exits 1 at the first difference from another instance's,
    /// and 3 when the two run different definitions, which are not compared. Writes nothing.
    Spine {
        #[arg(long)]
        instance: String,
        /// Another instance of the same flow to compare with.
        #[arg(long)]
        against: Option<String>,
    },
    /// The flow's driver skill (a SKILL.md), generated from its compiled IR: printed, or written
    /// to --out. With --check, write nothing and exit 1 when --out is not what it generates. An
    /// --out that is the definition itself is refused. With --json, prints {"ok", "skill",
    /// "path"}. Needs no workspace, and is the one flow command that is no MCP tool's twin.
    DriverSkill {
        /// The definition's YAML file.
        #[arg(long)]
        definition: PathBuf,
        /// The SKILL.md to write, or to check with --check.
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long, requires = "out")]
        check: bool,
    },
    /// Extend a claim's lease.
    Heartbeat {
        #[command(flatten)]
        session: SessionArg,
        #[arg(long)]
        claim_token: String,
    },
    /// A waiting human node: file its ask (first call), then take the owner's answer from the
    /// answers ledger once there is one. Call it again until it says answered.
    Ask {
        #[command(flatten)]
        session: SessionArg,
        #[arg(long)]
        instance: String,
        #[arg(long)]
        node_id: String,
        /// The asks ledger to file into (first call only).
        #[arg(long)]
        ledger: Option<String>,
        /// The ask's fields besides id, ask and asked_at, as a JSON object (first call only).
        #[arg(long)]
        ask: Option<String>,
    },
    /// Propose to the owner that this session adopt an instance it does not own: file the ask
    /// (first call), then carry out the owner's answer once it has landed on main. Call it again
    /// until it says adopted or declined; a decline is reported on every call, and --again (with
    /// --ledger and --ask) asks again.
    ProposeAdoption {
        #[command(flatten)]
        session: SessionArg,
        #[arg(long)]
        instance: String,
        /// The asks ledger to file into (first call, or with --again).
        #[arg(long)]
        ledger: Option<String>,
        /// The ask's fields besides those the kernel writes, as a JSON object (with --ledger).
        #[arg(long)]
        ask: Option<String>,
        /// Ask again, in place of a declined or stale proposal (needs --ledger and --ask).
        #[arg(long)]
        again: bool,
    },
    /// Propose to the owner that an instance be retired: file the ask (first call), then carry
    /// out the owner's answer once it has landed on main. Call it again until it says retired or
    /// declined; a decline is reported on every call, and --again (with --ledger and --ask) asks
    /// again.
    ProposeRetirement {
        #[command(flatten)]
        session: SessionArg,
        #[arg(long)]
        instance: String,
        /// The asks ledger to file into (first call, or with --again).
        #[arg(long)]
        ledger: Option<String>,
        /// The ask's fields besides those the kernel writes, as a JSON object (with --ledger).
        #[arg(long)]
        ask: Option<String>,
        /// Ask again, in place of a declined or stale proposal (needs --ledger and --ask).
        #[arg(long)]
        again: bool,
    },
    /// The flow instances this session owns that are not finished, each with its work (what the
    /// session can do now) and what it waits on, and refuses_stop. Needs a verified session;
    /// writes nothing.
    Open {
        #[command(flatten)]
        session: SessionArg,
    },
    /// The Stop-hook gate: run by Claude Code or Codex as a Stop hook. Reads the hook's JSON on
    /// stdin and refuses the final message, in the harness's format, while an instance this
    /// session owns is open and has work it can do now. Fails open: on any error the stop goes
    /// through, with the reason on stderr. Always exits 0. Needs no MCP twin: only a harness runs it.
    StopGate {
        /// The harness running the hook.
        #[arg(long, value_enum)]
        harness: GateHarness,
        /// This session as `surface:environment/id`. Unset, no flow is gated.
        #[arg(long, env = "MAESTRO_SESSION")]
        session: Option<String>,
        /// The harness cannot refuse a stop here: record StopUnblockable and file the alert instead.
        #[arg(long)]
        cannot_block: bool,
        /// The asks ledger fleet alerts are filed into (a file in the asks folder).
        #[arg(long, env = "MAESTRO_ALERT_LEDGER", default_value = maestro_mcp::stop::DEFAULT_ALERT_LEDGER)]
        alert_ledger: String,
        /// Report whether a settings file wires this gate as the harness's Stop hook, and exit 1
        /// when it does not. Reads no stdin and gates nothing.
        #[arg(long)]
        check_install: bool,
        /// The settings file --check-install reads. Default: ~/.claude/settings.json for claude,
        /// $CODEX_HOME/hooks.json (~/.codex/hooks.json) for codex.
        #[arg(long, requires = "check_install")]
        settings: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum GateHarness {
    Claude,
    Codex,
}

impl GateHarness {
    fn harness(self) -> maestro_mcp::stop::Harness {
        match self {
            GateHarness::Claude => maestro_mcp::stop::Harness::Claude,
            GateHarness::Codex => maestro_mcp::stop::Harness::Codex,
        }
    }
}

#[derive(Subcommand)]
enum AsksCommand {
    /// One ask in full, with its content hash.
    Show { ledger: String, id: String },
    /// Validate a ledger with the workspace's ask_ledger.py, the validator of record.
    Validate { file: PathBuf },
}

#[derive(clap::Args)]
struct SessionArg {
    /// This session as `surface:environment/id`, e.g. `t3:local/6ff25f7c`.
    #[arg(long, env = "MAESTRO_SESSION")]
    session: String,
}

#[derive(Subcommand)]
enum AskCommand {
    /// File one ask into a ledger through ask_ledger.py, under the ledger's lock.
    File {
        #[command(flatten)]
        session: SessionArg,
        /// Ledger file name in the asks folder (it must exist).
        #[arg(long)]
        ledger: String,
        /// The ask as a JSON object; `-` (the default) reads it from stdin.
        #[arg(long, default_value = "-")]
        ask: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Markdown,
    Json,
    /// `{"hookSpecificOutput": {...}}` for Claude Code style hooks; never fails the session.
    Hook,
}

fn store(cli: &Cli) -> Result<Store> {
    let paths = Paths::from_env_with(cli.workspace.clone()).map_err(|e| anyhow::anyhow!("the path registry: {e}"))?;
    let source: TreeSource = cli.tree.parse().map_err(|e: String| anyhow::anyhow!("--tree: {e}"))?;
    Ok(Store::new(paths, source))
}

/// Two instances `flow spine --against` does not compare, since they run different definitions:
/// exit 3, apart from a difference (exit 1).
#[derive(Debug)]
struct DefinitionMismatch(String);

impl std::fmt::Display for DefinitionMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DefinitionMismatch {}

/// A lock another writer held for the whole wait: exit 75 (EX_TEMPFAIL), as `ask_ledger.py` does, to retry.
#[derive(Debug)]
struct Busy(String);

impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Busy {}

fn call(store: &Store, tool: &str, args: Value) -> Result<Value> {
    maestro_mcp::call(store, tool, args).map_err(|e| {
        if e.data.as_ref().is_some_and(|d| d["busy"] == json!(true)) {
            anyhow::Error::new(Busy(e.message.to_string()))
        } else {
            anyhow::anyhow!("{}", e.message)
        }
    })
}

fn print_errors(v: &Value) {
    let errors = v.get("errors").and_then(|e| e.as_array()).cloned().unwrap_or_default();
    if v.get("data").is_some_and(Value::is_null) {
        println!("unreadable: {}", v["source"].as_str().unwrap_or("?"));
    }
    for e in errors.iter().take(5) {
        eprintln!("read error: {}: {}", e["at"].as_str().unwrap_or("?"), e["reason"].as_str().unwrap_or("?"));
    }
    if errors.len() > 5 {
        eprintln!("read error: and {} more (use --json)", errors.len() - 5);
    }
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// Whether argv asks for the hook format. Decided before parsing, so even a bad
/// argument on the hook path exits 0 instead of blocking the session (exit 2 blocks).
fn is_hook(args: &[String]) -> bool {
    args.iter().any(|a| a == "context")
        && args
            .iter()
            .enumerate()
            .any(|(i, a)| a == "--format=hook" || (a == "--format" && args.get(i + 1).is_some_and(|v| v == "hook")))
}

/// Whether the command line runs the Stop-hook gate (`flow stop-gate`), so it never exits 2.
fn is_stop_gate(args: &[String]) -> bool {
    args.windows(2).any(|w| w[0] == "flow" && w[1] == "stop-gate") && !args.iter().any(|a| a == "--check-install")
}

/// How long the hook may take before it gives up silently (`MAESTRO_HOOK_TIMEOUT_MS`, default 5000).
fn hook_budget() -> std::time::Duration {
    let ms = std::env::var("MAESTRO_HOOK_TIMEOUT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(5_000);
    std::time::Duration::from_millis(ms)
}

/// The variables that make git read another repository than the one it is pointed at. A git hook,
/// or a harness running inside one, sets them. Maestro reads each tree by its root alone, so it
/// drops them once, here, and every git process it starts (its own and the path registry's)
/// inherits the clean environment.
const GIT_REPO_ENV: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

/// Re-run this binary without [`GIT_REPO_ENV`] when any of them is set (std cannot unset a
/// variable in a running process without `unsafe`). `exec` returns only when it failed; Maestro
/// then carries on as it is, and its git reads may be unread, never another repo's data
/// mistaken for this one's (a root below its repo's top level is refused).
#[cfg(unix)]
fn drop_inherited_git_repo() {
    use std::os::unix::process::CommandExt as _;
    if !GIT_REPO_ENV.iter().any(|k| std::env::var_os(k).is_some()) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(std::env::args_os().skip(1));
    for key in GIT_REPO_ENV {
        cmd.env_remove(key);
    }
    let _ = cmd.exec();
}

#[cfg(not(unix))]
fn drop_inherited_git_repo() {}

fn main() -> std::process::ExitCode {
    // First: in the flow kernel's guard evaluator (this binary started again) it answers and
    // exits here; otherwise it registers this process as a guard host (BRO-3634).
    maestro_flow::kernel::guard_entry();
    drop_inherited_git_repo();
    let args: Vec<String> = std::env::args().collect();
    let hook = is_hook(&args);
    let gate = is_stop_gate(&args);
    let port_cmd = args.iter().any(|a| a == "delegation" || a == "economy");
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(_) if hook => return std::process::ExitCode::SUCCESS,
        // A Stop hook that exits 2 refuses the stop: a command line the gate cannot read lets it
        // through instead (it fails open), saying why.
        Err(e) if gate => {
            eprintln!("maestro stop-gate: the command line does not parse, so the stop goes through: {e}");
            return std::process::ExitCode::SUCCESS;
        }
        Err(e) => match port_cmd.then(|| port::usage_error(&e)).flatten() {
            Some(code) => return code,
            None => e.exit(),
        },
    };

    // The hook path never blocks a session: any failure or overrun prints nothing and exits 0.
    if hook {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(store(&cli).and_then(|store| context(&cli, &store)));
        });
        if let Ok(Ok(out)) = rx.recv_timeout(hook_budget()) {
            println!("{out}");
        }
        return std::process::ExitCode::SUCCESS;
    }

    // The ports keep the Python's exit codes and need no workspace store; nor does a receipt
    // check, which the flow kernel runs with a cleared environment.
    match &cli.command {
        Command::Delegation { command } => return port::delegation(command, cli.json),
        Command::Economy { command } => return port::economy(command, cli.json),
        Command::ParityEval => return port::eval_stdin(),
        Command::Dogfood { command: DogfoodCommand::VerifyReceipt { head, surface } } => {
            return verify_receipt(head, surface);
        }
        Command::Agents { command: AgentsCommand::Render { root, check } } => {
            return exit(render_agents(cli.json, root, *check));
        }
        Command::Flow { command: FlowCommand::DriverSkill { definition, out, check } } => {
            return exit(driver_skill(cli.json, definition, out.as_deref(), *check));
        }
        Command::Flow {
            command: FlowCommand::StopGate { harness, session, cannot_block, alert_ledger, check_install, settings },
        } => {
            if *check_install {
                return exit(check_gate(harness.harness(), settings.as_deref()));
            }
            let gate = maestro_mcp::stop::Gate {
                harness: harness.harness(),
                cannot_block: *cannot_block,
                session: session.clone(),
                alert_ledger: alert_ledger.clone(),
            };
            return stop_gate(&cli, &gate);
        }
        _ => {}
    }

    match run(&cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("maestro: {e:#}");
            if e.downcast_ref::<Busy>().is_some() {
                return std::process::ExitCode::from(75);
            }
            if e.downcast_ref::<DefinitionMismatch>().is_some() {
                return std::process::ExitCode::from(3);
            }
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<()> {
    let store = store(cli)?;
    match &cli.command {
        Command::Mcp => {
            let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
            runtime.block_on(maestro_mcp::serve_stdio(store)).map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Command::Serve { http, bind } => serve(store, *http, *bind)?,
        Command::Context { .. } => println!("{}", context(cli, &store)?),
        Command::Authority => authority(cli.json),
        Command::Hosts => hosts(cli.json),
        Command::Doctor { bstack } => doctor(cli, &store, bstack.as_deref())?,
        Command::Asks { command: Some(AsksCommand::Validate { file }), .. } => validate(&store, file)?,
        Command::Ask { command: AskCommand::File { session, ledger, ask } } => {
            let text = if ask == "-" {
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf)?;
                buf
            } else {
                ask.clone()
            };
            let ask: Value = serde_json::from_str(&text).context("--ask must be a JSON object")?;
            request(
                cli,
                &store,
                "maestro_ask_file",
                json!({ "session": session.session, "ledger": ledger, "ask": ask }),
            )?
        }
        Command::Queue { session, title, brief, project, initiative } => request(
            cli,
            &store,
            "maestro_work_queue",
            json!({ "session": session.session, "title": title, "brief": brief, "project": project, "initiative": initiative }),
        )?,
        Command::ActionRequest { session, action, reason } => request(
            cli,
            &store,
            "maestro_action_request",
            json!({ "session": session.session, "action": action, "reason": reason }),
        )?,
        Command::Close { session, receipt, summary } => request(
            cli,
            &store,
            "maestro_session_close",
            json!({ "session": session.session, "receipt": receipt, "summary": summary }),
        )?,
        Command::Handback { session, summary, next } => request(
            cli,
            &store,
            "maestro_handback",
            json!({ "session": session.session, "summary": summary, "next": next }),
        )?,
        Command::Escalate { session, reason } => {
            request(cli, &store, "maestro_escalate", json!({ "session": session.session, "reason": reason }))?
        }
        Command::Flow { command } => flow(&store, command)?,
        _ => read(cli, &store)?,
    }
    Ok(())
}

/// One agent request, through the MCP dispatch. The caller is unverified (BRO-3268).
fn request(cli: &Cli, store: &Store, tool: &str, args: Value) -> Result<()> {
    let v = call(store, tool, args)?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let r = &v["request"];
    match r["disposition"].as_str() {
        Some("applied") => println!(
            "filed {} in {} ({}); origin {} unverified",
            s(&r["result"]["id"]),
            s(&r["result"]["ledger"]),
            s(&v["validator"]),
            s(&r["session"]["id"])
        ),
        d => println!("{} {} {}: recorded, {}", s(&r["request"]), s(&r["id"]), s(&r["at"]), d.unwrap_or("?")),
    }
    Ok(())
}

/// A JSON argument: an object for `params` and `outputs`.
fn json_arg(flag: &str, text: Option<&str>) -> Result<Option<Value>> {
    text.map(|t| serde_json::from_str(t).with_context(|| format!("{flag} must be JSON"))).transpose()
}

/// A definition, read to one byte past the kernel's bound, so an oversized one is never read
/// whole. Over the bound it goes on with any character the cut split replaced, so the tool
/// refuses it for its size (`definition_invalid`) exactly as it refuses one sent over MCP;
/// within the bound it must be UTF-8, as every MCP argument is.
fn read_definition(source: impl std::io::Read) -> Result<String> {
    let limit = maestro_flow::kernel::MAX_SOURCE;
    let mut bytes = Vec::new();
    source.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }
    String::from_utf8(bytes).context("the definition is not UTF-8")
}

/// One flow verb, through the same tool an agent calls over MCP: the same JSON in and out. The
/// CLI adds no caller claim of its own; the caller variables (`MAESTRO_DIRECT_TOKEN`,
/// `MAESTRO_BRIDGE_CREDENTIAL`, `MAESTRO_CALLER_CWD`, `MAESTRO_CALLER_NODE`) carry it.
fn flow(store: &Store, command: &FlowCommand) -> Result<()> {
    use maestro_mcp::flow as f;
    let (tool, args) = match command {
        FlowCommand::Start { session, definition, params } => {
            let text = if definition == "-" {
                read_definition(std::io::stdin().lock())?
            } else {
                std::fs::File::open(definition)
                    .map_err(anyhow::Error::from)
                    .and_then(read_definition)
                    .with_context(|| format!("reading {definition}"))?
            };
            let params = json_arg("--params", params.as_deref())?;
            (f::START, json!({ "session": session.session, "definition": text, "params": params }))
        }
        FlowCommand::DriverSkill { .. } => bail!("flow driver-skill is not a tool"),
        FlowCommand::StopGate { .. } => bail!("flow stop-gate is not a tool"),
        FlowCommand::Open { session } => (f::OPEN, json!({ "session": session.session })),
        FlowCommand::Next { instance } => (f::NEXT, json!({ "instance": instance })),
        FlowCommand::Status { instance } => (f::STATUS, json!({ "instance": instance })),
        FlowCommand::Spine { instance, against } => (f::SPINE, json!({ "instance": instance, "against": against })),
        FlowCommand::Claim { session, instance, node_id } => {
            (f::CLAIM, json!({ "session": session.session, "instance": instance, "node_id": node_id }))
        }
        FlowCommand::Complete { session, claim_token, outputs, timeout_secs } => {
            let outputs = json_arg("--outputs", outputs.as_deref())?;
            let args = json!({ "session": session.session, "claim_token": claim_token, "outputs": outputs, "timeout_secs": timeout_secs });
            (f::COMPLETE, args)
        }
        FlowCommand::Choose { session, claim_token, edge } => {
            (f::CHOOSE, json!({ "session": session.session, "claim_token": claim_token, "edge": edge }))
        }
        FlowCommand::Heartbeat { session, claim_token } => {
            (f::HEARTBEAT, json!({ "session": session.session, "claim_token": claim_token }))
        }
        FlowCommand::Ask { session, instance, node_id, ledger, ask } => {
            let ask = json_arg("--ask", ask.as_deref())?;
            let args = json!({ "session": session.session, "instance": instance, "node_id": node_id, "ledger": ledger, "ask": ask });
            (f::ASK, args)
        }
        FlowCommand::ProposeAdoption { session, instance, ledger, ask, again }
        | FlowCommand::ProposeRetirement { session, instance, ledger, ask, again } => {
            let tool = match command {
                FlowCommand::ProposeAdoption { .. } => f::PROPOSE_ADOPTION,
                _ => f::PROPOSE_RETIREMENT,
            };
            let body = json_arg("--ask", ask.as_deref())?;
            let args = json!({ "session": session.session, "instance": instance, "ledger": ledger, "ask": body, "again": again });
            (tool, args)
        }
    };
    let v = call(store, tool, args)?;
    println!("{}", serde_json::to_string_pretty(&v)?);
    if v["ok"] == json!(true) {
        return Ok(());
    }
    if v["code"] == "definition_mismatch" {
        return Err(DefinitionMismatch(format!("definition_mismatch: {}", s(&v["reason"]))).into());
    }
    bail!("{}: {}", s(&v["code"]), s(&v["reason"]))
}

/// `maestro flow stop-gate`: the gate over stdin (`maestro_mcp::stop`), its answer on stdout and
/// its notes on stderr. Always exits 0: a Stop hook that exits 2 would refuse the stop, and the
/// gate refuses only through its answer.
fn stop_gate(cli: &Cli, gate: &maestro_mcp::stop::Gate) -> std::process::ExitCode {
    let mut input = Vec::new();
    let limit = maestro_mcp::stop::MAX_INPUT as u64 + 1;
    if let Err(e) = std::io::stdin().lock().take(limit).read_to_end(&mut input) {
        eprintln!("maestro stop-gate: stdin does not read ({e}), so the stop goes through.");
        println!("{}", gate.harness.allow());
        return std::process::ExitCode::SUCCESS;
    }
    let paths = match Paths::from_env_with(cli.workspace.clone()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("maestro stop-gate: the path registry: {e}; the stop goes through (the gate fails open).");
            println!("{}", gate.harness.allow());
            return std::process::ExitCode::SUCCESS;
        }
    };
    let out = maestro_mcp::stop::gate_in_env(&paths, gate, &input);
    for note in &out.notes {
        eprintln!("{note}");
    }
    println!("{}", out.stdout);
    std::process::ExitCode::SUCCESS
}

/// `maestro flow stop-gate --check-install`: whether `settings` (or the harness's default) wires
/// the gate as its Stop hook. Prints the report as JSON; exits 1 when it is not wired.
fn check_gate(harness: maestro_mcp::stop::Harness, settings: Option<&std::path::Path>) -> Result<()> {
    use maestro_mcp::stop::Harness;
    let home = || std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let path = match (settings, harness) {
        (Some(p), _) => p.to_path_buf(),
        (None, Harness::Claude) => home().join(".claude/settings.json"),
        (None, Harness::Codex) => std::env::var_os("CODEX_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".codex"))
            .join("hooks.json"),
    };
    let report = maestro_mcp::stop::check_install(harness, &path);
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.wired {
        bail!("{}", report.detail);
    }
    Ok(())
}

fn exit(result: Result<()>) -> std::process::ExitCode {
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("maestro: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// `maestro agents render`: every source's renders under `root`, written, or with `check`
/// compared, failing on the first that drifts (`maestro_flow::agents::check`).
fn render_agents(json: bool, root: &std::path::Path, check: bool) -> Result<()> {
    use maestro_flow::agents;
    if check {
        if let Some(drift) = agents::check(root)? {
            if json {
                println!("{}", json!({ "ok": false, "code": "drift", "drift": drift }));
            }
            bail!("{drift}");
        }
        if json {
            println!("{}", json!({ "ok": true }));
        } else {
            println!("every render matches its source");
        }
        return Ok(());
    }
    let written = agents::write(root)?;
    if json {
        println!("{}", json!({ "ok": true, "written": written.written, "removed": written.removed }));
    } else {
        for file in &written.written {
            println!("wrote   {file}");
        }
        for file in &written.removed {
            println!("removed {file}");
        }
    }
    Ok(())
}

/// Whether `a` and `b` name one file: the same device and inode when both exist (so a link or a
/// second path to it counts), and otherwise never, since a file that is not there is not `a`.
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        #[cfg(unix)]
        (Ok(x), Ok(y)) => {
            use std::os::unix::fs::MetadataExt as _;
            (x.dev(), x.ino()) == (y.dev(), y.ino())
        }
        #[cfg(not(unix))]
        (Ok(_), Ok(_)) => std::fs::canonicalize(a).ok() == std::fs::canonicalize(b).ok(),
        _ => false,
    }
}

/// `maestro flow driver-skill`: the driver skill generated from a definition
/// (`maestro_flow::driver`), printed, written to `out`, or with `check` compared with `out`. An
/// `out` that is the definition itself, by any path, is refused before anything is written.
fn driver_skill(json: bool, definition: &std::path::Path, out: Option<&std::path::Path>, check: bool) -> Result<()> {
    if let Some(path) = out
        && same_file(path, definition)
    {
        bail!("--out {} is the definition itself; nothing was written", path.display());
    }
    let text = std::fs::File::open(definition)
        .map_err(anyhow::Error::from)
        .and_then(read_definition)
        .with_context(|| format!("reading {}", definition.display()))?;
    let flow = maestro_flow::kernel::compile(&text).map_err(|e| anyhow::anyhow!("{}: {e}", e.code()))?;
    let skill = maestro_flow::driver::generate(&flow).map_err(|e| anyhow::anyhow!("{}: {e}", e.code()))?;
    let shown = out.map(|p| p.display().to_string());
    match out {
        None if json => println!("{}", json!({ "ok": true, "skill": skill, "path": null })),
        None => print!("{skill}"),
        Some(path) if check => {
            let on_disk = std::fs::read(path).ok();
            if on_disk.as_deref() != Some(skill.as_bytes()) {
                if json {
                    println!("{}", json!({ "ok": false, "code": "drift", "path": shown }));
                }
                bail!(
                    "{} drifts from the driver skill {} generates: run maestro flow driver-skill --out",
                    path.display(),
                    flow.name()
                );
            }
            if json {
                println!("{}", json!({ "ok": true, "skill": skill, "path": shown }));
            }
        }
        Some(path) => {
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
            }
            std::fs::write(path, &skill).with_context(|| format!("writing {}", path.display()))?;
            if json {
                println!("{}", json!({ "ok": true, "skill": skill, "path": shown }));
            }
        }
    }
    Ok(())
}

/// `maestro dogfood verify-receipt`: the receipt's check (`maestro_flow::dogfood`) over the outputs
/// on stdin, read to one byte past their bound. One JSON line on stdout either way; a failure's
/// reason on stderr too, where the kernel hands it back to the performer, and its own exit status.
fn verify_receipt(head: &str, surface: &str) -> std::process::ExitCode {
    let mut input = Vec::new();
    let limit = maestro_flow::dogfood::MAX_OUTPUTS as u64 + 1;
    if let Err(e) = std::io::stdin().lock().take(limit).read_to_end(&mut input) {
        input = format!("stdin could not be read ({e})").into_bytes();
    }
    match maestro_flow::dogfood::verify(&input, head, surface) {
        Ok(accepted) => {
            let mut v = json!(accepted);
            v["ok"] = json!(true);
            println!("{v}");
            std::process::ExitCode::SUCCESS
        }
        Err(failure) => {
            println!("{}", json!({ "ok": false, "code": failure.code(), "reason": failure.to_string() }));
            eprintln!("maestro: {}: {failure}", failure.code());
            std::process::ExitCode::from(failure.exit())
        }
    }
}

fn doctor(cli: &Cli, store: &Store, bstack: Option<&std::path::Path>) -> Result<()> {
    use maestro_store::doctor::{self, Status};
    let bstack = bstack.map_or_else(|| store.paths.workspace.join("bstack"), std::path::Path::to_path_buf);
    let checks = doctor::registry(&bstack, store.tree.source.clone());
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&json!({ "checks": checks }))?);
    } else {
        for c in &checks {
            let status = match c.status {
                Status::Ok => "ok    ",
                Status::Stale => "stale ",
                Status::Unread => "unread",
            };
            println!("{status} {:<8} {}", c.name, c.detail);
        }
    }
    if checks.iter().any(|c| c.status != Status::Ok) {
        bail!("the vendored path reader is not known to be current");
    }
    Ok(())
}

fn serve(store: Store, http: bool, bind: Option<std::net::SocketAddr>) -> Result<()> {
    use maestro_mcp::http::{self, Bind};
    if !http {
        bail!("`serve` needs --http (stdio is `maestro mcp`)");
    }
    let bind = Bind::from_arg(bind)?;
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let listener = http::listen(bind).await.with_context(|| format!("listening on {}", bind.addr()))?;
        let local = listener.local_addr()?;
        let cancel = tokio_util::sync::CancellationToken::new();
        let stop = cancel.clone();
        let signalled = shutdown_signal()?;
        tokio::spawn(async move {
            signalled.await;
            stop.cancel();
        });
        eprintln!("maestro: serving MCP on http://{local}{}", http::PATH);
        http::serve(store, listener, cancel).await?;
        Ok(())
    })
}

/// Ctrl-C, or SIGTERM from a service manager. The handlers are installed when this returns,
/// before the readiness line, so a stop sent the moment the daemon reports ready is caught.
#[cfg(unix)]
fn shutdown_signal() -> std::io::Result<impl std::future::Future<Output = ()>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    Ok(async move {
        tokio::select! {
            _ = term.recv() => {}
            _ = interrupt.recv() => {}
        }
    })
}

#[cfg(windows)]
fn shutdown_signal() -> std::io::Result<impl std::future::Future<Output = ()>> {
    let mut ctrl_c = tokio::signal::windows::ctrl_c()?;
    Ok(async move {
        ctrl_c.recv().await;
    })
}

fn read(cli: &Cli, store: &Store) -> Result<()> {
    let (tool, args) = match &cli.command {
        Command::Snapshot => ("maestro_snapshot", json!({})),
        Command::Needs { project } => ("maestro_needs_list", json!({ "project": project })),
        Command::Asks { command: Some(AsksCommand::Show { ledger, id }), .. } => {
            ("maestro_ask_get", json!({ "ledger": ledger, "id": id }))
        }
        Command::Asks { state, project, blocking, limit, .. } => {
            ("maestro_asks_list", json!({ "state": state, "project": project, "blocking": blocking, "limit": limit }))
        }
        Command::Work { state, limit } => ("maestro_work_list", json!({ "state": state, "limit": limit })),
        Command::Loops { id, drifting } => ("maestro_loops_get", json!({ "id": id, "drifting": drifting })),
        Command::Initiatives { all } => ("maestro_initiatives_list", json!({ "all": all })),
        Command::Fleet => ("maestro_fleet_get", json!({})),
        _ => unreachable!("handled in main"),
    };
    let v = call(store, tool, args)?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    match tool {
        "maestro_snapshot" => human_snapshot(&v),
        "maestro_needs_list" => {
            print_errors(&v);
            let Some(list) = v["data"].as_array().cloned() else {
                println!("Needs you: {}", unavailable(&errors_of(&v)));
                return Ok(());
            };
            let bound = if v["errors"].as_array().is_some_and(|e| !e.is_empty()) { "at least " } else { "" };
            println!("Needs you: {bound}{}", list.len());
            for n in &list {
                println!("  {}  {}", s(&n["key"]), s(&n["title"]));
            }
        }
        "maestro_asks_list" => {
            print_errors(&v);
            if v["readable"].as_bool() == Some(false) {
                println!("asks: {}", unavailable(&errors_of(&v)));
                return Ok(());
            }
            let list = v["asks"].as_array().cloned().unwrap_or_default();
            let total = v["total"].as_u64().unwrap_or(list.len() as u64);
            let from = partial(&errors_of(&v));
            if v["truncated"].as_bool() == Some(true) {
                println!("{} of {total} asks{from} (raise --limit for the rest)", list.len());
            } else {
                println!("{total} ask{}{from}", if total == 1 { "" } else { "s" });
            }
            for a in &list {
                let flag = if a["blocking"].as_bool() == Some(true) { "blocking " } else { "" };
                println!("  {}#{}  {flag}{}", s(&a["ledger"]), s(&a["id"]), s(&a["title"]));
            }
        }
        "maestro_ask_get" => println!("{}", serde_json::to_string_pretty(&v)?),
        "maestro_work_list" => {
            print_errors(&v);
            if v["readable"].as_bool() == Some(false) {
                println!("work: {}", unavailable(&errors_of(&v)));
                return Ok(());
            }
            for w in v["items"].as_array().into_iter().flatten() {
                println!("  {:<10} {}  {}", s(&w["work_state"]), s(&w["slug"]), s(&w["title"]));
            }
        }
        "maestro_loops_get" => {
            print_errors(&v);
            for l in v["data"].as_array().into_iter().flatten() {
                let last = &l["last_tick"];
                let tick = if last.is_null() {
                    "no tick receipts".to_string()
                } else {
                    format!("last tick {} {}", s(&last["started_at"]), s(&last["outcome"]))
                };
                println!("  {:<28} {:<16} {:<8} {tick}", s(&l["id"]), s(&l["substrate"]), s(&l["desired"]));
                for d in l["drift"].as_array().into_iter().flatten() {
                    println!("      drift {}: {}", s(&d["class"]), s(&d["detail"]));
                }
                for n in l["notices"].as_array().into_iter().flatten() {
                    println!("      notice {}: {}", s(&n["kind"]), s(&n["detail"]));
                }
            }
        }
        "maestro_initiatives_list" => {
            print_errors(&v);
            for i in v["data"].as_array().into_iter().flatten() {
                println!("  {:<24} {:<17} {}", s(&i["id"]), s(&i["status"]), s(&i["title"]));
                println!("      next: {}", s(&i["next_action"]));
            }
        }
        "maestro_fleet_get" => {
            print_errors(&v);
            for f in v["data"].as_array().into_iter().flatten() {
                println!(
                    "  {:<12} tick {} at {}  mode {}  sessions {}  open asks {}",
                    s(&f["scope"]),
                    f["tick"],
                    s(&f["ts"]),
                    s(&f["mode"]),
                    f["sessions"],
                    f["asks_open"]
                );
            }
        }
        _ => println!("{v}"),
    }
    Ok(())
}

fn human_snapshot(v: &Value) {
    println!("Maestro snapshot  {}  tree {}", s(&v["generated_at"]), s(&v["tree"]));
    // Unread is not empty (BRO-3322): an unread section prints why, never its counts, and a
    // partial one says its counts come from a partial read, and why. It claims no bound: some
    // counts (loop drift, notices) are derived from what a read did not find.
    let section = |name: &str, line: String| {
        let sec = &v[name];
        let errors = sec["errors"].as_array().cloned().unwrap_or_default();
        let (mark, line) = if sec["data"].is_null() {
            ("unreadable", unavailable(&errors))
        } else if !errors.is_empty() {
            ("partial", format!("{line}{}", partial(&errors)))
        } else {
            ("ok", line)
        };
        println!("  {name:<12} {mark:<11} {line}");
    };
    let needs = match v["needs"]["data"].as_array() {
        None => "unknown".to_string(),
        Some(list) => format!("{} hold{} on the owner", list.len(), if list.len() == 1 { "" } else { "s" }),
    };
    section("needs", needs);
    let a = &v["asks"]["data"];
    // Reading the working tree itself, nothing lands onto it and maestro/answers is not read: no
    // count is made. A count that could not be read says so, never 0.
    let landing = if v["tree"] == "worktree" {
        String::new()
    } else if a["unlanded"].is_null() {
        format!(", answered in this checkout or on {}: not known", maestro_store::views::ANSWERS_READ)
    } else {
        format!(
            ", {} answered in this checkout or on {}, not yet landed",
            a["unlanded"],
            maestro_store::views::ANSWERS_READ
        )
    };
    section(
        "asks",
        format!(
            "{} open ({} blocking{landing}), {} answered, {} ledgers",
            a["open"], a["open_blocking"], a["answered"], a["ledgers"]
        ),
    );
    let w = &v["work"]["data"];
    let work = w
        .as_object()
        .map(|m| m.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();
    section("work", work);
    let loops = v["loops"]["data"].as_array().cloned().unwrap_or_default();
    let count = |key: &str| loops.iter().filter(|l| l[key].as_array().is_some_and(|d| !d.is_empty())).count();
    section(
        "loops",
        format!("{} registered, {} with drift, {} with notices", loops.len(), count("drift"), count("notices")),
    );
    let inits = v["initiatives"]["data"].as_array().map_or(0, Vec::len);
    section("initiatives", format!("{inits} live"));
    let fleet = v["fleet"]["data"].as_array().cloned().unwrap_or_default();
    let fleet_line =
        fleet.iter().map(|f| format!("{} tick {}", s(&f["scope"]), f["tick"])).collect::<Vec<_>>().join(", ");
    section("fleet", fleet_line);
}

/// `unavailable (<first reason>)`, naming how many more there are.
fn unavailable(errors: &[Value]) -> String {
    if errors.is_empty() { "unavailable".to_string() } else { format!("unavailable ({})", reasons(errors)) }
}

/// The first read error, where and why, and how many more there are.
fn reasons(errors: &[Value]) -> String {
    let one = |e: &Value| format!("{}: {}", s(&e["at"]), s(&e["reason"]));
    match errors {
        [] => String::new(),
        [e] => one(e),
        [e, rest @ ..] => format!("{}; and {} more, use --json", one(e), rest.len()),
    }
}

/// `, from a partial read (<where>: <why>)`, or nothing for a whole read.
fn partial(errors: &[Value]) -> String {
    if errors.is_empty() { String::new() } else { format!(", from a partial read ({})", reasons(errors)) }
}

/// A command's read errors, from its JSON.
fn errors_of(v: &Value) -> Vec<Value> {
    v["errors"].as_array().cloned().unwrap_or_default()
}

fn context(cli: &Cli, store: &Store) -> Result<String> {
    let Command::Context { cwd, project, initiative, format, event, stdin, max_needs } = &cli.command else {
        unreachable!()
    };
    let mut cwd = cwd.clone();
    if *stdin && !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        if let Ok(payload) = serde_json::from_str::<Value>(&buf) {
            cwd = cwd.or_else(|| payload.get("cwd").and_then(|c| c.as_str()).map(str::to_string));
        }
    }
    if cwd.is_none() {
        cwd = std::env::current_dir().ok().map(|p| p.display().to_string());
    }
    let v = call(
        store,
        "maestro_context",
        json!({ "cwd": cwd, "project": project, "initiative": initiative, "max_needs": max_needs }),
    )?;
    let markdown = v["markdown"].as_str().context("context has no markdown")?.to_string();
    Ok(match format {
        Format::Markdown => markdown,
        Format::Json => serde_json::to_string_pretty(&v["packet"])?,
        Format::Hook => json!({
            "hookSpecificOutput": { "hookEventName": event, "additionalContext": markdown }
        })
        .to_string(),
    })
}

fn authority(as_json: bool) {
    let tools: Vec<Value> =
        maestro_mcp::TOOLS.iter().map(|t| json!({ "tool": t.name, "authority": t.authority })).collect();
    let owner: Vec<Value> = OwnerVerb::ALL
        .iter()
        .map(|v| json!({ "verb": v, "authority": Authority::OwnerAction, "undo_ms": if v.undoable() { UNDO_MS } else { 0 }, "exposed": false }))
        .collect();
    if as_json {
        let inv: Vec<Value> = INVARIANTS.iter().map(|(id, rule)| json!({ "id": id, "rule": rule })).collect();
        println!("{}", json!({ "tools": tools, "owner_verbs": owner, "invariants": inv }));
        return;
    }
    println!("Read tools (any caller):");
    for t in maestro_mcp::TOOLS {
        println!("  {}", t.name);
    }
    println!("Owner verbs (not exposed until an authenticated owner path exists):");
    for v in OwnerVerb::ALL {
        println!("  {}", serde_json::to_value(v).ok().and_then(|x| x.as_str().map(str::to_string)).unwrap_or_default());
    }
    println!("Standing invariants:");
    for (id, rule) in INVARIANTS {
        println!("  {id}: {rule}");
    }
}

fn hosts(as_json: bool) {
    let surfaces: Vec<Value> =
        Surface::KNOWN.iter().map(|s| json!({ "surface": s, "capabilities": surface_capabilities(s) })).collect();
    let harnesses: Vec<Value> =
        Harness::KNOWN.iter().map(|h| json!({ "harness": h, "capabilities": harness_capabilities(h) })).collect();
    let node = local_node();
    if as_json {
        println!("{}", json!({ "node": node, "surfaces": surfaces, "harnesses": harnesses }));
        return;
    }
    match &node {
        Some(n) => println!("Node: {} ({})", n.name.as_deref().unwrap_or("unnamed"), n.id),
        None => println!("Node: unknown (no tailscale; set MAESTRO_NODE to its stable node ID)"),
    }
    for (title, rows, key) in [("Surfaces", &surfaces, "surface"), ("Harnesses", &harnesses, "harness")] {
        println!("{title}:");
        for r in rows {
            println!("  {:<12} {}", s(&r[key]), true_keys(&r["capabilities"]));
        }
    }
    println!("These are what each surface and harness can do. Maestro launches and sends nothing yet (BRO-3276).");
}

/// This machine: `MAESTRO_NODE` (a stable node ID), else `tailscale status`, else unknown.
fn local_node() -> Option<Node> {
    if let Some(id) = std::env::var("MAESTRO_NODE").ok().map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) {
        return Some(Node { id: NodeId(id), name: None });
    }
    let out = run_bounded(
        std::process::Command::new("tailscale").args(["status", "--json", "--peers=false"]),
        std::time::Duration::from_secs(3),
    )?;
    Node::from_tailscale_status(&serde_json::from_slice(&out).ok()?)
}

/// A child's stdout if it exits 0 and its output closes within `limit`; otherwise nothing,
/// and a child still running is killed.
fn run_bounded(cmd: &mut std::process::Command, limit: std::time::Duration) -> Option<Vec<u8>> {
    use std::process::Stdio;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = tx.send(stdout.read_to_end(&mut buf).map(|_| buf));
    });
    let deadline = std::time::Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait().ok()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    // A grandchild can hold the pipe open after the child exits: the deadline covers the read too.
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    let buf = rx.recv_timeout(left).ok()?.ok()?;
    status.success().then_some(buf)
}

fn true_keys(caps: &Value) -> String {
    caps.as_object()
        .map(|m| {
            m.iter().filter(|(_, v)| v.as_bool() == Some(true)).map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(", ")
        })
        .unwrap_or_default()
}

fn validate(store: &Store, file: &PathBuf) -> Result<()> {
    let script = store.paths.workspace.join("scripts/ask_ledger.py");
    if !script.is_file() {
        bail!("validator of record not found at {}", script.display());
    }
    let status = std::process::Command::new("python3")
        .arg("-I")
        .arg(&script)
        .arg("validate")
        .arg(file)
        .status()
        .with_context(|| format!("running {}", script.display()))?;
    std::process::exit(status.code().unwrap_or(1));
}
