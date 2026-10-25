//! The App page embeds the pinned bridge and the foundation CSS, and nothing in either can
//! close the element that holds it.

use maestro_view::page;
use sha2::{Digest, Sha256};

/// sha256 of `assets/bridge.js`, the build of `bridge/src/bridge.ts` with
/// `@modelcontextprotocol/ext-apps` 2.0.3. Changing the bridge means rebuilding it
/// (`cd crates/view/bridge && bun run build`) and updating this pin in the same commit.
const BRIDGE_SHA256: &str = include_str!("../assets/bridge.js.sha256");

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn the_bridge_is_the_pinned_build() {
    assert_eq!(hex(&Sha256::digest(page::BRIDGE_JS.as_bytes())), BRIDGE_SHA256.trim());
    let pkg = include_str!("../bridge/package.json");
    assert!(pkg.contains(r#""@modelcontextprotocol/ext-apps": "2.0.3""#), "the bridge's dependency is pinned exactly");
}

#[test]
fn the_app_page_holds_one_script_and_no_remote_fetch() {
    let html = page::app();
    assert_eq!(html.matches("<script").count(), 1);
    assert_eq!(html.matches("</script>").count(), 1);
    assert!(html.contains(r#"<main id="maestro-root" data-poll-ms="15000""#));
    assert!(!page::BRIDGE_JS.to_ascii_lowercase().contains("</script"));
    assert!(!page::css().to_ascii_lowercase().contains("</style"));
    // The sandboxed frame fetches nothing: no font file, no stylesheet link.
    assert!(!page::css().contains("@font-face"));
    assert!(!page::css().contains("url("), "a url() in the CSS would be a fetch the frame cannot make");
    assert!(!html.contains("<link"));
}

/// The same digest as `BRIDGE_SHA256`, base64 as CSP writes it
/// (`openssl dgst -sha256 -binary assets/bridge.js | base64`). Update both together.
const BRIDGE_CSP_HASH: &str = "ZKbieos1yCzyS+MCBiLtX1/0BXQDI3untm7P5GRhhPQ=";

#[test]
fn the_app_page_runs_only_the_pinned_bridge_and_fetches_nothing() {
    let csp = page::app_csp();
    assert_eq!(
        csp,
        format!("default-src 'none'; style-src 'unsafe-inline'; img-src data:; script-src 'sha256-{BRIDGE_CSP_HASH}'")
    );
    let html = page::app();
    let meta = format!(r#"<meta http-equiv="Content-Security-Policy" content="{csp}">"#);
    let at = html.find(&meta).expect("the App page declares its policy");
    assert!(
        at < html.find("<style").unwrap() && at < html.find("<script").unwrap(),
        "the policy precedes what it governs"
    );
    let doc = page::snapshot("<p>x</p>", None);
    assert!(doc.contains(r#"<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:">"#));
}

#[test]
fn the_css_keeps_the_foundation_tokens_and_the_fallbacks() {
    let css = page::css();
    assert!(css.contains("--bv-ink: oklch(0.175 0.022 265)"));
    assert!(css.contains(r#"[data-theme="dark"]"#));
    assert!(css.contains("@media (prefers-reduced-motion: reduce)"));
    assert!(css.contains("@media (prefers-reduced-transparency: reduce)"));
    assert!(css.contains("--mv-spring: linear("));
}

#[test]
fn a_snapshot_is_standalone_and_inert() {
    let doc = page::snapshot("<p>x</p>", Some("dark"));
    assert!(doc.starts_with("<!DOCTYPE html>"));
    assert!(doc.contains(r#"<html lang="en" data-theme="dark">"#));
    assert!(!doc.contains("<script"));
    assert!(doc.contains(r#"<main id="maestro-root" data-snapshot="true"><p>x</p></main>"#));
    assert!(page::snapshot("", None).contains(r#"<html lang="en"><head>"#));
}
