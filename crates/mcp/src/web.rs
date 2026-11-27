//! Maestro's web pages (BRO-3588): Decisions, one decision, Today, Flows and Health, rendered on the
//! server by `maestro-view` from the same catalog blocks the MCP App places, laid out as the
//! approved design (BRO-3783, maestro#20), on the daemon
//! `maestro serve --http` already runs. A phone or browser on the tailnet reads them with no
//! Paseo and no MCP host.
//!
//! The pages read, and one path writes: the owner's answer, signed with a passkey
//! ([`crate::owner_web`], BRO-4042). The pages run one script, the owner script, pinned by hash,
//! and post no form. A request must name the bound address, a loopback name, or the configured
//! secure origin's host in `Host`, as on `/mcp`, and a browser request from another site is refused.

use crate::MaestroServer;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::uri::Authority;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use maestro_view::handoff::AnswerThread;
use maestro_view::page;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Clone)]
struct Web {
    server: MaestroServer,
    hosts: Arc<Vec<String>>,
    /// The owner's answering, or why the pages offer none.
    owner: Result<crate::owner_web::Owner, String>,
}

/// The environment variable naming the T3 thread answers are drafted in (BRO-3590).
pub const ANSWER_THREAD_ENV: &str = "MAESTRO_ANSWER_THREAD";

/// The answer thread `MAESTRO_ANSWER_THREAD` names. Unset is `None`; a value that does not parse
/// is `None` too, said once on stderr, so the pages stay read-only rather than link anywhere else.
pub fn answer_thread_from_env() -> Option<AnswerThread> {
    answer_thread_from(std::env::var(ANSWER_THREAD_ENV).ok().as_deref())
}

/// What a `MAESTRO_ANSWER_THREAD` value configures; the pure half of [`answer_thread_from_env`].
pub fn answer_thread_from(value: Option<&str>) -> Option<AnswerThread> {
    AnswerThread::parse(value?)
        .map_err(|e| eprintln!("maestro: ignoring {ANSWER_THREAD_ENV}: {e}; decisions offer no T3 handoff"))
        .ok()
}

/// The web routes for a listener bound at `local`, answering as the environment configures.
pub fn router(server: MaestroServer, local: SocketAddr) -> Router {
    let store = server.store();
    let owner = store
        .rel(maestro_store::ASKS)
        .and_then(|asks| crate::owner_web::Owner::from_env(store.paths.workspace.clone(), asks));
    // Unconfigured, the pages say why they take no answer; the log's first line stays the listener's.
    router_with(server, local, owner)
}

/// The web routes, answering with `owner` when it is configured.
pub fn router_with(server: MaestroServer, local: SocketAddr, owner: Result<crate::owner_web::Owner, String>) -> Router {
    let mut hosts = vec!["localhost".to_string(), "127.0.0.1".to_string(), "[::1]".to_string(), host_name(local)];
    if let Ok(o) = &owner {
        hosts.push(o.party.id.clone());
    }
    let files = server.file_pilot;
    let web = Web { server, hosts: Arc::new(hosts), owner };
    let mut router = Router::new()
        .route("/", get(root))
        .route("/owner", get(owner_page))
        .route("/owner/enroll/begin", post(enroll_begin))
        .route("/owner/enroll/finish", post(enroll_finish))
        .route("/owner/answer/begin", post(answer_begin))
        .route("/owner/answer/finish", post(answer_finish))
        .route("/owner/answer/undo", post(answer_undo))
        .route("/owner/answer/{id}", get(answer_receipt))
        .route("/decisions", get(decisions))
        .route("/decisions/{ledger}/{id}", get(decision))
        .route("/today", get(today))
        .route("/health", get(health))
        .route("/flows", get(flows))
        .route("/flows/kernel/{name}", get(flow_kernel))
        .route("/flows/run/{id}", get(flow_run))
        .route("/manifest.webmanifest", get(manifest))
        .route("/apple-touch-icon.png", get(icon_180))
        .route("/icon-192.png", get(icon_192))
        .route("/icon-512.png", get(icon_512));
    if files {
        router = router.route(crate::file_pilot::PATH, get(fixture_download));
    }
    router.with_state(web)
}

