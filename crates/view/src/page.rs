//! The documents Maestro serves: the MCP App page (the `ui://` resource, with the bridge),
//! and a self-contained snapshot of one render for hosts that cannot run MCP Apps.

use maud::{DOCTYPE, Markup, PreEscaped, html};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

/// The App's resource URI. A render tool names it in `_meta.ui.resourceUri`.
pub const APP_URI: &str = "ui://maestro/view.html";
/// The MCP Apps HTML profile.
pub const APP_MIME: &str = "text/html;profile=mcp-app";
/// How often the App polls for a new revision while visible.
pub const POLL_MS: u32 = 15_000;

/// The pinned MCP Apps bridge, built by `crates/view/bridge` (`bun run build`; `--check` in CI).
pub const BRIDGE_JS: &str = include_str!("../assets/bridge.js");
const FOUNDATION_CSS: &str = include_str!("../../../design-system/broomva/broomva-foundation.css");
const VIEW_CSS: &str = include_str!("../assets/view.css");

/// The foundation without its `@font-face` rule: the App runs in a sandboxed frame that can
/// fetch no font file, so the display face falls back to the system stack.
static CSS: LazyLock<String> = LazyLock::new(|| {
    let mut css = String::with_capacity(FOUNDATION_CSS.len() + VIEW_CSS.len());
    let mut rest = FOUNDATION_CSS;
    while let Some(at) = rest.find("@font-face") {
        css.push_str(&rest[..at]);
        rest = rest[at..].find('}').map_or("", |end| &rest[at + end + 1..]);
    }
    css.push_str(rest);
    css.push('\n');
    css.push_str(VIEW_CSS);
    css
});

/// The App's Content-Security-Policy: no fetch of any kind, and no script but the pinned
/// bridge, named by its hash. Markup the bridge swaps in can carry no running script or
/// handler even if a tool result were forged; maud already escapes every ledger value.
static APP_CSP: LazyLock<String> = LazyLock::new(|| {
    let hash = base64(&Sha256::digest(BRIDGE_JS.as_bytes()));
    format!("default-src 'none'; style-src 'unsafe-inline'; img-src data:; script-src 'sha256-{hash}'")
});
/// The snapshot runs no script at all.
const SNAPSHOT_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src data:";

/// The policy the App page declares.
pub fn app_csp() -> &'static str {
    &APP_CSP
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { char::from(ABC[(n >> (18 - 6 * i)) as usize & 63]) } else { '=' });
        }
    }
    out
}

/// Every style the view uses, as one stylesheet.
pub fn css() -> &'static str {
    &CSS
}

fn head(title: &str, csp: &str) -> Markup {
    html! {
        meta charset="utf-8";
        meta http-equiv="Content-Security-Policy" content=(csp);
        meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover";
        meta name="color-scheme" content="light dark";
        title { (title) }
        style { (PreEscaped(css())) }
    }
}

/// The MCP App page: an empty root the bridge fills from the render tool's result.
pub fn app() -> String {
    html! {
        (DOCTYPE)
        html lang="en" {
            head { (head("Maestro", app_csp())) }
            body {
                main id="maestro-root" data-poll-ms=(POLL_MS) aria-live="polite" {
                    p class="mv-empty bv-meta" { "Waiting for Maestro." }
                }
                script type="module" { (PreEscaped(BRIDGE_JS)) }
            }
        }
    }
    .into_string()
}

/// Home Screen and manifest icons.
pub const ICON_180_PNG: &[u8] = include_bytes!("../assets/icon-180.png");
pub const ICON_192_PNG: &[u8] = include_bytes!("../assets/icon-192.png");
pub const ICON_512_PNG: &[u8] = include_bytes!("../assets/icon-512.png");

/// The owner script (BRO-4042): the one script the web pages run, named in their policy by hash.
pub const OWNER_JS: &str = concat!(include_str!("../assets/owner.js"), "\n", include_str!("../assets/navigation.js"));

/// The web pages' policy (BRO-3588, BRO-4042): no form may post, no page may frame them, only
/// the same origin's manifest may load, and the one script that runs is the owner script,
/// named by its hash, talking only to its own origin.
/// The CSP source for an inline script: base64 of its SHA-256.
pub fn script_hash(text: &str) -> String {
    base64(&Sha256::digest(text.as_bytes()))
}

static WEB_CSP: LazyLock<String> = LazyLock::new(|| {
    let hash = script_hash(OWNER_JS);
    format!(
        "default-src 'none'; style-src 'unsafe-inline'; img-src 'self' data:; manifest-src 'self'; form-action 'none'; frame-ancestors 'none'; base-uri 'none'; script-src 'sha256-{hash}'; connect-src 'self'"
    )
});

