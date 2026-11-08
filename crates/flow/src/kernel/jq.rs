//! jq guards, through jaq, checked at compile time only. Replay never evaluates a guard: a
//! verdict is computed once, when it is written (`choose` for an edge, the end of an iteration
//! for an `until`, both 3b2), and recorded in the log.
//!
//! [`check`] parses and compiles a guard and refuses words and symbols that make it impure or
//! that are known to run without bound: user definitions (`def`, so no recursion), unbounded
//! generators (`range`, `repeat`, `recurse`, `while`, `until`, `combinations`), the clock and the
//! environment (`now`, `env`, `localtime`, `strflocaltime`), effects (`debug`, `stderr`, `halt`,
//! `halt_error` and their natives) and `*`, which repeats a string as many times as a number in
//! the log says. Input streams and modules are not defined at all. This denylist does not bound
//! a guard's work: `reduce` and nested iteration over the input can still be costly, so the
//! evaluator writers use (3b2) must bound it itself.

use jaq_core::load::lex::{StrPart, Tok, Token};
use jaq_core::load::{Arena, File, Lexer, Loader};
use jaq_core::{Compiler, data};
use jaq_json::Val;

type Data = data::JustLut<Val>;

/// Words a guard may not contain, wherever they appear (a field named so is written `.["range"]`).
const BANNED_WORDS: &[&str] = &[
    "def",
    "range",
    "repeat",
    "recurse",
    "while",
    "until",
    "combinations",
    "now",
    "env",
    "localtime",
    "strflocaltime",
    "debug",
    "debug_empty",
    "stderr",
    "stderr_empty",
    "halt",
    "halt_error",
];

/// The first banned word or symbol in `tokens`, interpolations and blocks included.
fn banned<'a>(tokens: &[Token<&'a str>]) -> Option<&'a str> {
    tokens.iter().find_map(|Token(s, tok)| match tok {
        Tok::Word if BANNED_WORDS.contains(s) => Some(*s),
        Tok::Sym if s.starts_with('*') => Some(*s),
        Tok::Block(inner) => banned(inner),
        Tok::Str(parts) => parts.iter().find_map(|p| match p {
            StrPart::Term(t) => banned(std::slice::from_ref(t)),
            _ => None,
        }),
        _ => None,
    })
}

/// Checks a guard without running it.
pub(crate) fn check(code: &str) -> Result<(), String> {
    let tokens = Lexer::new(code).lex().map_err(|errs| format!("it does not parse ({} error(s))", errs.len()))?;
    if let Some(word) = banned(&tokens) {
        return Err(format!("it uses {word}, which a guard may not"));
    }
    let defs = jaq_core::defs().chain(jaq_std::defs()).chain(jaq_json::defs());
    let funs = jaq_core::funs::<Data>().chain(jaq_std::funs::<Data>()).chain(jaq_json::funs::<Data>());
    let arena = Arena::default();
    let modules = Loader::new(defs)
        .load(&arena, File { code, path: () })
        .map_err(|errs| format!("it does not parse ({} error(s))", errs.len()))?;
    Compiler::default().with_funs(funs).compile(modules).map(|_| ()).map_err(|errs| {
        let names: Vec<&str> = errs.iter().flat_map(|(_, e)| e.iter().map(|(name, _)| *name)).collect();
        format!("it names filters or variables the kernel does not define: {}", names.join(", "))
    })
}
