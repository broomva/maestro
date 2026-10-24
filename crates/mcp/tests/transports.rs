//! Every tool scenario of `dispatch.rs`, over stdio and over Streamable HTTP, through a
//! real MCP client: each transport must return the JSON the in-process dispatch returns.
//!
//! Transports: a byte-stream pair served by `serve_io` (the code `maestro mcp` runs on
//! stdin/stdout), and `maestro serve --http`'s service on a loopback port. Each is driven
//! by a client that opens with `initialize` (2025-11-25, a legacy session over HTTP) and by
//! one that opens with `server/discover` (2026-07-28, stateless over HTTP).

use maestro_core::authority::OwnerVerb;
use maestro_mcp::http::{self, Bind};
use maestro_mcp::{MaestroServer, call};
use maestro_store::{Paths, Store, TreeSource};
use rmcp::model::{CallToolRequestParams, GetPromptRequestParams, ProtocolVersion, ReadResourceRequestParams};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt, RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Map, Value, json};
use std::path::Path;
use tokio_util::sync::CancellationToken;

type Client = RunningService<RoleClient, ()>;

fn fixture() -> Store {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/machine");
    Store::new(Paths::under(root), TreeSource::WorkingTree)
}

fn initialize() -> ClientLifecycleMode {
    ClientLifecycleMode::Initialize
}

fn discover() -> ClientLifecycleMode {
    ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] }
}

/// One server per transport for `store`, and a client of each lifecycle on each.
struct Transports {
    clients: Vec<(&'static str, Client)>,
    cancel: CancellationToken,
}

impl Transports {
    async fn start(store: &Store) -> Self {
        let cancel = CancellationToken::new();
        let listener = http::listen(Bind::named("127.0.0.1:0".parse().unwrap()).unwrap()).await.unwrap();
        let url = format!("http://{}{}", listener.local_addr().unwrap(), http::PATH);
        tokio::spawn(http::serve(store.clone(), listener, cancel.clone()));

        let mut clients = Vec::new();
        for (name, lifecycle) in [("stdio/initialize", initialize()), ("stdio/discover", discover())] {
            let (server_end, client_end) = tokio::io::duplex(1 << 20);
            let store = store.clone();
            tokio::spawn(async move { maestro_mcp::serve_io(store, tokio::io::split(server_end)).await });
            clients.push((name, ().serve_with_lifecycle(client_end, lifecycle).await.expect(name)));
        }
        for (name, lifecycle) in [("http/initialize", initialize()), ("http/discover", discover())] {
            let transport =
                StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(url.clone()));
            clients.push((name, ().serve_with_lifecycle(transport, lifecycle).await.expect(name)));
        }
        Self { clients, cancel }
    }

    async fn stop(self) {
        for (_, client) in self.clients {
            let _ = client.cancel().await;
        }
        self.cancel.cancel();
    }

    /// Call `tool` on every client and return each `CallToolResult` as JSON.
    async fn call(&self, tool: &str, args: &Value) -> Vec<(&'static str, Value)> {
        let mut out = Vec::new();
        for (name, client) in &self.clients {
            let mut params = CallToolRequestParams::new(tool.to_string());
            params.arguments = args.as_object().cloned();
            let result = client.call_tool(params).await.unwrap_or_else(|e| panic!("{name} {tool}: {e}"));
            out.push((*name, serde_json::to_value(result).unwrap()));
        }
        out
    }
}

/// What the in-process dispatch says a tool call returns, in `CallToolResult` terms.
fn expected(store: &Store, tool: &str, args: &Value) -> (bool, Value) {
    match call(store, tool, args.clone()) {
        Ok(v) => (false, v),
        Err(e) => (true, Value::String(e.message.to_string())),
    }
}

/// The client name's protocol half: `initialize` or `discover`.
fn lifecycle(name: &str) -> &str {
    name.split_once('/').map_or(name, |(_, l)| l)
}

/// Envelope fields MCP 2026-07-28 adds to a result, which the older protocol does not carry.
const ENVELOPE_2026: &[&str] = &["resultType", "cacheScope", "ttlMs"];

/// The result without the 2026-07-28 envelope fields.
fn payload(v: &Value) -> Value {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut() {
        o.retain(|k, _| !ENVELOPE_2026.contains(&k.as_str()));
    }
    v
}