async fn fixture_download(State(web): State<Web>, uri: Uri, headers: HeaderMap) -> Response {
    if headers.contains_key(header::ORIGIN) || !admitted(&uri, &headers, &web.hosts) {
        return refused();
    }
    crate::file_pilot::download().await.into_response()
}

/// How a `Host` header names the bound address: an IPv6 address in brackets.
fn host_name(local: SocketAddr) -> String {
    match local {
        SocketAddr::V4(a) => a.ip().to_string(),
        SocketAddr::V6(a) => format!("[{}]", a.ip()),
    }
}

/// The name one `Host` value gives: a well-formed authority with no userinfo and, if it has a
/// port, a numeric one; one trailing dot of a fully qualified name dropped. `None` otherwise.
fn host_of(value: &str) -> Option<&str> {
    let auth: Authority = value.parse().ok()?;
    if value.contains('@') || (auth.as_str() != auth.host() && auth.port_u16().is_none()) {
        return None;
    }
    let host = auth.host();
    let host = host.strip_suffix('.').unwrap_or(host);
    // `auth` borrows nothing: return the same span of `value`.
    value.get(..host.len()).filter(|h| h.eq_ignore_ascii_case(host))
}

/// Whether a request may be served: an origin-form target (a browser never sends another to
/// an origin server), exactly one `Host` naming an allowed name on any port, and no browser
/// marking it as sent from another site.
fn admitted(uri: &Uri, headers: &HeaderMap, hosts: &[String]) -> bool {
    if uri.scheme().is_some() || uri.authority().is_some() {
        return false;
    }
    let mut values = headers.get_all(header::HOST).iter();
    let (Some(host), None) = (values.next(), values.next()) else { return false };
    let Some(name) = host.to_str().ok().and_then(host_of) else { return false };
    if !hosts.iter().any(|h| h.eq_ignore_ascii_case(name)) {
        return false;
    }
    // Fails closed: a second value, or one that is not text, is refused like another site.
    let mut sites = headers.get_all("sec-fetch-site").iter();
    match (sites.next(), sites.next()) {
        (None, _) => true,
        (Some(site), None) => matches!(site.to_str(), Ok("same-origin" | "none")),
        (Some(_), Some(_)) => false,
    }
}

/// The host and port a request names in `Host`, comparable with the configured origin's: a page
/// opened at another host or port links to the secure page instead of offering controls its
/// answer would be refused from.
fn request_authority(headers: &HeaderMap, party: &crate::passkey::Party) -> Option<String> {
    let value = headers.get(header::HOST)?.to_str().ok()?;
    host_of(value)?;
    Some(crate::passkey::authority_of(value, party.origin.starts_with("https://")))
}

fn refused() -> Response {
    (StatusCode::FORBIDDEN, "refused: this page is served only to its own host and origin\n").into_response()
}

fn with_headers(mut res: Response, content_type: &'static str) -> Response {
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(page::web_csp()));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// Whether the asks were read in full and none is `ledger#id`: the "no such ask" of
/// `maestro_ask_get`, from the same lookup.
fn absent(store: &maestro_store::Store, ledger: &str, id: &str) -> bool {
    let section = store.asks();
    let (matches, unread) = crate::ask_matches(store, &section, ledger, id);
    section.data.is_some() && matches.is_empty() && unread.is_empty()
}

/// The owner's answering as the header shows it for this request.
fn owner_of(web: &Web, headers: &HeaderMap) -> maestro_view::render::Owner {
    match &web.owner {
        // Opened at another address than the secure one, the page links there to answer.
        Ok(o) if request_authority(headers, &o.party).is_some_and(|a| a != o.party.authority) => {
            maestro_view::render::Owner::Elsewhere(o.party.origin.clone())
        }
        Ok(o) => o.state(),
        Err(e) => maestro_view::render::Owner::Off(e.clone()),
    }
}

