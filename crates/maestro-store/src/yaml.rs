//! Ask ledgers read as the Paseo plugin reads them, within a defined subset of YAML.
//!
//! The plugin parses ledgers with the `yaml` package (2.x):
//! `parseDocument(source).toJS()` under the YAML 1.2 core schema, and an entry
//! hash covers that JavaScript value. This module reads ledgers with yaml-rust2
//! and resolves every scalar itself, porting the package's rules, so the value,
//! and so every hash and state, is the plugin's.
//!
//! Exact parity holds inside a **subset**, the YAML that ledger writers produce
//! (`ask_ledger.py`, PyYAML, the plugin itself). Outside it the two parsers
//! disagree in ways no rule over yaml-rust2 can close (block-scalar edge
//! semantics, NUL handling, byte-versus-char positions), so a document outside
//! the subset is **refused**, with the reason named, never misread. The owner
//! chose this on 2026-10-20 (BRO-3207). A full port of the package's parser is
//! the path to parity outside the subset.
//!
//! **Inside the subset** (exact):
//! - block mappings and sequences, flow `[...]`/`{...}`, comments;
//! - plain, single- and double-quoted scalars, single- and multi-line;
//! - block scalars `|`, `|-`, `>`, `>-`;
//! - anchors and aliases, counted as the package's `maxAliasCount` counts them;
//! - a leading byte-order mark and a leading `---`;
//! - CRLF line endings, and any Unicode text.
//!
//! Plain scalars resolve in the core schema's order: null, bool, octal `0o`,
//! decimal, hex `0x`, `.inf`/`.nan`, exponent, float, else a string. Numbers are
//! JavaScript doubles: `parseInt` and `parseFloat`, `Infinity` and `NaN` included.
//! Map keys stringify as `toJS` makes them. Duplicate scalar keys are an error,
//! and a later key that stringifies alike replaces the earlier one.
//!
//! **Outside the subset** (refused, the reason naming the feature):
//! - a tab, a lone carriage return, a NUL or other control character, a literal
//!   NEL/LS/PS, a byte-order mark past the start, invalid UTF-8;
//! - `%` directives, `...`, a `---` past the first line, more than one document;
//! - tags (`!!int`, `!custom`, …) and explicit keys (`? k`);
//! - block scalars with `+` chomping or an indentation digit;
//! - whitespace-only lines, and a non-empty document not ending in a newline;
//! - surrogate escapes (`"\uD83D"`);
//! - a collection as a map key, an implicit key over 1024 chars, and a plain
//!   scalar opening with `|`, `>`, `@`, `` ` `` or `%`;
//! - nesting deeper than [`MAX_DEPTH`].

use maestro_core::js::JsValue;
use std::collections::{BTreeMap, HashMap};
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser, Tag};
use yaml_rust2::scanner::{Marker, TScalarStyle};

/// The package's default `maxAliasCount`: an alias whose `count × aliasCount` passes it refuses the document.
const MAX_ALIAS_COUNT: u64 = 100;
/// Values one document may hold: a guard, far above any ledger, against an expansion blow-up.
const MAX_NODES: usize = 1_000_000;
/// Collections nested deeper than this are refused. Conversion recurses per level, and the
/// plugin's own limit is its engine's stack (node near 1 000 levels, bun near 8 000).
pub const MAX_DEPTH: usize = 256;
/// The YAML spec's limit on an implicit key, which the package enforces.
const MAX_IMPLICIT_KEY: usize = 1024;
/// The stack the parser runs on, so [`MAX_DEPTH`] holds whatever thread calls (a 2 MiB tokio
/// blocking thread, a test thread). Reserved, not committed: only what is used costs.
const PARSE_STACK: usize = 64 << 20;

/// A ledger read as the plugin reads it, plus anything the caller must report.
#[derive(Debug)]
pub struct Read {
    pub value: JsValue,
    pub warnings: Vec<String>,
}

/// The prefix of every refusal for YAML outside the subset.
pub const OUTSIDE: &str = "outside the YAML subset Maestro reads exactly";

