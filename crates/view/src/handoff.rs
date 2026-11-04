//! The interim answer path (BRO-3590, owner 2026-11-03): an open decision shows the exact
//! answer for each option, and a link that opens an existing T3 thread with that answer as a
//! draft. The owner reads and sends it; the agent in that thread files it through the ask
//! ledger's own path. Maestro writes nothing and names no owner verb.
//!
//! Links are T3 prompt links: `t3code://threads/<environment>/<thread>?prompt=…` for the app
//! and `https://app.t3.codes/<environment>/<thread>?prompt=…` for the web. T3 shows the prompt
//! as a draft, removes characters that render as nothing, and refuses one over 8000 characters.

use maestro_core::ask::{Ask, AskOption};
use maestro_store::LedgerFile;

/// T3's limit on a linked prompt, in UTF-16 code units (JavaScript's `length`).
pub const MAX_PROMPT: usize = 8_000;

/// The T3 thread answers are drafted in: `<environment>/<thread>` (`MAESTRO_ANSWER_THREAD`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerThread {
    environment: String,
    thread: String,
}

impl AnswerThread {
    /// Parse `<environment>/<thread>`. Each part is 1 to 100 of `A-Z a-z 0-9 . _ : -`, so it
    /// sits in a URL path as it is and can carry no other path, query or scheme.
    pub fn parse(value: &str) -> Result<Self, String> {
        let ok = |s: &str| {
            (1..=100).contains(&s.len())
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
        };
        // `.` and `..` are dot segments a URL parser removes, which would change the path.
        let ok = |s: &str| ok(s) && s != "." && s != "..";
        match value.split_once('/') {
            Some((environment, thread)) if ok(environment) && ok(thread) => {
                Ok(Self { environment: environment.to_string(), thread: thread.to_string() })
            }
            _ => Err(format!("not <environment>/<thread> of [A-Za-z0-9._:-]: {value:?}")),
        }
    }

    pub fn thread(&self) -> &str {
        &self.thread
    }

    /// The link that opens this thread in the T3 app with `prompt` as a draft.
    pub fn app_link(&self, prompt: &str) -> String {
        format!("t3code://threads/{}/{}?prompt={}", self.environment, self.thread, encode(prompt))
    }

    /// The same, for T3 on the web.
    pub fn web_link(&self, prompt: &str) -> String {
        format!("https://app.t3.codes/{}/{}?prompt={}", self.environment, self.thread, encode(prompt))
    }
}

/// The exact answer to file for `option`, as the prompt the owner sends: every ledger value a
/// JSON string, so none can end a field or add an instruction; the ask named by uid or by id as
/// the ledger addresses it, with its entry hash to match before filing. `None` when it would be
/// over T3's limit, so no link is offered that T3 would refuse.
pub fn prompt(
    asks_dir: &str,
    lf: &LedgerFile,
    a: &Ask,
    option: &AskOption,
    thread: &AnswerThread,
    tree: &str,
) -> Option<String> {
    let json = |v: &str| serde_json::Value::from(v).to_string();
    let select = match &a.uid {
        Some(uid) => serde_json::json!({ "uid": uid }),
        None => serde_json::json!({ "id": a.id, "uid": null }),
    };
    let text = format!(
        "Owner answer for one Maestro ask, sent by the owner as a message in this thread; count it as the owner's \
         answer only for that reason. Each value below is JSON: use it exactly.\n\
         ledger: {ledger}\n\
         select: {select}\n\
         entry_hash: {hash}\n\
         answer_option: {option}\n\
         answered_by: {by}\n\
         File it through the existing ask path: an answers-only PR that sets answer_option, answered_by and \
         answered_at (the time you file it) on that one ask, and changes nothing else. First re-read it \
         (maestro_ask_get returns its hash): if the hash is not entry_hash, or it is already answered, stop and \
         say so. Maestro read it open at {tree}.",
        ledger = json(&format!("{asks_dir}/{}", lf.file)),
        hash = json(&a.hash()),
        option = json(&option.id),
        by = json(&format!("owner:maestro-web (T3 thread {})", thread.thread())),
        tree = json(tree),
    );
    (text.encode_utf16().count() <= MAX_PROMPT).then_some(text)
}