/// A Flows page (`maestro_view::FlowsPage`): the flow ledger read for this request
/// ([`crate::flows_web`]) under the same header every page has. A kernel or run the ledger,
/// read in full, does not hold is no page.
async fn flows_page(web: &Web, uri: &Uri, headers: &HeaderMap, which: maestro_view::FlowsPage) -> Response {
    if !admitted(uri, headers, &web.hosts) {
        return refused();
    }
    let server = web.server.clone();
    let owner = owner_of(web, headers);
    let rendered = tokio::task::spawn_blocking(move || {
        let store = server.store();
        let ledger = crate::flows_web::read_shared(&store.paths);
        // Ages are counted from after the read, so no event the read saw is ahead of the clock.
        let at = maestro_store::views::now();
        let (html, title) = maestro_view::flows_page(&ledger, &which, at)?;
        Some((html, title, maestro_view::render::web_shell(&store, at, owner)))
    })
    .await;
    match rendered {
        Ok(Some((html, title, shell))) => {
            with_headers(page::web(&html, &title, "/flows", &shell).into_response(), "text/html; charset=utf-8")
        }
        Ok(None) => (StatusCode::NOT_FOUND, "no such flow\n").into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "the render did not finish\n").into_response(),
    }
}

async fn flows(State(web): State<Web>, uri: Uri, headers: HeaderMap) -> Response {
    flows_page(&web, &uri, &headers, maestro_view::FlowsPage::List).await
}

async fn flow_kernel(State(web): State<Web>, uri: Uri, headers: HeaderMap, Path(name): Path<String>) -> Response {
    flows_page(&web, &uri, &headers, maestro_view::FlowsPage::Kernel(name)).await
}

async fn flow_run(State(web): State<Web>, uri: Uri, headers: HeaderMap, Path(id): Path<String>) -> Response {
    flows_page(&web, &uri, &headers, maestro_view::FlowsPage::Run(id)).await
}

/// Render one block as a page, read from a store resolved for this request.
async fn page_of(
    web: &Web,
    uri: &Uri,
    headers: &HeaderMap,
    block: Value,
    ask: Option<(String, String)>,
    title: &'static str,
    current: &'static str,
) -> Response {
    if !admitted(uri, headers, &web.hosts) {
        return refused();
    }
    let spec = json!({ "root": "page", "blocks": { "page": block } });
    let validated = match maestro_view::catalog::validate(&spec) {
        Ok(v) => v,
        Err(e) => return (StatusCode::NOT_FOUND, format!("no such page: {e}\n")).into_response(),
    };
    let server = web.server.clone();
    let owner = owner_of(web, headers);
    let in_flight = web.owner.as_ref().map(|o| o.book.in_flight()).unwrap_or_default();
    let rendered = tokio::task::spawn_blocking(move || {
        let store = server.store().read_scope();
        // A readable ledger with no such ask is no page; an unread one still renders as unavailable.
        if let Some((ledger, id)) = &ask
            && absent(&store, ledger, id)
        {
            return None;
        }
        let at = maestro_store::views::now();
        Some(maestro_view::render::render_web_with_pending(&store, &validated, at, owner, in_flight))
    })
    .await;
    match rendered {
        Ok(Some((r, shell))) => {
            with_headers(page::web(&r.html, title, current, &shell).into_response(), "text/html; charset=utf-8")
        }
        Ok(None) => (StatusCode::NOT_FOUND, "no such ask\n").into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "the render did not finish\n").into_response(),
    }
}

async fn root(State(web): State<Web>, uri: Uri, headers: HeaderMap) -> Response {
    if !admitted(&uri, &headers, &web.hosts) {
        return refused();
    }
    Redirect::to("/decisions").into_response()
}

async fn decisions(State(web): State<Web>, uri: Uri, headers: HeaderMap) -> Response {
    page_of(&web, &uri, &headers, json!({ "type": "decisions" }), None, "Decisions", "/decisions").await
}

async fn decision(
    State(web): State<Web>,
    uri: Uri,
    headers: HeaderMap,
    Path((ledger, id)): Path<(String, String)>,
) -> Response {
    let block = json!({ "type": "decision", "props": { "ledger": ledger, "id": id } });
    page_of(&web, &uri, &headers, block, Some((ledger, id)), "Decision", "/decisions").await
}

