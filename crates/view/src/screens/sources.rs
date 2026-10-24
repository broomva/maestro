//! An ask's `sources`, as the plugin shows them: a port of `sourceUrl` and `sourceText` in
//! maestro-paseo `shared/rich-ask.ts`. A ticket opens on Linear, a PR on GitHub, an https URL
//! as itself; files and sessions are named, never linked.

use serde_json::Value;

pub(crate) struct Source {
    pub kind: String,
    pub text: String,
    pub url: Option<String>,
}

/// The sources the ask lists. An entry without a text `kind` and `ref` is not shown.
pub(crate) fn of(sources: Option<&Value>) -> Vec<Source> {
    sources
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let kind = s.get("kind")?.as_str()?;
            let r = s.get("ref")?.as_str()?;
            let label = s.get("label").and_then(Value::as_str).filter(|l| !l.is_empty());
            Some(Source {
                kind: kind.into(),
                text: label.map_or_else(|| text(kind, r), str::to_string),
                url: url(kind, r),
            })
        })
        .collect()
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
            let (path, anchor) = r.split_once('#').map_or((r, None), |(p, a)| (p, Some(a)));
            let name = path.rsplit('/').next().unwrap_or(r);
            anchor.map_or_else(|| name.to_string(), |a| format!("{name}#{a}"))
        }
        "session" => format!("session {}", r.chars().take(8).collect::<String>()),
        "url" => r.strip_prefix("https://").unwrap_or(r).trim_end_matches('/').to_string(),
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
        let s = of(Some(&json!([{ "kind": "pr", "ref": "a/b#1", "label": "view-core" }, { "kind": "ticket" }, "x"])));
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].text, "view-core");
    }
}
