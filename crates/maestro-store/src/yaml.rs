//! YAML as the Paseo plugin reads it.
//!
//! The plugin parses ledgers with the `yaml` package (2.x):
//! `parseDocument(source).toJS()` under the YAML 1.2 core schema. An entry hash
//! covers that JavaScript value, so Maestro must arrive at the same value, not
//! merely a similar one. serde_yaml resolves scalars by its own rules (`0004`
//! stays a string, `0b101` becomes 5, an integer past u64 fails the whole
//! file), so this module parses events with yaml-rust2 and resolves every
//! scalar itself, porting the package's rules:
//!
//! - **Plain scalars** try, in the core schema's order: null, bool, octal
//!   `0o`, decimal, hex `0x`, `.inf`/`.nan`, exponent, then float. Anything
//!   else is a string. Quoted and block scalars are always strings.
//! - **Numbers** are JavaScript doubles: `parseInt` and `parseFloat`, so a
//!   big integer rounds and keeps its entry instead of failing it.
//! - **Explicit tags:** a core tag (`!!str`, `!!int`, …) resolves with that
//!   tag's tests, falling back to a string when they fail (`!!float 1` is
//!   `"1"`). `!!binary` is bytes. `!!timestamp` is a `Date` built as
//!   `Date.UTC` builds it: out-of-range parts roll over, years 0–99 are 19xx,
//!   and a value its pattern does not match fails the document. `!!set` and
//!   `!!omap` are JavaScript collections that `canonical` sees as `{}`. Any
//!   other tag (`!custom`) reads its scalar as a string and its collection as is.
//! - **Map keys** are strings as `toJS` makes them: null is `""`, numbers print
//!   as JavaScript prints them (`Infinity`, `NaN`). A later key that stringifies
//!   alike replaces the earlier one. Two scalar keys with the same value are an
//!   error ("Map keys must be unique"), as in the package: `~` and `null`
//!   collide, and so do `.inf` and `.Inf`, but two `.nan` keys do not (NaN !== NaN).
//! - **Anchors and aliases** resolve to the anchored value. An alias adds one
//!   to its anchor's count, and a document whose `count × aliasCount` passes
//!   100 is refused, as the package's `maxAliasCount` refuses it. `<<` is an
//!   ordinary key, because YAML 1.2 has no merge keys.
//! - **More than one document** is an error, as `parseDocument` reports it.
//!
//! Known gaps, each refused loudly or reported, never silent:
//!
//! - A `%YAML 1.1` document is refused. The plugin reads it with YAML 1.1
//!   rules, which are not implemented here.
//! - A collection used as a map key is refused. The package writes it back in
//!   flow form with each scalar's original quoting, which this reader does not keep.
//! - A lone surrogate escape (`"\uD83D"`) cannot live in a Rust string. It is
//!   read as U+FFFD and reported, so that entry's hash differs from the plugin's.
//! - Collections nested deeper than [`MAX_DEPTH`] are refused. The plugin's limit
//!   is its engine's stack, which differs between node and bun.

use serde_json::{Map, Number, Value};
use std::collections::HashMap;
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser, Tag};
use yaml_rust2::scanner::{Marker, TScalarStyle};

const CORE: &str = "tag:yaml.org,2002:";
/// The package's default `maxAliasCount`: an alias whose `count × aliasCount` passes it refuses the document.
const MAX_ALIAS_COUNT: u64 = 100;
/// Values one document may expand to: a second guard, far above any ledger, against an alias blow-up.
const MAX_NODES: usize = 1_000_000;
/// Collections nested deeper than this are refused. yaml-rust2 caps flow nesting at 255
/// and has no cap on block nesting, and conversion recurses once per level, so an
/// unbounded block document would overflow the stack and abort the process (a 50 000-deep
/// `- - - …` did, inside yaml-rust2's own recursive `load`, which is why it is not used).
/// The plugin's own limit is its engine's stack: node fails near 1 000 levels and bun
/// near 8 000, so no exact parity exists past a few hundred.
pub const MAX_DEPTH: usize = 256;

/// A ledger read as the plugin reads it, plus anything read differently that the caller must report.
#[derive(Debug)]
pub struct Read {
    pub value: Value,
    pub warnings: Vec<String>,
}

/// Parse one YAML document to the JavaScript value `toJS()` would build.
pub fn parse(source: &str) -> Result<Read, String> {
    // A leading byte-order mark is not content: the package skips it, and so must the key it would prefix.
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    if let Some(version) = yaml_version(source)
        && version != "1.2"
    {
        return Err(format!(
            "a %YAML {version} document: the plugin reads it with YAML {version} rules, which Maestro does not implement"
        ));
    }
    let mut text = std::borrow::Cow::Borrowed(source);
    let mut lone = 0;
    // yaml-rust2 refuses a surrogate half in a `\u` escape, which the package keeps. Repair only
    // the double-quoted scalar the scanner names, then parse again; each pass fixes one scalar.
    for _ in 0..=source.len() {
        match parse_events(&text) {
            Ok(value) => {
                let warnings = if lone > 0 {
                    vec![format!(
                        "{lone} lone surrogate escape(s) read as U+FFFD: the plugin keeps them, so these entries hash differently"
                    )]
                } else {
                    Vec::new()
                };
                return Ok(Read { value, warnings });
            }
            Err((e, Some(at))) if e.contains("invalid Unicode character escape") => {
                let Some((fixed, n)) = repair_quoted_scalar(&text, at) else { return Err(e) };
                lone += n;
                text = std::borrow::Cow::Owned(fixed);
            }
            Err((e, _)) => return Err(e),
        }
    }
    Err("too many surrogate escapes to repair".into())
}

