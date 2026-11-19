//! The web pages (BRO-3588) over a real loopback listener, as `maestro serve --http` runs
//! them: each page renders its block from the ledgers with no script and no form, and the
//! daemon refuses a foreign `Host`, a cross-site browser request and any write method.

use maestro_mcp::http::{self, Bind};
use maestro_store::{Paths, Store, TreeSource};
use maestro_view::page;
use std::net::SocketAddr;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

fn fixture() -> Store {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/machine");
    Store::new(Paths::under(root), TreeSource::WorkingTree)
}

async fn start(store: Store) -> (SocketAddr, CancellationToken) {
    let cancel = CancellationToken::new();
    let listener = http::listen(Bind::named("127.0.0.1:0".parse().unwrap()).unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(http::serve(store, listener, cancel.clone()));
    (addr, cancel)
}

/// One HTTP/1.1 exchange, read to the end: the status, the headers (names lowercased) and the body.
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

async fn send(addr: SocketAddr, method: &str, path: &str, host: &str, extra: &[(&str, &str)]) -> Reply {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    for (n, v) in extra {
        req.push_str(&format!("{n}: {v}\r\n"));
    }
    if method != "GET" {
        req.push_str("Content-Length: 0\r\n");
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").expect("a response head");
    let mut lines = head.lines();
    let status = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Reply { status, headers, body: body.to_string() }
}

fn assert_page(r: &Reply, what: &str, current: &str) {
    assert_eq!(r.status, 200, "{what}: {}", r.body);
    assert_eq!(r.header("content-type"), Some("text/html; charset=utf-8"), "{what}");
    assert_eq!(r.header("content-security-policy"), Some(page::WEB_CSP), "{what}");
    assert_eq!(r.header("x-frame-options"), Some("DENY"), "{what}");
    assert_eq!(r.header("cache-control"), Some("no-store"), "{what}");
    assert!(r.body.contains(r#"<main id="maestro-root" data-web="true">"#), "{what}: no root");
    assert!(r.body.contains(r#"class="mv-screen""#), "{what}: no rendered screen");
    assert!(
        r.body.contains(&format!(r#"href="{current}" aria-current="page""#)),
        "{what}: nav does not mark {current}"
    );
    // Read-only and script-free: no script, no form, nothing that posts.
    for banned in ["<script", "<form", "formaction", "onclick"] {
        assert!(!r.body.contains(banned), "{what}: carries {banned}");
    }
}

#[tokio::test]
async fn each_page_renders_its_block_from_the_ledgers() {
    let (addr, cancel) = start(fixture()).await;
    let host = addr.to_string();

    let r = send(addr, "GET", "/decisions", &host, &[]).await;
    assert_page(&r, "decisions", "/decisions");
    assert!(r.body.contains("Merge the demo PR now"), "the inbox lists the fixture's open ask");

    let r = send(addr, "GET", "/decisions/demo-arc.yaml/merge-now", &host, &[]).await;
    assert_page(&r, "decision", "/decisions");
    assert!(r.body.contains("Merge now"), "the decision shows the ask's options");
    // The approved layout (BRO-3783): the shell's count, the queue beside the decision, its row current.
    assert!(r.body.contains(r#"<span class="mv-nav-count">"#), "the shell counts the open asks");
    assert!(
        r.body.contains(r#"<a href="/decisions/demo-arc.yaml/merge-now" aria-current="true">"#),
        "the queue marks the decision shown: {}",
        r.body
    );

    for (path, what) in [("/today", "today"), ("/health", "health")] {
        assert_page(&send(addr, "GET", path, &host, &[]).await, what, path);
    }
    cancel.cancel();
}

#[tokio::test]
async fn the_root_redirects_to_decisions_and_the_manifest_starts_there() {
    let (addr, cancel) = start(fixture()).await;
    let host = addr.to_string();
    let r = send(addr, "GET", "/", &host, &[]).await;
    assert_eq!((r.status, r.header("location")), (303, Some("/decisions")));

    let r = send(addr, "GET", "/manifest.webmanifest", &host, &[]).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-type"), Some("application/manifest+json"));
    let m: serde_json::Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(m["start_url"], "/decisions");
    cancel.cancel();
}

#[tokio::test]
async fn a_foreign_host_or_a_cross_site_request_is_refused() {
    let (addr, cancel) = start(fixture()).await;
    let host = addr.to_string();
    for path in ["/", "/decisions", "/today", "/health", "/manifest.webmanifest", "/decisions/demo-arc.yaml/merge-now"]
    {
        let r = send(addr, "GET", path, "evil.example", &[]).await;
        assert_eq!(r.status, 403, "{path} with a foreign Host");
        let r = send(addr, "GET", path, &host, &[("Sec-Fetch-Site", "cross-site")]).await;
        assert_eq!(r.status, 403, "{path} from another site");
        assert!(!r.body.contains("mv-screen"), "{path}: a refusal renders nothing");
    }
    // A browser navigating on its own (typed URL, bookmark) is served.
    assert_eq!(send(addr, "GET", "/decisions", "localhost", &[("Sec-Fetch-Site", "none")]).await.status, 200);
    cancel.cancel();
}

#[tokio::test]
async fn no_page_takes_a_write_and_unknown_paths_are_not_found() {
    let (addr, cancel) = start(fixture()).await;
    let host = addr.to_string();
    for method in ["POST", "PUT", "DELETE", "PATCH"] {
        for path in ["/", "/decisions", "/decisions/demo-arc.yaml/merge-now", "/today", "/health"] {
            assert_eq!(send(addr, method, path, &host, &[]).await.status, 405, "{method} {path}");
        }
    }
    assert_eq!(send(addr, "GET", "/answer", &host, &[]).await.status, 404);
    // A decision whose props fail the catalog's validation is no page.
    let long = "x".repeat(300);
    assert_eq!(send(addr, "GET", &format!("/decisions/{long}/a"), &host, &[]).await.status, 404);
    cancel.cancel();
}

#[tokio::test]
async fn an_absolute_target_or_a_second_host_is_refused() {
    let (addr, cancel) = start(fixture()).await;
    let host = addr.to_string();
    // Routed by its path, an absolute-form target would pass a Host check it never named.
    let r = send(addr, "GET", "http://evil.example/decisions", &host, &[]).await;
    assert_eq!(r.status, 403, "absolute-form target");
    let r = send(addr, "GET", "/decisions", &host, &[("Host", "evil.example")]).await;
    assert!(r.status == 403 || r.status == 400, "two Host headers: {}", r.status);
    assert!(!r.body.contains("mv-screen"));
    for bad in ["localhost:not-a-port", "user@localhost"] {
        assert_eq!(send(addr, "GET", "/decisions", bad, &[]).await.status, 403, "{bad}");
    }
    assert_eq!(send(addr, "GET", "/decisions", "localhost.", &[]).await.status, 200, "a fully qualified loopback name");
    cancel.cancel();
}

#[tokio::test]
async fn a_missing_ask_is_not_found_but_an_unread_ledger_still_renders() {
    let (addr, cancel) = start(fixture()).await;
    let host = addr.to_string();
    // The ledger read in full and holds no such ask.
    let r = send(addr, "GET", "/decisions/demo-arc.yaml/gone", &host, &[]).await;
    assert_eq!(r.status, 404, "{}", r.body);
    // A ledger that did not parse may hold it: that is unavailable, rendered, never "not found".
    let r = send(addr, "GET", "/decisions/broken.yaml/anything", &host, &[]).await;
    assert_page(&r, "unread ledger", "/decisions");
    cancel.cancel();
}

/// A server whose decisions draft answers in `thread`, as `MAESTRO_ANSWER_THREAD` would set it.
async fn start_with_thread(thread: &str) -> (SocketAddr, CancellationToken) {
    let cancel = CancellationToken::new();
    let listener = http::listen(Bind::named("127.0.0.1:0".parse().unwrap()).unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let thread = maestro_view::handoff::AnswerThread::parse(thread).unwrap();
    let router = maestro_mcp::web::router_with(maestro_mcp::MaestroServer::new(fixture()), addr, Some(thread));
    let stop = cancel.clone();
    tokio::spawn(async move { axum::serve(listener, router).with_graceful_shutdown(stop.cancelled_owned()).await });
    (addr, cancel)
}

#[tokio::test]
async fn an_open_decision_drafts_each_answer_in_the_t3_thread() {
    let (addr, cancel) = start_with_thread("env-1/mcp:abc").await;
    let host = addr.to_string();
    let r = send(addr, "GET", "/decisions/demo-arc.yaml/merge-now", &host, &[]).await;
    assert_page(&r, "handoff", "/decisions");
    assert!(r.body.contains(r#"data-handoff="t3""#), "no handoff");
    for option in ["merge", "hold"] {
        assert!(r.body.contains(&format!(r#"data-option="{option}""#)), "{option}");
        // The exact answer, as the owner reads it before sending.
        assert!(r.body.contains(&format!("answer_option: &quot;{option}&quot;")), "{option}: exact answer");
    }
    assert!(
        r.body.contains(r#"href="t3code://threads/env-1/mcp:abc?prompt=Owner%20answer%20for%20one%20Maestro%20ask"#)
    );
    // The ledger Maestro read, and the ask as that ledger addresses it.
    assert!(r.body.contains("path: &quot;.control/asks/demo-arc.yaml&quot;"), "ledger path");
    assert!(
        r.body.contains(
            r#"maestro_ask_get: {&quot;id&quot;:&quot;merge-now&quot;,&quot;ledger&quot;:&quot;demo-arc.yaml&quot;}"#
        ),
        "re-read args"
    );
    assert!(r.body.contains(r#"select: {&quot;id&quot;:&quot;merge-now&quot;,&quot;uid&quot;:null}"#), "selector");
    assert!(r.body.contains(r#"href="https://app.t3.codes/env-1/mcp:abc?prompt=Owner%20answer"#));
    // Answered and idea asks get no handoff; nothing on the page writes.
    for done in ["/decisions/demo-arc.yaml/old-one", "/decisions/demo-arc.yaml/pick-name"] {
        let r = send(addr, "GET", done, &host, &[]).await;
        assert_page(&r, done, "/decisions");
        assert!(!r.body.contains("data-handoff"), "{done}");
    }
    cancel.cancel();
}

#[tokio::test]
async fn without_an_answer_thread_the_decision_stays_read_only() {
    let cancel = CancellationToken::new();
    let listener = http::listen(Bind::named("127.0.0.1:0".parse().unwrap()).unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = maestro_mcp::web::router_with(maestro_mcp::MaestroServer::new(fixture()), addr, None);
    let stop = cancel.clone();
    tokio::spawn(async move { axum::serve(listener, router).with_graceful_shutdown(stop.cancelled_owned()).await });
    let r = send(addr, "GET", "/decisions/demo-arc.yaml/merge-now", &addr.to_string(), &[]).await;
    assert_page(&r, "no thread", "/decisions");
    assert!(!r.body.contains("data-handoff") && !r.body.contains("t3code://"));
    cancel.cancel();
}

/// A T3 registry (`statev2.sqlite`) with one live thread waiting on one AskUserQuestion card.
fn t3_with_a_waiting_card(dir: &Path) {
    std::fs::create_dir_all(dir.join("userdata")).unwrap();
    let script = r#"
import json, sqlite3, sys
conn = sqlite3.connect(sys.argv[1])
conn.executescript("""
CREATE TABLE orchestration_v2_projection_threads (
  thread_id TEXT PRIMARY KEY, project_id TEXT, title TEXT, default_provider TEXT,
  active_provider_thread_id TEXT, created_at TEXT, updated_at TEXT, archived_at TEXT,
  deleted_at TEXT, payload_json TEXT);
CREATE TABLE orchestration_v2_projection_runtime_requests (
  runtime_request_id TEXT, thread_id TEXT, kind TEXT, status TEXT, created_at TEXT, payload_json TEXT);
CREATE TABLE orchestration_v2_projection_turn_items (
  turn_item_id TEXT, thread_id TEXT, type TEXT, payload_json TEXT);
""")
conn.execute("INSERT INTO orchestration_v2_projection_threads VALUES (?,?,?,?,?,?,?,?,?,?)",
    ("mcp:q", "p", "t", "claude", None, "2026-11-19T00:00:00Z", "2026-11-19T00:00:00Z", None, None,
     json.dumps({"worktreePath": "/w"})))
conn.execute("INSERT INTO orchestration_v2_projection_runtime_requests VALUES (?,?,?,?,?,?)",
    ("req-1", "mcp:q", "user_input", "pending", "2026-11-19T10:00:00Z", "{}"))
conn.execute("INSERT INTO orchestration_v2_projection_turn_items VALUES (?,?,?,?)",
    ("item-1", "mcp:q", "user_input_request",
     json.dumps({"requestId": "req-1", "questions": [{"header": "Deploy", "question": "Ship tonight?"}]})))
conn.commit()
"#;
    let out = std::process::Command::new("python3")
        .arg("-I")
        .arg("-c")
        .arg(script)
        .arg(dir.join("userdata/statev2.sqlite"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// The "needs you" pill's value on `/today`.
fn needs_pill(body: &str) -> String {
    let at = body.find(r#"data-pill="needs you""#).expect("the needs pill");
    let rest = &body[at..];
    let v = rest.find(r#"class="mv-pill-value"#).expect("a value");
    let open = rest[v..].find('>').unwrap() + v + 1;
    rest[open..open + rest[open..].find('<').unwrap()].to_string()
}

async fn serve_with(server: maestro_mcp::MaestroServer) -> (SocketAddr, CancellationToken) {
    let cancel = CancellationToken::new();
    let listener = http::listen(Bind::named("127.0.0.1:0".parse().unwrap()).unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(http::serve_server(server, listener, cancel.clone()));
    (addr, cancel)
}

/// BRO-3936 (P20 round 2): the owner's pages count a waiting AskUserQuestion card a live T3
/// session holds, and every surface that lists needs lists it: the daemon's own registry reads
/// T3, `/today`'s "needs you" counts one more than without it, `/decisions` lists it in "Needs
/// you" once no ledger ask is open, and `maestro_ui_render` and `maestro_needs_list` list it.
#[tokio::test]
async fn a_waiting_question_card_reaches_the_pages() {
    let t3 = tempfile::tempdir().unwrap();
    t3_with_a_waiting_card(t3.path());
    let with_t3 = || maestro_hosts::SurfaceRegistry::new(None, Some(maestro_hosts::T3Registry::new(t3.path())), None);
    let without_t3 = maestro_hosts::SurfaceRegistry::new(None, None, None);

    let (addr, cancel) = serve_with(maestro_mcp::MaestroServer::new(fixture()).shared().with_hosts(without_t3)).await;
    let before = needs_pill(&send(addr, "GET", "/today", &addr.to_string(), &[]).await.body);
    cancel.cancel();
    let server = maestro_mcp::MaestroServer::new(fixture()).shared().with_hosts(with_t3());
    let (addr, cancel) = serve_with(server.clone()).await;
    let r = send(addr, "GET", "/today", &addr.to_string(), &[]).await;
    assert_page(&r, "today", "/today");
    let after = needs_pill(&r.body);
    assert_eq!(after.parse::<usize>().unwrap(), before.parse::<usize>().unwrap() + 1, "{before} -> {after}");
    cancel.cancel();

    // A machine whose asks are all answered: `/decisions` falls back to the Needs card, which lists the card.
    let calm = tempfile::tempdir().unwrap();
    copy_tree(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/machine"), calm.path());
    let asks = calm.path().join("workspace/.control/asks");
    for entry in std::fs::read_dir(&asks).unwrap() {
        std::fs::remove_file(entry.unwrap().path()).unwrap();
    }
    let store = Store::new(Paths::under(calm.path().to_path_buf()), TreeSource::WorkingTree);
    let (addr, cancel) = serve_with(maestro_mcp::MaestroServer::new(store).shared().with_hosts(with_t3())).await;
    let r = send(addr, "GET", "/decisions", &addr.to_string(), &[]).await;
    assert_page(&r, "decisions", "/decisions");
    assert!(r.body.contains("Question: Deploy: Ship tonight?"), "the card is in Needs you: {}", r.body);
    assert!(r.body.contains("question:t3:"), "keyed by its session: {}", r.body);
    assert!(!r.body.contains(r#"class="mv-calm""#), "a waiting card is not calm: {}", r.body);
    cancel.cancel();

    let rendered = server
        .call("maestro_ui_render", serde_json::json!({ "root": "n", "blocks": { "n": { "type": "needs" } } }))
        .unwrap();
    assert!(rendered["html"].as_str().unwrap().contains("Question: Deploy: Ship tonight?"), "{rendered}");
    // The poll is the render's second call site: it renders the same live holds.
    let spec = serde_json::json!({ "root": "n", "blocks": { "n": { "type": "needs" } } });
    let polled = server.call("maestro_ui_poll", serde_json::json!({ "spec": spec })).unwrap();
    assert!(polled.to_string().contains("Question: Deploy: Ship tonight?"), "{polled}");
    let needs = server.call("maestro_needs_list", serde_json::json!({})).unwrap();
    assert!(needs.to_string().contains("Question: Deploy: Ship tonight?"), "{needs}");
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}
