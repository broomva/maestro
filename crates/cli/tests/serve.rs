//! End to end: `maestro serve --http` binds where it may, answers MCP, and stops on SIGTERM.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};

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

#[test]
fn serve_http_answers_tools_list_and_stops_on_sigterm() {
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

    // A 2026-07-28 request needs no session: one POST, one answer.
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2026-07-28\r\nMcp-Method: tools/list\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response:?}");
    assert!(response.contains("\"maestro_snapshot\""), "{response}");

    let killed = Command::new("kill").args(["-TERM", &child.id().to_string()]).status().unwrap();
    assert!(killed.success());
    let status = child.wait().unwrap();
    assert!(status.success(), "SIGTERM should be a clean stop: {status}");
}