/// Parse one ledger to the JavaScript value `toJS()` would build, or refuse it.
pub fn parse(source: &str) -> Result<Read, String> {
    let owned = source.to_string();
    std::thread::Builder::new()
        .name("maestro-yaml".into())
        .stack_size(PARSE_STACK)
        .spawn(move || parse_here(&owned))
        .map_err(|e| format!("could not start the YAML reader: {e}"))?
        .join()
        .map_err(|_| "the YAML reader failed".to_string())?
}

fn parse_here(source: &str) -> Result<Read, String> {
    // A leading byte-order mark is not content: the package skips it, and so must the key it would prefix.
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    if let Some(reason) = outside_subset(source) {
        return Err(format!("{OUTSIDE}: {reason}"));
    }
    let value = parse_events(source)?;
    Ok(Read { value, warnings: Vec::new() })
}

/// The first lexical feature of `source` outside the subset, if any.
fn outside_subset(source: &str) -> Option<String> {
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        let bad = match c {
            '\t' => Some("a tab character"),
            '\r' if chars.peek() != Some(&'\n') => Some("a carriage return outside a CRLF line ending"),
            '\u{feff}' => Some("a byte-order mark past the start"),
            '\u{85}' | '\u{2028}' | '\u{2029}' => Some("a literal NEL, LS or PS line break"),
            c if (c as u32) < 0x20 && c != '\n' && c != '\r' => Some("a NUL or other control character"),
            '\u{7f}' => Some("a DEL control character"),
            _ => None,
        };
        if let Some(bad) = bad {
            return Some(bad.into());
        }
    }
    if !source.is_empty() && !source.ends_with('\n') {
        return Some("a document that does not end with a newline".into());
    }
    for (n, raw) in source.split('\n').enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let trimmed = line.trim_start_matches(' ');
        if line.starts_with('%') {
            return Some(format!("a directive (line {})", n + 1));
        }
        if line == "..." || line.starts_with("... ") {
            return Some(format!("a document end marker `...` (line {})", n + 1));
        }
        if (line == "---" || line.starts_with("--- ")) && n > 0 {
            return Some(format!("a `---` past the first line (line {})", n + 1));
        }
        if !line.is_empty() && trimmed.is_empty() {
            return Some(format!("a whitespace-only line (line {})", n + 1));
        }
        let after_dashes = trimmed.trim_start_matches(['-', ' ']);
        if trimmed.starts_with("? ") || trimmed == "?" || (trimmed.starts_with('-') && after_dashes.starts_with("? ")) {
            return Some(format!("an explicit key `?` (line {})", n + 1));
        }
    }
    // A surrogate half in a `\u` escape: yaml-rust2 refuses it, the package keeps it.
    let bytes = source.as_bytes();
    for i in 0..bytes.len().saturating_sub(5) {
        if bytes[i] == b'\\'
            && bytes[i + 1] == b'u'
            && matches!(bytes[i + 2], b'd' | b'D')
            && matches!(bytes[i + 3], b'8'..=b'9' | b'a'..=b'f' | b'A'..=b'F')
        {
            return Some("a surrogate escape (`\\uD800`–`\\uDFFF`)".into());
        }
    }
    None
}

fn parse_events(source: &str) -> Result<JsValue, String> {
    let src: Vec<char> = source.chars().collect();
    let line_starts =
        std::iter::once(0).chain(src.iter().enumerate().filter(|(_, c)| **c == '\n').map(|(i, _)| i + 1)).collect();
    let mut builder = Builder { src, line_starts, ..Builder::default() };
    let mut parser = Parser::new_from_str(source);
    // Drive the parser one event at a time: its own `load` recurses once per nesting level,
    // and this loop stops at the first refusal instead of parsing the rest.
    loop {
        let (event, mark) = parser.next_token().map_err(|e| e.to_string())?;
        let end = event == Event::StreamEnd;
        builder.on_event(event, mark);
        if end || builder.error.is_some() {
            break;
        }
    }
    if let Some(e) = builder.error {
        return Err(e);
    }
    if builder.documents > 1 {
        return Err("Source contains multiple documents; the plugin reads one".into());
    }
    match builder.root {
        None => Ok(JsValue::Null),
        Some(node) => {
            let mut cx =
                Cx { anchors: &builder.anchors, budget: MAX_NODES, aliases: HashMap::new(), cache: HashMap::new() };
            to_js(&node, &mut cx, 0)
        }
    }
}

