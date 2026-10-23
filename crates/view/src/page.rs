//! The documents Maestro serves: the MCP App page (the `ui://` resource, with the bridge),
//! and a self-contained snapshot of one render for hosts that cannot run MCP Apps.

use maud::{DOCTYPE, Markup, PreEscaped, html};
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

/// Every style the view uses, as one stylesheet.
pub fn css() -> &'static str {
    &CSS
}

fn head(title: &str) -> Markup {
    html! {
        meta charset="utf-8";
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
            head { (head("Maestro")) }
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

/// One render as a standalone document with no script: what a host without MCP Apps shows
/// (T3's `html_render` in Claude threads). It does not update; its footer names the tree read.
pub fn snapshot(fragment: &str, theme: Option<&str>) -> String {
    html! {
        (DOCTYPE)
        html lang="en" data-theme=[theme] {
            head { (head("Maestro snapshot")) }
            body {
                main id="maestro-root" data-snapshot="true" { (PreEscaped(fragment)) }
            }
        }
    }
    .into_string()
}
