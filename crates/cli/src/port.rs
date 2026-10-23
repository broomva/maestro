//! `maestro delegation` and `maestro economy`: the CLIs of the workspace's `scripts/delegation.py`
//! and `scripts/fleet_economy.py`, over the ported `maestro_core::{delegation, economy}`.
//!
//! Output, messages and exit codes are the Python's, so the two can be run side by side
//! (`scripts/parity-delegation.py`). This module holds the I/O the core does not: loading a
//! graph, binding its repos to directories, reading each repo's policy and checking files exist.

use clap::{Args, Subcommand};
use maestro_core::delegation::{self as d, BriefNode, Disk, Fault, Ref, Thinking, Tier};
use maestro_core::{economy as e, py};
use serde_json::{Map, Value, json};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::Path;
use std::process::ExitCode;

#[derive(Subcommand)]
pub enum DelegationCommand {
    /// Tier, verifier and needs for one unit of work.
    #[command(args_override_self = true)]
    Assess(AssessArgs),
    /// Validate an agent graph; print each node's tier and Maestro fields.
    #[command(args_override_self = true)]
    Plan {
        graph: String,
        #[command(flatten)]
        bind: BindArgs,
    },
    /// The delegation block for one node of a valid graph.
    #[command(args_override_self = true)]
    Brief {
        graph: String,
        node: String,
        #[command(flatten)]
        bind: BindArgs,
    },
    /// The tiers and the models they map to.
    Ladder,
}

#[derive(Args)]
pub struct BindArgs {
    /// A graph with no `repos`: the repo whose .control/policy.yaml names governance paths (default: cwd).
    #[arg(long)]
    root: Option<String>,
    /// Where a repo the graph names is checked out here (overrides its `repos` path).
    #[arg(long = "repo", value_name = "NAME=PATH")]
    repos: Vec<String>,
    /// Skip, and name, nodes whose repo is not checked out here; default is an error.
    #[arg(long)]
    skip_absent_repos: bool,
}

#[derive(Args)]
pub struct AssessArgs {
    /// The repo whose .control/policy.yaml names governance paths (default: cwd).
    #[arg(long)]
    root: Option<String>,
    /// Classify with the built-in governance paths only (a repo with no policy file).
    #[arg(long)]
    no_policy: bool,
    #[arg(long, default_value = "implement")]
    kind: String,
    /// argparse's `nargs="*"` stores: a repeated `--paths` replaces the earlier list.
    #[arg(long, num_args = 0.., action = clap::ArgAction::Set)]
    paths: Vec<String>,
    /// Omit when unknown; unknown counts as 0 for the LOC rules.
    #[arg(long, allow_negative_numbers = true, value_parser = py_int)]
    est_loc: Option<i64>,
    #[arg(long, allow_negative_numbers = true, value_parser = py_int)]
    files: Option<i64>,
    #[arg(long, default_value_t = 1, allow_negative_numbers = true, value_parser = py_int)]
    repos: i64,
    #[arg(long)]
    public_api: bool,
    #[arg(long)]
    one_way_door: bool,
    #[arg(long)]
    spec: Option<String>,
    #[arg(long, default_value_t = 0, allow_negative_numbers = true, value_parser = py_int)]
    failed_attempts: i64,
    #[arg(long)]
    observed_tier: Option<String>,
    /// capacity.cadence.mode from the snapshot (read_only or read-only).
    #[arg(long)]
    mode: Option<String>,
    #[arg(long)]
    allow_apex: bool,
    /// The done check; with --brief, print the delegation block instead of JSON.
    #[arg(long)]
    done: Option<String>,
    #[arg(long)]
    brief: bool,
    #[arg(long)]
    ticket: Option<String>,
}

#[derive(Subcommand)]
pub enum EconomyCommand {
    /// The snapshot's capacity section: forecast, cadence mode and launch gates.
    #[command(args_override_self = true)]
    Capacity {
        /// `provider_manager.py --json usage` output; `-` reads stdin.
        #[arg(long)]
        usage: String,
        /// The sample cache (JSON lines), already holding the current sample. Default: none.
        #[arg(long)]
        samples: Option<String>,
        /// The time to forecast from (ISO 8601). Default: now.
        #[arg(long)]
        now: Option<String>,
        /// `provider_manager.py --json state` output: whether failover covers a stale active
        /// account, and the balancer's bounds. Default: none, so a stale active account fails closed.
        #[arg(long)]
        state: Option<String>,
    },
    /// Maestro's economy by tier, and what to re-plan.
    #[command(args_override_self = true)]
    Tiers {
        #[arg(long)]
        path: Option<String>,
    },
}

/// argparse's error line for a flag value its `type` refuses (`py_int`), as the port's usage errors
/// print it; any other clap error is clap's.
pub fn usage_error(e: &clap::Error) -> Option<ExitCode> {
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    if e.kind() != ErrorKind::ValueValidation {
        return None;
    }
    let Some(ContextValue::String(arg)) = e.get(ContextKind::InvalidArg) else { return None };
    let flag = arg.split_whitespace().next()?;
    let why = std::error::Error::source(e)?.to_string();
    Some(fail(format!("error: argument {flag}: {why}"), 2))
}

/// argparse's `type=int`: Python's `int()` on the argument (`" 1_000 "` is 1000, `"１２"` is 12).
fn py_int(s: &str) -> Result<i64, String> {
    let bad = || format!("invalid int value: {}", py::repr_str(s));
    // int() strips str.isspace except U+001C..U+001F, which it refuses.
    let t = s.trim_matches(|c: char| py::is_space(c) && !('\u{1c}'..='\u{1f}').contains(&c));
    let (neg, digits) = match t.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let ascii: Option<String> = digits.chars().map(|c| if c == '_' { Some('_') } else { py::decimal(c) }).collect();
    let ascii = ascii.ok_or_else(bad)?;
    let ok = ascii.starts_with(|c: char| c.is_ascii_digit())
        && ascii.ends_with(|c: char| c.is_ascii_digit())
        && !ascii.contains("__");
    if !ok {
        return Err(bad());
    }
    let sign = if neg { "-" } else { "" };
    // int() has no bound; a value past i64 is refused, never wrapped (README).
    format!("{sign}{}", ascii.replace('_', ""))
        .parse()
        .map_err(|_| format!("an integer past 64 bits is refused: {}", py::repr_str(s)))
}

/// fleet_economy's `_fmt`: a count as `1.2M`, `3.4k`, or `%g`; `-` for none.
fn fmt_count(v: &Value) -> String {
    let Some(n) = py::num(Some(v)) else { return "-".into() };
    for (unit, div) in [("B", 1e9), ("M", 1e6), ("k", 1e3)] {
        if n.abs() >= div {
            return format!("{:.1}{unit}", n / div);
        }
    }
    py::fmt_g(n)
}

