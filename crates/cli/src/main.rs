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
    Hosts,
    /// Self-checks: is the vendored path reader the one bstack generates from its registry?
    Doctor {
        /// The bstack checkout to compare with, read at --tree. Default: <workspace>/bstack.
        #[arg(long)]
        bstack: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum AsksCommand {
    /// One ask in full, with its content hash.
    Show { ledger: String, id: String },
    /// Validate a ledger with the workspace's ask_ledger.py, the validator of record.
    Validate { file: PathBuf },
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Markdown,
    Json,
    /// `{"hookSpecificOutput": {...}}` for Claude Code style hooks; never fails the session.
    Hook,
}

fn store(cli: &Cli) -> Result<Store> {
    let mut paths = Paths::from_env().map_err(|e| anyhow::anyhow!("the path registry: {e}"))?;
    if let Some(ws) = &cli.workspace {
        paths.workspace = ws.clone();
    }
    let source: TreeSource = cli.tree.parse().map_err(|e: String| anyhow::anyhow!("--tree: {e}"))?;
    Ok(Store::new(paths, source))
}

fn call(store: &Store, tool: &str, args: Value) -> Result<Value> {
    maestro_mcp::call(store, tool, args).map_err(|e| anyhow::anyhow!("{}", e.message))
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

/// How long the hook may take before it gives up silently (`MAESTRO_HOOK_TIMEOUT_MS`, default 5000).
fn hook_budget() -> std::time::Duration {
    let ms = std::env::var("MAESTRO_HOOK_TIMEOUT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(5_000);
    std::time::Duration::from_millis(ms)
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let hook = is_hook(&args);
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(_) if hook => return std::process::ExitCode::SUCCESS,
        Err(e) => e.exit(),
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

    match run(&cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("maestro: {e:#}");
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
        _ => read(cli, &store)?,
    }
    Ok(())
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
                println!("Needs you: unknown (the ask ledgers could not be read)");
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
                println!("asks: unreadable ({})", s(&v["source"]));
                return Ok(());
            }
            let list = v["asks"].as_array().cloned().unwrap_or_default();
            let total = v["total"].as_u64().unwrap_or(list.len() as u64);
            if v["truncated"].as_bool() == Some(true) {
                println!("{} of {total} asks (raise --limit for the rest)", list.len());
            } else {
                println!("{total} ask{}", if total == 1 { "" } else { "s" });
            }
            for a in &list {
                let flag = if a["blocking"].as_bool() == Some(true) { "blocking " } else { "" };
                println!("  {}#{}  {flag}{}", s(&a["ledger"]), s(&a["id"]), s(&a["title"]));
            }
        }
        "maestro_ask_get" => println!("{}", serde_json::to_string_pretty(&v)?),
        "maestro_work_list" => {
            print_errors(&v);
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
    let section = |name: &str, line: String| {
        let sec = &v[name];
        let mark = if sec["data"].is_null() {
            "unreadable"
        } else if sec["errors"].as_array().is_some_and(|e| !e.is_empty()) {
            "partial"
        } else {
            "ok"
        };
        println!("  {name:<12} {mark:<11} {line}");
    };
    let needs = match v["needs"]["data"].as_array() {
        None => "unknown".to_string(),
        Some(list) => format!("{} hold{} on the owner", list.len(), if list.len() == 1 { "" } else { "s" }),
    };
    section("needs", needs);
    let a = &v["asks"]["data"];
    section(
        "asks",
        format!(
            "{} open ({} blocking, {} answered locally and waiting to land), {} answered, {} ledgers",
            a["open"], a["open_blocking"], a["unlanded"], a["answered"], a["ledgers"]
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