async fn today(State(web): State<Web>, uri: Uri, headers: HeaderMap) -> Response {
    page_of(&web, &uri, &headers, json!({ "type": "today" }), None, "Today", "/today").await
}

async fn health(State(web): State<Web>, uri: Uri, headers: HeaderMap) -> Response {
    page_of(&web, &uri, &headers, json!({ "type": "health" }), None, "Health", "/health").await
}

async fn manifest(State(web): State<Web>, uri: Uri, headers: HeaderMap) -> Response {
    if !admitted(&uri, &headers, &web.hosts) {
        return refused();
    }
    with_headers(page::web_manifest().into_response(), "application/manifest+json")
}

fn icon(web: &Web, uri: &Uri, headers: &HeaderMap, bytes: &'static [u8]) -> Response {
    if !admitted(uri, headers, &web.hosts) {
        return refused();
    }
    with_headers(bytes.into_response(), "image/png")
}

async fn icon_180(State(web): State<Web>, uri: Uri, headers: HeaderMap) -> Response {
    icon(&web, &uri, &headers, page::ICON_180_PNG)
}

async fn icon_192(State(web): State<Web>, uri: Uri, headers: HeaderMap) -> Response {
    icon(&web, &uri, &headers, page::ICON_192_PNG)
}

async fn icon_512(State(web): State<Web>, uri: Uri, headers: HeaderMap) -> Response {
    icon(&web, &uri, &headers, page::ICON_512_PNG)
}

/// A JSON reply.
fn json_reply(status: StatusCode, v: Value) -> Response {
    let mut res = (status, v.to_string()).into_response();
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

fn refusal(status: StatusCode, why: impl std::fmt::Display) -> Response {
    json_reply(status, json!({ "error": why.to_string() }))
}

/// The owner's routes admit a request only from the page itself: admitted as every page is,
/// JSON, and from the configured secure origin.
#[allow(clippy::result_large_err)] // the refusal is the reply itself, sent at once
fn owner_admitted<'w>(
    web: &'w Web,
    uri: &Uri,
    headers: &HeaderMap,
    json_body: bool,
) -> Result<&'w crate::owner_web::Owner, Response> {
    if !admitted(uri, headers, &web.hosts) {
        return Err(refused());
    }
    let owner = web.owner.as_ref().map_err(|e| refusal(StatusCode::SERVICE_UNAVAILABLE, e))?;
    let origin_ok = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) == Some(owner.party.origin.as_str());
    let json_ok = !json_body
        || headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(';').next().is_some_and(|t| t.trim().eq_ignore_ascii_case("application/json")));
    if json_body && !(origin_ok && json_ok) {
        return Err(refusal(
            StatusCode::FORBIDDEN,
            "refused: only Maestro's own page at its secure address may send this",
        ));
    }
    Ok(owner)
}

#[allow(clippy::result_large_err)] // the refusal is the reply itself, sent at once
fn body_of<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, Response> {
    serde_json::from_slice(body)
        .map_err(|e| refusal(StatusCode::BAD_REQUEST, format!("the request does not read: {e}")))
}

async fn owner_page(State(web): State<Web>, uri: Uri, headers: HeaderMap) -> Response {
    if !admitted(&uri, &headers, &web.hosts) {
        return refused();
    }
    let state = match &web.owner {
        Err(e) => page::OwnerPage::Off(e.clone()),
        Ok(o) => match crate::passkey::Keyring::load(&o.dir) {
            Ok(k) if k.credentials.is_empty() => page::OwnerPage::Enroll,
            Ok(k) => page::OwnerPage::Enrolled(k.credentials.iter().map(|c| c.enrolled_at.clone()).collect()),
            Err(e) => page::OwnerPage::Off(e),
        },
    };
    // The header from a real read, as on every page, so it never claims a read that was not tried.
    let server = web.server.clone();
    let shell = tokio::task::spawn_blocking(move || {
        let spec = json!({ "root": "page", "blocks": { "page": { "type": "decisions" } } });
        let v = maestro_view::catalog::validate(&spec).ok()?;
        let off = maestro_view::render::Owner::Off(String::new());
        Some(maestro_view::render::render_web(&server.store(), &v, maestro_store::views::now(), off).1)
    })
    .await
    .ok()
    .flatten()
    .unwrap_or_default();
    with_headers(page::owner(&state, &shell).into_response(), "text/html; charset=utf-8")
}