/// The version of any `%YAML` directive before the first document marker.
fn yaml_version(source: &str) -> Option<String> {
    for line in source.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some(rest) = t.strip_prefix("%YAML") {
            return rest.split_whitespace().next().map(str::to_string);
        }
        if !t.starts_with('%') {
            return None; // past the directives: `---` or content
        }
    }
    None
}

/// Rewrite the surrogate escapes of the double-quoted scalar opening at char index `at`:
/// a valid `\uD8xx\uDCxx` pair becomes `\UXXXXXXXX`, a lone half `\uFFFD`. Escaped
/// backslashes are skipped, so `\\uD83D` stays text. Returns the source and the lone count.
fn repair_quoted_scalar(source: &str, at: usize) -> Option<(String, usize)> {
    let start = source.char_indices().nth(at).map(|(i, _)| i)?;
    if !source[start..].starts_with('"') {
        return None;
    }
    let body = &source[start + 1..];
    let mut end = None;
    let mut chars = body.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '"' => {
                end = Some(i);
                break;
            }
            _ => {}
        }
    }
    let end = start + 1 + end?;
    let (fixed, lone) = rewrite(&source[start + 1..end], Pair::Yaml);
    Some((format!("{}{fixed}{}", &source[..=start], &source[end..]), lone))
}

/// Parse events into a value. An error carries the scanner's char index, when it names one.
fn parse_events(source: &str) -> Result<Value, (String, Option<usize>)> {
    let mut builder = Builder::default();
    let mut parser = Parser::new_from_str(source);
    // Drive the parser one event at a time. Its own `load` recurses once per nesting level,
    // which a deep enough document turns into a stack overflow; this loop does not, and it
    // stops at the first refusal instead of parsing the rest.
    loop {
        let (event, mark) = parser.next_token().map_err(|e| (e.to_string(), Some(e.marker().index())))?;
        let end = event == Event::StreamEnd;
        builder.on_event(event, mark);
        if end || builder.error.is_some() {
            break;
        }
    }
    if let Some(e) = builder.error {
        return Err((e, None));
    }
    if builder.documents > 1 {
        return Err(("Source contains multiple documents; the plugin reads one".into(), None));
    }
    match builder.root {
        None => Ok(Value::Null),
        Some(node) => {
            let mut cx = Cx { anchors: &builder.anchors, budget: MAX_NODES, aliases: HashMap::new() };
            to_js(&node, &mut cx, 0).map_err(|e| (e, None))
        }
    }
}

#[derive(Debug, Clone)]
enum Node {
    Scalar { value: String, style: TScalarStyle, tag: Option<Tag> },
    Seq(Option<Tag>, Vec<Node>),
    Map(Option<Tag>, Vec<(Node, Node)>),
    Alias(usize),
}

enum Open {
    Seq(usize, Option<Tag>, Vec<Node>),
    Map(usize, Option<Tag>, Vec<(Node, Node)>, Option<Node>),
}

#[derive(Default)]
struct Builder {
    stack: Vec<Open>,
    anchors: HashMap<usize, Node>,
    root: Option<Node>,
    documents: usize,
    error: Option<String>,
}

impl Builder {
    fn push(&mut self, node: Node, anchor: usize) {
        if anchor > 0 {
            self.anchors.insert(anchor, node.clone());
        }
        match self.stack.last_mut() {
            None => self.root = Some(node),
            Some(Open::Seq(_, _, items)) => items.push(node),
            Some(Open::Map(_, _, pairs, key)) => match key.take() {
                None => *key = Some(node),
                Some(k) => pairs.push((k, node)),
            },
        }
    }
}

impl MarkedEventReceiver for Builder {
    fn on_event(&mut self, ev: Event, _mark: Marker) {
        // Once the document is refused, build nothing more: the error is the result.
        if self.error.is_some() {
            return;
        }
        if matches!(ev, Event::SequenceStart(..) | Event::MappingStart(..)) && self.stack.len() >= MAX_DEPTH {
            self.error = Some(format!("collections nested deeper than {MAX_DEPTH} levels"));
            return;
        }
        match ev {
            Event::DocumentStart => self.documents += 1,
            Event::Scalar(value, style, anchor, tag) => self.push(Node::Scalar { value, style, tag }, anchor),
            Event::Alias(id) => self.push(Node::Alias(id), 0),
            Event::SequenceStart(anchor, tag) => self.stack.push(Open::Seq(anchor, tag, Vec::new())),
            Event::MappingStart(anchor, tag) => self.stack.push(Open::Map(anchor, tag, Vec::new(), None)),
            Event::SequenceEnd => {
                if let Some(Open::Seq(anchor, tag, items)) = self.stack.pop() {
                    self.push(Node::Seq(tag, items), anchor);
                }
            }
            Event::MappingEnd => {
                if let Some(Open::Map(anchor, tag, pairs, pending)) = self.stack.pop() {
                    if pending.is_some() {
                        self.error.get_or_insert_with(|| "a mapping ended with a key and no value".into());
                    }
                    self.push(Node::Map(tag, pairs), anchor);
                }
            }
            Event::Nothing | Event::StreamStart | Event::StreamEnd | Event::DocumentEnd => {}
        }
    }
}

