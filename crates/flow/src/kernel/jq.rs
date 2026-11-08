//! jq guards, through jaq. A guard is a pure function of the recorded state it is given: the
//! filters that read the clock, the environment, the time zone or input streams, or that print or
//! halt, are left out of the library, so a guard that names one does not compile.

use jaq_core::load::{Arena, File, Loader};
use jaq_core::{Compiler, Ctx, Vars, data};
use jaq_json::Val;

type Data = data::JustLut<Val>;
pub(crate) type Filter = jaq_core::compile::Filter<jaq_core::Native<Data>>;

/// Filters and definitions a guard may not use: each reads something other than its input
/// (`now`, `env`, `localtime`, `strflocaltime`, `input`, `inputs`) or has an effect (`debug`,
/// `stderr`, `halt`, `halt_error`), so replay over the same log could differ or act.
const DENIED: &[&str] = &[
    "now",
    "env",
    "localtime",
    "strflocaltime",
    "input",
    "inputs",
    "debug",
    "debug_empty",
    "stderr",
    "stderr_empty",
    "halt",
    "halt_error",
];

/// Parses and compiles a guard. The error is a person-readable summary of jaq's report.
pub(crate) fn compile(code: &str) -> Result<Filter, String> {
    let defs = jaq_core::defs().chain(jaq_std::defs()).chain(jaq_json::defs()).filter(|d| !DENIED.contains(&d.name));
    let funs = jaq_core::funs::<Data>()
        .chain(jaq_std::funs::<Data>())
        .chain(jaq_json::funs::<Data>())
        .filter(|f| !DENIED.contains(&f.0));
    let arena = Arena::default();
    let modules = Loader::new(defs)
        .load(&arena, File { code, path: () })
        .map_err(|errs| format!("it does not parse ({} error(s))", errs.len()))?;
    Compiler::default().with_funs(funs).compile(modules).map_err(|errs| {
        let names: Vec<&str> = errs.iter().flat_map(|(_, e)| e.iter().map(|(name, _)| *name)).collect();
        format!("it names filters or variables the kernel does not define: {}", names.join(", "))
    })
}

/// Whether `filter` holds over `input`: its first output is exactly `true`. False, an error, no
/// output or any other value does not hold.
pub(crate) fn holds(filter: &Filter, input: &serde_json::Value) -> bool {
    let Ok(bytes) = serde_json::to_vec(input) else { return false };
    let Ok(val) = jaq_json::read::parse_single(&bytes) else { return false };
    let ctx = Ctx::<Data>::new(&filter.lut, Vars::new([]));
    matches!(filter.id.run((ctx, val)).next(), Some(Ok(Val::Bool(true))))
}