async fn enroll_begin(State(web): State<Web>, uri: Uri, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    let owner = match owner_admitted(&web, &uri, &headers, true) {
        Ok(o) => o,
        Err(r) => return r,
    };
    #[derive(serde::Deserialize)]
    struct Req {
        #[serde(default)]
        code: String,
    }
    let req: Req = match body_of(&body) {
        Ok(r) => r,
        Err(r) => return r,
    };
    match owner.enroll_begin(&req.code) {
        Ok(v) => json_reply(StatusCode::OK, v),
        Err(e) => refusal(StatusCode::FORBIDDEN, e),
    }
}

async fn enroll_finish(State(web): State<Web>, uri: Uri, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    let owner = match owner_admitted(&web, &uri, &headers, true) {
        Ok(o) => o,
        Err(r) => return r,
    };
    #[derive(serde::Deserialize)]
    struct Req {
        nonce: String,
        registration: crate::passkey::Registration,
    }
    let req: Req = match body_of(&body) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let at = jiff::Timestamp::now().strftime("%Y-%m-%dT%H:%M:%SZ").to_string();
    match owner.enroll_finish(&req.nonce, &req.registration, &at) {
        Ok(()) => json_reply(StatusCode::OK, json!({ "enrolled": true })),
        Err(e) => refusal(StatusCode::FORBIDDEN, e),
    }
}

async fn answer_begin(State(web): State<Web>, uri: Uri, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    let owner = match owner_admitted(&web, &uri, &headers, true) {
        Ok(o) => o.clone(),
        Err(r) => return r,
    };
    let req: crate::owner_web::AnswerBegin = match body_of(&body) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let server = web.server.clone();
    let done = tokio::task::spawn_blocking(move || owner.answer_begin(&server.store(), &req)).await;
    match done {
        Ok(Ok(v)) => json_reply(StatusCode::OK, v),
        Ok(Err(e)) => refusal(StatusCode::CONFLICT, e),
        Err(_) => refusal(StatusCode::INTERNAL_SERVER_ERROR, "the check did not finish"),
    }
}

async fn answer_finish(State(web): State<Web>, uri: Uri, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    let owner = match owner_admitted(&web, &uri, &headers, true) {
        Ok(o) => o,
        Err(r) => return r,
    };
    #[derive(serde::Deserialize)]
    struct Req {
        nonce: String,
        assertion: crate::passkey::Assertion,
    }
    let req: Req = match body_of(&body) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let now = jiff::Timestamp::now();
    // Disk writes and syncs: on the blocking pool, as the begin is.
    let owner = owner.clone();
    let done = tokio::task::spawn_blocking(move || owner.answer_finish(&req.nonce, &req.assertion, now)).await;
    let Ok(done) = done else { return refusal(StatusCode::INTERNAL_SERVER_ERROR, "the answer did not finish") };
    match done {
        Ok(r) => {
            let mut v = serde_json::to_value(&r).unwrap_or_default();
            v["now_ms"] = json!(now.as_millisecond());
            json_reply(StatusCode::OK, v)
        }
        Err(e) => refusal(StatusCode::FORBIDDEN, e),
    }
}

async fn answer_undo(State(web): State<Web>, uri: Uri, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    let owner = match owner_admitted(&web, &uri, &headers, true) {
        Ok(o) => o,
        Err(r) => return r,
    };
    #[derive(serde::Deserialize)]
    struct Req {
        id: String,
    }
    let req: Req = match body_of(&body) {
        Ok(r) => r,
        Err(r) => return r,
    };
    match owner.undo(&req.id, jiff::Timestamp::now().as_millisecond()) {
        Ok(r) => json_reply(StatusCode::OK, serde_json::to_value(&r).unwrap_or_default()),
        Err(e) => refusal(StatusCode::CONFLICT, e),
    }
}

