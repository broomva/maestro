//! An ask's `sources`, as the plugin shows them: a port of `sourceUrl` and `sourceText` in
//! maestro-paseo `shared/rich-ask.ts`. A ticket opens on Linear, a PR on GitHub, an https URL
//! as itself; files and sessions are named, never linked.

use serde_json::Value;

pub(crate) struct Source {
    pub kind: String,
    pub text: String,
    pub url: Option<String>,
}

/// `SOURCE_KINDS`
const KINDS: [&str; 7] = ["ticket", "pr", "doc", "spec", "url", "file", "session"];
/// `SOURCES_MAX`
const MAX: usize = 10;

/// The sources the ask lists, as `sourcesOf` keeps them: a known kind and a ref of that kind's
/// shape (trimmed), its label if it is at most 80 long; at most ten.
pub(crate) fn of(sources: Option<&Value>) -> Vec<Source> {
    sources
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|s| s.is_object())
        .filter_map(|s| {
            let kind = s.get("kind")?.as_str()?;
            let r = super::rich::trim_ws(s.get("ref").and_then(Value::as_str).unwrap_or(""));
            if !KINDS.contains(&kind) || !shape(kind, r) {
                return None;
            }
            let label = super::rich::words(s.get("label"), 80);
            Some(Source { kind: kind.into(), text: label.unwrap_or_else(|| text(kind, r)), url: url(kind, r) })
        })
        .take(MAX)
        .collect()
}

/// `sourceShape`
fn shape(kind: &str, r: &str) -> bool {
    match kind {
        "ticket" => ticket(r),
        "pr" => pr(r).is_some(),
        "url" => https(r),
        "session" => session(r),
        _ => safe_relative(r),
    }
}

/// `/^[0-9a-f]{8}(?:-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})?$/`
fn session(r: &str) -> bool {
    let hex = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    let parts: Vec<&str> = r.split('-').collect();
    match parts.as_slice() {
        [a] => hex(a, 8),
        [a, b, c, d, e] => hex(a, 8) && hex(b, 4) && hex(c, 4) && hex(d, 4) && hex(e, 12),
        _ => false,
    }
}

/// `safeRelative(ref)`: no control character; the path before any `#` is relative, with no
/// `~`, `:` or backslash, and no empty, `.` or `..` part.
fn safe_relative(r: &str) -> bool {
    if r.chars().any(|c| (c as u32) < 0x20 || c == '\u{7f}') {
        return false;
    }
    let path = r.split('#').next().unwrap_or("");
    if path.is_empty() || path.starts_with('/') || path.starts_with('~') || path.contains(':') || path.contains('\\') {
        return false;
    }
    path.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

/// `/^[A-Z][A-Z0-9]{1,9}-\d+$/`
fn ticket(r: &str) -> bool {
    let Some((team, num)) = r.split_once('-') else { return false };
    let mut team = team.chars();
    team.next().is_some_and(|c| c.is_ascii_uppercase())
        && (1..=9).contains(&team.clone().count())
        && team.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && !num.is_empty()
        && num.bytes().all(|b| b.is_ascii_digit())
}

/// `/^([\w.-]+)\/([\w.-]+)#(\d+)$/`
fn pr(r: &str) -> Option<(&str, &str, &str)> {
    let word =
        |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
    let (repo, num) = r.split_once('#')?;
    let (owner, name) = repo.split_once('/')?;
    (word(owner) && word(name) && !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()))
        .then_some((owner, name, num))
}

/// `/^https:\/\/[!-.0-~]+(?:\/[!-~]*)?$/`: printable ASCII, a host with no `/`.
fn https(r: &str) -> bool {
    let Some(rest) = r.strip_prefix("https://") else { return false };
    let (host, path) = rest.split_once('/').map_or((rest, None), |(h, p)| (h, Some(p)));
    !host.is_empty()
        && host.bytes().all(|b| matches!(b, b'!'..=b'.' | b'0'..=b'~'))
        && path.is_none_or(|p| p.bytes().all(|b| matches!(b, b'!'..=b'~')))
}

fn url(kind: &str, r: &str) -> Option<String> {
    match kind {
        "ticket" if ticket(r) => Some(format!("https://linear.app/broomva/issue/{r}")),
        "pr" => pr(r).map(|(o, n, num)| format!("https://github.com/{o}/{n}/pull/{num}")),
        "url" if https(r) => Some(r.to_string()),
        _ => None,
    }
}

