//! jq guards, through jaq. Replay never evaluates a guard: a verdict is computed once, when it is
//! written (`choose` for an edge, the end of an iteration for an `until`), and recorded in the
//! log. This module checks guards at compile time and holds the bounded evaluator writers call.
//!
//! A guard is a pure, bounded function of its input. [`check`] refuses, before anything runs, the
//! words and symbols that break that: user definitions (`def`, so no recursion), unbounded
//! generators (`range`, `repeat`, `recurse`, `while`, `until`, `combinations`), filters that read
//! something other than the input (`now`, `env`, `input`, `inputs`, `localtime`,
//! `strflocaltime`), filters with an effect (`debug`, `stderr`, `halt`, `halt_error` and their
//! natives) and `*`, which repeats a string as many times as a number in the log says. Module
//! loading is off in jaq's default loader. [`evaluate`] runs what passes on a worker thread with a
//! time limit, as a backstop.

use jaq_core::load::lex::{StrPart, Tok, Token};
use jaq_core::load::{Arena, File, Lexer, Loader};
use jaq_core::{Compiler, Ctx, Vars, data};
use jaq_json::Val;
use std::sync::mpsc;
use std::time::Duration;

type Data = data::JustLut<Val>;
type Filter = jaq_core::compile::Filter<jaq_core::Native<Data>>;

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
    "input",
    "inputs",
    "localtime",
    "strflocaltime",
    "debug",
    "debug_empty",
    "stderr",
    "stderr_empty",
    "halt",
    "halt_error",
];

/// Why a guard was not evaluated, or did not give a verdict. Each has a stable code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GuardRefusal {
    #[error("The guard is not allowed: {0}.")]
    Invalid(String),
    #[error("The guard ran past its limit of {0:?}, so it gave no verdict.")]
    Timeout(Duration),
    #[error("The guard raised an error, so it gave no verdict.")]
    Error,
    #[error("The guard's first output is not a boolean, so it gave no verdict.")]
    NotBoolean,
}

impl GuardRefusal {
    pub fn code(&self) -> &'static str {
        match self {
            GuardRefusal::Invalid(_) => "invalid_guard",
            GuardRefusal::Timeout(_) => "guard_timeout",
            GuardRefusal::Error => "guard_error",
            GuardRefusal::NotBoolean => "guard_not_boolean",
        }
    }
}

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

fn compile(code: &str) -> Result<Filter, String> {
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
    Compiler::default().with_funs(funs).compile(modules).map_err(|errs| {
        let names: Vec<&str> = errs.iter().flat_map(|(_, e)| e.iter().map(|(name, _)| *name)).collect();
        format!("it names filters or variables the kernel does not define: {}", names.join(", "))
    })
}

/// Checks a guard without running it.
pub(crate) fn check(code: &str) -> Result<(), String> {
    compile(code).map(|_| ())
}

/// Evaluates a guard over `input`, on a worker thread, within `limit`. The guard is checked first,
/// on that thread, as [`check`] does. The verdict is the first output, which must be a boolean. A guard past its limit is abandoned on its thread, which ends
/// when the guard does; [`check`]'s bans keep that to work the input itself bounds.
pub fn evaluate(code: &str, input: &serde_json::Value, limit: Duration) -> Result<bool, GuardRefusal> {
    let code = code.to_string();
    let bytes = serde_json::to_vec(input).map_err(|_| GuardRefusal::Error)?;
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("flow-guard".into())
        .spawn(move || {
            let _ = tx.send(run(&code, &bytes));
        })
        .map_err(|_| GuardRefusal::Error)?;
    match rx.recv_timeout(limit) {
        Ok(verdict) => verdict,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(GuardRefusal::Timeout(limit)),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(GuardRefusal::Error),
    }
}

fn run(code: &str, input: &[u8]) -> Result<bool, GuardRefusal> {
    let filter = compile(code).map_err(GuardRefusal::Invalid)?;
    let val = jaq_json::read::parse_single(input).map_err(|_| GuardRefusal::Error)?;
    let ctx = Ctx::<Data>::new(&filter.lut, Vars::new([]));
    match filter.id.run((ctx, val)).next() {
        Some(Ok(Val::Bool(b))) => Ok(b),
        Some(Ok(_)) | None => Err(GuardRefusal::NotBoolean),
        Some(Err(_)) => Err(GuardRefusal::Error),
    }
}
