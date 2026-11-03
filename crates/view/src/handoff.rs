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

/// T3's limit on a linked prompt, in characters.
pub const MAX_PROMPT: usize = 8_000;
/// The most characters of a ledger label quoted in a prompt.
const MAX_LABEL: usize = 200;

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

/// The exact answer to file for `option`, as the prompt the owner sends.
pub fn prompt(lf: &LedgerFile, a: &Ask, option: &AskOption, thread: &AnswerThread, tree: &str) -> String {
    let label: String = option.label.chars().take(MAX_LABEL).collect();
    let text = format!(
        "Owner answer for Maestro ask {file}#{handle}: option `{id}` ({label}).\n\
         File it through the existing ask path: an answers-only PR to .control/asks/{file} that sets, on that ask, \
         answer_option: \"{id}\", answered_by: \"owner:maestro-web (T3 thread {thread})\" and answered_at: the time you file it. \
         Do not answer it any other way. Maestro read it open at {tree}; if it is already answered there, say so instead.",
        file = lf.file,
        handle = a.handle(),
        id = option.id,
        thread = thread.thread(),
    );
    text.chars().take(MAX_PROMPT).collect()
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
        let t = AnswerThread::parse("030f399c-549a-4f0e-8f56-4c18c8b48e51/mcp:8f6597dc-f92e-49ad-9d5e-bfd7cc614f2f").unwrap();
        assert_eq!(t.thread(), "mcp:8f6597dc-f92e-49ad-9d5e-bfd7cc614f2f");
        for bad in ["", "env", "/t", "e/", "e/t/x", "e/t?x=1", "e/t#x", "e /t", "javascript:x/t%2F", &format!("e/{}", "t".repeat(101))]
        {
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