/// `x or 0` printed as the Python prints it.
fn or_zero(v: &Value) -> String {
    if py::truthy(Some(v)) { py::str_of(v) } else { "0".into() }
}

fn fail(msg: impl AsRef<str>, code: u8) -> ExitCode {
    eprintln!("{}", msg.as_ref());
    ExitCode::from(code)
}

fn cwd() -> String {
    std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| ".".into())
}

fn expanduser(p: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match p.strip_prefix('~') {
        Some("") => home,
        Some(rest) if rest.starts_with('/') => format!("{home}{rest}"),
        _ => p.to_string(),
    }
}

/// `os.path.realpath`, non-strict: symlinks resolved where they exist, the rest kept.
pub fn realpath(filename: &str) -> String {
    let mut rest: Vec<Option<String>> = filename.split('/').rev().map(|s| Some(s.to_string())).collect();
    let mut part_count = rest.len();
    let mut path = if filename.starts_with('/') { "/".to_string() } else { cwd() };
    let mut seen: HashMap<String, Option<String>> = HashMap::new();
    while part_count > 0 {
        let Some(name) = rest.pop() else { break };
        let Some(name) = name else {
            if let Some(Some(link)) = rest.pop() {
                seen.insert(link, Some(path.clone()));
            }
            continue;
        };
        part_count -= 1;
        if name.is_empty() || name == "." {
            continue;
        }
        if name == ".." {
            path = match path.rfind('/') {
                Some(0) | None => "/".into(),
                Some(i) => path[..i].to_string(),
            };
            continue;
        }
        let newpath = if path == "/" { format!("/{name}") } else { format!("{path}/{name}") };
        let Ok(meta) = std::fs::symlink_metadata(&newpath) else {
            path = newpath;
            continue;
        };
        if !meta.file_type().is_symlink() {
            path = newpath;
            continue;
        }
        if let Some(cached) = seen.get(&newpath) {
            match cached {
                Some(p) => {
                    path = p.clone();
                    continue;
                }
                None => {
                    path = newpath;
                    continue;
                }
            }
        }
        let Ok(target) = std::fs::read_link(&newpath) else {
            path = newpath;
            continue;
        };
        let target = target.to_string_lossy().into_owned();
        if target.starts_with('/') {
            path = "/".into();
        }
        seen.insert(newpath.clone(), None);
        rest.push(Some(newpath));
        rest.push(None);
        let parts: Vec<Option<String>> = target.split('/').rev().map(|s| Some(s.to_string())).collect();
        part_count += parts.len();
        rest.extend(parts);
    }
    path
}

/// The first YAML feature outside the subset this CLI reads as PyYAML's `safe_load` does: an
/// anchor (so any alias, which must follow its anchor), a tag, a merge key, a plain scalar PyYAML
/// (YAML 1.1) and serde_yaml (YAML 1.2) read differently, or a mapping key that is not a string. No
/// graph or policy uses them, and each is a place where the two build different values, so they are
/// refused, with the reason and line named, never misread (the rule `maestro_store::yaml` follows
/// for ask ledgers).
fn outside_subset(text: &str) -> Option<String> {
    use yaml_rust2::parser::{Event, Parser};
    use yaml_rust2::scanner::TScalarStyle;
    if let Some(why) = outside_lexically(text) {
        return Some(why);
    }
    let mut parser = Parser::new_from_str(text);
    // Open collections: (is a mapping, the next node is a key).
    let mut open: Vec<(bool, bool)> = Vec::new();
    loop {
        // What the check cannot scan is refused: serde_yaml (libyaml) reads documents yaml-rust2
        // refuses, and PyYAML need not read them as libyaml does.
        let (event, mark) = match parser.next_token() {
            Ok(e) => e,
            Err(e) => return Some(format!("YAML the subset check cannot scan: {e}")),
        };
        let is_key = matches!(open.last(), Some((true, true)));
        let why = match &event {
            Event::StreamEnd => return None,
            Event::Scalar(_, _, _, Some(_)) | Event::SequenceStart(_, Some(_)) | Event::MappingStart(_, Some(_)) => {
                "a tag".to_string()
            }
            Event::Scalar(_, _, anchor, _) | Event::SequenceStart(anchor, _) | Event::MappingStart(anchor, _)
                if *anchor > 0 =>
            {
                "an anchor".to_string()
            }
            Event::Scalar(v, style, _, _) => {
                let why = match style {
                    TScalarStyle::Plain => plain_scalar_outside(v, is_key),
                    _ => None,
                };
                if let Some(why) = why {
                    why
                } else {
                    if let Some(top) = open.last_mut().filter(|t| t.0) {
                        top.1 = !top.1;
                    }
                    continue;
                }
            }
            Event::MappingStart(..) | Event::SequenceStart(..) => {
                if is_key {
                    "a mapping key that is not a string".to_string()
                } else {
                    open.push((matches!(event, Event::MappingStart(..)), true));
                    continue;
                }
            }
            Event::MappingEnd | Event::SequenceEnd => {
                open.pop();
                if let Some(top) = open.last_mut().filter(|t| t.0) {
                    top.1 = !top.1;
                }
                continue;
            }
            _ => continue,
        };
        return Some(format!("{why} (line {})", mark.line()));
    }
}

/// The first character the two readers scan differently: a tab, a carriage return outside a CRLF
/// line ending, a control character, a line or paragraph separator (U+0085, U+2028, U+2029), and a
/// byte-order mark past the start.
fn outside_lexically(text: &str) -> Option<String> {
    let mut line = 1;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let why = match c {
            '\n' => {
                line += 1;
                continue;
            }
            '\r' if chars.peek().map(|p| p.1) == Some('\n') => continue,
            '\t' => "a tab",
            '\r' => "a carriage return outside a CRLF line ending",
            '\u{85}' | '\u{2028}' | '\u{2029}' => "a Unicode line or paragraph separator",
            '\u{feff}' if i > 0 => "a byte-order mark past the start",
            c if c.is_control() => "a control character",
            _ => continue,
        };
        return Some(format!("{why} (line {line})"));
    }
    None
}

/// What PyYAML's `safe_load` resolves a plain scalar to (its implicit resolvers, YAML 1.1).
#[derive(Debug, PartialEq, Eq)]
enum Yaml11 {
    Str,
    Null,
    Bool,
    Int,
    Float,
    Timestamp,
    Merge,
    Value,
}

