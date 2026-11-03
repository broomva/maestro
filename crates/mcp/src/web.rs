//! Maestro's web pages (BRO-3588): Decisions, one decision, Today and Health, rendered on the
//! server by `maestro-view` from the same catalog blocks the MCP App places, on the daemon
//! `maestro serve --http` already runs. A phone or browser on the tailnet reads them with no
//! Paseo and no MCP host.
//!
//! Read-only. Every route is GET, the pages run no script and post no form, and no route
//! writes a ledger: answering waits for the authenticated owner path (BRO-3273, BRO-3279).
//! A request must name the bound address (or a loopback name) in `Host`, as on `/mcp`, and a
//! browser request from another site is refused.

use crate::MaestroServer;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::uri::Authority;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use maestro_view::page;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Clone)]
struct Web {
    server: MaestroServer,
    hosts: Arc<Vec<String>>,
}

/// The web routes for a listener bound at `local`.
pub fn router(server: MaestroServer, local: SocketAddr) -> Router {
    let hosts = vec!["localhost".to_string(), "127.0.0.1".to_string(), "[::1]".to_string(), host_name(local)];
    let web = Web { server, hosts: Arc::new(hosts) };
    Router::new()
        .route("/", get(root))
        .route("/decisions", get(decisions))
        .route("/decisions/{ledger}/{id}", get(decision))
        .route("/today", get(today))
        .route("/health", get(health))
        .route("/manifest.webmanifest", get(manifest))
        .with_state(web)
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
    match headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        None => true,
        Some(site) => site == "same-origin" || site == "none",
    }
}

fn refused() -> Response {
    (StatusCode::FORBIDDEN, "refused: this page is served only to its own host and origin\n").into_response()
}

fn with_headers(mut res: Response, content_type: &'static str) -> Response {
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(page::WEB_CSP));
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
    let rendered = tokio::task::spawn_blocking(move || {
        let store = server.store();
        // A readable ledger with no such ask is no page; an unread one still renders as unavailable.
        if let Some((ledger, id)) = &ask
            && absent(&store, ledger, id)
        {
            return None;
        }
        Some(maestro_view::render::render(&store, &validated))
    })
    .await;
    match rendered {
        Ok(Some(r)) => with_headers(page::web(&r.html, title, current).into_response(), "text/html; charset=utf-8"),
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
        for host in ["localhost", "localhost.:7171", "127.0.0.1:7171", "[::1]", "[::1]:7171", "100.112.149.32:7171", "LOCALHOST:1"] {
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
    }

    #[test]
    fn an_ipv6_bound_address_is_named_in_brackets() {
        assert_eq!(host_name("[fd7a:115c:a1e0::1]:7171".parse().unwrap()), "[fd7a:115c:a1e0::1]");
        assert_eq!(host_name("100.112.149.32:7171".parse().unwrap()), "100.112.149.32");
    }
}
