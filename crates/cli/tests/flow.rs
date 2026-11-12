//! `maestro flow` end to end through the built binary (BRO-3634; BRO-3624 item 4): writes need a
//! Direct launch token or a bridge credential, reads need nothing, the JSON is the tool's, and the
//! binary is a guard host (`guard_entry` first in `main`), so a guarded `choose` runs, over the
//! CLI and over the MCP server it serves.

use maestro_store::{Paths, Store, TreeSource};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

/// review (work) -> pick (choice, guarded on the review's outputs) -> ship.
const DECIDE: &str = r#"
flow: decide
version: 1
params: [dir]
workdir: "{{params.dir}}"
nodes:
  review: {class: work, verify: [sh, -c, 'test -f ok']}
  pick: {class: choice, needs: [review], edges: {ship: {guard: '.nodes.review.outputs.verdict == "pass"'}}}
  ship: {class: pure, run: ['true'], switch: {on: pick, case: ship}}
"#;

/// A scratch machine: a workspace with `.broomva/`, the flow's work folder, the definition, and a
/// Direct registry file with one live session in the work folder.
struct Machine {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    work: PathBuf,
}

fn machine() -> Machine {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir_all(root.join("workspace/.broomva")).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(root.join("decide.yaml"), DECIDE).unwrap();
    let registry = json!([{ "id": "d1", "cwd": work, "token": "tok-1", "liveness": "live" }]);
    std::fs::write(root.join("direct.json"), registry.to_string()).unwrap();
    Machine { _tmp: tmp, root, work }
}

impl Machine {
    /// The binary with a clean environment: no session, no registry, no caller claim.
    fn bare(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_maestro"));
        cmd.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.join("home"))
            .env("BROOMVA_WORKSPACE", self.root.join("workspace"))
            .env("BROOMVA_HOME", self.root.join("broomva-home"))
            .env("MAESTRO_FLEET_STATE", self.root.join("fleet"))
            .env("MAESTRO_TREE", "worktree")
            .env("MAESTRO_SESSION", "direct:-/d1");
        cmd
    }

    /// The binary as the verified Direct session `d1`: the registry, its launch token and folder.
    fn verified(&self) -> Command {
        let mut cmd = self.bare();
        cmd.env("MAESTRO_DIRECT_REGISTRY", self.root.join("direct.json"))
            .env("MAESTRO_DIRECT_TOKEN", "tok-1")
            .env("MAESTRO_CALLER_CWD", &self.work);
        cmd
    }

    fn store(&self) -> Store {
        Store::new(Paths::under(&self.root), TreeSource::WorkingTree)
    }

    fn start(&self) -> String {
        let v = ok(self
            .verified()
            .args(["flow", "start", "--definition"])
            .arg(self.root.join("decide.yaml"))
            .args(["--params", &json!({ "dir": self.work }).to_string()]));
        v["instance"].as_str().unwrap().to_string()
    }
}

