//! End to end: the built binary speaks MCP over stdio and serves the hook format.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

fn fixture() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/machine")
}

fn maestro() -> Command {
    let root = fixture();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_maestro"));
    cmd.env("MAESTRO_WORKSPACE", root.join("workspace"))
        .env("MAESTRO_WORK_ROOT", root.join("work"))
        .env("BROOMVA_HOME", root.join("broomva-home"))
        .env("MAESTRO_FLEET_STATE", root.join("fleet-reconcile"))
        .env("MAESTRO_TREE", "worktree");
    cmd
}

#[test]
fn mcp_handshake_lists_and_calls_tools() {
    let mut child =
        maestro().arg("mcp").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut send = |v: Value| {
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    };
    let mut recv = || {
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };

    send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}));
    let init = recv();
    assert_eq!(init["result"]["serverInfo"]["name"], "maestro");
    assert!(init["result"]["capabilities"]["prompts"].is_object());
    send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));

    send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    let tools = recv();
    let names: Vec<_> =
        tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    assert!(names.contains(&"maestro_needs_list".to_string()));
    assert!(!names.iter().any(|n| n.contains("answer")), "no owner verb is listed");

    send(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"maestro_needs_list","arguments":{}}}));
    let needs = recv();
    assert_eq!(needs["result"]["structuredContent"]["data"].as_array().unwrap().len(), 1);

    send(
        json!({"jsonrpc":"2.0","id":4,"method":"prompts/get","params":{"name":"maestro_context","arguments":{"project":"demo"}}}),
    );
    let prompt = recv();
    let text = prompt["result"]["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(text.contains("Needs you: 1 open hold"));

    send(json!({"jsonrpc":"2.0","id":5,"method":"resources/read","params":{"uri":"maestro://needs"}}));
    let res = recv();
    assert_eq!(res["result"]["contents"][0]["mimeType"], "application/json");

    send(
        json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"maestro_ask_get","arguments":{"ledger":"nope.yaml","id":"x"}}}),
    );
    let missing = recv();
    assert_eq!(missing["result"]["isError"], true);

    drop(send);
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn hook_format_is_harness_shaped_and_reads_cwd_from_stdin() {
    let mut child = maestro()
        .args(["context", "--format", "hook", "--stdin", "--event", "SessionStart"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let cwd = fixture().join("workspace/apps/demo");
    writeln!(child.stdin.take().unwrap(), "{}", json!({ "cwd": cwd, "hook_event_name": "SessionStart" })).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let v: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    let ctx = v["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(ctx.contains("initiative `demo`"));
}

#[test]
fn hook_never_fails_the_session() {
    let output = maestro()
        .env("MAESTRO_WORKSPACE", "/nonexistent")
        .args(["--tree", "origin/main", "context", "--format", "hook"])
        .output()
        .unwrap();
    assert!(output.status.success());
}