/// PyYAML's resolver regexes, by hand (Python `re.match` with `$`).
fn yaml11(v: &str) -> Yaml11 {
    let b = v.as_bytes();
    let sign = |i: usize| if matches!(b.get(i), Some(b'+' | b'-')) { i + 1 } else { i };
    // The end of a run of bytes in `set` from `i`.
    let run = |mut i: usize, set: &dyn Fn(u8) -> bool| {
        while i < b.len() && set(b[i]) {
            i += 1;
        }
        i
    };
    let digit = |c: u8| c.is_ascii_digit();
    let digit_ = |c: u8| c.is_ascii_digit() || c == b'_';
    // `[eE][-+][0-9]+` or nothing, to the end.
    let exp_end = |i: usize| {
        i == b.len()
            || (matches!(b[i], b'e' | b'E')
                && matches!(b.get(i + 1), Some(b'+' | b'-'))
                && run(i + 2, &digit) == b.len()
                && i + 2 < b.len())
    };
    // `(:[0-5]?[0-9])+` from `i`: its end, if at least one group.
    let sexagesimal = |mut i: usize| {
        let mut groups = 0;
        while b.get(i) == Some(&b':') {
            let j = i + 1;
            if b.get(j).is_some_and(|c| (b'0'..=b'5').contains(c)) && b.get(j + 1).is_some_and(|c| c.is_ascii_digit()) {
                i = j + 2;
            } else if b.get(j).is_some_and(|c| c.is_ascii_digit()) {
                i = j + 1;
            } else {
                break;
            }
            groups += 1;
        }
        (groups > 0).then_some(i)
    };
    let is_bool = matches!(
        v,
        "yes"
            | "Yes"
            | "YES"
            | "no"
            | "No"
            | "NO"
            | "true"
            | "True"
            | "TRUE"
            | "false"
            | "False"
            | "FALSE"
            | "on"
            | "On"
            | "ON"
            | "off"
            | "Off"
            | "OFF"
    );
    if is_bool {
        return Yaml11::Bool;
    }
    if matches!(v, "~" | "null" | "Null" | "NULL" | "") {
        return Yaml11::Null;
    }
    if v == "<<" {
        return Yaml11::Merge;
    }
    if v == "=" {
        return Yaml11::Value;
    }
    let s = sign(0);
    let int = {
        let bin =
            b[s..].starts_with(b"0b") && s + 2 < b.len() && run(s + 2, &|c| matches!(c, b'0' | b'1' | b'_')) == b.len();
        let oct = b.get(s) == Some(&b'0')
            && s + 1 < b.len()
            && run(s + 1, &|c| (b'0'..=b'7').contains(&c) || c == b'_') == b.len();
        let dec = (b.get(s) == Some(&b'0') && s + 1 == b.len())
            || (b.get(s).is_some_and(|c| (b'1'..=b'9').contains(c)) && run(s + 1, &digit_) == b.len());
        let hex = b[s..].starts_with(b"0x")
            && s + 2 < b.len()
            && run(s + 2, &|c| c.is_ascii_hexdigit() || c == b'_') == b.len();
        let sexa =
            b.get(s).is_some_and(|c| (b'1'..=b'9').contains(c)) && sexagesimal(run(s + 1, &digit_)) == Some(b.len());
        bin || oct || dec || hex || sexa
    };
    if int {
        return Yaml11::Int;
    }
    let float = {
        let lead = b.get(s).is_some_and(|c| c.is_ascii_digit());
        let int_end = run(s, &digit_);
        let plain = lead && b.get(int_end) == Some(&b'.') && exp_end(run(int_end + 1, &digit_));
        let dot = b.first() == Some(&b'.') && b.len() > 1 && digit(b[1]) && exp_end(run(1, &digit_));
        let sexa =
            lead && sexagesimal(int_end).is_some_and(|i| b.get(i) == Some(&b'.') && run(i + 1, &digit_) == b.len());
        let inf = matches!(&v[s..], ".inf" | ".Inf" | ".INF");
        let nan = matches!(v, ".nan" | ".NaN" | ".NAN");
        plain || dot || sexa || inf || nan
    };
    if float {
        return Yaml11::Float;
    }
    let n = |i: usize, k: usize| b.len() >= i + k && b[i..i + k].iter().all(u8::is_ascii_digit);
    let date = b.len() == 10 && n(0, 4) && b[4] == b'-' && n(5, 2) && b[7] == b'-' && n(8, 2);
    let stamp = (|| {
        if !(n(0, 4) && b.get(4) == Some(&b'-')) {
            return false;
        }
        let mut i = 5;
        let one_or_two = |i: usize| {
            if n(i, 2) {
                Some(i + 2)
            } else if n(i, 1) {
                Some(i + 1)
            } else {
                None
            }
        };
        let Some(j) = one_or_two(i) else { return false };
        if b.get(j) != Some(&b'-') {
            return false;
        }
        let Some(j) = one_or_two(j + 1) else { return false };
        i = if matches!(b.get(j), Some(b'T' | b't')) {
            j + 1
        } else {
            let k = run(j, &|c| c == b' ' || c == b'\t');
            if k == j {
                return false;
            }
            k
        };
        let Some(j) = one_or_two(i) else { return false };
        if !(b.get(j) == Some(&b':') && n(j + 1, 2) && b.get(j + 3) == Some(&b':') && n(j + 4, 2)) {
            return false;
        }
        let mut i = j + 6;
        if b.get(i) == Some(&b'.') {
            i = run(i + 1, &digit);
        }
        if i == b.len() {
            return true;
        }
        let k = run(i, &|c| c == b' ' || c == b'\t');
        match b.get(k) {
            Some(b'Z') => k + 1 == b.len(),
            Some(b'+' | b'-') => {
                let Some(j) = one_or_two(k + 1) else { return false };
                j == b.len() || (b.get(j) == Some(&b':') && n(j + 1, 2) && j + 3 == b.len())
            }
            _ => false,
        }
    })();
    if date || stamp {
        return Yaml11::Timestamp;
    }
    Yaml11::Str
}

