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
    // The name that wins (BROOMVA_WORKSPACE), and the older ones cleared: an ambient value must not leak in.
    cmd.env("BROOMVA_WORKSPACE", root.join("workspace"))
        .env_remove("MAESTRO_WORKSPACE")
        .env_remove("BSTACK_WORKSPACE")
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
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        [
            "maestro_ask_get",
            "maestro_asks_list",
            "maestro_context",
            "maestro_fleet_get",
            "maestro_initiatives_list",
            "maestro_loops_get",
            "maestro_needs_list",
            "maestro_snapshot",
            "maestro_ui_catalog",
            "maestro_ui_poll",
            "maestro_ui_render",
            "maestro_work_list",
        ],
        "every harness registration answers these tools"
    );
    assert!(!names.iter().any(|n| n.contains("answer")), "no owner verb is listed");

    send(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"maestro_needs_list","arguments":{}}}));
    let needs = recv();
    assert_eq!(needs["result"]["structuredContent"]["data"].as_array().unwrap().len(), 3);

    send(
        json!({"jsonrpc":"2.0","id":4,"method":"prompts/get","params":{"name":"maestro_context","arguments":{"project":"demo"}}}),
    );
    let prompt = recv();
    let text = prompt["result"]["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(text.contains("Needs you: at least 3 open holds"), "{text}");

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
        .env("BROOMVA_WORKSPACE", "/nonexistent")
        .args(["--tree", "origin/main", "context", "--format", "hook"])
        .output()
        .unwrap();
    assert!(output.status.success());
}

#[test]
fn a_bad_argument_on_the_hook_path_still_exits_zero() {
    let output = maestro().args(["context", "--format", "hook", "--bogus"]).output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    // Off the hook path the same mistake is a usage error.
    assert!(!maestro().args(["context", "--bogus"]).output().unwrap().status.success());
}

