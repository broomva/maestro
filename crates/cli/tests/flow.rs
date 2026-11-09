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