/// Why a plain scalar is outside the subset: PyYAML and serde_yaml would build different values
/// from it, or it is a mapping key that is not a string. Inside: strings both read as the same
/// string, the null words, `true`/`false` in their three spellings, decimal integers within 64 bits
/// and `[-+]digits.digits` floats (with a signed exponent), which both read alike.
fn plain_scalar_outside(v: &str, is_key: bool) -> Option<String> {
    let shown = py::repr_str(v);
    // PyYAML (1.1) ends a plain scalar at a `?` in a flow collection, serde_yaml (1.2) keeps it.
    if v.contains('?') {
        return Some(format!("a plain scalar {shown} holding `?` (quote it)"));
    }
    let both = serde_yaml::from_str::<serde_yaml::Value>(v).ok();
    let differ = || Some(format!("a plain scalar {shown} that YAML 1.1 and 1.2 read differently (quote it)"));
    let py = yaml11(v);
    if py == Yaml11::Merge {
        return Some("a merge key `<<`".into());
    }
    if is_key && py != Yaml11::Str {
        return Some(format!("a mapping key {shown} that is not a string (quote it)"));
    }
    let b = v.as_bytes();
    let s = usize::from(matches!(b.first(), Some(b'+' | b'-')));
    let digits = |i: usize, j: usize| i < j && b[i..j].iter().all(u8::is_ascii_digit);
    match py {
        Yaml11::Str => match both {
            Some(serde_yaml::Value::String(x)) if x == v => None,
            _ => differ(),
        },
        Yaml11::Null => None,
        Yaml11::Bool if matches!(v, "true" | "True" | "TRUE" | "false" | "False" | "FALSE") => None,
        Yaml11::Int => {
            let same = v
                .parse::<i64>()
                .ok()
                .zip(both.as_ref().and_then(serde_yaml::Value::as_i64))
                .is_some_and(|(a, b)| a == b);
            // A leading zero is octal to PyYAML; serde_yaml reads it as a string, so `same` fails.
            if same { None } else { differ() }
        }
        Yaml11::Float => {
            let dot = b.iter().position(|c| *c == b'.').unwrap_or(b.len());
            let exp = b.iter().position(|c| matches!(c, b'e' | b'E')).unwrap_or(b.len());
            let plain = digits(s, dot) && digits(dot + 1, exp) && (exp == b.len() || digits(exp + 2, b.len()));
            let same = v
                .parse::<f64>()
                .ok()
                .zip(both.as_ref().and_then(serde_yaml::Value::as_f64))
                .is_some_and(|(a, b)| a == b);
            if plain && same { None } else { differ() }
        }
        _ => differ(),
    }
}

/// YAML within the subset (see [`outside_subset`]); anything outside it is `Fault::Value` naming why.
fn load_yaml(text: &str) -> Result<serde_yaml::Value, Fault> {
    let outside = |why: String| Fault::Value(format!("{}: {why}", maestro_store::yaml::OUTSIDE));
    if let Some(why) = outside_subset(text) {
        return Err(outside(why));
    }
    let y: serde_yaml::Value =
        serde_yaml::from_str(text).map_err(|e| Fault::Crash { kind: "YAMLError", msg: e.to_string() })?;
    Ok(y)
}

/// The keys of a loaded mapping in file order (a JSON value sorts its keys; the Python's dict keeps
/// file order, and its messages and nested-repo checks follow it).
fn keys_in_order(y: &serde_yaml::Value) -> Vec<String> {
    match y {
        serde_yaml::Value::Mapping(m) => {
            m.keys().map(|k| serde_json::to_value(k).map(|k| py::str_of(&k)).unwrap_or_default()).collect()
        }
        _ => vec![],
    }
}

/// The keys of a JSON graph's `repos` object in file order, a repeated key at its first position
/// (as `json.loads` builds the dict). A repeated `repos` key is the last one, as `json.loads` keeps it.
fn json_repos_order(text: &str) -> Vec<String> {
    use serde::de::{Deserializer, IgnoredAny, MapAccess, Visitor};
    struct Keys(Vec<String>);
    impl<'de> serde::Deserialize<'de> for Keys {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Keys;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("a mapping")
                }
                fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Keys, A::Error> {
                    let mut keys = Vec::new();
                    while let Some((k, IgnoredAny)) = m.next_entry::<String, IgnoredAny>()? {
                        if !keys.contains(&k) {
                            keys.push(k);
                        }
                    }
                    Ok(Keys(keys))
                }
            }
            d.deserialize_map(V)
        }
    }
    struct Graph(Option<Keys>);
    impl<'de> serde::Deserialize<'de> for Graph {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Graph;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("a mapping")
                }
                fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Graph, A::Error> {
                    let mut repos = None;
                    while let Some(k) = m.next_key::<String>()? {
                        if k == "repos" {
                            repos = m.next_value::<Option<Keys>>()?;
                        } else {
                            m.next_value::<IgnoredAny>()?;
                        }
                    }
                    Ok(Graph(repos))
                }
            }
            d.deserialize_map(V)
        }
    }
    serde_json::from_str::<Graph>(text).ok().and_then(|g| g.0).map(|k| k.0).unwrap_or_default()
}

/// The exception `open(path).read()` raises for `e`.
fn os_error_kind(e: &std::io::Error) -> &'static str {
    match e.kind() {
        std::io::ErrorKind::NotFound => "FileNotFoundError",
        std::io::ErrorKind::IsADirectory => "IsADirectoryError",
        std::io::ErrorKind::PermissionDenied => "PermissionError",
        std::io::ErrorKind::InvalidData => "UnicodeDecodeError",
        _ => "OSError",
    }
}

/// The graph (`json.loads` for `.json`, else `yaml.safe_load(...) or {}`), and its `repos` names in
/// file order. A graph that is not a mapping is the Python's AttributeError at its first `.get`.
fn load_graph(path: &str) -> Result<(Value, Vec<String>), Fault> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Fault::Crash { kind: os_error_kind(&e), msg: format!("{path}: {e}") })?;
    let (v, order): (Value, Vec<String>) = if path.ends_with(".json") {
        let v = maestro_core::schema::parse_json(&text)
            .map_err(|e| Fault::Crash { kind: "JSONDecodeError", msg: e.to_string() })?;
        (v, json_repos_order(&text))
    } else {
        let y = load_yaml(&text)?;
        let v = serde_json::to_value(&y).map_err(|e| Fault::Crash { kind: "YAMLError", msg: e.to_string() })?;
        let order = y.get("repos").map(keys_in_order).unwrap_or_default();
        (if py::truthy(Some(&v)) { v } else { json!({}) }, order)
    };
    if !v.is_object() {
        return Err(Fault::Crash {
            kind: "AttributeError",
            msg: format!("'{}' object has no attribute 'get'", d::type_name(&v)),
        });
    }
    maestro_core::schema::graph(&v)?;
    Ok((v, order))
}

/// A repo's `.control/policy.yaml` require_human paths. An unreadable policy is `Fault::Value`
/// with the Python's message shape.
fn policy_of(root: &str) -> Result<Vec<String>, Fault> {
    let path = py::join(&py::join(root, ".control"), "policy.yaml");
    let text = std::fs::read_to_string(&path).map_err(|e| {
        Fault::Value(match e.kind() {
            std::io::ErrorKind::NotFound => {
                format!("{path}: [Errno 2] No such file or directory: {}", py::repr_str(&path))
            }
            _ => format!("{path}: {e}"),
        })
    })?;
    // Every failure to load is the Python's PolicyUnreadable (its `except Exception`).
    let data = load_yaml(&text)
        .and_then(|y| serde_json::to_value(y).map_err(|e| Fault::Value(e.to_string())))
        .map_err(|f| Fault::Value(format!("{path}: {f}")))?;
    d::require_human_paths(&data).map_err(|f| Fault::Value(format!("{path}: {f}")))
}