/// An anchor's alias bookkeeping, as the package keeps it in `ctx.anchors`.
#[derive(Clone, Copy)]
struct AliasData {
    count: u64,
    alias_count: u64,
}

/// What one conversion carries: the anchors, the node budget, and each anchor's alias counts.
struct Cx<'a> {
    anchors: &'a HashMap<usize, Node>,
    budget: usize,
    aliases: HashMap<usize, AliasData>,
}

/// A resolved scalar as JavaScript holds it, before it becomes JSON.
#[derive(Debug, Clone, PartialEq)]
enum Js {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    /// Milliseconds since the epoch: a `Date`.
    Date(i64),
    Bytes(Vec<u8>),
}

impl Js {
    fn into_value(self) -> Value {
        match self {
            Js::Null => Value::Null,
            Js::Bool(b) => Value::Bool(b),
            Js::Num(x) => js_num(x),
            Js::Str(s) => Value::String(s),
            // `canonical` turns a Date into `toISOString()`.
            Js::Date(ms) => Value::String(iso_string(ms)),
            // `canonical` sees a byte array as an object of index keys.
            Js::Bytes(b) => {
                Value::Object(b.into_iter().enumerate().map(|(i, v)| (i.to_string(), Value::from(v))).collect())
            }
        }
    }
}

/// `depth` counts collections as resolved, aliases included: a chain of anchors each
/// wrapping the last nests deeper than any one of them was written, so the cap applies here too.
fn to_js(node: &Node, cx: &mut Cx<'_>, depth: usize) -> Result<Value, String> {
    cx.budget = cx.budget.checked_sub(1).ok_or("Excessive alias count: the document expands past its limit")?;
    if depth > MAX_DEPTH {
        return Err(format!("collections nested deeper than {MAX_DEPTH} levels once aliases are resolved"));
    }
    match node {
        Node::Scalar { value, style, tag } => Ok(scalar(value, *style, tag.as_ref())?.into_value()),
        Node::Alias(id) => {
            let target = alias_target(*id, cx)?;
            to_js(target, cx, depth)
        }
        Node::Seq(tag, items) => {
            let values: Vec<Value> = items.iter().map(|n| to_js(n, cx, depth + 1)).collect::<Result<_, _>>()?;
            // `!!omap` is a JavaScript Map: `canonical` finds no own keys on it.
            Ok(if core_tag(tag.as_ref()) == Some("omap") { Value::Object(Map::new()) } else { Value::Array(values) })
        }
        Node::Map(tag, pairs) => {
            let mut out = Map::new();
            let mut seen = std::collections::HashSet::new();
            for (k, v) in pairs {
                let key = key_scalar(k, cx)?;
                // `uniqueKeys` compares two scalar *nodes* by value; an alias key is a different node.
                if matches!(k, Node::Scalar { .. })
                    && let Some(id) = key_identity(&key)
                    && !seen.insert(id)
                {
                    return Err("Map keys must be unique".into());
                }
                let name = key_string(&key)?;
                out.insert(name, to_js(v, cx, depth + 1)?);
            }
            // `!!set` is a JavaScript Set: `canonical` finds no own keys on it.
            Ok(if core_tag(tag.as_ref()) == Some("set") { Value::Object(Map::new()) } else { Value::Object(out) })
        }
    }
}

fn core_tag(tag: Option<&Tag>) -> Option<&str> {
    tag.filter(|t| t.handle == CORE).map(|t| t.suffix.as_str())
}

/// Resolve an alias, counting it as the package's `Alias.resolve` does: the anchor's count
/// rises by one, its alias count is taken once (`getAliasCount`), and their product past
/// `maxAliasCount` refuses the document.
fn alias_target<'a>(id: usize, cx: &mut Cx<'a>) -> Result<&'a Node, String> {
    let target = cx.anchors.get(&id).ok_or("an alias names no anchor")?;
    let mut data = cx.aliases.get(&id).copied().unwrap_or(AliasData { count: 1, alias_count: 0 });
    data.count += 1;
    if data.alias_count == 0 {
        data.alias_count = alias_count(target, cx);
    }
    cx.aliases.insert(id, data);
    if data.count.saturating_mul(data.alias_count) > MAX_ALIAS_COUNT {
        return Err("Excessive alias count indicates a resource exhaustion attack".into());
    }
    Ok(target)
}

/// The package's `getAliasCount`: 1 for a scalar, an alias's own count × aliasCount, and the
/// largest over a collection's items (0 for an empty one, which therefore never trips the limit).
fn alias_count(node: &Node, cx: &Cx<'_>) -> u64 {
    match node {
        Node::Scalar { .. } => 1,
        Node::Alias(id) => cx.aliases.get(id).map_or(0, |d| d.count.saturating_mul(d.alias_count)),
        Node::Seq(_, items) => items.iter().map(|n| alias_count(n, cx)).max().unwrap_or(0),
        Node::Map(_, pairs) => pairs.iter().map(|(k, v)| alias_count(k, cx).max(alias_count(v, cx))).max().unwrap_or(0),
    }
}

