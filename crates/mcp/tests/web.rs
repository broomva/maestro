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