struct Repo {
    dir: String,
    policy: Value,
}

/// name -> {dir, policy} for every repo a graph names, in the graph's order.
/// `Fault::Value` is the Python's `ValueError` (exit 2); `Fault::Crash` its traceback (exit 1).
fn bind_repos(
    graph: &Value,
    order: &[String],
    root: &str,
    bindings: &[(String, String)],
) -> Result<Vec<(String, Repo)>, Fault> {
    let named = py::truthy(graph.get("repos"));
    let home = py::or(graph.get("repo"), None).map_or_else(String::new, py::str_of);
    // `(graph.get("repos") or {}).items()`: the schema holds repos to a mapping or null.
    let declared: Vec<(String, Value)> = match graph.get("repos").and_then(Value::as_object).filter(|o| !o.is_empty()) {
        Some(o) if order.len() == o.len() && order.iter().all(|k| o.contains_key(k)) => {
            order.iter().map(|k| (k.clone(), o[k].clone())).collect()
        }
        Some(_) => return Err(Fault::Value("the order of the graph's repos is unreadable".into())),
        None => vec![(home, json!(root))],
    };
    let mut out: Vec<(String, Repo)> = Vec::new();
    for (name, v) in declared {
        if named && (name.is_empty() || name.contains(':')) {
            return Err(Fault::Value(format!("repo name {}: a name is non-empty and has no ':'", py::repr_str(&name))));
        }
        let (mut path, policy) = match &v {
            Value::Object(o) => (o.get("path").cloned(), o.get("policy").cloned().unwrap_or(Value::Null)),
            other => (Some(other.clone()), Value::Null),
        };
        if let Some((_, p)) = bindings.iter().rev().find(|(n, _)| *n == name) {
            path = Some(json!(p));
        }
        let Some(path) = path.filter(|p| py::truthy(Some(p))) else {
            return Err(Fault::Value(format!("repo {} has no path", py::repr_str(&name))));
        };
        out.push((name, Repo { dir: realpath(&expanduser(&py::str_of(&path))), policy }));
    }
    let mut unknown: Vec<&str> =
        bindings.iter().map(|(n, _)| n.as_str()).filter(|n| !out.iter().any(|(o, _)| o == n)).collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        unknown.dedup();
        return Err(Fault::Value(format!("--repo names {}, which the graph does not", unknown.join(", "))));
    }
    let mut seen: HashMap<&str, &str> = HashMap::new();
    for (name, r) in &out {
        if let Some(first) = seen.get(r.dir.as_str()) {
            return Err(Fault::Value(format!(
                "repos {first} and {name} are one directory ({}): a place has one name",
                r.dir
            )));
        }
        seen.insert(&r.dir, name);
    }
    Ok(out)
}

/// The Python's traceback exit for a fault it does not catch.
fn crash(f: &Fault) -> ExitCode {
    fail(format!("Traceback (most recent call last):\n  {f}"), 1)
}

fn plan_or_brief(graph_path: &str, node: Option<&str>, bind: &BindArgs, json_out: bool) -> ExitCode {
    let (graph, order) = match load_graph(graph_path) {
        Ok(g) => g,
        Err(Fault::Value(why)) => return fail(format!("  [error] {graph_path}: {why}"), 1),
        Err(f @ Fault::Refused { .. }) => return fail(format!("  [error] {graph_path}: {f}"), 1),
        Err(f) => return crash(&f),
    };
    if py::truthy(graph.get("repos")) && bind.root.is_some() {
        return fail("this graph names its repos: bind them with --repo NAME=PATH, not --root", 2);
    }
    let root = bind.root.clone().filter(|r| !r.is_empty()).unwrap_or_else(cwd); // `o.root or os.getcwd()`
    if let Some(bad) = bind.repos.iter().find(|x| x.split_once('=').is_none_or(|(n, p)| n.is_empty() || p.is_empty())) {
        return fail(format!("  [error] --repo takes NAME=PATH, not {}", py::repr_str(bad)), 2);
    }
    let bindings: Vec<(String, String)> =
        bind.repos.iter().filter_map(|x| x.split_once('=').map(|(n, p)| (n.to_string(), p.to_string()))).collect();
    let repos = match bind_repos(&graph, &order, &root, &bindings) {
        Ok(r) => r,
        Err(Fault::Value(e)) => return fail(format!("  [error] {e}"), 2),
        Err(f) => return crash(&f),
    };
    let absent: HashSet<String> =
        repos.iter().filter(|(_, r)| !Path::new(&r.dir).is_dir()).map(|(n, _)| n.clone()).collect();
    if !absent.is_empty() && !bind.skip_absent_repos {
        let mut names: Vec<&String> = absent.iter().collect();
        names.sort();
        for name in names {
            let dir = &repos.iter().find(|(n, _)| n == name).expect("bound").1.dir;
            eprintln!(
                "  [error] repo {name} ({dir}) is not here: bind it with --repo {name}=PATH, or pass --skip-absent-repos"
            );
        }
        return ExitCode::from(1);
    }
    // `{n.get("id") for n in nodes if n.get("id") and (n.get("repo") or home) in absent}`; the
    // schema holds ids and repos to strings.
    let text = d::text;
    let home = text(graph.get("repo")).unwrap_or("");
    let mut skipped: Vec<String> = Vec::new();
    for n in graph.get("nodes").and_then(Value::as_array).map_or(&[][..], Vec::as_slice) {
        let Some(id) = text(n.get("id")) else { continue };
        if absent.contains(text(n.get("repo")).unwrap_or(home)) {
            skipped.push(id.to_string());
        }
    }
    skipped.sort();
    skipped.dedup();
    for nid in &skipped {
        eprintln!("  [skip] {nid}: its repo is not checked out here; structure checked only (--skip-absent-repos)");
    }
    let dirs: Vec<(String, String)> = repos.iter().map(|(n, r)| (n.clone(), r.dir.clone())).collect();
    let policies: RefCell<HashMap<String, Vec<String>>> = RefCell::new(HashMap::new());
    let policy_for = |name: &str| -> Result<Vec<String>, Fault> {
        if let Some(p) = policies.borrow().get(name) {
            return Ok(p.clone());
        }
        let Some((_, r)) = repos.iter().find(|(n, _)| n == name) else {
            return Err(Fault::Value(format!("repo {} is not declared", py::repr_str(name))));
        };
        let file = py::join(&py::join(&r.dir, ".control"), "policy.yaml");
        let has_file = Path::new(&file).is_file();
        let got = if r.policy == json!("none") {
            if has_file {
                return Err(Fault::Value(format!(
                    "repo {name} declares policy: none, but {} has .control/policy.yaml",
                    r.dir
                )));
            }
            vec![]
        } else {
            policy_of(&r.dir).map_err(|f| match f {
                Fault::Value(e) => {
                    let hint = if has_file {
                        String::new()
                    } else {
                        format!(" (a repo with no policy file declares `policy: none` in `repos.{name}`)")
                    };
                    Fault::Value(format!("{e}{hint}"))
                }
                crash => crash,
            })?
        };
        policies.borrow_mut().insert(name.to_string(), got.clone());
        Ok(got)
    };
    let exists = |r: &Ref| -> bool {
        if absent.contains(&r.repo) {
            return true; // unknown here: not checked out, so never reported missing
        }
        dirs.iter().find(|(n, _)| *n == r.repo).is_some_and(|(_, dir)| Path::new(&py::join(dir, &r.path)).is_file())
    };
    let disk = Disk { exists: &exists, policy_for: &policy_for };
    let skip: HashSet<String> = skipped.into_iter().collect();
    let (errors, nodes) = match d::validate(&graph, &[], Some(&disk), Some(&dirs), &skip) {
        Ok(r) => r,
        Err(f) => return crash(&f),
    };
    if !errors.is_empty() {
        for err in errors {
            eprintln!("  [error] {err}");
        }
        return ExitCode::from(1);
    }
    if let Some(want) = node {
        // `n["id"] == o.node`: only a string id names a node from the command line.
        let Some(n) = nodes.iter().find(|n| n.id.as_str() == Some(want)) else {
            return fail(format!("no node {}", py::repr_str(want)), 2);
        };
        let ticket = graph.get("ticket").filter(|t| py::truthy(Some(t))).map(py::str_of);
        let spawn = n.spawn_default.as_str().and_then(Tier::parse).unwrap_or(n.tier);
        let brief = BriefNode {
            tier: n.tier,
            spawn_default: spawn,
            thinking: n.thinking.as_str().unwrap_or(""),
            verify: n.verify.as_str().unwrap_or(""),
            done: &py::str_of(&n.done),
            inputs: n.inputs.iter().cloned().zip(n.input_refs.iter().cloned().map(Some)).collect(),
            repo: Some(&n.repo),
            repo_dirs: &dirs,
        };
        println!("{}", d::brief_block(&brief, ticket.as_deref()));
        return ExitCode::SUCCESS;
    }
    if json_out {
        let all: Vec<Value> = nodes.iter().map(d::PlanNode::to_json).collect();
        println!("{}", serde_json::to_string_pretty(&all).expect("JSON"));
        return ExitCode::SUCCESS;
    }
    for n in &nodes {
        let thinking = n.thinking.as_str().and_then(Thinking::parse).unwrap_or(n.tier.rung().thinking);
        // `','.join(depends_on)` takes strings only.
        if let Some(bad) = n.depends_on.iter().find(|d| !d.is_string()) {
            return crash(&Fault::Crash {
                kind: "TypeError",
                msg: format!("sequence item 0: expected str instance, {} found", d::type_name(bad)),
            });
        }
        let after: Vec<String> = n.depends_on.iter().map(py::str_of).collect();
        let after = after.join(",");
        // A number right-aligns in a format width, a string left-aligns.
        let id = match &n.id {
            Value::String(s) => format!("{s:16}"),
            other => format!("{:>16}", py::str_of(other)),
        };
        println!(
            "{id} {:12} {:10} {:28} thinking={:6} verify={:8} after={}",
            n.kind,
            n.tier.as_str(),
            n.tier.rung().provider,
            thinking.as_str(),
            py::str_of(&n.verify),
            if after.is_empty() { "-" } else { &after }
        );
    }
    ExitCode::SUCCESS
}

