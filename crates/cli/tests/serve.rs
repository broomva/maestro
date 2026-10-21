//! End to end: `maestro serve --http` binds where it may, answers MCP, and stops on SIGTERM.
//! Unix only: the stops are signals sent with `kill`.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn maestro() -> Command {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/machine");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_maestro"));
    cmd.env("MAESTRO_WORKSPACE", root.join("workspace"))
        .env("MAESTRO_WORK_ROOT", root.join("work"))
        .env("BROOMVA_HOME", root.join("broomva-home"))
        .env("MAESTRO_FLEET_STATE", root.join("fleet-reconcile"))
        .env("MAESTRO_TREE", "worktree");
    cmd
}

#[test]
fn a_non_loopback_non_tailnet_bind_is_refused() {
    for addr in ["0.0.0.0:7171", "[::]:7171", "192.168.1.10:7171"] {
        let out = maestro().args(["serve", "--http", "--bind", addr]).output().unwrap();
        assert!(!out.status.success(), "{addr}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("refusing to listen on"), "{addr}: {err}");
    }
}

#[test]
fn serve_needs_the_http_flag() {
    let out = maestro().args(["serve", "--bind", "127.0.0.1:0"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("needs --http"));
}

/// Start `serve --http` on an ephemeral loopback port; returns the child and its `host:port`.
fn start() -> (Child, String) {
    let mut child = maestro()
        .args(["serve", "--http", "--bind", "127.0.0.1:0"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    err.read_line(&mut line).unwrap();
    let url = line.trim().strip_prefix("maestro: serving MCP on http://").unwrap_or_else(|| panic!("{line}"));
    let (addr, path) = url.split_once('/').unwrap();
    assert!(addr.starts_with("127.0.0.1:"), "{addr}");
    assert_eq!(path, "mcp");
    (child, addr.to_string())
}

#[test]
fn serve_http_answers_tools_list_and_stops_on_sigterm() {
    let (mut child, addr) = start();
    let addr = addr.as_str();

    // A 2026-07-28 request needs no session: one POST, one answer.
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2026-07-28\r\nMcp-Method: tools/list\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    // Read to EOF, or for at most 5 s: an event stream would stay open, and this test must fail, not hang.
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    let response = String::from_utf8_lossy(&raw).into_owned();
    assert!(response.starts_with("HTTP/1.1 200"), "{response:?}");
    // A stateless request-response is plain JSON, not an event stream (`with_json_response`;
    // legacy sessions still answer as an event stream).
    assert!(response.to_ascii_lowercase().contains("\r\ncontent-type: application/json\r\n"), "{response:?}");
    assert!(response.contains("\"maestro_snapshot\""), "{response}");

    let killed = Command::new("kill").args(["-TERM", &child.id().to_string()]).status().unwrap();
    assert!(killed.success());
    let status = child.wait().unwrap();
    assert!(status.success(), "SIGTERM should be a clean stop: {status}");
}

#[test]
fn a_client_stalled_mid_body_does_not_hold_the_daemon_past_its_grace() {
    let (mut child, addr) = start();
    // Headers promise 500 bytes; the body stops after 11 and the socket stays open.
    let mut stalled = TcpStream::connect(&addr).unwrap();
    write!(
        stalled,
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: 500\r\n\r\n{{\"jsonrpc\":"
    )
    .unwrap();
    stalled.flush().unwrap();
    std::thread::sleep(Duration::from_millis(300));

    let asked = Instant::now();
    assert!(Command::new("kill").args(["-TERM", &child.id().to_string()]).status().unwrap().success());
    let deadline = asked + maestro_mcp::http::SHUTDOWN_GRACE + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("still running {:?} after SIGTERM with a stalled client", asked.elapsed());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status}");
    drop(stalled);
}

#[test]
fn sigterm_right_after_the_readiness_line_is_a_clean_stop() {
    // A service manager may stop the daemon the moment it reports ready: the handler must exist by then.
    // bash's builtin `kill` lands within microseconds of the line; spawning `kill` is too slow to see the race.
    // Probabilistic: the old order failed 12-19 of 20 here on an idle machine, but none under heavy load.
    let script = r#"bad=0
for i in $(seq 1 20); do
  coproc M { exec "$BIN" serve --http --bind 127.0.0.1:0 2>&1; }
  pid=$M_PID
  read -r line <&"${M[0]}"
  kill -TERM "$pid"
  wait "$pid" || bad=$((bad + 1))
done
echo "bad=$bad""#;
    let daemon = maestro();
    let out = Command::new("bash")
        .envs(daemon.get_envs().filter_map(|(k, v)| Some((k, v?))))
        .env("BIN", daemon.get_program())
        .args(["-c", script])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), "bad=0", "SIGTERMs right after the readiness line killed the daemon by signal");
}