/// The printed JSON, and the exit status.
fn run(cmd: &mut Command) -> (Value, Output) {
    let out = cmd.output().unwrap();
    let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!("not JSON: {}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    });
    (v, out)
}

fn ok(cmd: &mut Command) -> Value {
    let (v, out) = run(cmd);
    assert!(out.status.success() && v["ok"] == true, "{v}\n{}", String::from_utf8_lossy(&out.stderr));
    v
}

/// A refusal: exit 1, the JSON's code.
fn refused(cmd: &mut Command) -> String {
    let (v, out) = run(cmd);
    assert_eq!(out.status.code(), Some(1), "{v}");
    assert_eq!(v["ok"], false, "{v}");
    v["code"].as_str().unwrap().to_string()
}

#[test]
fn cli_writes_need_a_launch_token_or_a_credential() {
    let m = machine();
    let start = |cmd: &mut Command| {
        cmd.args(["flow", "start", "--definition"])
            .arg(m.root.join("decide.yaml"))
            .args(["--params", &json!({ "dir": m.work }).to_string()]);
    };
    // No registry and no claim: unverified. A registry but no token: refused. Neither writes.
    let mut bare = m.bare();
    start(&mut bare);
    assert_eq!(refused(&mut bare), "caller_unverified");
    let mut no_token = m.bare();
    no_token.env("MAESTRO_DIRECT_REGISTRY", m.root.join("direct.json")).env("MAESTRO_CALLER_CWD", &m.work);
    start(&mut no_token);
    assert_eq!(refused(&mut no_token), "caller_refused");
    let mut wrong = m.verified();
    wrong.env("MAESTRO_DIRECT_TOKEN", "not-the-token");
    start(&mut wrong);
    assert_eq!(refused(&mut wrong), "caller_refused");
    assert!(!m.root.join("workspace/.broomva/flows").exists(), "a refused write made a ledger");

    // The verified session writes; anyone reads; an unverified claim is still refused.
    let fi = m.start();
    let next = ok(m.bare().args(["flow", "next", "--instance", &fi]));
    assert_eq!(next["next"]["enabled"][0]["node"], "review");
    assert_eq!(
        refused(m.bare().args(["flow", "claim", "--instance", &fi, "--node-id", "review"])),
        "caller_unverified"
    );
    ok(m.verified().args(["flow", "claim", "--instance", &fi, "--node-id", "review"]));
    // The human step's verb too (BRO-3635): unverified, refused; verified, it reaches the kernel,
    // which files asks only for a human node.
    let ask = ["flow", "ask", "--instance", &fi, "--node-id", "review"];
    assert_eq!(refused(m.bare().args(ask)), "caller_unverified");
    assert_eq!(refused(m.verified().args(ask)), "not_human");
    // And the proposals: unverified, refused; the owner proposing to adopt its own, refused.
    for verb in ["propose-adoption", "propose-retirement"] {
        assert_eq!(refused(m.bare().args(["flow", verb, "--instance", &fi])), "caller_unverified", "{verb}");
    }
    assert_eq!(refused(m.verified().args(["flow", "propose-adoption", "--instance", &fi])), "already_owner");
}

/// The binary calls `guard_entry` first in `main`, so a guarded choose runs through it (without
/// that, it would be refused guard_host_missing).
#[test]
fn a_guarded_choose_runs_through_the_cli() {
    let m = machine();
    let fi = m.start();
    let review = ok(m.verified().args(["flow", "claim", "--instance", &fi, "--node-id", "review"]));
    let token = review["claim_token"].as_str().unwrap();
    ok(m.verified().args(["flow", "heartbeat", "--claim-token", token]));
    std::fs::write(m.work.join("ok"), "").unwrap();
    let outputs = json!({ "verdict": "pass" }).to_string();
    ok(m.verified().args(["flow", "complete", "--claim-token", token, "--outputs", &outputs]));
    let pick = ok(m.verified().args(["flow", "claim", "--instance", &fi, "--node-id", "pick"]));
    let token = pick["claim_token"].as_str().unwrap();
    let chosen = ok(m.verified().args(["flow", "choose", "--claim-token", token, "--edge", "ship"]));
    assert_eq!(chosen["edge"], "ship");
    let status = ok(m.bare().args(["flow", "status", "--instance", &fi]));
    assert_eq!(status["status"]["nodes"]["ship"]["state"], "ready", "{status}");
}

/// A definition over the kernel's bound is refused for its size as the tool refuses it, JSON and
/// all, from a file or stdin, even where the read's cut splits a character.
#[test]
fn an_oversized_definition_is_the_tools_refusal() {
    let m = machine();
    let mut text = "a".repeat(maestro_flow::kernel::MAX_SOURCE);
    text.push('é');
    std::fs::write(m.root.join("big.yaml"), &text).unwrap();
    let file = m.verified().args(["flow", "start", "--definition"]).arg(m.root.join("big.yaml")).output().unwrap();
    let mut piped = m
        .verified()
        .args(["flow", "start", "--definition", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    piped.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    let piped = piped.wait_with_output().unwrap();
    for out in [file, piped] {
        assert_eq!(out.status.code(), Some(1));
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| panic!("not JSON: {}", String::from_utf8_lossy(&out.stderr)));
        assert_eq!(v["code"], "definition_invalid", "{v}");
        assert!(v["reason"].as_str().unwrap().contains("more than 262144"), "{v}");
    }
}

/// The CLI prints exactly what the tool returns, results and refusals alike.
#[test]
fn the_cli_prints_the_tools_json() {
    let m = machine();
    let fi = m.start();
    let store = m.store();
    for (args, tool, tool_args) in [
        (vec!["flow", "status", "--instance", fi.as_str()], "maestro_flow_status", json!({ "instance": fi })),
        (vec!["flow", "next", "--instance", fi.as_str()], "maestro_flow_next", json!({ "instance": fi })),
        (vec!["flow", "next", "--instance", "fi_missing"], "maestro_flow_next", json!({ "instance": "fi_missing" })),
        (vec!["flow", "next", "--instance", "../x"], "maestro_flow_next", json!({ "instance": "../x" })),
        (vec!["flow", "spine", "--instance", fi.as_str()], "maestro_flow_spine", json!({ "instance": fi })),
        (
            vec!["flow", "spine", "--instance", fi.as_str(), "--against", fi.as_str()],
            "maestro_flow_spine",
            json!({ "instance": fi, "against": fi }),
        ),
        (
            vec!["flow", "spine", "--instance", fi.as_str(), "--against", "fi_missing"],
            "maestro_flow_spine",
            json!({ "instance": fi, "against": "fi_missing" }),
        ),
    ] {
        let (cli, _) = run(m.bare().args(&args));
        let tool = maestro_mcp::call(&store, tool, tool_args).unwrap();
        assert_eq!(cli, tool, "{args:?}");
    }
}

/// `maestro mcp` is the same binary, so the MCP server is a guard host too.
#[test]
fn the_mcp_server_runs_a_guarded_choose() {
    let m = machine();
    std::fs::write(m.work.join("ok"), "").unwrap();
    let mut child = m.verified().arg("mcp").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut id = 0;
    let mut rpc = |method: &str, params: Value| {
        id += 1;
        writeln!(stdin, "{}", json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };
    rpc(
        "initialize",
        json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "0" } }),
    );
    let mut tool = |name: &str, mut args: Value| {
        args["session"] = json!("direct:-/d1");
        let v = rpc("tools/call", json!({ "name": name, "arguments": args }));
        let got = v["result"]["structuredContent"].clone();
        assert_eq!(got["ok"], true, "{name}: {v}");
        got
    };
    let fi =
        tool("maestro_flow_start", json!({ "definition": DECIDE, "params": { "dir": m.work } }))["instance"].clone();
    let review = tool("maestro_flow_claim", json!({ "instance": fi, "node_id": "review" }))["claim_token"].clone();
    tool("maestro_flow_complete", json!({ "claim_token": review, "outputs": { "verdict": "pass" } }));
    let pick = tool("maestro_flow_claim", json!({ "instance": fi, "node_id": "pick" }))["claim_token"].clone();
    assert_eq!(tool("maestro_flow_choose", json!({ "claim_token": pick, "edge": "ship" }))["edge"], "ship");
    drop(stdin);
    let _ = child.wait();
}