#[test]
fn the_hook_gives_up_silently_when_over_budget() {
    // --stdin with an open, silent pipe would wait forever; the budget ends it.
    let mut child = maestro()
        .env("MAESTRO_HOOK_TIMEOUT_MS", "300")
        .args(["context", "--format", "hook", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let _keep_open = child.stdin.take();
    let started = std::time::Instant::now();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn hosts_reports_surfaces_and_harnesses_apart() {
    let out = maestro().args(["hosts", "--json"]).env("MAESTRO_NODE", "Test-Node").output().unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["node"], json!({ "id": "Test-Node" }));
    let names = |key: &str, field: &str| -> Vec<String> {
        v[key].as_array().unwrap().iter().map(|r| r[field].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(names("surfaces", "surface"), ["paseo", "t3", "direct"]);
    assert_eq!(names("harnesses", "harness"), ["claude-code", "codex", "gemini", "agy"]);
    for r in v["surfaces"].as_array().unwrap() {
        assert!(r["capabilities"].get("mcp").is_none(), "a surface does not serve MCP: {r}");
    }
    for r in v["harnesses"].as_array().unwrap() {
        assert!(r["capabilities"].get("launch").is_none(), "a harness does not launch sessions: {r}");
    }
}

#[test]
fn a_hung_tailscale_does_not_hang_hosts() {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("tailscale");
    std::fs::write(&fake, "#!/bin/sh\nexec sleep 30\n").unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let started = std::time::Instant::now();
    let out = maestro()
        .args(["hosts", "--json"])
        .env_remove("MAESTRO_NODE")
        .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(started.elapsed() < std::time::Duration::from_secs(10), "took {:?}", started.elapsed());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["node"], Value::Null);
}

fn fake_tailscale(script: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("tailscale");
    std::fs::write(&fake, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    dir
}

fn node_with(dir: &tempfile::TempDir, maestro_node: Option<&str>) -> (Value, std::time::Duration) {
    let mut cmd = maestro();
    cmd.args(["hosts", "--json"]).env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()));
    match maestro_node {
        Some(n) => cmd.env("MAESTRO_NODE", n),
        None => cmd.env_remove("MAESTRO_NODE"),
    };
    let started = std::time::Instant::now();
    let out = cmd.output().unwrap();
    assert!(out.status.success());
    (serde_json::from_slice::<Value>(&out.stdout).unwrap()["node"].clone(), started.elapsed())
}

const STATUS: &str = r#"{"Self":{"ID":"nTest","DNSName":"box.example.ts.net."}}"#;

#[test]
fn a_failed_tailscale_names_no_node_even_with_json() {
    let ok = fake_tailscale(&format!("echo '{STATUS}'"));
    assert_eq!(node_with(&ok, None).0, json!({ "id": "nTest", "name": "box" }));
    let failed = fake_tailscale(&format!("echo '{STATUS}'; exit 1"));
    assert_eq!(node_with(&failed, None).0, Value::Null);
}

#[test]
fn a_blank_maestro_node_falls_through_to_tailscale() {
    let ok = fake_tailscale(&format!("echo '{STATUS}'"));
    assert_eq!(node_with(&ok, Some("  ")).0, json!({ "id": "nTest", "name": "box" }));
    assert_eq!(node_with(&ok, Some(" nPinned ")).0, json!({ "id": "nPinned" }));
}

#[test]
fn a_grandchild_holding_the_pipe_does_not_hang_hosts() {
    let pids = tempfile::tempdir().unwrap();
    let pidfile = pids.path().join("pid");
    let dir = fake_tailscale(&format!("sleep 30 &\necho $! > {}\necho '{STATUS}'", pidfile.display()));
    let (node, took) = node_with(&dir, None);
    let pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_string();
    let _ = Command::new("kill").arg(&pid).status();
    assert!(took < std::time::Duration::from_secs(10), "took {took:?}");
    assert_eq!(node, Value::Null, "output that never closes is not trusted");
}

#[test]
fn a_hung_tailscale_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let bin = fake_tailscale(&format!("echo $$ > {}\nexec sleep 30", pidfile.display()));
    let (node, _) = node_with(&bin, None);
    assert_eq!(node, Value::Null);
    let pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_string();
    let alive = Command::new("kill").args(["-0", &pid]).stderr(Stdio::null()).status().unwrap().success();
    assert!(!alive, "the timed-out tailscale {pid} is still running");
}

/// However long the ticks ledger grows, the hook reads only its tail: a 100 MB ledger whose last
/// lines are the fixture's reads the same context. This debug build parses the 2 MiB tail in about
/// 0.2 s and a full read in about 10 s, so 1 s tells them apart; the 200 ms budget is held by
/// `scripts/tail-bench.sh` on the release binary.
#[test]
fn a_100_mb_ticks_ledger_is_read_by_its_tail() {
    let home = tempfile::tempdir().unwrap();
    let ledger = home.path().join("ledger/loops/ticks.jsonl");
    std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    let fixture_ticks = std::fs::read_to_string(fixture().join("broomva-home/ledger/loops/ticks.jsonl")).unwrap();
    let filler = json!({
        "v": 1, "id": "F", "ts": "2026-09-01T00:00:00Z", "stream": "loops/ticks", "type": "loop.tick.started",
        "actor": "agent:aaaa1111", "subject": "loop:filler", "refs": {}, "cause": null, "data": {}, "prev": "sha256:0",
    })
    .to_string()
        + "\n";
    let mut text = String::with_capacity(101 * 1024 * 1024);
    while text.len() < 100 * 1024 * 1024 {
        text.push_str(&filler);
    }
    text.push_str(&fixture_ticks);
    std::fs::write(&ledger, &text).unwrap();
    drop(text);

    let hook = |home: &Path| {
        let started = std::time::Instant::now();
        let out = maestro()
            .env("BROOMVA_HOME", home)
            .args(["context", "--format", "hook", "--cwd"])
            .arg(fixture().join("workspace/apps/demo"))
            .output()
            .unwrap();
        assert!(out.status.success());
        (started.elapsed(), String::from_utf8(out.stdout).unwrap())
    };
    let (_, small) = hook(&fixture().join("broomva-home"));
    hook(home.path()); // warm the page cache, as a hook run after the ledger's last write finds it
    let (took, big) = hook(home.path());
    assert!(small.contains("demo-tick"), "{small}");
    // The tail holds every fixture tick, so the context is the same, except that a cut tail
    // claims no missing receipts: the ticks before it are unread.
    let receipts = "\n- `silent`: the contract names tick receipts and the ledger holds none for this loop";
    let receipts = serde_json::to_string(receipts).unwrap();
    let receipts = receipts.trim_matches('"');
    assert!(small.contains(receipts), "{small}");
    assert_eq!(big, small.replacen(receipts, "", 1));
    assert!(took < std::time::Duration::from_secs(1), "took {took:?}");
}