fn text(kind: &str, r: &str) -> String {
    match kind {
        "doc" | "spec" | "file" => {
            // `ref.split("#", 2)`: the path, and the anchor up to any second `#`.
            let mut parts = r.split('#');
            let path = parts.next().unwrap_or(r);
            let name = path.rsplit('/').next().unwrap_or(r);
            match parts.next() {
                Some(anchor) if !anchor.is_empty() => format!("{name}#{anchor}"),
                _ => name.to_string(),
            }
        }
        "session" => format!("session {}", r.chars().take(8).collect::<String>()),
        "url" => {
            let host = r.strip_prefix("https://").unwrap_or(r);
            host.strip_suffix('/').unwrap_or(host).to_string()
        }
        _ => r.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn links_only_what_the_plugin_links() {
        assert_eq!(url("ticket", "BRO-3203").as_deref(), Some("https://linear.app/broomva/issue/BRO-3203"));
        assert_eq!(url("ticket", "B-1"), None, "a one-letter team");
        assert_eq!(url("ticket", "BRO-"), None);
        assert_eq!(url("ticket", "bro-1"), None);
        assert_eq!(url("pr", "broomva/maestro#12").as_deref(), Some("https://github.com/broomva/maestro/pull/12"));
        assert_eq!(url("pr", "broomva/maestro#x"), None);
        assert_eq!(url("pr", "a/b/c#1"), None);
        assert_eq!(url("url", "https://example.com/a?b=1").as_deref(), Some("https://example.com/a?b=1"));
        assert_eq!(url("url", "http://example.com"), None);
        assert_eq!(url("url", "javascript:alert(1)"), None);
        assert_eq!(url("url", "https://exa mple.com"), None);
        assert_eq!(url("url", "https://example.com/ü"), None);
        assert_eq!(url("file", "docs/a.md"), None);
        assert_eq!(url("session", "2fb204ca"), None);
    }

    #[test]
    fn texts_follow_the_plugin() {
        assert_eq!(text("spec", "docs/specs/x.html#owner"), "x.html#owner");
        assert_eq!(text("session", "2fb204ca-0b7b-42cd-b9e1-4b95e7be50bb"), "session 2fb204ca");
        assert_eq!(text("url", "https://example.com/"), "example.com");
        assert_eq!(text("ticket", "BRO-1"), "BRO-1");
        assert_eq!(text("doc", "a/b.md#x#y"), "b.md#x");
        assert_eq!(text("url", "https://e.com//"), "e.com/");
        let s = of(Some(&json!([{ "kind": "pr", "ref": "a/b#1", "label": "view-core" }, { "kind": "ticket" }, "x"])));
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].text, "view-core");
    }

    #[test]
    fn a_source_the_plugin_drops_is_not_shown() {
        let kept = |v: serde_json::Value| of(Some(&json!([v]))).len() == 1;
        assert!(kept(json!({ "kind": "doc", "ref": " docs/a.md#x " })));
        assert_eq!(
            of(Some(&json!([{ "kind": "ticket", "ref": " BRO-7\n" }])))[0].url.as_deref(),
            Some("https://linear.app/broomva/issue/BRO-7")
        );
        assert!(kept(json!({ "kind": "session", "ref": "2fb204ca" })));
        assert!(!kept(json!({ "kind": "javascript", "ref": "x" })), "an unknown kind");
        assert!(!kept(json!({ "kind": "url", "ref": "data:text/html,x" })));
        assert!(!kept(json!({ "kind": "ticket", "ref": "nope" })));
        assert!(!kept(json!({ "kind": "session", "ref": "2FB204CA" })));
        for bad in ["/etc/passwd", "~/x", "a/../b", "a//b", "./a", "c:x", "a\\b", "a\u{1}b", ""] {
            assert!(!kept(json!({ "kind": "file", "ref": bad })), "{bad:?}");
        }
        let long = json!({ "kind": "ticket", "ref": "BRO-1", "label": "x".repeat(81) });
        assert_eq!(of(Some(&json!([long])))[0].text, "BRO-1", "a label over 80 falls back to the ref");
        let many: Vec<_> = (0..12).map(|i| json!({ "kind": "ticket", "ref": format!("BRO-{i}") })).collect();
        assert_eq!(of(Some(&json!(many))).len(), 10);
    }
}