#[derive(Debug, Clone)]
enum Node {
    Scalar {
        value: String,
        style: TScalarStyle,
    },
    Seq(Vec<Node>),
    Map(Vec<(Node, Node)>),
    Alias(usize),
    /// A node carrying an anchor: converted once, and its value kept for the aliases to it.
    Anchored(usize, Box<Node>),
}

enum Open {
    Seq(usize, Vec<Node>),
    Map(usize, Vec<(Node, Node)>, Option<Node>),
}

#[derive(Default)]
struct Builder {
    stack: Vec<Open>,
    anchors: HashMap<usize, Node>,
    root: Option<Node>,
    documents: usize,
    error: Option<String>,
    /// The source as chars, to read a block scalar's header.
    src: Vec<char>,
    /// Where each line starts, in chars.
    line_starts: Vec<usize>,
}

impl Builder {
    fn push(&mut self, node: Node, anchor: usize) {
        let node = if anchor > 0 {
            self.anchors.insert(anchor, node.clone());
            Node::Anchored(anchor, Box::new(node))
        } else {
            node
        };
        match self.stack.last_mut() {
            None => self.root = Some(node),
            Some(Open::Seq(_, items)) => items.push(node),
            Some(Open::Map(_, pairs, key)) => match key.take() {
                None => *key = Some(node),
                Some(k) => pairs.push((k, node)),
            },
        }
    }

    /// A mark as a char index. yaml-rust2 advances `index` (and `col`, until the line ends) by
    /// bytes over a block scalar's content lines, so after non-ASCII content the index drifts;
    /// its line and column do not, so the position is the line's start plus the column.
    fn char_index(&self, mark: &Marker) -> usize {
        let line = mark.line().saturating_sub(1);
        self.line_starts.get(line).map_or(self.src.len(), |&start| (start + mark.col()).min(self.src.len()))
    }

    fn refuse(&mut self, reason: impl Into<String>) {
        self.error.get_or_insert_with(|| format!("{OUTSIDE}: {}", reason.into()));
    }