/// A map key as a resolved scalar. A collection key is refused: see the module's known gaps.
fn key_scalar(node: &Node, cx: &mut Cx<'_>) -> Result<Js, String> {
    match node {
        Node::Scalar { value, style, tag } => scalar(value, *style, tag.as_ref()),
        Node::Alias(id) => {
            let target = alias_target(*id, cx)?;
            key_scalar(target, cx)
        }
        Node::Seq(..) | Node::Map(..) => Err(
            "a collection used as a map key: the plugin writes it back in flow form with its original quoting, which Maestro does not keep"
                .into(),
        ),
    }
}

/// What `a.value === b.value` compares for a scalar key: type and value. Null equals null, numbers
/// compare as doubles (0 and -0 equal, NaN never), and a Date or byte array is an object, equal to
/// nothing else, so it has no identity here.
#[derive(PartialEq, Eq, Hash)]
enum KeyId {
    Null,
    Bool(bool),
    Num(u64),
    Str(String),
}

fn key_identity(key: &Js) -> Option<KeyId> {
    Some(match key {
        Js::Null => KeyId::Null,
        Js::Bool(b) => KeyId::Bool(*b),
        Js::Num(x) if x.is_nan() => return None,
        Js::Num(x) => KeyId::Num(if *x == 0.0 { 0 } else { x.to_bits() }),
        Js::Str(s) => KeyId::Str(s.clone()),
        Js::Date(_) | Js::Bytes(_) => return None,
    })
}

/// The package's `stringifyKey`: null is "", anything else `String(value)`.
fn key_string(key: &Js) -> Result<String, String> {
    Ok(match key {
        Js::Null => String::new(),
        Js::Bool(b) => b.to_string(),
        Js::Num(x) if x.is_nan() => "NaN".into(),
        Js::Num(x) if x.is_infinite() => if *x > 0.0 { "Infinity" } else { "-Infinity" }.into(),
        Js::Num(x) => maestro_core::ask::js_double(*x),
        Js::Str(s) => s.clone(),
        // An object key is written back from its node; this reader does not keep the source form.
        Js::Date(_) | Js::Bytes(_) => return Err("a tagged map key (a date or bytes) Maestro cannot stringify".into()),
    })
}

fn scalar(value: &str, style: TScalarStyle, tag: Option<&Tag>) -> Result<Js, String> {
    Ok(match tag {
        Some(tag) if tag.handle == CORE => tagged(value, &tag.suffix)?,
        // A non-specific `!` or any local/unknown tag: the scalar is its text.
        Some(_) => Js::Str(value.to_string()),
        None if style == TScalarStyle::Plain => plain(value),
        None => Js::Str(value.to_string()),
    })
}

/// A plain scalar under the core schema, in the schema's own test order.
fn plain(s: &str) -> Js {
    null_of(s)
        .or_else(|| bool_of(s))
        .or_else(|| int_of(s).map(Js::Num))
        .or_else(|| float_of(s).map(Js::Num))
        .unwrap_or_else(|| Js::Str(s.to_string()))
}

fn tagged(s: &str, suffix: &str) -> Result<Js, String> {
    let resolved = match suffix {
        "str" => Some(Js::Str(s.to_string())),
        "null" => null_of(s),
        "bool" => bool_of(s),
        "int" => int_of(s).map(Js::Num),
        "float" => float_of(s).map(Js::Num),
        "binary" => Some(Js::Bytes(binary(s))),
        // A failed timestamp throws in the package, which fails the document.
        "timestamp" => Some(timestamp(s).ok_or("!!timestamp expects a date, starting with yyyy-mm-dd")?),
        _ => None,
    };
    // Any other failed test falls back to the string tag, as the package does (with a warning).
    Ok(resolved.unwrap_or_else(|| Js::Str(s.to_string())))
}

fn null_of(s: &str) -> Option<Js> {
    matches!(s, "" | "~" | "null" | "Null" | "NULL").then_some(Js::Null)
}

fn bool_of(s: &str) -> Option<Js> {
    match s {
        "true" | "True" | "TRUE" => Some(Js::Bool(true)),
        "false" | "False" | "FALSE" => Some(Js::Bool(false)),
        _ => None,
    }
}