/// The policy the web pages declare.
pub fn web_csp() -> &'static str {
    &WEB_CSP
}

/// The web pages' styles (BRO-3783): the shell, the three-column decision and the phone layout.
/// Only on the web pages, so the App's stylesheet holds only what the App places.
const WEB_CSS: &str = concat!(include_str!("../assets/web.css"), include_str!("../assets/today.css"));
const HEALTH_CSS: &str = include_str!("../assets/health.css");

/// One place in the web pages' navigation: its path and its label.
pub const WEB_NAV: [(&str, &str); 3] = [("/decisions", "Decisions"), ("/today", "Today"), ("/health", "Health")];

/// What the web pages' header says (BRO-3783), from the same read as the page under it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shell {
    /// The tree read, as the snapshot names it: `git:<rev>@<commit>` or `worktree`.
    pub tree: String,
    /// Open asks and how many of them block, when the asks were read.
    pub open: Option<(usize, usize)>,
    /// The asks read in part: every count is a lower bound.
    pub partial: bool,
    /// What the needs list holds for the owner (asks and work), when it read in full.
    pub needs: Option<usize>,
    /// Nothing waits on the owner, by a whole read: the one condition for "Nothing needs you",
    /// the same one the calm screen shows on.
    pub calm: bool,
    /// How long ago the commit read was made (`3 h`), when it is known.
    pub committed: Option<String>,
}

impl Shell {
    /// The line beside the wordmark. Unread is said, never shown as none; a partial read is a lower
    /// bound; "Nothing needs you" only when [`Shell::calm`] says so.
    pub fn summary(&self) -> String {
        let Some((n, blocking)) = self.open else { return "Decisions could not be read".into() };
        if self.calm {
            return "Nothing needs you".into();
        }
        if n == 0 && !self.partial {
            return match self.needs {
                Some(h) if h > 0 => {
                    format!("No decision open, {h} other {} for you", if h == 1 { "hold" } else { "holds" })
                }
                _ => "No decision open; not everything that could need you was read".into(),
            };
        }
        let at_least = if self.partial { "at least " } else { "" };
        let noun = if n == 1 { "decision" } else { "decisions" };
        let blocks = match blocking {
            0 if self.partial => "none seen blocking".to_string(),
            0 => "none blocking".to_string(),
            b => format!("{b} blocking"),
        };
        format!("{at_least}{n} {noun} for you, {blocks}")
    }

    /// The summary's state for styling: `unavailable`, `partial`, `calm`, `open` (asks open), or
    /// `held` (no ask open, and something else holds or was not read). A phone shows the line only
    /// when it is not `calm` or `open`.
    pub fn state(&self) -> &'static str {
        match self.open {
            None => "unavailable",
            Some(_) if self.partial => "partial",
            Some(_) if self.calm => "calm",
            Some((0, _)) => "held",
            Some(_) => "open",
        }
    }

    /// The count on the Decisions tab: `3`, `3+` for a partial read, `?` when unread.
    pub fn badge(&self) -> String {
        match self.open {
            None => "?".into(),
            Some((n, _)) if self.partial => format!("{n}+"),
            Some((n, _)) => n.to_string(),
        }
    }

    /// The tree as the header shows it: `main 0123abc` for `git:origin/main@0123abc4567`. The
    /// commit is the text after the last `@`, cut by characters; an unresolved one is said.
    pub fn tree_short(&self) -> String {
        match self.tree.strip_prefix("git:").and_then(|t| t.rsplit_once('@')) {
            Some((rev, "unresolved")) => format!("{} (commit unresolved)", rev.strip_prefix("origin/").unwrap_or(rev)),
            Some((rev, commit)) => {
                let rev = rev.strip_prefix("origin/").unwrap_or(rev);
                format!("{rev} {}", commit.chars().take(7).collect::<String>())
            }
            None => self.tree.clone(),
        }
    }
}

