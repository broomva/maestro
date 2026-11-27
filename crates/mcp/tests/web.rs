//! The web pages (BRO-3588) over a real loopback listener, as `maestro serve --http` runs
//! them: each page renders its block from the ledgers with one script, the owner script pinned
//! by hash, and no form; the daemon refuses a foreign `Host`, a cross-site browser request and
//! any write method but the owner's passkey-signed answer (BRO-4042).

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
    body_bytes: Vec<u8>,
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
    let body = extra.iter().find(|(n, _)| *n == "x-body").map_or("", |(_, v)| *v);
    if method != "GET" {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req = req.replace(&format!("x-body: {body}\r\n"), "");
    req.push_str("\r\n");
    req.push_str(body);
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let body_at = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let body_bytes = raw[body_at..].to_vec();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").expect("a response head");
    let mut lines = head.lines();
    let status = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Reply { status, headers, body: body.to_string(), body_bytes }
}

fn assert_page(r: &Reply, what: &str, current: &str) {
    assert_eq!(r.status, 200, "{what}: {}", r.body);
    assert_eq!(r.header("content-type"), Some("text/html; charset=utf-8"), "{what}");
    assert_eq!(r.header("content-security-policy"), Some(page::web_csp()), "{what}");
    assert_eq!(r.header("x-frame-options"), Some("DENY"), "{what}");
    assert_eq!(r.header("cache-control"), Some("no-store"), "{what}");
    assert!(r.body.contains(r#"<main id="maestro-root" data-web="true">"#), "{what}: no root");
    assert!(r.body.contains(r#"class="mv-screen""#), "{what}: no rendered screen");
    assert!(
        r.body.contains(&format!(r#"href="{current}" aria-current="page""#)),
        "{what}: nav does not mark {current}"
    );
    // One script, the owner script the policy names by hash; no form, no handler.
    assert_eq!(r.body.matches("<script").count(), 1, "{what}: one script");
    assert!(r.body.contains(&format!("<script>{}</script>", page::OWNER_JS)), "{what}: the pinned script");
    for banned in ["<form", "formaction", "onclick"] {
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
    // The fixture holds no flow ledger: the Flows page says it was not read, never that none ran.
    let r = send(addr, "GET", "/flows", &host, &[]).await;
    assert_page(&r, "flows", "/flows");
    assert!(r.body.contains("The flow ledger was not read"), "{}", r.body);
    assert!(!r.body.contains("No run is listed"));
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
    assert_eq!(m["id"], "/decisions");
    assert_eq!(m["icons"][0]["sizes"], "192x192");
    assert_eq!(m["icons"][1]["sizes"], "512x512");
    assert_eq!(m["icons"][1]["purpose"], "any maskable");
    for path in ["/apple-touch-icon.png", "/icon-192.png", "/icon-512.png"] {
        let icon = send(addr, "GET", path, &host, &[]).await;
        assert_eq!(icon.status, 200, "{path}");
        assert_eq!(icon.header("content-type"), Some("image/png"));
        assert!(icon.body_bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "{path}");
    }
    cancel.cancel();
}

#[tokio::test]
async fn a_foreign_host_or_a_cross_site_request_is_refused() {
    let (addr, cancel) = start(fixture()).await;
    let host = addr.to_string();
    for path in [
        "/",
        "/decisions",
        "/today",
        "/health",
        "/manifest.webmanifest",
        "/apple-touch-icon.png",
        "/icon-192.png",
        "/icon-512.png",
        "/decisions/demo-arc.yaml/merge-now",
    ] {
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
        for path in ["/", "/decisions", "/decisions/demo-arc.yaml/merge-now", "/today", "/health", "/flows"] {
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

/// A server whose decisions take the owner's answer at `origin`, with passkeys in `dir`.
async fn start_owner(
    dir: &Path,
    origin: &str,
    enrolled: bool,
) -> (SocketAddr, CancellationToken, maestro_mcp::owner_web::Owner) {
    let cancel = CancellationToken::new();
    let listener = http::listen(Bind::named("127.0.0.1:0".parse().unwrap()).unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let owner_dir = dir.join("owner");
    if enrolled {
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::write(
            owner_dir.join("passkeys.json"),
            r#"{"credentials":[{"id":"Y3JlZA","public_key":"x","enrolled_at":"t","rp_id":"localhost"}]}"#,
        )
        .unwrap();
    }
    let writer = maestro_mcp::answers::Writer {
        workspace: dir.join("ws"),
        asks: ".control/asks".into(),
        checkout: dir.join("co"),
        owner_dir: owner_dir.clone(),
        t3: None,
        fallback_thread: None,
        remote: false,
    };
    let party = maestro_mcp::passkey::Party::parse(origin).unwrap();
    let owner = maestro_mcp::owner_web::Owner::new(party, owner_dir, writer);
    let router = maestro_mcp::web::router_with(maestro_mcp::MaestroServer::new(fixture()), addr, Ok(owner.clone()));
    let stop = cancel.clone();
    tokio::spawn(async move { axum::serve(listener, router).with_graceful_shutdown(stop.cancelled_owned()).await });
    (addr, cancel, owner)
}

#[tokio::test]
async fn an_open_decision_takes_its_answer_in_place_once_a_passkey_is_set_up() {
    let d = tempfile::tempdir().unwrap();
    let (addr, cancel, _) = start_owner(d.path(), "http://localhost:7181", true).await;
    let r = send(addr, "GET", "/decisions/demo-arc.yaml/merge-now", "localhost:7181", &[]).await;
    assert_page(&r, "answering", "/decisions");
    assert!(r.body.contains(r#"data-answer-bar="true""#), "no answer bar");
    for option in ["merge", "hold"] {
        assert!(r.body.contains(&format!(r#"name="mv-answer-option" value="{option}""#)), "{option}: a choice");
    }
    // Never preselected, and no T3 hand-off.
    let inputs: Vec<&str> = r.body.split("<input").skip(1).map(|t| t.split('>').next().unwrap_or("")).collect();
    assert!(!inputs.is_empty() && inputs.iter().all(|t| !t.contains("checked")), "preselected");
    assert!(!r.body.contains("t3code://") && !r.body.contains("data-handoff"));
    // The right host on another port is another address too.
    let r = send(addr, "GET", "/decisions/demo-arc.yaml/merge-now", "localhost:8181", &[]).await;
    assert!(!r.body.contains(r#"name="mv-answer-option""#), "controls offered on another port");
    // At another address than the secure one, the page links there instead.
    let r = send(addr, "GET", "/decisions/demo-arc.yaml/merge-now", &addr.to_string(), &[]).await;
    assert_page(&r, "elsewhere", "/decisions");
    assert!(
        r.body.contains(r#"href="http://localhost:7181/decisions/demo-arc.yaml/merge-now""#),
        "no link to the secure page"
    );
    assert!(!r.body.contains(r#"name="mv-answer-option""#));
    // An answered ask and an idea that is answered take no answer.
    let r = send(addr, "GET", "/decisions/demo-arc.yaml/old-one", "localhost:7181", &[]).await;
    assert_page(&r, "answered", "/decisions");
    assert!(!r.body.contains(r#"name="mv-answer-option""#) && !r.body.contains(r#"data-answer-bar="true""#));
    cancel.cancel();
}

#[tokio::test]
async fn before_a_passkey_is_set_up_the_decision_links_to_setup() {
    let d = tempfile::tempdir().unwrap();
    let (addr, cancel, owner) = start_owner(d.path(), "http://localhost:7181", false).await;
    let r = send(addr, "GET", "/decisions/demo-arc.yaml/merge-now", "localhost:7181", &[]).await;
    assert_page(&r, "enroll", "/decisions");
    assert!(r.body.contains(r#"href="/owner""#) && !r.body.contains(r#"name="mv-answer-option""#));
    let r = send(addr, "GET", "/owner", "localhost:7181", &[]).await;
    assert_eq!(r.status, 200);
    assert!(r.body.contains(r#"data-enroll="true""#));
    // Setup needs the printed code.
    let post = |body: String| async move {
        send(
            addr,
            "POST",
            "/owner/enroll/begin",
            "localhost:7181",
            &[("Origin", "http://localhost:7181"), ("Content-Type", "application/json"), ("x-body", &body)],
        )
        .await
    };
    assert_eq!(post(r#"{"code":"NOTTHIS1"}"#.into()).await.status, 403);
    let ok = post(format!(r#"{{"code":"{}"}}"#, owner.setup_code().unwrap())).await;
    assert_eq!(ok.status, 200, "{}", ok.body);
    assert!(ok.body.contains(r#""rp_id":"localhost""#));
    cancel.cancel();
}

#[tokio::test]
async fn the_owner_routes_take_only_json_from_the_page_itself() {
    let d = tempfile::tempdir().unwrap();
    let (addr, cancel, _) = start_owner(d.path(), "http://localhost:7181", true).await;
    let body = r#"{"ledger":"demo-arc.yaml","handle":"merge-now","hash":"stale","option":"merge","note":""}"#;
    let json = ("Content-Type", "application/json");
    let mine = ("Origin", "http://localhost:7181");
    let begin = |h: Vec<(&'static str, &'static str)>| async move {
        let mut h = h;
        h.push(("x-body", body));
        send(addr, "POST", "/owner/answer/begin", "localhost:7181", &h).await
    };
    // Another origin, no origin, or not JSON: refused before anything is read.
    assert_eq!(begin(vec![("Origin", "https://evil.example"), json]).await.status, 403);
    assert_eq!(begin(vec![json]).await.status, 403);
    assert_eq!(begin(vec![mine, ("Content-Type", "text/plain")]).await.status, 403);
    assert_eq!(begin(vec![mine, json, ("Sec-Fetch-Site", "cross-site")]).await.status, 403);
    // From the page: a stale hash is refused as changed.
    let r = begin(vec![mine, json]).await;
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(r.body.contains("changed since you opened it"), "{}", r.body);
    // A finish with no request behind it is refused; an unknown receipt is not found.
    let fin = r#"{"nonce":"never","assertion":{"credential":"Y3JlZA","client_data":"e30","authenticator_data":"AA","signature":"AA"}}"#;
    let r = send(addr, "POST", "/owner/answer/finish", "localhost:7181", &[mine, json, ("x-body", fin)]).await;
    assert_eq!(r.status, 403, "{}", r.body);
    let r = send(addr, "GET", "/owner/answer/nothing", "localhost:7181", &[]).await;
    assert_eq!(r.status, 404);
    cancel.cancel();
}

#[tokio::test]
async fn without_a_secure_origin_the_decision_says_why_it_takes_no_answer() {
    let (addr, cancel) = start(fixture()).await;
    let r = send(addr, "GET", "/decisions/demo-arc.yaml/merge-now", &addr.to_string(), &[]).await;
    assert_page(&r, "no origin", "/decisions");
    assert!(r.body.contains("Answering is not available on this page"));
    assert!(!r.body.contains(r#"name="mv-answer-option""#) && !r.body.contains("t3code://"));
    let r = send(
        addr,
        "POST",
        "/owner/answer/begin",
        &addr.to_string(),
        &[("Origin", "http://localhost"), ("Content-Type", "application/json"), ("x-body", "{}")],
    )
    .await;
    assert_eq!(r.status, 503);
    cancel.cancel();
}