/// `0o[0-7]+`, `[-+]?[0-9]+`, `0x[0-9a-fA-F]+`, each through `parseInt`.
fn int_of(s: &str) -> Option<f64> {
    if let Some(oct) = s.strip_prefix("0o") {
        return radix(oct, 8);
    }
    if let Some(hex) = s.strip_prefix("0x") {
        return radix(hex, 16);
    }
    let digits = s.strip_prefix(['-', '+']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<f64>().ok()
}

/// `parseInt(digits, base)` for base 8 or 16: the exact integer, rounded once to the nearest
/// double (ties to even). Accumulating in f64 would round at every step and drift past 2^53.
fn radix(digits: &str, base: u32) -> Option<f64> {
    if digits.is_empty() {
        return None;
    }
    let bits_per = if base == 16 { 4 } else { 3 };
    let mut bits: Vec<bool> = Vec::with_capacity(digits.len() * bits_per);
    for c in digits.chars() {
        let d = c.to_digit(base)?;
        bits.extend((0..bits_per).rev().map(|i| d >> i & 1 == 1));
    }
    let first = bits.iter().position(|&b| b).unwrap_or(bits.len());
    let bits = &bits[first..];
    if bits.is_empty() {
        return Some(0.0);
    }
    if bits.len() > 1024 {
        return Some(f64::INFINITY);
    }
    // The top 53 bits, then round on the 54th with the rest as sticky.
    let mut mantissa: u64 = 0;
    for &b in bits.iter().take(53) {
        mantissa = mantissa << 1 | u64::from(b);
    }
    let mut exp = bits.len().saturating_sub(53) as i32;
    if bits.len() > 53 {
        let half = bits[53];
        let sticky = bits[54..].iter().any(|&b| b);
        if half && (sticky || mantissa & 1 == 1) {
            mantissa += 1;
            if mantissa == 1 << 53 {
                mantissa >>= 1;
                exp += 1;
            }
        }
    }
    Some(mantissa as f64 * 2f64.powi(exp))
}

/// `.inf`/`.nan` forms, then `[-+]?(\.d+|d+(\.d*)?)[eE][-+]?d+`, then `[-+]?(\.d+|d+\.d*)`, through `parseFloat`.
fn float_of(s: &str) -> Option<f64> {
    let unsigned = s.strip_prefix(['-', '+']).unwrap_or(s);
    if matches!(s, ".nan" | ".NaN" | ".NAN") {
        return Some(f64::NAN);
    }
    if matches!(unsigned, ".inf" | ".Inf" | ".INF") {
        return Some(if s.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY });
    }
    let (mantissa, exp) = match unsigned.find(['e', 'E']) {
        Some(i) => (&unsigned[..i], Some(&unsigned[i + 1..])),
        None => (unsigned, None),
    };
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    let mantissa_ok = match mantissa.split_once('.') {
        // `.d+`, `d+.d*`; a bare `d+` only with an exponent.
        Some((int, frac)) => (int.is_empty() && digits(frac)) || (digits(int) && (frac.is_empty() || digits(frac))),
        None => exp.is_some() && digits(mantissa),
    };
    let exp_ok = exp.is_none_or(|e| digits(e.strip_prefix(['-', '+']).unwrap_or(e)));
    if !mantissa_ok || !exp_ok {
        return None;
    }
    s.parse::<f64>().ok()
}

/// A JavaScript number as JSON: integral doubles within 2^53 as integers, and a non-finite one
/// as a marked string ([`maestro_core::ask::NONFINITE`]), so it still hashes as `null` and reads
/// as `Infinity` or `NaN`.
fn js_num(x: f64) -> Value {
    const SAFE: f64 = 9_007_199_254_740_992.0;
    if !x.is_finite() {
        let name = if x.is_nan() {
            "NaN"
        } else if x > 0.0 {
            "Infinity"
        } else {
            "-Infinity"
        };
        return Value::String(format!("{}{name}", maestro_core::ask::NONFINITE));
    }
    if x.fract() == 0.0 && x.abs() <= SAFE {
        return Value::from(x as i64); // -0 becomes 0, as JSON.stringify prints it
    }
    Number::from_f64(x).map_or(Value::Null, Value::Number)
}

/// `!!binary`: base64 decoded leniently, as Node's `Buffer.from(s, 'base64')` decodes it
/// (bytes outside the alphabet skipped, decoding stops at `=`).
fn binary(s: &str) -> Vec<u8> {
    let mut bits = 0u32;
    let mut n = 0;
    let mut bytes = Vec::new();
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => continue,
        };
        bits = (bits << 6) | u32::from(v);
        n += 6;
        if n >= 8 {
            n -= 8;
            bytes.push((bits >> n) as u8);
            bits &= (1 << n) - 1;
        }
    }
    bytes
}