/// One render as a web page for a browser (BRO-3588): the shell with `current` marked, the
/// manifest a phone installs it from, and the owner script (BRO-4042), the page's only script.
pub fn web(fragment: &str, title: &str, current: &str, shell: &Shell) -> String {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                // frame-ancestors is enforced by the HTTP header, not supported in a meta policy.
                (head(&format!("{title} · Maestro"), &web_csp().replace("; frame-ancestors 'none'", "")))
                meta name="theme-color" content="#f7f6f1" media="(prefers-color-scheme: light)";
                meta name="theme-color" content="#0b1622" media="(prefers-color-scheme: dark)";
                meta name="mobile-web-app-capable" content="yes";
                meta name="apple-mobile-web-app-capable" content="yes";
                meta name="apple-mobile-web-app-title" content="Maestro";
                meta name="apple-mobile-web-app-status-bar-style" content="black-translucent";
                link rel="apple-touch-icon" sizes="180x180" href="/apple-touch-icon.png";
                link rel="icon" type="image/png" sizes="192x192" href="/icon-192.png";
                link rel="manifest" href="/manifest.webmanifest";
                style { (PreEscaped(WEB_CSS)) }
                style { (PreEscaped(HEALTH_CSS)) }
            }
            body class="mv-web" {
                header class="mv-web-head" {
                    p class="mv-brand" {
                        a class="mv-wordmark" href="/decisions" { "Maestro" }
                        " "
                        span class="mv-brand-sub" data-state=(shell.state()) { (shell.summary()) }
                    }
                    nav class="mv-tabs mv-web-nav" aria-label="Maestro" {
                        @for (path, label) in WEB_NAV {
                            a class="mv-tab" href=(path) aria-current=[(path == current).then_some("page")] {
                                (label)
                                @if path == "/decisions" { " " span class="mv-nav-count" { (shell.badge()) } }
                            }
                        }
                    }
                    p class="mv-web-tree bv-meta" {
                        span class="mv-dot" aria-hidden="true" {}
                        span class="bv-code" title=(shell.tree) { (shell.tree_short()) }
                        @if let Some(ago) = &shell.committed { " · committed " (ago) " ago" }
                    }
                }
                main id="maestro-root" data-web="true" { (PreEscaped(fragment)) }
                script { (PreEscaped(OWNER_JS)) }
            }
        }
    }
    .into_string()
}

/// The owner page (BRO-4042): set up the owner's passkey on this device, once.
pub fn owner(state: &OwnerPage, shell: &Shell) -> String {
    let body = html! {
        section class="mv-owner" {
            p class="mv-kicker" { "Your passkey" }
            @match state {
                OwnerPage::Off(why) => {
                    h1 class="mv-owner-title" { "Answering is not set up here" }
                    p class="mv-owner-sub" { (why) }
                },
                OwnerPage::Enroll => {
                    h1 class="mv-owner-title" { "Set up your passkey" }
                    p class="mv-owner-sub" {
                        "Maestro writes an answer only when you sign it with Touch ID or Face ID. "
                        "Set it up once on this device with the setup code Maestro printed when it started."
                    }
                    label class="mv-owner-code" {
                        span { "Setup code" }
                        input type="text" name="code" autocomplete="one-time-code" inputmode="text" spellcheck="false";
                    }
                    button class="mv-go" type="button" data-enroll="true" { "Set up with Touch ID or Face ID" }
                    p class="mv-answer-status" data-enroll-status="true" role="status" aria-live="polite" {}
                },
                OwnerPage::Enrolled(when) => {
                    h1 class="mv-owner-title" { "Your passkey is set up" }
                    p class="mv-owner-sub" {
                        "Decisions take your answer in place; each answer asks for Touch ID or Face ID. "
                        "If a passkey below was not set up by you, tell Maestro's maintainer before answering."
                    }
                    ul class="mv-owner-keys" {
                        @for at in when { li class="bv-code" { "Set up " (at) } }
                    }
                    a class="mv-go" href="/decisions" { "Go to decisions" }
                },
            }
        }
    };
    web(&body.into_string(), "Your passkey", "", shell)
}

/// What the owner page shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerPage {
    Off(String),
    Enroll,
    /// When each enrolled passkey was set up, so one the owner did not add is visible.
    Enrolled(Vec<String>),
}

/// The web app manifest and its Home Screen icons.
pub fn web_manifest() -> String {
    serde_json::json!({
        "id": "/decisions",
        "description": "Decisions, today and health for your workspace.",
        "icons": [
            { "src": "/icon-192.png", "sizes": "192x192", "type": "image/png", "purpose": "any" },
            { "src": "/icon-512.png", "sizes": "512x512", "type": "image/png", "purpose": "any maskable" }
        ],
        "name": "Maestro",
        "short_name": "Maestro",
        "start_url": "/decisions",
        "scope": "/",
        "display": "standalone",
        "background_color": "#f7f6f1",
        "theme_color": "#f7f6f1",
    })
    .to_string()
}

/// One render as a standalone document with no script: what a host without MCP Apps shows
/// (T3's `html_render` in Claude threads). It does not update; its footer names the tree read.
pub fn snapshot(fragment: &str, theme: Option<&str>) -> String {
    html! {
        (DOCTYPE)
        html lang="en" data-theme=[theme] {
            head { (head("Maestro snapshot", SNAPSHOT_CSP)) }
            body {
                main id="maestro-root" data-snapshot="true" { (PreEscaped(fragment)) }
            }
        }
    }
    .into_string()
}