async fn answer_receipt(State(web): State<Web>, uri: Uri, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let owner = match owner_admitted(&web, &uri, &headers, false) {
        Ok(o) => o,
        Err(r) => return r,
    };
    if !crate::answers::valid_id(&id) {
        return refusal(StatusCode::NOT_FOUND, "no such answer");
    }
    match owner.receipt(&id) {
        Some(r) => json_reply(StatusCode::OK, serde_json::to_value(&r).unwrap_or_default()),
        None => refusal(StatusCode::NOT_FOUND, "no such answer"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admitted(headers: &HeaderMap, hosts: &[String]) -> bool {
        super::admitted(&Uri::from_static("/decisions"), headers, hosts)
    }

    fn headers(host: &str, site: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_str(host).unwrap());
        if let Some(s) = site {
            h.insert("sec-fetch-site", HeaderValue::from_str(s).unwrap());
        }
        h
    }

    fn allowed() -> Vec<String> {
        vec!["localhost".into(), "127.0.0.1".into(), "[::1]".into(), "100.112.149.32".into()]
    }

    #[test]
    fn a_host_must_name_an_allowed_address_on_any_port() {
        for host in [
            "localhost",
            "localhost.:7171",
            "127.0.0.1:7171",
            "[::1]",
            "[::1]:7171",
            "100.112.149.32:7171",
            "LOCALHOST:1",
        ] {
            assert!(admitted(&headers(host, None), &allowed()), "{host}");
        }
        for host in [
            "evil.example",
            "evil.example.:7171",
            "evil.example:7171",
            "127.0.0.2:7171",
            "[::2]:7171",
            "100.112.149.33",
            "localhost:not-a-port",
            "localhost:",
            "user@localhost:7171",
            "localhost..",
        ] {
            assert!(!admitted(&headers(host, None), &allowed()), "{host}");
        }
        assert!(!admitted(&HeaderMap::new(), &allowed()), "no Host");
        let mut two = headers("localhost:7171", None);
        two.append(header::HOST, HeaderValue::from_static("evil.example"));
        assert!(!admitted(&two, &allowed()), "two Host headers");
    }

    #[test]
    fn an_absolute_form_target_is_refused_whatever_its_host() {
        let h = headers("127.0.0.1:7171", None);
        assert!(super::admitted(&Uri::from_static("/today"), &h, &allowed()));
        for target in ["http://evil.example/decisions", "http://127.0.0.1:7171/decisions"] {
            assert!(!super::admitted(&Uri::from_static(target), &h, &allowed()), "{target}");
        }
    }

    #[test]
    fn a_browser_request_from_another_site_is_refused() {
        for site in ["same-origin", "none"] {
            assert!(admitted(&headers("localhost:7171", Some(site)), &allowed()), "{site}");
        }
        for site in ["cross-site", "same-site", ""] {
            assert!(!admitted(&headers("localhost:7171", Some(site)), &allowed()), "{site}");
        }
        let mut two = headers("localhost:7171", Some("same-origin"));
        two.append("sec-fetch-site", HeaderValue::from_static("cross-site"));
        assert!(!admitted(&two, &allowed()), "two Sec-Fetch-Site values");
        let mut opaque = headers("localhost:7171", None);
        opaque.insert("sec-fetch-site", HeaderValue::from_bytes(b"same-origin\xff").unwrap());
        assert!(!admitted(&opaque, &allowed()), "a value that is not text");
    }

    #[test]
    fn the_answer_thread_is_configured_only_by_a_valid_value() {
        assert_eq!(answer_thread_from(None), None);
        assert_eq!(answer_thread_from(Some("env/mcp:abc")), AnswerThread::parse("env/mcp:abc").ok());
        for bad in ["", "env", "./mcp:abc", "env/..", "env/t?x"] {
            assert_eq!(answer_thread_from(Some(bad)), None, "{bad:?}");
        }
    }

    #[test]
    fn an_ipv6_bound_address_is_named_in_brackets() {
        assert_eq!(host_name("[fd7a:115c:a1e0::1]:7171".parse().unwrap()), "[fd7a:115c:a1e0::1]");
        assert_eq!(host_name("100.112.149.32:7171".parse().unwrap()), "100.112.149.32");
    }
}
