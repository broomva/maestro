//! jq guards, through jaq: checked at compile time ([`check`]), and run only by the guard
//! evaluator's child process ([`first`], see `guard`). Replay never evaluates a guard: a verdict
//! is computed once, when it is written (`choose` for an edge, `complete` for an effect's
//! preconditions, the end of an iteration for an `until`), and recorded in the log.
//!
//! [`check`] parses and compiles a guard and refuses words and symbols that make it impure or
//! that are known to run without bound: user definitions (`def`, so no recursion), unbounded
//! generators (`range`, `repeat`, `recurse`, `while`, `until`, `combinations`), the clock and the
//! environment (`now`, `env`, `localtime`, `strflocaltime`), effects (`debug`, `stderr`, `halt`,
//! `halt_error` and their natives) and `*`, which repeats a string as many times as a number in
//! the log says. Input streams and modules are not defined at all. This denylist does not bound
//! a guard's work: `reduce` and nested iteration over the input can still be costly, so the
//! evaluator bounds it itself, in a child process it kills at a timeout (`guard`).

use jaq_core::load::lex::{StrPart, Tok, Token};
use jaq_core::load::{Arena, File, Lexer, Loader};
use jaq_core::{Compiler, Ctx, Vars, data, unwrap_valr};
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
    with_filter(code, |_| ())
}

/// Compiles `code` with the kernel's definitions and hands the filter to `then`.
fn with_filter<T>(code: &str, then: impl FnOnce(&jaq_core::Filter<Data>) -> T) -> Result<T, String> {
    let defs = jaq_core::defs().chain(jaq_std::defs()).chain(jaq_json::defs());
    let funs = jaq_core::funs::<Data>().chain(jaq_std::funs::<Data>()).chain(jaq_json::funs::<Data>());
    let arena = Arena::default();
    let modules = Loader::new(defs)
        .load(&arena, File { code, path: () })
        .map_err(|errs| format!("it does not parse ({} error(s))", errs.len()))?;
    let filter = Compiler::default().with_funs(funs).compile(modules).map_err(|errs| {
        let names: Vec<&str> = errs.iter().flat_map(|(_, e)| e.iter().map(|(name, _)| *name)).collect();
        format!("it names filters or variables the kernel does not define: {}", names.join(", "))
    })?;
    Ok(then(&filter))
}

/// Runs `code` over the JSON `input` and reads its first output: `true` holds, `false` does not,
/// and anything else (an error, another value, no output) is an error. Only the first output is
/// computed. The denylist is not applied here (the caller checks it before the evaluator starts),
/// and nothing here bounds the run: only the guard evaluator's child process calls this, and the
/// kernel kills that process at a timeout.
pub(crate) fn first(code: &str, input: &[u8]) -> Result<bool, String> {
    let input = jaq_json::read::parse_single(input).map_err(|e| format!("the input does not parse ({e})"))?;
    with_filter(code, |filter| {
        let ctx = Ctx::<Data>::new(&filter.lut, Vars::new([]));
        match filter.id.run((ctx, input)).map(unwrap_valr).next() {
            Some(Ok(Val::Bool(holds))) => Ok(holds),
            Some(Ok(_)) => Err("its first output is not true or false".to_string()),
            Some(Err(e)) => Err(format!("it failed: {e}")),
            None => Err("it gave no output".to_string()),
        }
    })?
}