/// `!!timestamp`, ported from the package's `timestamp.resolve`:
///
/// ```text
/// ^(\d{4})-(\d{1,2})-(\d{1,2})(?:(?:t|T|[ \t]+)(\d{1,2}):(\d{1,2}):(\d{1,2}(\.\d+)?)(?:[ \t]*(Z|[-+][012]?\d(?::\d{2})?))?)?$
/// ```
///
/// then `Date.UTC(year, month - 1, day, h, m, s, ms)`, which rolls every part over (Feb 31 is
/// Mar 3) and reads years 0–99 as 19xx, less the offset in minutes (an offset under 30 is hours).
fn timestamp(s: &str) -> Option<Js> {
    let digits =
        |t: &str, min: usize, max: usize| (min..=max).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_digit());
    let take_digits = |t: &str| t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
    let (y, rest) = s.split_at_checked(take_digits(s))?;
    let rest = rest.strip_prefix('-')?;
    let (mo, rest) = rest.split_at_checked(take_digits(rest))?;
    let rest = rest.strip_prefix('-')?;
    let (d, rest) = rest.split_at_checked(take_digits(rest))?;
    if !digits(y, 4, 4) || !digits(mo, 1, 2) || !digits(d, 1, 2) {
        return None;
    }
    let (mut hh, mut mi, mut sec, mut ms, mut offset_min) = (0i64, 0i64, 0i64, 0i64, 0i64);
    if !rest.is_empty() {
        let rest = rest
            .strip_prefix(['t', 'T'])
            .or_else(|| rest.starts_with([' ', '\t']).then(|| rest.trim_start_matches([' ', '\t'])))?;
        let (h, rest) = rest.split_at_checked(take_digits(rest))?;
        let rest = rest.strip_prefix(':')?;
        let (m, rest) = rest.split_at_checked(take_digits(rest))?;
        let rest = rest.strip_prefix(':')?;
        let (sc, rest) = rest.split_at_checked(take_digits(rest))?;
        if !digits(h, 1, 2) || !digits(m, 1, 2) || !digits(sc, 1, 2) {
            return None;
        }
        let rest = match rest.strip_prefix('.') {
            Some(frac) => {
                let n = take_digits(frac);
                if n == 0 {
                    return None;
                }
                // `Number((match[7] + '00').substr(1, 3))`: the first three digits, right-padded.
                let three: String = format!("{}00", &frac[..n]).chars().take(3).collect();
                ms = three.parse().ok()?;
                &frac[n..]
            }
            None => rest,
        };
        (hh, mi, sec) = (h.parse().ok()?, m.parse().ok()?, sc.parse().ok()?);
        let zone = rest.trim_start_matches([' ', '\t']);
        if !zone.is_empty() && zone != "Z" {
            let (sign, body) = match zone.as_bytes().first()? {
                b'+' => (1, &zone[1..]),
                b'-' => (-1, &zone[1..]),
                _ => return None,
            };
            let (oh, om) = match body.split_once(':') {
                Some((oh, om)) => (oh, Some(om)),
                None => (body, None),
            };
            // `[012]?\d`, then optionally `:\d{2}`.
            let hours_ok = match oh.as_bytes() {
                [b] => b.is_ascii_digit(),
                [a, b] => matches!(a, b'0'..=b'2') && b.is_ascii_digit(),
                _ => false,
            };
            if !hours_ok || om.is_some_and(|m| !digits(m, 2, 2)) {
                return None;
            }
            // parseSexagesimal: hours·60 + minutes; a value under 30 is hours.
            let mut total = oh.parse::<i64>().ok()?;
            if let Some(m) = om {
                total = total * 60 + m.parse::<i64>().ok()?;
            }
            let total = sign * total;
            offset_min = if total.abs() < 30 { total * 60 } else { total };
        }
    } else if s.len() != y.len() + mo.len() + d.len() + 2 {
        return None;
    }
    let mut year: i64 = y.parse().ok()?;
    if (0..=99).contains(&year) {
        year += 1900;
    }
    let month0 = mo.parse::<i64>().ok()? - 1;
    let (ym, mn) = (year + month0.div_euclid(12), month0.rem_euclid(12) + 1);
    let days = days_from_civil(ym, mn, 1) + d.parse::<i64>().ok()? - 1;
    let time = hh * 3_600_000 + mi * 60_000 + sec * 1_000 + ms;
    Some(Js::Date(days * 86_400_000 + time - 60_000 * offset_min))
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `Date#toISOString()`: `YYYY-MM-DDTHH:mm:ss.sssZ`, with a signed six-digit year outside 0–9999.
fn iso_string(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let in_day = ms.rem_euclid(86_400_000);
    // civil_from_days (Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let year = if (0..=9999).contains(&y) {
        format!("{y:04}")
    } else {
        format!("{}{:06}", if y < 0 { '-' } else { '+' }, y.abs())
    };
    format!(
        "{year}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        in_day / 3_600_000,
        in_day / 60_000 % 60,
        in_day / 1_000 % 60,
        in_day % 1_000
    )
}

/// JSON text with each lone surrogate escape replaced by `�`; valid pairs are kept.
pub fn lone_surrogates_to_replacement(text: &str) -> String {
    let (fixed, _) = rewrite(text, Pair::Json);
    fixed
}

#[derive(Clone, Copy)]
enum Pair {
    /// Write a valid pair back as `\uXXXX\uXXXX` (JSON has no `\U`).
    Json,
    /// Write a valid pair as `\UXXXXXXXX` (yaml-rust2 rejects a `\u` surrogate half).
    Yaml,
}

