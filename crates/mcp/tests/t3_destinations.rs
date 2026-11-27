//! End-to-end regression tests for BRO-4229: verified T3 destinations at ask filing, pre-signature
//! visibility on decision screens, environment-aware answer wake, transport limitation reporting,
//! explicit fallback, signed ledger integrity, wrong-target prevention, and retry dedupe.

use maestro_hosts::{IdentityMode, SurfaceRegistry, T3CODE_THREAD_ID, T3Registry, ThreadVars};
use maestro_mcp::answers::{self, Book, Intent, Receipt, Stage, T3, Writer};
use maestro_mcp::http::{self, Bind};
use maestro_mcp::passkey::Assertion;
use maestro_store::delivery::DeliveryStatus;
use maestro_store::{Paths, Store, TreeSource};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

const LOCAL_ENV: &str = "env-local-nuc";
const LINKED_ENV: &str = "env-remote-mac";
const UNKNOWN_ENV: &str = "env-rogue";

const FULL_UUID_A: &str = "60f14287-1111-4222-8333-444444444444";
const SHORT_UUID_A: &str = "60f14287";
const FULL_UUID_B: &str = "d054d967-5555-4666-8777-888888888888";
const WORKTREE_LABEL_B: &str = "t3-d054d967";
const HOST_AUTHORED_ID: &str = "mcp:launch:ab%3Acd";
const COLLIDING_ID: &str = "thread-colliding-id";
const FALLBACK_THREAD: &str = "meta-orchestrator-thread";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write_executable(path: &Path, body: &str) {
    use std::io::Write;
    let mut child = Command::new("/bin/sh")
        .args(["-c", r#"cat > "$1" && chmod 0755 "$1""#, "sh"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(body.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());
}

fn setup_t3_fixture(root: &Path) -> T3Registry {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("environment-id"), format!("{LOCAL_ENV}\n")).unwrap();
    let threads = json!([
        {
            "id": FULL_UUID_A,
            "cwd": "/tmp/ws-a",
            "environment_id": LOCAL_ENV,
            "provider": "claudeAgent",
            "created_at": "2026-11-20T00:00:00Z",
            "run_id": "run-a",
        },
        {
            "id": FULL_UUID_B,
            "cwd": "/tmp/t3-d054d967",
            "environment_id": LOCAL_ENV,
            "provider": "claudeAgent",
            "created_at": "2026-11-20T00:00:00Z",
            "run_id": "run-b",
        },
        {
            "id": HOST_AUTHORED_ID,
            "cwd": "/tmp/ws-c",
            "environment_id": LOCAL_ENV,
            "provider": "codex",
            "created_at": "2026-11-20T00:00:00Z",
            "run_id": "run-c",
        },
        {
            "id": COLLIDING_ID,
            "cwd": "/tmp/ws-local-collide",
            "environment_id": LOCAL_ENV,
            "provider": "codex",
            "created_at": "2026-11-20T00:00:00Z",
            "run_id": "run-d",
        },
        {
            "id": FALLBACK_THREAD,
            "cwd": "/tmp/ws-meta",
            "environment_id": LOCAL_ENV,
            "provider": "claudeAgent",
            "created_at": "2026-11-20T00:00:00Z",
            "run_id": "run-meta",
        },
    ]);
    std::fs::write(root.join("threads.json"), serde_json::to_vec(&threads).unwrap()).unwrap();
    std::fs::write(
        root.join("linked-environments.json"),
        serde_json::to_vec(&json!([
            { "environment_id": LINKED_ENV, "fenced": false },
        ]))
        .unwrap(),
    )
    .unwrap();
    T3Registry::new(root)
}

fn setup_ask_store(dir: &Path) -> (Store, PathBuf) {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/requests");
    let workspace = dir.join("workspace");
    let asks = workspace.join(".broomva/asks");
    std::fs::create_dir_all(&asks).unwrap();
    std::fs::create_dir_all(workspace.join("scripts")).unwrap();
    std::fs::copy(fixtures.join("ask_ledger_stub.py"), workspace.join("scripts/ask_ledger.py")).unwrap();
    std::fs::copy(fixtures.join("ledger.yaml"), asks.join("ledger.yaml")).unwrap();
    let origin = dir.join("origin.git");
    git(dir, &["init", "-q", "--bare", origin.to_str().unwrap()]);
    git(&workspace, &["init", "-q", "-b", "main"]);
    git(&workspace, &["add", "."]);
    git(&workspace, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
    git(&workspace, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(&workspace, &["push", "-q", "-u", "origin", "main"]);
    let mut paths = Paths::under(dir);
    paths.workspace = workspace.clone();
    let store = Store::new(paths, TreeSource::WorkingTree);
    (store, fixtures)
}

fn sample_ask(fixtures: &Path, id: &str) -> Value {
    let mut ask: Value = serde_json::from_str(&std::fs::read_to_string(fixtures.join("ask.json")).unwrap()).unwrap();
    ask["id"] = json!(id);
    ask
}

fn found_hosts(t3: T3Registry, vars: &[(&str, &str)]) -> SurfaceRegistry {
    let tv = ThreadVars::from_pairs(vars.iter().map(|(k, v)| (*k, (*v).to_string())));
    SurfaceRegistry::new(None, Some(t3), None)
        .with_t3_found(true)
        .with_identity_mode(IdentityMode::Attributed)
        .with_thread_vars(tv)
}

async fn get_page(addr: SocketAddr, path: &str, host: &str) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").expect("response head");
    let status: u16 = head.lines().next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    (status, body.to_string())
}

#[test]
fn ask_filing_rejects_short_uuid_worktree_label_and_unlinked_env_and_resolves_exact_local_on_shared_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let (store, fixtures) = setup_ask_store(dir.path());
    let t3 = setup_t3_fixture(&dir.path().join("t3"));
    let server = maestro_mcp::MaestroServer::new(store.clone()).with_hosts(found_hosts(t3.clone(), &[])).shared();

    // 1. Short UUID (observed I6 failure) is rejected at filing on the shared daemon.
    let err = server
        .call(
            "maestro_ask_file",
            json!({
                "session": format!("t3:local/{SHORT_UUID_A}"),
                "ledger": "ledger.yaml",
                "ask": sample_ask(&fixtures, "short-uuid-ask"),
            }),
        )
        .unwrap_err();
    assert!(err.message.contains("does not resolve to an open T3 thread"), "{}", err.message);

    // 2. Worktree label (observed Inkblot failure) is rejected at filing on the shared daemon.
    let err = server
        .call(
            "maestro_ask_file",
            json!({
                "session": format!("t3:local/{WORKTREE_LABEL_B}"),
                "ledger": "ledger.yaml",
                "ask": sample_ask(&fixtures, "worktree-label-ask"),
            }),
        )
        .unwrap_err();
    assert!(err.message.contains("does not resolve to an open T3 thread"), "{}", err.message);

    // 3. Unknown remote environment is rejected (never routed or looked up locally).
    let err = server
        .call(
            "maestro_ask_file",
            json!({
                "session": format!("t3:{UNKNOWN_ENV}/{COLLIDING_ID}"),
                "ledger": "ledger.yaml",
                "ask": sample_ask(&fixtures, "unknown-env-ask"),
            }),
        )
        .unwrap_err();
    assert!(err.message.contains("neither this host's T3 environment nor a linked"), "{}", err.message);

    // 4. Exact local UUID canonicalizes `local` to the host's authoritative environment id,
    //    while keeping caller authority unverified (`verified: false`).
    let res = server
        .call(
            "maestro_ask_file",
            json!({
                "session": format!("t3:local/{FULL_UUID_A}"),
                "ledger": "ledger.yaml",
                "ask": sample_ask(&fixtures, "exact-local-ask"),
            }),
        )
        .unwrap();
    let expected_origin = format!("t3:{LOCAL_ENV}/{FULL_UUID_A}");
    assert_eq!(res["request"]["session"], json!({ "surface": "t3", "environment": LOCAL_ENV, "id": FULL_UUID_A }));
    assert_eq!(res["request"]["verified"], false, "claimed destination must never become verified authority");
    assert_eq!(res["destination"]["status"], "resolved_local");
    assert_eq!(res["destination"]["session"], expected_origin);
    let filed_ask =
        store.asks().data.unwrap().into_iter().flat_map(|l| l.ledger.asks).find(|a| a.id == "exact-local-ask").unwrap();
    assert_eq!(filed_ask.origin.as_ref().and_then(|o| o.session.as_deref()), Some(expected_origin.as_str()));

    // 5. Exact host-authored percent-encoded T3 id succeeds cleanly.
    let res = server
        .call(
            "maestro_ask_file",
            json!({
                "session": format!("t3:{LOCAL_ENV}/{HOST_AUTHORED_ID}"),
                "ledger": "ledger.yaml",
                "ask": sample_ask(&fixtures, "host-authored-ask"),
            }),
        )
        .unwrap();
    assert_eq!(res["request"]["session"], json!({ "surface": "t3", "environment": LOCAL_ENV, "id": HOST_AUTHORED_ID }));
    assert_eq!(res["request"]["verified"], false);
    assert_eq!(res["destination"]["status"], "resolved_local");
    assert_eq!(res["destination"]["session"], format!("t3:{LOCAL_ENV}/{HOST_AUTHORED_ID}"));

    // 6. Linked remote environment is preserved verbatim without local lookup and flagged as not
    //    directly routable by the local T3 CLI transport.
    let res = server
        .call(
            "maestro_ask_file",
            json!({
                "session": format!("t3:{LINKED_ENV}/{COLLIDING_ID}"),
                "ledger": "ledger.yaml",
                "ask": sample_ask(&fixtures, "linked-remote-ask"),
            }),
        )
        .unwrap();
    assert_eq!(res["request"]["session"], json!({ "surface": "t3", "environment": LINKED_ENV, "id": COLLIDING_ID }));
    assert_eq!(res["request"]["verified"], false);
    assert_eq!(res["destination"]["status"], "remote_linked");
    assert_eq!(res["destination"]["session"], format!("t3:{LINKED_ENV}/{COLLIDING_ID}"));
    assert_eq!(res["destination"]["direct_t3_cli_routable"], false);

    // 7. Non-T3 callers (Paseo / Direct) keep legitimate filing without T3 registry interference.
    let res = server
        .call(
            "maestro_ask_file",
            json!({
                "session": "paseo:-/ae7d9f21",
                "ledger": "ledger.yaml",
                "ask": sample_ask(&fixtures, "paseo-caller-ask"),
            }),
        )
        .unwrap();
    assert_eq!(res["request"]["session"], json!({ "surface": "paseo", "id": "ae7d9f21" }));
    assert_eq!(res["request"]["verified"], false);
    assert_eq!(res["destination"]["status"], "non_t3");
    assert_eq!(res["destination"]["session"], "paseo:-/ae7d9f21");

    // 8. Disconnected/unreadable T3 registry fails closed for T3 destinations.
    let broken_hosts = found_hosts(t3.with_disconnected(true), &[]);
    let broken_server = maestro_mcp::MaestroServer::new(store).with_hosts(broken_hosts).shared();
    let err = broken_server
        .call(
            "maestro_ask_file",
            json!({
                "session": format!("t3:local/{FULL_UUID_A}"),
                "ledger": "ledger.yaml",
                "ask": sample_ask(&fixtures, "broken-reg-ask"),
            }),
        )
        .unwrap_err();
    assert!(err.message.contains("RegistryUnavailable"), "{}", err.message);
}

#[test]
fn ask_filing_in_t3_process_resolves_omitted_session_and_rejects_mismatched_short_or_worktree_claims() {
    let dir = tempfile::tempdir().unwrap();
    let (store, fixtures) = setup_ask_store(dir.path());
    let t3 = setup_t3_fixture(&dir.path().join("t3"));
    let hosts = found_hosts(t3.clone(), &[(T3CODE_THREAD_ID, FULL_UUID_A)]);

    // 1. Omitted session resolves automatically to exact canonical `t3:<env>/<full-id>` while
    //    keeping `verified: false` (claimless request authority never elevated).
    let res = maestro_mcp::call_with_hosts(
        &store,
        &hosts,
        "maestro_ask_file",
        json!({ "ledger": "ledger.yaml", "ask": sample_ask(&fixtures, "omitted-session-ask") }),
    )
    .unwrap();
    let expected = format!("t3:{LOCAL_ENV}/{FULL_UUID_A}");
    assert_eq!(res["request"]["session"], json!({ "surface": "t3", "environment": LOCAL_ENV, "id": FULL_UUID_A }));
    assert_eq!(res["request"]["verified"], false);
    assert_eq!(res["destination"]["status"], "resolved_local");
    assert_eq!(res["destination"]["session"], expected);

    // 2. Claiming short UUID or worktree label or another thread while T3CODE_THREAD_ID is set is refused.
    for bad in [
        format!("t3:local/{SHORT_UUID_A}"),
        format!("t3:local/{WORKTREE_LABEL_B}"),
        format!("t3:local/{FULL_UUID_B}"),
        "paseo:-/ae7d9f21".to_string(),
    ] {
        let err = maestro_mcp::call_with_hosts(
            &store,
            &hosts,
            "maestro_ask_file",
            json!({ "session": bad, "ledger": "ledger.yaml", "ask": sample_ask(&fixtures, "mismatch-ask") }),
        )
        .unwrap_err();
        assert!(err.message.contains("ThreadMismatch"), "{bad}: {}", err.message);
    }

    // 3. Configured (non-found) T3 surface still requires credentials for claimless requests.
    let configured = SurfaceRegistry::new(None, Some(t3), None)
        .with_identity_mode(IdentityMode::Attributed)
        .with_thread_vars(ThreadVars::from_pairs([(T3CODE_THREAD_ID, FULL_UUID_A.to_string())]));
    let err = maestro_mcp::call_with_hosts(
        &store,
        &configured,
        "maestro_ask_file",
        json!({ "ledger": "ledger.yaml", "ask": sample_ask(&fixtures, "configured-claimless-ask") }),
    )
    .unwrap_err();
    assert!(err.message.contains("authenticated bridge credential or launch token is required"), "{}", err.message);
}

#[tokio::test]
async fn unresolved_and_remote_origins_are_visible_before_signing_on_decision_pages() {
    let dir = tempfile::tempdir().unwrap();
    let t3 = setup_t3_fixture(&dir.path().join("t3"));
    let ws = dir.path().join("workspace");
    let asks = ws.join(".broomva/asks");
    std::fs::create_dir_all(&asks).unwrap();
    let yaml = [
        "arc: destinations",
        "asks:",
        "  - id: ask-short",
        "    headline: Short UUID ask",
        "    ask: Decide on short UUID ask",
        "    options:",
        "      - id: yes",
        "        label: Yes",
        "    origin:",
        "      via: maestro",
        &format!("      session: t3:local/{SHORT_UUID_A}"),
        "      verified: false",
        "  - id: ask-worktree",
        "    headline: Worktree label ask",
        "    ask: Decide on worktree label ask",
        "    options:",
        "      - id: yes",
        "        label: Yes",
        "    origin:",
        "      via: maestro",
        &format!("      session: t3:local/{WORKTREE_LABEL_B}"),
        "      verified: false",
        "  - id: ask-linked",
        "    headline: Linked remote ask",
        "    ask: Decide on linked remote ask",
        "    options:",
        "      - id: yes",
        "        label: Yes",
        "    origin:",
        "      via: maestro",
        &format!("      session: t3:{LINKED_ENV}/{COLLIDING_ID}"),
        "      verified: false",
        "  - id: ask-unlinked",
        "    headline: Unlinked remote ask",
        "    ask: Decide on unlinked remote ask",
        "    options:",
        "      - id: yes",
        "        label: Yes",
        "    origin:",
        "      via: maestro",
        &format!("      session: t3:{UNKNOWN_ENV}/{COLLIDING_ID}"),
        "      verified: false",
        "  - id: ask-local-ok",
        "    headline: Exact local ask",
        "    ask: Decide on exact local ask",
        "    options:",
        "      - id: yes",
        "        label: Yes",
        "    origin:",
        "      via: maestro",
        &format!("      session: t3:{LOCAL_ENV}/{FULL_UUID_A}"),
        "      verified: false",
        "",
    ]
    .join("\n");
    std::fs::write(asks.join("destinations.yaml"), yaml).unwrap();
    let mut paths = Paths::under(dir.path());
    paths.workspace = ws.clone();
    let store = Store::new(paths, TreeSource::WorkingTree);

    let owner_dir = dir.path().join("owner");
    std::fs::create_dir_all(&owner_dir).unwrap();
    std::fs::write(
        owner_dir.join("passkeys.json"),
        r#"{"credentials":[{"id":"Y3JlZA","public_key":"x","enrolled_at":"t","rp_id":"localhost"}]}"#,
    )
    .unwrap();
    let listener = http::listen(Bind::named("127.0.0.1:0".parse().unwrap()).unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let host = format!("localhost:{}", addr.port());
    let origin = format!("http://{host}");
    let writer = Writer {
        workspace: ws,
        asks: ".broomva/asks".into(),
        checkout: dir.path().join("co"),
        owner_dir: owner_dir.clone(),
        t3: None,
        t3_registry: Some(t3.clone()),
        fallback_thread: Some(FALLBACK_THREAD.into()),
        remote: false,
    };
    let party = maestro_mcp::passkey::Party::parse(&origin).unwrap();
    let owner = maestro_mcp::owner_web::Owner::new(party, owner_dir, writer);
    let server = maestro_mcp::MaestroServer::new(store).with_hosts(found_hosts(t3, &[]));
    let router = maestro_mcp::web::router_with(server, addr, Ok(owner));
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move { axum::serve(listener, router).with_graceful_shutdown(stop.cancelled_owned()).await });

    let (st, short_html) = get_page(addr, "/decisions/destinations.yaml/ask-short", &host).await;
    assert_eq!(st, 200);
    assert!(short_html.contains(r#"data-delivery-route="unresolved""#), "{short_html}");
    assert!(short_html.contains(r#"data-tone="warning""#));
    assert!(short_html.contains(FALLBACK_THREAD));
    assert!(short_html.contains(r#"data-answer-bar="true""#));

    let (st, wt_html) = get_page(addr, "/decisions/destinations.yaml/ask-worktree", &host).await;
    assert_eq!(st, 200);
    assert!(wt_html.contains(r#"data-delivery-route="unresolved""#), "{wt_html}");
    assert!(wt_html.contains(WORKTREE_LABEL_B));

    let (st, linked_html) = get_page(addr, "/decisions/destinations.yaml/ask-linked", &host).await;
    assert_eq!(st, 200);
    assert!(linked_html.contains(r#"data-delivery-route="remote-unsupported""#), "{linked_html}");
    assert!(linked_html.contains(LINKED_ENV));
    assert!(linked_html.contains("installed T3 CLI transport sends only to local threads"));

    let (st, unlinked_html) = get_page(addr, "/decisions/destinations.yaml/ask-unlinked", &host).await;
    assert_eq!(st, 200);
    assert!(unlinked_html.contains(r#"data-delivery-route="remote-unlinked""#), "{unlinked_html}");
    assert!(unlinked_html.contains("will not be routed locally"));

    let (st, local_html) = get_page(addr, "/decisions/destinations.yaml/ask-local-ok", &host).await;
    assert_eq!(st, 200);
    assert!(local_html.contains(r#"data-delivery-route="direct-local""#), "{local_html}");
    assert!(local_html.contains(FULL_UUID_A));
    assert!(!local_html.contains(r#"data-delivery-route="unresolved""#));

    cancel.cancel();
}

#[test]
fn wake_prevents_wrong_target_distinguishes_transport_limitation_and_dedupes_retries_with_signed_commit_integrity() {
    let dir = tempfile::tempdir().unwrap();
    let t3_reg = setup_t3_fixture(&dir.path().join("t3"));
    let log_file = dir.path().join("t3-calls.log");
    let stub_t3 = dir.path().join("stub-t3");
    let stub_body =
        ["#!/bin/sh", &format!(r#"printf 'TARGET=%s\n' "$3" >> '{}'"#, log_file.display()), "exit 0", ""].join("\n");
    write_executable(&stub_t3, &stub_body);
    let read_targets = || -> Vec<String> {
        std::fs::read_to_string(&log_file)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.strip_prefix("TARGET=").map(str::to_string))
            .collect()
    };

    let sample_receipt = Receipt {
        id: "r1AAAAAAAAAAAAAA".into(),
        intent: Intent::answer("t.yaml", "a", "h", "yes", None),
        label: "Yes".into(),
        stage: Stage::Written,
        signed_at: "2026-11-27T00:00:00Z".into(),
        applies_at_ms: 0,
        commit: Some("deadbeef".into()),
        woke: None,
        delivery: None,
        landing: None,
        error: None,
    };
    let cands = vec![stub_t3.clone()];

    // 1. Short UUID and worktree label NEVER invoke local `t3 thread send` on the invalid target;
    //    they route explicitly to fallback with `DeliveryStatus::FallbackNotified`.
    let (woke, status) = answers::wake_with_registry(
        &cands,
        Some(&t3_reg),
        Some(&format!("t3:local/{SHORT_UUID_A}")),
        Some(FALLBACK_THREAD),
        &sample_receipt,
        "deadbeef",
    );
    assert_eq!(status, DeliveryStatus::FallbackNotified);
    assert!(woke.contains("unresolved on this host") && woke.contains(FALLBACK_THREAD), "{woke}");
    assert_eq!(read_targets(), vec![FALLBACK_THREAD.to_string()]);

    std::fs::write(&log_file, "").unwrap();
    let (woke, status) = answers::wake_with_registry(
        &cands,
        Some(&t3_reg),
        Some(&format!("t3:local/{WORKTREE_LABEL_B}")),
        Some(FALLBACK_THREAD),
        &sample_receipt,
        "deadbeef",
    );
    assert_eq!(status, DeliveryStatus::FallbackNotified);
    assert!(woke.contains("unresolved on this host"), "{woke}");
    assert_eq!(read_targets(), vec![FALLBACK_THREAD.to_string()]);

    // 2. Linked remote environment with a colliding local thread id NEVER sends to the local
    //    colliding thread, distinguishes the CLI transport limitation, and routes to fallback
    //    even if the remote thread id matches the local fallback thread id!
    std::fs::write(&log_file, "").unwrap();
    let (woke, status) = answers::wake_with_registry(
        &cands,
        Some(&t3_reg),
        Some(&format!("t3:{LINKED_ENV}/{COLLIDING_ID}")),
        Some(FALLBACK_THREAD),
        &sample_receipt,
        "deadbeef",
    );
    assert_eq!(status, DeliveryStatus::FallbackNotified);
    assert!(woke.contains("T3 CLI transport cannot route linked environment"), "{woke}");
    assert_eq!(read_targets(), vec![FALLBACK_THREAD.to_string()], "colliding local thread must never be sent to");

    // Without fallback configured, linked remote transport limitation stays `WrittenOnly`.
    std::fs::write(&log_file, "").unwrap();
    let (woke, status) = answers::wake_with_registry(
        &cands,
        Some(&t3_reg),
        Some(&format!("t3:{LINKED_ENV}/{COLLIDING_ID}")),
        None,
        &sample_receipt,
        "deadbeef",
    );
    assert_eq!(status, DeliveryStatus::WrittenOnly);
    assert!(woke.contains("T3 CLI transport cannot route linked environment"), "{woke}");
    assert!(read_targets().is_empty(), "zero local sends when remote and no fallback");

    // Remote thread whose id happens to equal `FALLBACK_THREAD` STILL routes to local fallback!
    std::fs::write(&log_file, "").unwrap();
    let (woke, status) = answers::wake_with_registry(
        &cands,
        Some(&t3_reg),
        Some(&format!("t3:{LINKED_ENV}/{FALLBACK_THREAD}")),
        Some(FALLBACK_THREAD),
        &sample_receipt,
        "deadbeef",
    );
    assert_eq!(status, DeliveryStatus::FallbackNotified);
    assert!(woke.contains("routed to fallback T3 thread"), "{woke}");
    assert_eq!(read_targets(), vec![FALLBACK_THREAD.to_string()]);

    // 3. Exact local thread sends directly and returns `DeliveryStatus::PrimaryNotified`.
    std::fs::write(&log_file, "").unwrap();
    let (woke, status) = answers::wake_with_registry(
        &cands,
        Some(&t3_reg),
        Some(&format!("t3:{LOCAL_ENV}/{FULL_UUID_A}")),
        Some(FALLBACK_THREAD),
        &sample_receipt,
        "deadbeef",
    );
    assert_eq!(status, DeliveryStatus::PrimaryNotified);
    assert_eq!(woke, format!("told T3 thread {FULL_UUID_A}"));
    assert_eq!(read_targets(), vec![FULL_UUID_A.to_string()]);

    // 4. Signed ledger commit integrity + retry dedupe in `answers::apply`.
    let ws = dir.path().join("ans-ws");
    let asks = ws.join(".broomva/asks");
    std::fs::create_dir_all(&asks).unwrap();
    std::fs::create_dir_all(ws.join("scripts")).unwrap();
    let ledger_text = [
        "arc: demo",
        "asks:",
        "  - id: a1",
        "    ask: Ship?",
        "    options:",
        "      - id: yes",
        "        label: Yes",
        "    origin:",
        &format!("      session: t3:{LOCAL_ENV}/{FULL_UUID_A}"),
        "",
    ]
    .join("\n");
    let remote_ledger = [
        "arc: remote-demo",
        "asks:",
        "  - id: a2",
        "    ask: Ship remote?",
        "    options:",
        "      - id: yes",
        "        label: Yes",
        "    origin:",
        &format!("      session: t3:{LINKED_ENV}/{COLLIDING_ID}"),
        "",
    ]
    .join("\n");
    std::fs::write(asks.join("t.yaml"), &ledger_text).unwrap();
    std::fs::write(asks.join("remote.yaml"), &remote_ledger).unwrap();
    std::fs::write(
        ws.join("scripts/ask_ledger.py"),
        "import sys\nprint('ask-ledger: 1 ask(s), 0 decision(s), 0 error(s)')\nsys.exit(0)\n",
    )
    .unwrap();
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["add", "."]);
    git(&ws, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);

    let parsed =
        maestro_core::ask::AskLedger::from_value(maestro_store::yaml::parse(&ledger_text).unwrap().value).unwrap().0;
    let hash = maestro_store::answer::find(&parsed, &maestro_store::answer::Select { handle: "a1" }).unwrap().hash();

    let writer = Writer {
        workspace: ws.clone(),
        asks: ".broomva/asks".into(),
        checkout: dir.path().join("ans-co"),
        owner_dir: dir.path().join("ans-owner"),
        t3: Some(T3::Fixed(stub_t3)),
        t3_registry: Some(t3_reg),
        fallback_thread: Some(FALLBACK_THREAD.into()),
        remote: false,
    };
    let assertion = Assertion {
        credential: "c".into(),
        client_data: "d".into(),
        authenticator_data: "e".into(),
        signature: "f".into(),
    };
    let make_receipt = || Receipt {
        id: "r9AAAAAAAAAAAAAA".into(),
        intent: Intent::answer("t.yaml", "a1", &hash, "yes", Some("approved")),
        label: "Yes".into(),
        stage: Stage::Signed,
        signed_at: "2026-11-27T00:00:00Z".into(),
        applies_at_ms: 0,
        commit: None,
        woke: None,
        delivery: None,
        landing: None,
        error: None,
    };

    std::fs::write(&log_file, "").unwrap();
    let book1 = Book::default();
    book1.put(make_receipt());
    let origin = format!("t3:{LOCAL_ENV}/{FULL_UUID_A}");
    answers::apply(&writer, &book1, "r9AAAAAAAAAAAAAA", &assertion, "n1", "owner (passkey)", Some(&origin));
    let r1 = book1.get("r9AAAAAAAAAAAAAA").unwrap();
    assert_eq!(r1.stage, Stage::Written, "{:?}", r1.error);
    assert_eq!(r1.delivery, Some(DeliveryStatus::PrimaryNotified));
    let commit1 = r1.commit.clone().unwrap();
    let msg = git(&writer.checkout, &["log", "-1", "--format=%B"]);
    assert!(msg.contains("Maestro-Owner-Proof: {"), "signed commit proof must stay intact: {msg}");
    assert_eq!(read_targets(), vec![FULL_UUID_A.to_string()]);

    // Simulate daemon restart / retry after wake was already logged (fast-forward workspace to commit1).
    git(&ws, &["fetch", "-q", writer.checkout.to_str().unwrap(), &commit1]);
    git(&ws, &["merge", "-q", "--ff-only", &commit1]);
    let book2 = Book::default();
    book2.put(make_receipt());
    answers::apply(&writer, &book2, "r9AAAAAAAAAAAAAA", &assertion, "n1", "owner (passkey)", Some(&origin));
    let r2 = book2.get("r9AAAAAAAAAAAAAA").unwrap();
    assert_eq!(r2.stage, Stage::Written);
    assert_eq!(r2.commit.as_deref(), Some(commit1.as_str()));
    assert_eq!(r2.delivery, Some(DeliveryStatus::PrimaryNotified));
    assert_eq!(read_targets().len(), 1, "retry after already-logged notification must not send a duplicate wake");
    // 5. `answers::apply` on a linked-remote ask whose thread id collides with a local thread:
    //    writes the signed commit intact, never wakes the colliding local thread, routes to
    //    fallback with `FallbackNotified`, and dedupes the fallback wake on retry.
    let parsed_remote =
        maestro_core::ask::AskLedger::from_value(maestro_store::yaml::parse(&remote_ledger).unwrap().value).unwrap().0;
    let hash_remote =
        maestro_store::answer::find(&parsed_remote, &maestro_store::answer::Select { handle: "a2" }).unwrap().hash();
    let make_remote_receipt = || Receipt {
        id: "r8BBBBBBBBBBBBBB".into(),
        intent: Intent::answer("remote.yaml", "a2", &hash_remote, "yes", None),
        label: "Yes".into(),
        stage: Stage::Signed,
        signed_at: "2026-11-27T00:00:00Z".into(),
        applies_at_ms: 0,
        commit: None,
        woke: None,
        delivery: None,
        landing: None,
        error: None,
    };
    std::fs::write(&log_file, "").unwrap();
    let book3 = Book::default();
    book3.put(make_remote_receipt());
    let remote_origin = format!("t3:{LINKED_ENV}/{COLLIDING_ID}");
    answers::apply(&writer, &book3, "r8BBBBBBBBBBBBBB", &assertion, "n2", "owner (passkey)", Some(&remote_origin));
    let r3 = book3.get("r8BBBBBBBBBBBBBB").unwrap();
    assert_eq!(r3.stage, Stage::Written, "{:?}", r3.error);
    assert_eq!(r3.delivery, Some(DeliveryStatus::FallbackNotified));
    assert!(
        r3.woke.as_deref().unwrap_or_default().contains("T3 CLI transport cannot route linked environment"),
        "{:?}",
        r3.woke
    );
    assert_eq!(read_targets(), vec![FALLBACK_THREAD.to_string()], "colliding local thread must never be woken");

    let commit3 = r3.commit.clone().unwrap();
    git(&ws, &["fetch", "-q", writer.checkout.to_str().unwrap(), &commit3]);
    git(&ws, &["merge", "-q", "--ff-only", &commit3]);
    let book4 = Book::default();
    book4.put(make_remote_receipt());
    answers::apply(&writer, &book4, "r8BBBBBBBBBBBBBB", &assertion, "n2", "owner (passkey)", Some(&remote_origin));
    assert_eq!(read_targets().len(), 1, "retry after fallback notification must not send a duplicate fallback wake");
}

#[test]
fn wake_t3_target_preserves_environment_and_supports_percent_encoded_ids() {
    let t = answers::t3_target_of("t3:env-mac/mcp:launch:ab%3Acd").unwrap();
    assert_eq!(t.environment, Some("env-mac"));
    assert_eq!(t.thread, "mcp:launch:ab%3Acd");

    let local = answers::t3_target_of("t3:local/60f14287-1111-4222-8333-444444444444").unwrap();
    assert_eq!(local.environment, None);
    assert_eq!(local.thread, "60f14287-1111-4222-8333-444444444444");

    assert!(answers::t3_target_of("t3:local/-flag").is_none());
    assert!(answers::t3_target_of("t3:local/..").is_none());
    assert!(answers::t3_target_of("t3:/abc").is_none());
    assert!(answers::t3_target_of("paseo:-/abc").is_none());
}