    /// The header of the block scalar whose content mark is `at`: what follows `|` or `>`.
    fn block_header(&self, at: usize) -> String {
        let src = &self.src;
        // An empty block scalar's mark sits on its `|`/`>` itself; one with content, past it.
        if matches!(src.get(at), Some('|' | '>')) {
            return src[at + 1..].iter().take_while(|&&c| c != '\n' && c != '#').collect::<String>().trim().to_string();
        }
        let mut i = at.min(src.len());
        while i > 0 && src[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !matches!(src[i - 1], '|' | '>' | '\n') {
            i -= 1;
        }
        src[i..].iter().take_while(|&&c| c != '\n' && c != '#').collect::<String>().trim().to_string()
    }
}

impl MarkedEventReceiver for Builder {
    fn on_event(&mut self, ev: Event, mark: Marker) {
        // Once the document is refused, build nothing more: the error is the result.
        if self.error.is_some() {
            return;
        }
        if matches!(ev, Event::SequenceStart(..) | Event::MappingStart(..)) && self.stack.len() >= MAX_DEPTH {
            self.error = Some(format!("collections nested deeper than {MAX_DEPTH} levels"));
            return;
        }
        let tag: Option<&Tag> = match &ev {
            Event::Scalar(_, _, _, t) | Event::SequenceStart(_, t) | Event::MappingStart(_, t) => t.as_ref(),
            _ => None,
        };
        if let Some(t) = tag {
            let shown = format!("a tag (`{}{}`)", t.handle, t.suffix);
            return self.refuse(shown);
        }
        if let Event::Scalar(value, style, ..) = &ev {
            match style {
                TScalarStyle::Literal | TScalarStyle::Folded => {
                    let header = self.block_header(self.char_index(&mark));
                    if !matches!(header.as_str(), "" | "-") {
                        return self
                            .refuse(format!("a block scalar header other than `|`, `|-`, `>`, `>-` (`{header}`)"));
                    }
                }
                TScalarStyle::Plain => {
                    if let Some(c) = value.chars().next()
                        && matches!(c, '|' | '>' | '@' | '`' | '%')
                    {
                        return self.refuse(format!("a plain scalar opening with `{c}`"));
                    }
                }
                _ => {}
            }
            // An implicit key is the scalar a mapping awaits as its key.
            let is_key = matches!(self.stack.last(), Some(Open::Map(_, _, None)));
            if is_key && value.chars().count() > MAX_IMPLICIT_KEY {
                return self.refuse(format!("an implicit key over {MAX_IMPLICIT_KEY} characters"));
            }
        }
        match ev {
            Event::DocumentStart => self.documents += 1,
            Event::Scalar(value, style, anchor, _) => {
                // A block scalar with no content line is "" under clip and strip chomping, the only
                // ones in the subset. yaml-rust2 gives "\n" for one that ends the document, so a
                // value of line breaks alone, which only an empty block scalar can have, is "".
                let empty_block =
                    matches!(style, TScalarStyle::Literal | TScalarStyle::Folded) && value.chars().all(|c| c == '\n');
                let value = if empty_block { String::new() } else { value };
                self.push(Node::Scalar { value, style }, anchor)
            }
            Event::Alias(id) => self.push(Node::Alias(id), 0),
            Event::SequenceStart(anchor, _) => self.stack.push(Open::Seq(anchor, Vec::new())),
            Event::MappingStart(anchor, _) => self.stack.push(Open::Map(anchor, Vec::new(), None)),
            Event::SequenceEnd => {
                if let Some(Open::Seq(anchor, items)) = self.stack.pop() {
                    self.push(Node::Seq(items), anchor);
                }
            }
            Event::MappingEnd => {
                if let Some(Open::Map(anchor, pairs, pending)) = self.stack.pop() {
                    if pending.is_some() {
                        self.error.get_or_insert_with(|| "a mapping ended with a key and no value".into());
                    }
                    self.push(Node::Map(pairs), anchor);
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

/// What one conversion carries: the anchors, the node budget, each anchor's alias counts, and
/// each anchored node's value, which an alias returns as the package's `Alias.resolve` does
/// (`data.res`), so the aliases inside it are never counted twice.
struct Cx<'a> {
    anchors: &'a HashMap<usize, Node>,
    budget: usize,
    aliases: HashMap<usize, AliasData>,
    cache: HashMap<usize, JsValue>,
}

/// A resolved scalar as JavaScript holds it.
#[derive(Debug, Clone, PartialEq)]
enum Js {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
}

impl Js {
    fn into_value(self) -> JsValue {
        match self {
            Js::Null => JsValue::Null,
            Js::Bool(b) => JsValue::Bool(b),
            Js::Num(x) => JsValue::Num(x),
            Js::Str(s) => JsValue::Str(s),
        }
    }
}

/// `depth` counts collections as resolved, aliases included, so a chain of anchors each wrapping
/// the last is held to [`MAX_DEPTH`] too.
fn to_js(node: &Node, cx: &mut Cx<'_>, depth: usize) -> Result<JsValue, String> {
    cx.budget = cx.budget.checked_sub(1).ok_or("the document expands past its limit")?;
    if depth > MAX_DEPTH {
        return Err(format!("collections nested deeper than {MAX_DEPTH} levels once aliases are resolved"));
    }
    match node {
        Node::Scalar { value, style } => Ok(scalar(value, *style).into_value()),
        Node::Anchored(id, inner) => {
            let value = to_js(inner, cx, depth)?;
            cx.cache.insert(*id, value.clone());
            Ok(value)
        }
        Node::Alias(id) => {
            let target = alias_target(*id, cx)?;
            match cx.cache.get(id) {
                Some(value) => Ok(value.clone()),
                // An alias inside its own anchor: not resolved yet, so it nests until the cap refuses it.
                None => to_js(target, cx, depth + 1),
            }
        }
        Node::Seq(items) => items.iter().map(|n| to_js(n, cx, depth + 1)).collect::<Result<_, _>>().map(JsValue::Arr),
        Node::Map(pairs) => {
            let mut out: BTreeMap<String, JsValue> = BTreeMap::new();
            let mut seen = std::collections::HashSet::new();
            for (k, v) in pairs {
                let key = key_scalar(k, cx)?;
                // `uniqueKeys` compares two scalar *nodes* by value; an alias key is a different node.
                if is_scalar_node(k)
                    && let Some(id) = key_identity(&key)
                    && !seen.insert(id)
                {
                    return Err("Map keys must be unique".into());
                }
                out.insert(key_string(&key), to_js(v, cx, depth + 1)?);
            }
            Ok(JsValue::Obj(out))
        }
    }
}

fn is_scalar_node(node: &Node) -> bool {
    match node {
        Node::Scalar { .. } => true,
        Node::Anchored(_, inner) => is_scalar_node(inner),
        _ => false,
    }
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
        Node::Anchored(_, inner) => alias_count(inner, cx),
        Node::Alias(id) => cx.aliases.get(id).map_or(0, |d| d.count.saturating_mul(d.alias_count)),
        Node::Seq(items) => items.iter().map(|n| alias_count(n, cx)).max().unwrap_or(0),
        Node::Map(pairs) => pairs.iter().map(|(k, v)| alias_count(k, cx).max(alias_count(v, cx))).max().unwrap_or(0),
    }
}

/// A map key as a resolved scalar. A collection key is outside the subset: the package writes
/// it back in flow form with each scalar's original quoting, which this reader does not keep.
fn key_scalar(node: &Node, cx: &mut Cx<'_>) -> Result<Js, String> {
    match node {
        Node::Scalar { value, style } => Ok(scalar(value, *style)),
        Node::Anchored(_, inner) => key_scalar(inner, cx),
        Node::Alias(id) => {
            let target = alias_target(*id, cx)?;
            key_scalar(target, cx)
        }
        Node::Seq(..) | Node::Map(..) => Err(format!("{OUTSIDE}: a collection used as a map key")),
    }
}

/// What `a.value === b.value` compares for a scalar key: type and value. Null equals null, and
/// numbers compare as doubles (0 and -0 equal, NaN never).
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
    })
}

/// The package's `stringifyKey`: null is "", anything else `String(value)`.
fn key_string(key: &Js) -> String {
    match key {
        Js::Null => String::new(),
        Js::Bool(b) => b.to_string(),
        Js::Num(x) => maestro_core::ask::js_string(*x),
        Js::Str(s) => s.clone(),
    }
}

fn scalar(value: &str, style: TScalarStyle) -> Js {
    if style == TScalarStyle::Plain { plain(value) } else { Js::Str(value.to_string()) }
}

/// A plain scalar under the core schema, in the schema's own test order.
fn plain(s: &str) -> Js {
    null_of(s)
        .or_else(|| bool_of(s))
        .or_else(|| int_of(s).map(Js::Num))
        .or_else(|| float_of(s).map(Js::Num))
        .unwrap_or_else(|| Js::Str(s.to_string()))
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

/// JSON text with each lone surrogate escape replaced by `\uFFFD`; valid pairs are kept.
/// `JSON.parse` keeps a lone half, serde_json refuses it: used to read `_events.jsonl` lines.
pub fn lone_surrogates_to_replacement(text: &str) -> String {
    let unit = |s: &str| -> Option<u32> {
        let hex = s.get(..4)?;
        if hex.bytes().all(|b| b.is_ascii_hexdigit()) { u32::from_str_radix(hex, 16).ok() } else { None }
    };
    let mut out = String::with_capacity(text.len());
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
        let pair_low = after.get(4..).and_then(|t| t.strip_prefix("\\u")).and_then(unit);
        match unit(after) {
            Some(0xD800..=0xDBFF) if pair_low.is_some_and(|lo| (0xDC00..=0xDFFF).contains(&lo)) => {
                out.push_str(&at[..12]);
                rest = &at[12..];
            }
            Some(0xD800..=0xDFFF) => {
                out.push_str("\\uFFFD");
                rest = &at[6..];
            }
            _ => {
                out.push_str("\\u");
                rest = &at[2..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The value as a caller sees it (JSON): what the tests compare.
    fn js(source: &str) -> serde_json::Value {
        parse(source).unwrap().value.to_json()
    }

    fn refused(source: &str) -> String {
        parse(source).unwrap_err()
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
    fn quoted_scalars_stay_strings() {
        assert_eq!(js("a: '1e3'\nb: \"true\"\nc: \"tab\\there\"\n"), json!({"a":"1e3","b":"true","c":"tab\there"}));
    }

    #[test]
    fn numbers_are_javascript_doubles() {
        let v = parse("u: 18446744073709551616\nd: 20261019123456789012\nx: 0x400000000000021\ni: .inf\nn: .nan\n")
            .unwrap()
            .value;
        assert_eq!(maestro_core::ask::text(v.get("d")).as_deref(), Some("20261019123456790000"));
        assert_eq!(maestro_core::ask::text(v.get("x")).as_deref(), Some("288230376151711800"), "parseInt rounds once");
        assert_eq!(maestro_core::ask::text(v.get("i")).as_deref(), Some("Infinity"));
        assert_eq!(maestro_core::ask::text(v.get("n")).as_deref(), Some("NaN"));
        assert_eq!(
            maestro_core::ask::canonical_string(&v),
            r#"{"d":20261019123456790000,"i":null,"n":null,"u":18446744073709552000,"x":288230376151711800}"#
        );
    }

    #[test]
    fn keys_stringify_as_to_js_does() {
        let v = js("0x10: hex\n1.0: float\n~: n\ntrue: b\n.inf: i\n-.inf: m\n.nan: x\n.NaN: y\n");
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["", "-Infinity", "1", "16", "Infinity", "NaN", "true"]);
        assert_eq!(v["NaN"], "y", "NaN keys never collide, so the later one wins");
        assert_eq!(js("1: number\n\"1\": string\n"), json!({"1": "string"}), "same JS key: last wins");
        for dup in ["a: 1\na: 2\n", "0x10: a\n16: b\n", "~: a\nnull: b\n", ".inf: a\n.Inf: b\n"] {
            assert!(refused(dup).contains("unique"), "{dup:?}");
        }
        assert_eq!(js("a: 1\nb: &k b\n*k : 3\n"), json!({"a": 1, "b": 3}), "an alias key is another node");
    }

    #[test]
    fn aliases_count_and_resolve_as_the_package_does() {
        let copies = |base: &str, n: usize| format!("base: &b {base}\ncopies:\n{}", "  - *b\n".repeat(n));
        assert!(parse(&copies("[x]", 99)).is_ok(), "count 100 × aliasCount 1 is not past 100");
        assert!(refused(&copies("[x]", 100)).contains("Excessive alias count"));
        assert!(refused(&copies("x", 100)).contains("Excessive alias count"));
        assert!(parse(&copies("[]", 150)).is_ok(), "an empty collection has aliasCount 0");
        // Weighted through nesting: 10 aliases to `a` inside `b` make each alias to `b` count for 11.
        let nested = format!("a: &a [x]\nb: &b [{}]\nc: [{}]\n", ["*a"; 10].join(", "), ["*b"; 11].join(", "));
        assert!(refused(&nested).contains("Excessive alias count"));
        // An alias returns the anchor's value: the aliases inside it are not counted again.
        let cached = format!("a: &a x\nb: &b [*a]\nc: [{}]\nd: [{}]\n", ["*b"; 49].join(", "), ["*a"; 50].join(", "));
        assert!(parse(&cached).is_ok(), "{:?}", parse(&cached).err());
        assert_eq!(
            js("b: &b {x: 1}\nr: *b\nm:\n  <<: *b\n"),
            json!({"b": {"x": 1}, "r": {"x": 1}, "m": {"<<": {"x": 1}}})
        );
        assert!(refused("v: &a [*a]\n").contains("nested deeper"), "a self-alias is refused, not looped");
    }

    #[test]
    fn the_subset_refuses_what_it_cannot_read_exactly() {
        for (doc, why) in [
            ("a:\tb\n", "tab"),
            ("a: x\ry\n", "carriage return"),
            ("a: x\u{0}\n", "control character"),
            ("a: x\u{85}y\n", "NEL"),
            ("a: \u{feff}x\n", "byte-order mark"),
            ("a: 1", "newline"),
            ("%YAML 1.2\n---\na: 1\n", "directive"),
            ("a: 1\n...\n", "end marker"),
            ("a: 1\n---\nb: 2\n", "`---` past the first line"),
            ("a: 1\n  \nb: 2\n", "whitespace-only"),
            ("? k\n: v\n", "explicit key"),
            ("- ? k\n  : v\n", "explicit key"),
            ("a: \"\\uD83D\"\n", "surrogate"),
            ("a: !!int 1\n", "tag"),
            ("a: !custom x\n", "tag"),
            ("a: |+\n  x\n", "block scalar header"),
            ("a: |2\n   x\n", "block scalar header"),
            ("{[a, b]: v}\n", "collection used as a map key"),
        ] {
            let e = refused(doc);
            assert!(e.contains(OUTSIDE) && e.contains(why), "{doc:?}: {e}");
        }
        // An implicit key over the spec's 1024 chars: yaml-rust2 refuses it too, before this reader's check.
        assert!(parse(&format!("{}: v\n", "k".repeat(1025))).is_err());
        assert!(parse(&format!("{}: v\n", "k".repeat(1024))).is_ok(), "1024 chars is still an implicit key");
        assert!(parse("a: @x\n").is_err(), "a reserved indicator: yaml-rust2 refuses it in its own words");
    }

    #[test]
    fn the_subset_keeps_what_ledgers_hold() {
        let doc = "\u{feff}---\nasks:\n  - id: a\n    ask: \"Pick one? ¿sí? 日本語\"\n    gates: [x, y]\n    note: >-\n      folded\n      text\n    body: |\n      keep\n\n      lines\n    q: 'it''s'\n    long: a plain\n      continued line\n    origin: &o {agent_id: z}\n  - id: b\n    origin: *o\n";
        let v = js(doc);
        assert_eq!(v["asks"][0]["note"], "folded text");
        assert_eq!(v["asks"][0]["body"], "keep\n\nlines\n");
        assert_eq!(v["asks"][0]["long"], "a plain continued line");
        assert_eq!(v["asks"][1]["origin"], json!({"agent_id": "z"}));
        assert_eq!(js("a: 1\r\nb: 2\r\n"), json!({"a": 1, "b": 2}), "CRLF is a line break");
        assert_eq!(js(""), json!(null));
        // An empty block scalar reads as "", at the end of the document too.
        assert_eq!(js("a: |\n"), json!({"a": ""}));
        assert_eq!(js("x:\n  - q: >\n\n\n"), json!({"x": [{"q": ""}]}));
        assert_eq!(js("x:\n  y:\n    z: |-\n\n"), json!({"x": {"y": {"z": ""}}}));
        // yaml-rust2's index drifts by bytes after non-ASCII block content: headers are still found.
        let drift = "a: |\n  ¿sí? 日本語 ñññ\nb: >-\n  Round 2\n  text\nc: |\n  ok\n";
        assert_eq!(js(drift), json!({"a": "¿sí? 日本語 ñññ\n", "b": "Round 2 text", "c": "ok\n"}));
    }

    #[test]
    fn deep_nesting_is_refused_not_a_stack_overflow() {
        let block = |n: usize| format!("x:\n  {}v\n", "- ".repeat(n));
        assert!(parse(&block(MAX_DEPTH - 1)).is_ok(), "at the cap it reads");
        assert!(refused(&block(50_000)).contains("deeper than"));
        assert!(parse(&format!("x: {}{}\n", "[".repeat(300), "]".repeat(300))).is_err());
        // An alias returns its anchor's value, as the package does, so nesting it deeper reads.
        let deep_alias =
            format!("a: &a {}v{}\nb: {}*a{}\n", "[".repeat(200), "]".repeat(200), "[".repeat(100), "]".repeat(100));
        assert!(parse(&deep_alias).is_ok());
    }

    #[test]
    fn lone_surrogates_in_json_lines_become_replacement_chars() {
        assert_eq!(
            lone_surrogates_to_replacement(r#"{"a":"x \ud83d y","b":"\ud83d\ude00","c":"\\ud83d"}"#),
            r#"{"a":"x \uFFFD y","b":"\ud83d\ude00","c":"\\ud83d"}"#
        );
    }
}