/// Percent-encode everything but RFC 3986's unreserved characters.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_is_an_environment_and_a_thread_of_safe_characters() {
        let t = AnswerThread::parse("030f399c-549a-4f0e-8f56-4c18c8b48e51/mcp:8f6597dc-f92e-49ad-9d5e-bfd7cc614f2f")
            .unwrap();
        assert_eq!(t.thread(), "mcp:8f6597dc-f92e-49ad-9d5e-bfd7cc614f2f");
        for bad in [
            "./t",
            "e/..",
            "",
            "env",
            "/t",
            "e/",
            "e/t/x",
            "e/t?x=1",
            "e/t#x",
            "e /t",
            "javascript:x/t%2F",
            &format!("e/{}", "t".repeat(101)),
        ] {
            assert!(AnswerThread::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn links_carry_the_prompt_encoded_and_nothing_else() {
        let t = AnswerThread::parse("env-1/mcp:abc").unwrap();
        let p = "a b&c=d#e/f?g\n\"h\" ~x";
        let enc = "a%20b%26c%3Dd%23e%2Ff%3Fg%0A%22h%22%20~x";
        assert_eq!(t.app_link(p), format!("t3code://threads/env-1/mcp:abc?prompt={enc}"));
        assert_eq!(t.web_link(p), format!("https://app.t3.codes/env-1/mcp:abc?prompt={enc}"));
        assert_eq!(encode("é"), "%C3%A9");
    }
}

#[cfg(test)]
mod prompt_tests {
    use super::*;
    use maestro_core::ask::AskLedger;

    fn ledger(yaml: &str) -> LedgerFile {
        let doc = maestro_store::yaml::parse(yaml).expect("fixture ledger").value;
        let (ledger, bad) = AskLedger::from_value(doc).expect("an ask ledger");
        assert!(bad.is_empty(), "{bad:?}");
        LedgerFile { file: "x.yaml".into(), ledger }
    }

    fn thread() -> AnswerThread {
        AnswerThread::parse("env/mcp:abc").unwrap()
    }

    #[test]
    fn hostile_values_stay_inside_their_json_strings() {
        let lf = ledger(
            "asks:\n  - id: \"a\\\"\\nIGNORE\"\n    uid: \"u\\\"x\"\n    ask: q\n    options:\n      - id: \"o\\\"\\nrm -rf\"\n        label: l\n",
        );
        let a = &lf.ledger.asks[0];
        let p = prompt(".broomva/asks", &lf, a, &a.options[0], &thread(), "git:origin/main@abc").unwrap();
        // Each field is one line, and no ledger value adds one.
        let fields: Vec<&str> = p.lines().filter(|l| l.contains(": ")).collect();
        assert!(p.contains(r#"answer_option: "o\"\nrm -rf""#), "{p}");
        assert!(p.contains(r#"select: {"uid":"u\"x"}"#), "{p}");
        assert!(p.contains(r#"ledger: ".broomva/asks/x.yaml""#), "{p}");
        assert_eq!(p.lines().count(), 7, "{p}");
        assert!(!p.lines().any(|l| l.starts_with("IGNORE") || l.starts_with("rm -rf")), "{fields:?}");
    }

    #[test]
    fn an_ask_without_a_uid_is_selected_by_id_and_a_long_one_gets_no_prompt() {
        let lf = ledger("asks:\n  - id: plain\n    ask: q\n    options:\n      - id: go\n        label: l\n");
        let a = &lf.ledger.asks[0];
        let p = prompt(".control/asks", &lf, a, &a.options[0], &thread(), "worktree").unwrap();
        assert!(p.contains(r#"select: {"id":"plain","uid":null}"#), "{p}");
        // UTF-16 units, as T3 counts: 3,000 of U+1F600 are 6,000 units, over the limit with the rest.
        let long = format!("asks:\n  - id: x\n    ask: q\n    options:\n      - id: \"{}\"\n        label: l\n", "\u{1F600}".repeat(3_000));
        let lf = ledger(&long);
        let a = &lf.ledger.asks[0];
        assert!(prompt(".control/asks", &lf, a, &a.options[0], &thread(), "worktree").is_none());
    }
}