/// argparse's error line for a value outside `choices` (its usage block is not reproduced).
fn invalid_choice(flag: &str, v: &str, choices: &[&str]) -> ExitCode {
    fail(format!("error: argument {flag}: invalid choice: {} (choose from {})", py::repr_str(v), choices.join(", ")), 2)
}

fn assess(a: &AssessArgs) -> ExitCode {
    let mut kinds: Vec<&str> =
        d::MECHANICAL_KINDS.iter().chain(&d::WORK_KINDS).chain(&d::FRONTIER_KINDS).copied().collect();
    kinds.sort_unstable();
    if !kinds.contains(&a.kind.as_str()) {
        return invalid_choice("--kind", &a.kind, &kinds);
    }
    if let Some(t) = &a.observed_tier
        && Tier::parse(t).is_none()
    {
        return invalid_choice("--observed-tier", t, &Tier::ALL.map(Tier::as_str));
    }
    let mode = a.mode.as_deref().map(d::norm_mode);
    if let Some(m) = &mode
        && m != "normal"
        && !d::SAVING_MODES.contains(&m.as_str())
    {
        return invalid_choice("--mode", m, &["normal", "conserve", "critical", "read_only"]);
    }
    let root = a.root.clone().filter(|r| !r.is_empty()).unwrap_or_else(cwd); // `o.root or os.getcwd()`
    let human = if a.no_policy {
        vec![]
    } else {
        match policy_of(&root) {
            Ok(h) => h,
            Err(crash @ Fault::Crash { .. }) => return self::crash(&crash),
            Err(e) => {
                return fail(
                    format!("policy unreadable, refusing to classify (pass --no-policy for a repo without one): {e}"),
                    3,
                );
            }
        }
    };
    let spec = a.spec.as_deref().filter(|s| !s.is_empty()).map(py::normpath);
    if let Some(s) = &spec
        && !(d::is_spec_path(s) && Path::new(&py::join(&root, s)).is_file())
    {
        return fail(
            format!(
                "--spec {} must be an existing file under {} of {root}",
                a.spec.as_deref().unwrap_or(""),
                d::SPEC_DIRS.join(", ")
            ),
            2,
        );
    }
    let sig = json!({
        "kind": a.kind, "paths": a.paths, "est_loc": a.est_loc, "files": a.files, "repos": a.repos,
        "public_api": a.public_api, "one_way_door": a.one_way_door, "spec": spec,
        "failed_attempts": a.failed_attempts, "observed_tier": a.observed_tier, "mode": mode,
        "allow_apex": a.allow_apex,
    });
    let r = match d::assess(sig.as_object().expect("an object"), &human) {
        Ok(r) => r,
        Err(f) => return fail(format!("{f}"), 2),
    };
    if a.brief {
        let done = a.done.as_deref().unwrap_or("");
        if py::strip(done).is_empty() {
            return fail("--brief needs --done: a brief without a done check is not delegable", 2);
        }
        if !r.needs.is_empty() {
            return fail(
                format!("not delegable as one node: needs {} first ({})", r.needs.join(", "), r.reasons.join("; ")),
                1,
            );
        }
        let brief = BriefNode {
            tier: r.executor,
            spawn_default: r.executor,
            thinking: r.thinking.as_str(),
            verify: r.verify,
            done,
            inputs: spec.iter().map(|s| (s.clone(), None)).collect(),
            repo: None,
            repo_dirs: &[],
        };
        println!("{}", d::brief_block(&brief, a.ticket.as_deref().filter(|t| !t.is_empty())));
        return ExitCode::SUCCESS;
    }
    let mut out = serde_json::to_value(&r).expect("JSON");
    out["contract"] = d::contract(r.executor, Some(r.thinking));
    if let Some(author) = r.author {
        out["author_contract"] = d::contract(author, None);
    }
    println!("{}", serde_json::to_string_pretty(&out).expect("JSON"));
    ExitCode::SUCCESS
}