/// The one field that differs between two reads of the same tree is the snapshot's
/// clock: blank every `generated_at`, also inside JSON carried as text content.
fn unclocked(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), if k == "generated_at" { json!("<clock>") } else { unclocked(v) }))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(unclocked).collect()),
        Value::String(s) if s.starts_with('{') => match serde_json::from_str::<Value>(s) {
            Ok(inner) => Value::String(unclocked(&inner).to_string()),
            Err(_) => v.clone(),
        },
        _ => v.clone(),
    }
}

/// Assert `results` agree: identical JSON between transports on one protocol version,
/// identical payload across versions.
fn assert_identical(what: &str, results: &[(&str, Value)]) {
    let results: Vec<_> = results.iter().map(|(n, v)| (*n, unclocked(v))).collect();
    for (name, got) in &results {
        for (other, theirs) in &results {
            if lifecycle(name) == lifecycle(other) {
                assert_eq!(got, theirs, "{what}: {name} differs from {other}");
            } else {
                assert_eq!(payload(got), payload(theirs), "{what}: {name} payload differs from {other}");
            }
        }
    }
}

/// Every transport agrees with the dispatch, and with every other transport (see [`assert_identical`]).
async fn agree(t: &Transports, store: &Store, tool: &str, args: Value) -> Value {
    let (is_error, want) = expected(store, tool, &args);
    let want = unclocked(&want);
    let results = t.call(tool, &args).await;
    assert_identical(&format!("{tool} {args}"), &results);
    let first = &results[0].1;
    for (name, got) in &results {
        assert_eq!(got["isError"], json!(is_error), "{tool} {args} over {name}: {got}");
        if is_error {
            assert_eq!(got["content"][0]["text"], want, "{tool} {args} over {name}");
        } else {
            assert_eq!(unclocked(&got["structuredContent"]), want, "{tool} {args} over {name}");
        }
    }
    first.clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn every_tool_scenario_is_identical_over_stdio_and_http() {
    let store = fixture();
    let t = Transports::start(&store).await;

    // every_tool_answers_on_the_fixture
    for spec in maestro_mcp::TOOLS {
        let args = match spec.name {
            "maestro_ask_get" => json!({ "ledger": "demo-arc.yaml", "id": "merge-now" }),
            "maestro_ui_render" => json!({ "root": "s", "blocks": { "s": { "type": "snapshot" } } }),
            "maestro_ui_poll" => json!({ "spec": { "root": "s", "blocks": { "s": { "type": "snapshot" } } } }),
            _ => json!({}),
        };
        let got = agree(&t, &store, spec.name, args).await;
        assert_eq!(got["isError"], false, "{}", spec.name);
    }
    // no_owner_verb_is_reachable
    for verb in OwnerVerb::ALL {
        let name = serde_json::to_value(verb).unwrap().as_str().unwrap().replace('.', "_");
        let got = agree(&t, &store, &format!("maestro_{name}"), json!({})).await;
        assert_eq!(got["isError"], true, "{name}");
    }
    agree(&t, &store, "maestro_answer_ask", json!({})).await;
    // asks_list_filters_and_hashes, asks_list_reports_total_and_truncation
    for args in [
        json!({}),
        json!({ "blocking": true, "project": "demo" }),
        json!({ "state": "bogus" }),
        json!({ "state": "all", "limit": 1 }),
    ] {
        agree(&t, &store, "maestro_asks_list", args).await;
    }
    // ask_get, including an_ask_in_an_unreadable_ledger_is_unreadable_not_missing
    for (ledger, id) in [
        ("demo-arc.yaml", "merge-now"),
        ("broken.yaml", "anything"),
        ("alt-key.yaml", "gone"),
        ("demo-arc.yaml", "gone"),
    ] {
        agree(&t, &store, "maestro_ask_get", json!({ "ledger": ledger, "id": id })).await;
    }
    // work_state_filter_uses_canonical_labels
    for state in ["Needs you", "Standing", "review"] {
        agree(&t, &store, "maestro_work_list", json!({ "state": state })).await;
    }
    // context_returns_markdown_and_packet, and its markdown as the text content
    let ctx = agree(&t, &store, "maestro_context", json!({ "project": "demo" })).await;
    assert_eq!(ctx["content"][0]["text"], ctx["structuredContent"]["markdown"]);
    // filters on the other tools
    agree(&t, &store, "maestro_needs_list", json!({ "project": "demo" })).await;
    agree(&t, &store, "maestro_loops_get", json!({ "drifting": true })).await;
    agree(&t, &store, "maestro_initiatives_list", json!({ "all": true })).await;
    agree(&t, &store, "maestro_work_list", json!({ "limit": "x" })).await;
    // ui.rs: a render's text is its summary, a snapshot's text is the document, a bad spec is an error
    let spec = json!({ "root": "m", "blocks": { "m": { "type": "stack", "children": ["a", "x"] },
        "a": { "type": "ask", "props": { "ledger": "demo-arc.yaml", "id": "merge-now" } }, "x": { "type": "nope" } } });
    let r = agree(&t, &store, "maestro_ui_render", spec.clone()).await;
    assert_eq!(r["content"][0]["text"], r["structuredContent"]["text"]);
    let mut snap = spec.clone();
    snap["format"] = json!("snapshot");
    agree(&t, &store, "maestro_ui_render", snap).await;
    let bad = agree(&t, &store, "maestro_ui_render", json!({ "root": "z", "blocks": {} })).await;
    assert_eq!(bad["isError"], true);
    agree(
        &t,
        &store,
        "maestro_ui_poll",
        json!({ "spec": r["structuredContent"]["spec"], "revision": r["structuredContent"]["revision"] }),
    )
    .await;

    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_prompts_and_resources_are_identical_over_every_transport() {
    let store = fixture();
    let t = Transports::start(&store).await;
    let mut seen: Vec<(&'static str, Value)> = Vec::new();
    for (name, client) in &t.clients {
        let tools = client.list_all_tools().await.unwrap();
        let prompts = client.list_all_prompts().await.unwrap();
        let resources = client.list_all_resources().await.unwrap();
        let mut read = Map::new();
        for r in &resources {
            let got = client.read_resource(ReadResourceRequestParams::new(r.uri.clone())).await.unwrap();
            read.insert(r.uri.clone(), serde_json::to_value(got).unwrap());
        }
        let mut args = Map::new();
        args.insert("project".into(), json!("demo"));
        let prompt =
            client.get_prompt(GetPromptRequestParams::new("maestro_context").with_arguments(args)).await.unwrap();
        seen.push((
            name,
            json!({ "tools": tools, "prompts": prompts, "resources": resources, "read": read, "prompt": prompt }),
        ));
    }
    assert_eq!(seen[0].1["tools"].as_array().unwrap().len(), maestro_mcp::TOOLS.len());
    assert_eq!(seen[0].1["tools"], serde_json::to_value(MaestroServer::tools()).unwrap());
    let parts = |key: &str| seen.iter().map(|(n, v)| (*n, v[key].clone())).collect::<Vec<_>>();
    assert_identical("tools/list", &parts("tools"));
    assert_identical("prompts/list", &parts("prompts"));
    assert_identical("resources/list", &parts("resources"));
    for (uri, _, _) in maestro_mcp::RESOURCES {
        let reads = seen.iter().map(|(n, v)| (*n, v["read"][*uri].clone())).collect::<Vec<_>>();
        assert_identical(uri, &reads);
    }
    assert_identical("prompts/get", &parts("prompt"));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreadable_workspace_reads_the_same_over_every_transport() {
    // unreadable_asks_are_not_an_empty_list
    let store = Store::new(Paths::under("/nonexistent/maestro-test"), TreeSource::WorkingTree);
    let t = Transports::start(&store).await;
    for tool in ["maestro_asks_list", "maestro_needs_list", "maestro_context", "maestro_snapshot"] {
        agree(&t, &store, tool, json!({})).await;
    }
    t.stop().await;

    // an_ambiguous_bare_id_is_refused
    let tmp = tempfile::tempdir().unwrap();
    let asks = tmp.path().join("workspace/.control/asks");
    std::fs::create_dir_all(&asks).unwrap();
    std::fs::write(
        asks.join("dup.yaml"),
        "asks:\n  - id: a\n    ask: one?\n  - id: a\n    ask: two?\n  - id: a\n    uid: u1\n    ask: three?\n",
    )
    .unwrap();
    let store = Store::new(Paths::under(tmp.path()), TreeSource::WorkingTree);
    let t = Transports::start(&store).await;
    let refused = agree(&t, &store, "maestro_ask_get", json!({ "ledger": "dup.yaml", "id": "a" })).await;
    assert_eq!(refused["isError"], true);
    agree(&t, &store, "maestro_ask_get", json!({ "ledger": "dup.yaml", "id": "u1" })).await;
    t.stop().await;
}

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_after_origin_main_moves_sees_the_new_commit() {
    // A fixture "remote" and a clone whose origin/main the server reads, as on a real machine.
    let tmp = tempfile::tempdir().unwrap();
    let upstream = tmp.path().join("upstream");
    let asks = upstream.join(".control/asks");
    std::fs::create_dir_all(&asks).unwrap();
    std::fs::write(asks.join("one.yaml"), "asks:\n  - id: a\n    ask: first?\n").unwrap();
    git(&upstream, &["init", "-q", "-b", "main"]);
    git(&upstream, &["add", "."]);
    git(&upstream, &["commit", "-q", "-m", "one"]);
    let ws = tmp.path().join("workspace");
    git(tmp.path(), &["clone", "-q", upstream.to_str().unwrap(), ws.to_str().unwrap()]);

    let mut paths = Paths::under(tmp.path());
    paths.workspace = ws.clone();
    let store = Store::new(paths, TreeSource::GitRev("origin/main".into()));
    let t = Transports::start(&store).await;
    let count = |results: Vec<(&str, Value)>| {
        results.iter().map(|(_, v)| v["structuredContent"]["asks"].as_array().unwrap().len()).collect::<Vec<_>>()
    };
    let all = json!({ "state": "all" });
    assert_eq!(count(t.call("maestro_asks_list", &all).await), [1, 1, 1, 1]);

    // origin/main moves: a new commit upstream, fetched into the clone. The sessions stay open.
    std::fs::write(asks.join("two.yaml"), "asks:\n  - id: b\n    ask: second?\n").unwrap();
    git(&upstream, &["add", "."]);
    git(&upstream, &["commit", "-q", "-m", "two"]);
    git(&ws, &["fetch", "-q", "origin"]);
    assert_eq!(count(t.call("maestro_asks_list", &all).await), [2, 2, 2, 2], "a server answered from the old commit");
    t.stop().await;
}

#[test]
fn only_a_named_address_may_be_non_loopback() {
    // `Bind` has two constructors; the unnamed one is loopback, the named one refuses the rest.
    assert!(Bind::loopback().addr().ip().is_loopback());
    assert!(Bind::named("0.0.0.0:7171".parse().unwrap()).is_err());
    assert!(Bind::named("[::]:7171".parse().unwrap()).is_err());
    assert!(Bind::named("192.168.1.10:7171".parse().unwrap()).is_err());
    assert!(Bind::named("100.101.102.103:7171".parse().unwrap()).is_ok());
}

/// POST one raw request and return the status code.
async fn status(addr: std::net::SocketAddr, headers: &[(&str, &str)]) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;
    let mut req = format!(
        "POST {} HTTP/1.1\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n",
        http::PATH,
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut head = [0u8; 12];
    stream.read_exact(&mut head).await.unwrap();
    std::str::from_utf8(&head[9..12]).unwrap().parse().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn http_refuses_a_foreign_host_and_any_browser_origin() {
    let cancel = CancellationToken::new();
    let listener = http::listen(Bind::named("127.0.0.1:0".parse().unwrap()).unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(http::serve(fixture(), listener, cancel.clone()));
    let host = addr.to_string();
    let port = addr.port().to_string();
    assert_eq!(status(addr, &[("Host", &host)]).await, 200);
    assert_eq!(status(addr, &[("Host", &format!("localhost:{port}"))]).await, 200);
    // DNS rebinding: a name that resolves here but is not ours.
    assert_eq!(status(addr, &[("Host", &format!("evil.example:{port}"))]).await, 403);
    // A browser page, whatever its origin, may not drive the daemon.
    assert_eq!(status(addr, &[("Host", &host), ("Origin", "http://evil.example")]).await, 403);
    assert_eq!(status(addr, &[("Host", &host), ("Origin", &format!("http://{host}"))]).await, 403);
    cancel.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn http_takes_the_bound_address_as_host() {
    // A tailnet bind cannot be made on every machine, so serve the service built for a tailnet
    // address from a loopback socket: `Host` is checked against the address the service was given.
    let tailnet: std::net::SocketAddr = "100.100.100.100:7171".parse().unwrap();
    let cancel = CancellationToken::new();
    let mcp = http::service(MaestroServer::new(fixture()), tailnet, cancel.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, axum::Router::new().nest_service(http::PATH, mcp))
            .with_graceful_shutdown(stop.cancelled_owned())
            .await
    });
    assert_eq!(status(addr, &[("Host", "100.100.100.100:7171")]).await, 200);
    assert_eq!(status(addr, &[("Host", "100.100.100.101:7171")]).await, 403);
    cancel.cancel();
}