/// Scan escapes left to right, so `\\uD83D` (an escaped backslash, then text) is never read as an escape.
fn rewrite(text: &str, pair: Pair) -> (String, usize) {
    let unit = |s: &str| -> Option<u32> {
        u32::from_str_radix(s.get(..4)?, 16)
            .ok()
            .filter(|_| s.get(..4).is_some_and(|h| h.bytes().all(|b| b.is_ascii_hexdigit())))
    };
    let mut out = String::with_capacity(text.len());
    let mut lone = 0;
    let mut rest = text;
    while let Some(i) = rest.find('\\') {
        out.push_str(&rest[..i]);
        let at = &rest[i..];
        if let Some(after) = at.strip_prefix("\\\\") {
            out.push_str("\\\\");
            rest = after;
            continue;
        }
        let Some(after) = at.strip_prefix("\\u") else {
            out.push('\\');
            rest = &at[1..];
            continue;
        };
        match unit(after) {
            Some(hi @ 0xD800..=0xDBFF) => match after.get(4..).and_then(|t| t.strip_prefix("\\u")).and_then(unit) {
                Some(lo @ 0xDC00..=0xDFFF) => {
                    match pair {
                        Pair::Yaml => {
                            let cp = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                            out.push_str(&format!("\\U{cp:08X}"));
                        }
                        Pair::Json => out.push_str(&at[..12]),
                    }
                    rest = &at[12..];
                }
                _ => {
                    out.push_str("\\uFFFD");
                    lone += 1;
                    rest = &at[6..];
                }
            },
            Some(0xDC00..=0xDFFF) => {
                out.push_str("\\uFFFD");
                lone += 1;
                rest = &at[6..];
            }
            _ => {
                out.push_str("\\u");
                rest = &at[2..];
            }
        }
    }
    out.push_str(rest);
    (out, lone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn js(source: &str) -> Value {
        parse(source).unwrap().value
    }

    #[test]
    fn plain_scalars_resolve_by_the_core_schema() {
        let v = js(
            "a: 0004\nb: 0b101\nc: 1_000\nd: 0o17\ne: 0x1F\nf: +5\ng: yes\nh: tRue\ni: ~\nj:\nk: 1e3\nl: .5\nm: 5.\n",
        );
        assert_eq!(
            v,
            json!({"a":4,"b":"0b101","c":"1_000","d":15,"e":31,"f":5,"g":"yes","h":"tRue","i":null,"j":null,"k":1000,"l":0.5,"m":5})
        );
    }

    #[test]
    fn quoted_scalars_stay_strings_and_tags_decide() {
        assert_eq!(
            js("a: '1e3'\nb: \"true\"\nc: !!int \"42\"\nd: !!float 1\ne: !custom foo\n"),
            json!({"a":"1e3","b":"true","c":42,"d":"1","e":"foo"})
        );
    }

    #[test]
    fn big_integers_round_instead_of_failing() {
        let v = js("u: 18446744073709551616\nn: -9223372036854775809\nd: 20261019123456789012\n");
        assert_eq!(v["u"].as_f64(), Some(18446744073709551616.0));
        assert_eq!(maestro_core::ask::js_number(v["d"].as_number().unwrap()), "20261019123456790000");
    }

    #[test]
    fn keys_stringify_as_to_js_does() {
        let v = js("0x10: hex\n1.0: float\n~: n\ntrue: b\n.inf: i\n-.inf: m\n.nan: x\n.NaN: y\n");
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["", "-Infinity", "1", "16", "Infinity", "NaN", "true"]);
        assert_eq!(v["NaN"], "y", "NaN keys never collide, so the later one wins");
        assert!(parse("~: a\nnull: b\n").unwrap_err().contains("unique"), "null === null");
        assert!(parse(".inf: a\n.Inf: b\n").unwrap_err().contains("unique"), "Infinity === Infinity");
        assert_eq!(js("a: 1\n&k b: 2\n*k : 3\n"), json!({"a": 1, "b": 3}), "an alias key is another node");
        assert!(parse("? [a, b]\n: v\n").unwrap_err().contains("collection used as a map key"));
        assert_eq!(
            js("1: number\n\"1\": string\n"),
            json!({"1": "string"}),
            "same JS key, different values: last wins"
        );
        assert!(parse("a: 1\na: 2\n").unwrap_err().contains("unique"));
        assert!(parse("0x10: a\n16: b\n").unwrap_err().contains("unique"), "both resolve to 16");
    }

    #[test]
    fn documents_the_plugin_refuses_are_refused() {
        assert!(parse("a: 1\n---\nb: 2\n").unwrap_err().contains("multiple documents"));
        assert!(parse("%YAML 1.1\n---\na: yes\n").unwrap_err().contains("1.1"));
        assert_eq!(js("%YAML 1.2\n---\na: 1\n"), json!({"a": 1}));
    }

    #[test]
    fn surrogate_escapes_pair_or_report() {
        let r = parse("a: \"\\uD83D\\uDE00\"\nb: \"cut \\uD83D x\"\n").unwrap();
        assert_eq!(r.value, json!({"a": "\u{1F600}", "b": "cut \u{FFFD} x"}));
        assert_eq!(r.warnings.len(), 1);
        assert!(parse("a: \"\\uD83D\\uDE00\"\n").unwrap().warnings.is_empty(), "a valid pair is no loss");
    }

    #[test]
    fn binary_and_timestamp_resolve_as_the_package_does() {
        assert_eq!(js("b: !!binary aGk=\n"), json!({"b": {"0": 104, "1": 105}}));
        assert_eq!(js("t: !!timestamp 2026-10-19\n"), json!({"t": "2026-10-19T00:00:00.000Z"}));
        assert_eq!(js("t: !!timestamp 2026-10-19T10:30:00.5-05:00\n"), json!({"t": "2026-10-19T15:30:00.500Z"}));
        assert_eq!(js("t: 2026-10-19\n"), json!({"t": "2026-10-19"}), "untagged dates stay strings in 1.2");
    }

    #[test]
    fn deep_nesting_is_refused_not_a_stack_overflow() {
        let block = |n: usize| format!("x:\n  {}v\n", "- ".repeat(n));
        // On the 2 MiB stack that std threads and tokio's blocking pool give the hook and the MCP server.
        let run = |src: String| {
            std::thread::Builder::new().stack_size(2 << 20).spawn(move || parse(&src).map(|_| ())).unwrap().join()
        };
        let deep = run(block(50_000)).expect("no stack overflow past the cap");
        assert!(deep.unwrap_err().contains("deeper than"));
        assert!(run(block(MAX_DEPTH - 1)).expect("no stack overflow at the cap").is_ok(), "at the cap it reads");
        let flow = run(format!("x: {}{}\n", "[".repeat(300), "]".repeat(300))).expect("no stack overflow");
        assert!(flow.is_err());
        // Each anchor wraps the previous one: the package's alias count refuses it first.
        let mut chain = String::from("a0: &a0 [v]\n");
        for i in 1..2000 {
            chain.push_str(&format!("a{i}: &a{i} [*a{}]\n", i - 1));
        }
        let aliased = run(chain).expect("no stack overflow through aliases");
        assert!(aliased.unwrap_err().contains("Excessive alias count"));
        // One alias, so the count stays low, but 200 + 100 levels once resolved: the depth cap refuses it.
        let deep_alias =
            format!("a: &a {}v{}\nb: {}*a{}\n", "[".repeat(200), "]".repeat(200), "[".repeat(100), "]".repeat(100));
        let resolved = run(deep_alias).expect("no stack overflow through aliases");
        assert!(resolved.unwrap_err().contains("once aliases are resolved"));
    }

    #[test]
    fn alias_counts_follow_max_alias_count() {
        let copies = |base: &str, n: usize| format!("base: &b {base}\ncopies:\n{}", "  - *b\n".repeat(n));
        assert!(parse(&copies("[x]", 99)).is_ok(), "count 100 × aliasCount 1 is not past 100");
        assert!(parse(&copies("[x]", 100)).unwrap_err().contains("Excessive alias count"));
        assert!(parse(&copies("x", 100)).unwrap_err().contains("Excessive alias count"));
        assert!(parse(&copies("[]", 150)).is_ok(), "an empty collection has aliasCount 0");
        // Weighted through nesting: 10 aliases to `a` inside `b` make each alias to `b` count for 11.
        let nested = format!("a: &a [x]\nb: &b [{}]\nc: [{}]\n", ["*a"; 10].join(", "), ["*b"; 11].join(", "));
        assert!(parse(&nested).unwrap_err().contains("Excessive alias count"));
    }

    #[test]
    fn bom_directives_and_radix_rounding_match_the_package() {
        assert_eq!(js("\u{feff}asks: [1]\n"), json!({"asks": [1]}), "a BOM is not part of the first key");
        assert!(parse("%TAG ! tag:x,2026:\n%YAML 1.1\n---\na: 1\n").unwrap_err().contains("1.1"));
        // parseInt rounds once: 0x400000000000021 is 288230376151711776 + 1, nearest double ...800.
        assert_eq!(
            maestro_core::ask::js_number(js("v: 0x400000000000021\n")["v"].as_number().unwrap()),
            "288230376151711800"
        );
        assert_eq!(js("v: 0o17\nw: 0x1F\n"), json!({"v": 15, "w": 31}));
    }

    #[test]
    fn timestamps_roll_over_as_date_utc_does() {
        let t = |s: &str| js(&format!("t: !!timestamp {s}\n"))["t"].as_str().unwrap().to_string();
        assert_eq!(t("2026-02-31"), "2026-03-03T00:00:00.000Z");
        assert_eq!(t("2026-13-45"), "2027-02-14T00:00:00.000Z");
        assert_eq!(t("0050-01-01"), "1950-01-01T00:00:00.000Z");
        assert_eq!(t("2026-10-19 23:59:60.1234 +5"), "2026-10-19T19:00:00.123Z");
        assert_eq!(t("2026-10-19t10:30:00-03:30"), "2026-10-19T14:00:00.000Z");
        assert_eq!(t("9999-12-31 24:00:00"), "+010000-01-01T00:00:00.000Z");
        for bad in [
            "2026-10-19T10:30:00+999999999",
            "yesterday",
            "2026-10-19t10:30:00z",
            "2026-10-19T10:30:00-0530",
            "99999-01-01",
        ] {
            assert!(parse(&format!("t: !!timestamp {bad}\n")).unwrap_err().contains("!!timestamp"), "{bad}");
        }
    }

    #[test]
    fn surrogate_repair_stays_inside_the_failing_scalar() {
        let r = parse("a: \"x \\\\uD83D \\uDE00\"\nw: 'lit \\uD83D\\uDE00'\n").unwrap();
        assert_eq!(r.value["w"], "lit \\uD83D\\uDE00", "a single-quoted scalar has no escapes");
        assert_eq!(r.value["a"], "x \\uD83D \u{FFFD}", "an escaped backslash is text, not an escape");
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].starts_with("1 lone"));
    }

    #[test]
    fn collection_tags_read_as_the_package_reads_them() {
        assert_eq!(
            js("s: !!set {a: , b: }\no: !!omap [{a: 1}]\np: !!pairs [{a: 1}, {a: 2}]\nc: !custom {a: 1}\n"),
            json!({"s": {}, "o": {}, "p": [{"a": 1}, {"a": 2}], "c": {"a": 1}})
        );
    }

    #[test]
    fn aliases_copy_and_merge_keys_are_plain_keys() {
        assert_eq!(
            js("b: &b {x: 1}\nr: *b\nm:\n  <<: *b\n"),
            json!({"b": {"x": 1}, "r": {"x": 1}, "m": {"<<": {"x": 1}}})
        );
    }
}