pub fn ladder() -> Value {
    let m: Map<String, Value> = d::LADDER
        .iter()
        .map(|(t, r)| {
            (t.as_str().to_string(), json!({"provider": r.provider, "thinking": r.thinking, "agent": r.agent}))
        })
        .collect();
    Value::Object(m)
}

pub fn delegation(cmd: &DelegationCommand, json_out: bool) -> ExitCode {
    match cmd {
        DelegationCommand::Ladder => {
            println!("{}", serde_json::to_string_pretty(&ladder()).expect("JSON"));
            ExitCode::SUCCESS
        }
        DelegationCommand::Assess(a) => assess(a),
        DelegationCommand::Plan { graph, bind } => plan_or_brief(graph, None, bind, json_out),
        DelegationCommand::Brief { graph, node, bind } => plan_or_brief(graph, Some(node), bind, json_out),
    }
}

fn read_input(path: &str) -> Result<String, String> {
    if path == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).map_err(|e| e.to_string())?;
        Ok(s)
    } else {
        std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
    }
}

/// A fault as the one line `maestro economy` prints before exiting 1.
fn fault_text(f: Fault) -> String {
    match f {
        Fault::Value(m) => m,
        Fault::Crash { kind, msg } => format!("{kind}: {msg}"),
        refused @ Fault::Refused { .. } => refused.to_string(),
    }
}

pub fn economy(cmd: &EconomyCommand, json_out: bool) -> ExitCode {
    match cmd {
        EconomyCommand::Capacity { usage, samples, now, state } => {
            let run = || -> Result<Value, String> {
                let rows = maestro_core::schema::parse_json(&read_input(usage)?)?;
                // `for a in rows or []`: nothing for a falsy value; a mapping or a string would be
                // iterated and each item fail `.get`, a number is not iterable. Refused.
                let rows = match rows {
                    Value::Array(a) => a,
                    v if !py::truthy(Some(&v)) => vec![],
                    v => return Err(format!("--usage is a {}, not a list of rows", d::type_name(&v))),
                };
                let samples: Vec<Value> = match samples {
                    // A line that is not JSON is skipped, as the tick skips it; one outside the schema is refused.
                    Some(p) => read_input(p)?
                        .lines()
                        .filter_map(|l| match maestro_core::schema::parse_json(l) {
                            Ok(v) => Some(Ok(v)),
                            Err(e) if e.ends_with(&format!("({})", maestro_core::schema::OUTSIDE)) => Some(Err(e)),
                            Err(_) => None,
                        })
                        .collect::<Result<_, _>>()?,
                    None => vec![],
                };
                let now = match now {
                    Some(n) => {
                        maestro_core::schema::time("--now", Some(&json!(n))).map_err(fault_text)?;
                        e::parse_ts(Some(&json!(n))).map_err(fault_text)?.ok_or("--now is empty")?
                    }
                    None => jiff::Timestamp::now().as_microsecond(),
                };
                let gate = match state {
                    None => e::Gate::default(),
                    Some(p) => {
                        let st = maestro_core::schema::parse_json(&read_input(p)?)?;
                        let st = st.as_object().ok_or("--state is not a JSON object")?;
                        e::gate_bounds(st, now, &rows).map_err(fault_text)?
                    }
                };
                e::capacity(&rows, &samples, now, &gate).map_err(fault_text)
            };
            match run() {
                Ok(v) => {
                    println!("{}", serde_json::to_string_pretty(&v).expect("JSON"));
                    ExitCode::SUCCESS
                }
                Err(err) => fail(format!("maestro: {err}"), 1),
            }
        }
        EconomyCommand::Tiers { path } => {
            let path = path.clone().unwrap_or_else(|| expanduser("~/.config/maestro-paseo/economy.json"));
            let data: Value = match std::fs::read_to_string(&path)
                .map_err(|e| match e.kind() {
                    std::io::ErrorKind::NotFound => {
                        format!("[Errno 2] No such file or directory: {}", py::repr_str(&path))
                    }
                    _ => e.to_string(),
                })
                .and_then(|t| maestro_core::schema::parse_json(&t))
            {
                Ok(d) => d,
                Err(err) => return fail(format!("no economy by tier yet ({err})"), 1),
            };
            let (rows, signals) = match e::tier_signals(&data) {
                Ok(r) => r,
                Err(f @ Fault::Refused { .. }) => return fail(format!("maestro: {f}"), 1),
                Err(f) => return crash(&f),
            };
            if json_out {
                let out = json!({"generatedAt": data.get("generatedAt"), "rows": rows, "replan": signals});
                println!("{}", serde_json::to_string_pretty(&out).expect("JSON"));
                return ExitCode::SUCCESS;
            }
            println!(
                "{:10} {:>5} {:>8} {:>12} {:>10} {:>6} {:>5}",
                "tier", "runs", "approved", "tok/approved", "first-pass", "raised", "escal"
            );
            for r in &rows {
                let fp = py::num(Some(&r["first_pass"])).map_or_else(|| "-".into(), |f| format!("{:.0}%", f * 100.0));
                println!(
                    "{:10} {:>5} {:>8} {:>12} {:>10} {:>6} {:>5}",
                    py::str_of(&r["tier"]),
                    py::str_of(&r["runs"]),
                    or_zero(&r["approved"]),
                    fmt_count(&r["tokens_per_approved"]),
                    fp,
                    or_zero(&r["raised_runs"]),
                    or_zero(&r["escalations"])
                );
            }
            for s in signals {
                println!("re-plan: {s}");
            }
            ExitCode::SUCCESS
        }
    }
}