/// A tools/call over HTTP ([`http_call`]); its structured result.
fn http_tool(addr: &str, name: &str, args: Value) -> Value {
    http_call(addr, name, args)["structuredContent"].clone()
}

/// One stateless MCP 2026-07-28 tools/call to `maestro serve --http` at `addr`; the whole result.
fn http_call(addr: &str, name: &str, args: Value) -> Value {
    use std::io::Read as _;
    let meta = json!({ "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {} });
    let body =
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": args, "_meta": meta } })
            .to_string();
    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2026-07-28\r\nMcp-Method: tools/call\r\nMcp-Name: {name}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    let response = String::from_utf8_lossy(&raw).into_owned();
    let json = response.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or_default();
    let v: Value = serde_json::from_str(json).unwrap_or_else(|_| panic!("{response}"));
    v["result"].clone()
}

/// A shared daemon (`serve --http`) takes no caller claim from its own environment: a caller
/// naming the session its variables describe is refused, and verifies only with its own claim.
#[test]
fn the_http_daemon_takes_no_claim_from_its_environment() {
    let m = machine();
    // A Paseo session in the work folder, and a valid signed credential for it, issued with the
    // daemon's own bridge secret: the claim its variables would make.
    let agents = m.root.join("agents");
    std::fs::create_dir_all(agents.join("work")).unwrap();
    let agent = json!({
        "id": "agent-flow", "cwd": m.work, "provider": "claude", "createdAt": "2026-11-07T00:00:00Z",
        "lastStatus": "running", "pid": 4101, "startedAt": "2026-11-07T00:00:01Z",
        "persistence": { "sessionId": "handle-1" }
    });
    std::fs::write(agents.join("work/agent-flow.json"), agent.to_string()).unwrap();
    let issuer = maestro_hosts::SurfaceRegistry::new(Some(maestro_hosts::PaseoRegistry::new(&agents)), None, None)
        .with_bridge_secret("daemon-secret");
    let claim = maestro_hosts::CallerClaim::paseo(Some("agent-flow"), Some(m.work.to_string_lossy().into_owned()))
        .with_peer(maestro_hosts::PeerCredentials { pid: std::process::id(), uid: 1000 });
    let verified = issuer.verify_caller(&claim).unwrap();
    let now = maestro_hosts::current_time_ms();
    let node: maestro_hosts::NodeId = "node-flow".parse().unwrap();
    let credential = issuer.issue_bridge_credential(&verified, node, now - 5_000, now + 600_000).unwrap();
    let mut child = m
        .verified()
        .env("MAESTRO_PASEO_AGENTS_DIR", &agents)
        .env("MAESTRO_BRIDGE_SECRET", "daemon-secret")
        .env("MAESTRO_BRIDGE_CREDENTIAL", serde_json::to_string(&credential).unwrap())
        .args(["serve", "--http", "--bind", "127.0.0.1:0"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    err.read_line(&mut line).unwrap();
    let url = line.trim().strip_prefix("maestro: serving MCP on http://").unwrap_or_else(|| panic!("{line}"));
    let addr = url.split_once('/').unwrap().0.to_string();
    let start = json!({ "session": "direct:-/d1", "definition": DECIDE, "params": { "dir": m.work } });
    let refused = http_tool(&addr, "maestro_flow_start", start.clone());
    assert_eq!(refused["code"], "caller_refused", "{refused}");
    // The daemon's credential is nobody's claim either: a caller that knows the session's folder
    // but holds no credential is refused.
    let paseo =
        json!({ "session": "paseo:-/agent-flow", "cwd": m.work, "definition": DECIDE, "params": { "dir": m.work } });
    let refused = http_tool(&addr, "maestro_flow_start", paseo);
    assert_eq!(refused["code"], "caller_refused", "{refused}");
    assert!(!m.root.join("workspace/.broomva/flows").exists(), "the daemon's variables wrote a flow");
    // Nor for an agent request: a handback naming the session is refused, not recorded verified.
    let handback = http_call(&addr, "maestro_handback", json!({ "session": "direct:-/d1", "summary": "s" }));
    assert_eq!(handback["isError"], true, "{handback}");
    assert!(handback.to_string().contains("launch token is required"), "{handback}");
    assert!(!m.root.join("broomva-home/ledger/requests/requests.jsonl").exists(), "the handback was journalled");
    let mut own = start;
    own["token"] = json!("tok-1");
    own["cwd"] = json!(m.work);
    let begun = http_tool(&addr, "maestro_flow_start", own);
    assert_eq!(begun["ok"], true, "{begun}");
    let _ = child.kill();
    let _ = child.wait();
}

/// A human node's ask filed through the CLI end to end (BRO-3635): `--ledger` and `--ask` reach the
/// kernel, which files through the writer of record (here the stub) and links the row.
#[test]
fn an_ask_is_filed_through_the_cli() {
    let m = machine();
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/requests");
    let ws = m.root.join("workspace");
    std::fs::create_dir_all(ws.join(".control/asks")).unwrap();
    std::fs::create_dir_all(ws.join("scripts")).unwrap();
    std::fs::copy(fixtures.join("ledger.yaml"), ws.join(".control/asks/ledger.yaml")).unwrap();
    std::fs::copy(fixtures.join("ask_ledger_stub.py"), ws.join("scripts/ask_ledger.py")).unwrap();
    std::fs::write(
        m.root.join("gate.yaml"),
        "flow: gate\nversion: 1\nnodes:\n  owner: {class: human, ask: Ship it?}\n",
    )
    .unwrap();
    // The scratch HOME hides this machine's Python packages (PyYAML) from the writer.
    let user_base = Command::new("python3").args(["-m", "site", "--user-base"]).output().unwrap();
    let user_base = String::from_utf8(user_base.stdout).unwrap().trim().to_string();
    let cli = || {
        let mut cmd = m.verified();
        cmd.env("PYTHONUSERBASE", &user_base).env("MAESTRO_PASEO_ASK_LOCKS", m.root.join("locks"));
        cmd
    };
    let begun = ok(cli().args(["flow", "start", "--definition"]).arg(m.root.join("gate.yaml")));
    let fi = begun["instance"].as_str().unwrap().to_string();
    let body = json!({
        "class": "decision", "gates": ["fixture"], "blocking": false, "default": "hold", "preanswerable": false,
        "exhausted": ["read: a fixture", "default: a fixture"],
    });
    let ask = ["flow", "ask", "--instance", &fi, "--node-id", "owner", "--ledger", "ledger.yaml", "--ask"];
    let v = ok(cli().args(ask).arg(body.to_string()));
    assert_eq!(v["state"], "waiting", "{v}");
    let id = v["ask_id"].as_str().unwrap();
    let text = std::fs::read_to_string(ws.join(".control/asks/ledger.yaml")).unwrap();
    assert_eq!(text.matches(&format!("id: \"{id}\"")).count(), 1, "filed once");
    let status = ok(m.bare().args(["flow", "status", "--instance", &fi]));
    assert_eq!(status["relay"][0]["state"], "owner", "{status}");
}

/// A proposal to adopt an instance filed through the CLI end to end (BRO-3635): another verified
/// session's `--ledger` and `--ask` reach the kernel, which files the ask through the writer of
/// record (here the stub) and links it; the owner and the work are as they were.
#[test]
fn a_proposal_is_filed_through_the_cli() {
    let m = machine();
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/requests");
    let ws = m.root.join("workspace");
    std::fs::create_dir_all(ws.join(".control/asks")).unwrap();
    std::fs::create_dir_all(ws.join("scripts")).unwrap();
    std::fs::copy(fixtures.join("ledger.yaml"), ws.join(".control/asks/ledger.yaml")).unwrap();
    std::fs::copy(fixtures.join("ask_ledger_stub.py"), ws.join("scripts/ask_ledger.py")).unwrap();
    let registry = json!([
        { "id": "d1", "cwd": m.work, "token": "tok-1", "liveness": "live" },
        { "id": "d2", "cwd": m.work, "token": "tok-2", "liveness": "live" },
    ]);
    std::fs::write(m.root.join("direct.json"), registry.to_string()).unwrap();
    let user_base = Command::new("python3").args(["-m", "site", "--user-base"]).output().unwrap();
    let user_base = String::from_utf8(user_base.stdout).unwrap().trim().to_string();
    let fi = m.start();
    let mut other = m.verified();
    other
        .env("MAESTRO_SESSION", "direct:-/d2")
        .env("MAESTRO_DIRECT_TOKEN", "tok-2")
        .env("PYTHONUSERBASE", &user_base)
        .env("MAESTRO_PASEO_ASK_LOCKS", m.root.join("locks"));
    let body = json!({
        "class": "decision", "gates": ["fixture"], "blocking": false, "stakes": "The owner is gone.",
        "why_you": "Only the owner hands a flow over.", "exhausted": ["read: a fixture", "default: a fixture"],
    });
    let v = ok(other
        .args(["flow", "propose-adoption", "--instance", &fi, "--ledger", "ledger.yaml", "--ask"])
        .arg(body.to_string()));
    assert_eq!((&v["state"], &v["proposal"]), (&json!("waiting"), &json!("adoption")), "{v}");
    let id = v["ask_id"].as_str().unwrap();
    assert!(id.starts_with("flow-adopt-"), "{id}");
    let text = std::fs::read_to_string(ws.join(".control/asks/ledger.yaml")).unwrap();
    assert_eq!(text.matches(&format!("id: \"{id}\"")).count(), 1, "filed once");
    let status = ok(m.bare().args(["flow", "status", "--instance", &fi]));
    assert_eq!(status["status"]["owner"], json!({ "surface": "direct", "id": "d1" }), "{status}");
    assert_eq!(status["status"]["proposals"][0]["state"], "owner", "{status}");
    // The owner goes on writing.
    ok(m.verified().args(["flow", "claim", "--instance", &fi, "--node-id", "review"]));
}