/// One parity case: a ported function on JSON arguments. Used by `scripts/parity-delegation.py`
/// and by `crates/core/tests/parity_delegation.rs` (which evaluates the same cases in-process).
pub fn eval_stdin() -> ExitCode {
    let mut s = String::new();
    if let Err(err) = std::io::stdin().read_to_string(&mut s) {
        return fail(err.to_string(), 2);
    }
    let cases: Vec<Value> =
        match maestro_core::schema::parse_json(&s).and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string())) {
            Ok(c) => c,
            Err(err) => return fail(&err, 2),
        };
    let out: Vec<Value> = cases
        .iter()
        .map(|c| match c.get("fn").and_then(Value::as_str) {
            Some("load_yaml") => yaml_case(c["args"]["text"].as_str().unwrap_or("")),
            _ => maestro_core::parity::eval(c),
        })
        .collect();
    println!("{}", serde_json::to_string(&out).expect("JSON"));
    ExitCode::SUCCESS
}

/// The CLI's YAML reader on one document: its value, or the refusal or crash.
fn yaml_case(text: &str) -> Value {
    match load_yaml(text)
        .and_then(|y| serde_json::to_value(&y).map_err(|e| Fault::Crash { kind: "YAMLError", msg: e.to_string() }))
    {
        Ok(v) => json!({"ok": v}),
        Err(Fault::Value(m)) => json!({"refused": m}),
        Err(Fault::Crash { kind, msg }) => json!({"crash": kind, "msg": msg}),
        Err(f) => json!({"refused": f.to_string()}),
    }
}

#[cfg(test)]
mod tests {
    /// `int()` under Python 3.14: Unicode digits and str.isspace padding read, except U+001C..U+001F.
    #[test]
    fn py_int_is_pythons_int() {
        assert_eq!(super::py_int(" 1_000\u{3000}"), Ok(1000));
        assert_eq!(super::py_int("-\u{ff13}\u{ff10}"), Ok(-30));
        assert_eq!(super::py_int("+\u{663}"), Ok(3));
        for bad in ["\u{1c}9", "9\u{1f}", "1__0", "_1", "1_", "\u{b2}", "", "-", "0x1"] {
            assert!(super::py_int(bad).is_err(), "{bad:?}");
        }
        assert!(super::py_int("9223372036854775808").unwrap_err().contains("past 64 bits"));
    }

    /// One witness per subset guard, each a document PyYAML reads differently from serde_yaml or
    /// raises on (checked with PyYAML 6 under Python 3.14): the fuzz golden measures misreads only, so
    /// a guard whose witness it lacks is held here.
    #[test]
    fn each_subset_guard_refuses_its_witness() {
        for (doc, why) in [
            ("a: b\rc: d\n", "a carriage return"), // PyYAML: {a: b, c: d}
            ("a: b\u{2028}c: d\n", "a Unicode line or paragraph separator"), // PyYAML: {a: b, c: d}
            ("a: b\u{85}c: d\n", "a Unicode line or paragraph separator"), // PyYAML: {a: b, c: d}
            ("a: x\u{feff}y\n", "a byte-order mark past the start"),
            ("a: \u{7}\n", "a control character"), // PyYAML: ReaderError
            ("c:\n  <<: {b: 1}\n", "a merge key"), // PyYAML: {c: {b: 1}}
            ("a: 010\n", "010"),                   // PyYAML: {a: 8}
        ] {
            match load_yaml(doc) {
                Err(Fault::Value(e)) => assert!(e.contains(why), "{doc:?}: {e}"),
                other => panic!("{doc:?} read: {other:?}"),
            }
        }
    }

    use super::*;

    /// The hand-written resolver is PyYAML's on every scalar of its golden (`scripts/parity-delegation.py`).
    #[test]
    fn yaml11_is_pyyamls_resolver() {
        let golden: Vec<(String, String)> =
            serde_json::from_str(include_str!("../../../fixtures/delegation/yaml11-golden.json")).unwrap();
        let mut bad = Vec::new();
        for (s, tag) in &golden {
            let want = match tag.as_str() {
                "str" => Yaml11::Str,
                "null" => Yaml11::Null,
                "bool" => Yaml11::Bool,
                "int" => Yaml11::Int,
                "float" => Yaml11::Float,
                "timestamp" => Yaml11::Timestamp,
                "merge" => Yaml11::Merge,
                "value" => Yaml11::Value,
                other => panic!("tag {other}"),
            };
            if yaml11(s) != want {
                bad.push(format!("{s:?}: want {want:?}, got {:?}", yaml11(s)));
            }
        }
        assert!(golden.len() > 1000 && bad.is_empty(), "{} of {}:\n{}", bad.len(), golden.len(), bad.join("\n"));
    }

    /// Every document of the golden the CLI reads, PyYAML reads to the same value, types strict
    /// (`scripts/parity-delegation.py`, `yaml_differential`); where PyYAML raises or builds what JSON
    /// cannot hold, the CLI refuses.
    #[test]
    fn the_cli_reads_yaml_as_pyyaml_or_refuses() {
        let golden: Vec<Value> =
            serde_json::from_str(include_str!("../../../fixtures/delegation/yaml-golden.json")).unwrap();
        fn same(a: &Value, b: &Value) -> bool {
            match (a, b) {
                (Value::Number(x), Value::Number(y)) => x.is_f64() == y.is_f64() && x.as_f64() == y.as_f64(),
                (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
                (Value::Object(x), Value::Object(y)) => {
                    x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
                }
                _ => a == b,
            }
        }
        let (mut read, mut bad) = (0, Vec::new());
        for g in &golden {
            let text = g["text"].as_str().unwrap();
            let Ok(v) = load_yaml(text).and_then(|y| serde_json::to_value(y).map_err(|e| Fault::Value(e.to_string())))
            else {
                continue;
            };
            read += 1;
            if !g["pyyaml"].get("value").is_some_and(|w| same(w, &v)) {
                bad.push(format!("{text:?}: pyyaml {}, rust {v}", g["pyyaml"]));
            }
        }
        assert!(golden.len() > 5000 && read > 1000, "{read} of {} read", golden.len());
        assert!(bad.is_empty(), "{} misread:\n{}", bad.len(), bad[..bad.len().min(10)].join("\n"));
    }

    #[test]
    fn plain_scalars_both_readers_agree_on_are_inside() {
        for v in ["make test", "a.rs", "true", "False", "null", "~", "", "0", "-7", "+3", "1.5", "-2.25e+3", "0.51.0"] {
            assert_eq!(plain_scalar_outside(v, false), None, "{v}");
        }
        for v in [
            "yes",
            "off",
            "0250",
            "0o17",
            "0x1F",
            "1_000",
            "1:20",
            "1e3",
            ".5",
            "1.",
            ".inf",
            ".nan",
            "2026-10-23",
            "2026-10-23T12:00:00Z",
            "9223372036854775808",
            "=",
            "<<",
        ] {
            assert!(plain_scalar_outside(v, false).is_some(), "{v}");
        }
        for v in ["1", "true", "null", "1.5"] {
            assert!(plain_scalar_outside(v, true).is_some(), "key {v}");
        }
        assert_eq!(plain_scalar_outside("id", true), None);
    }
}
