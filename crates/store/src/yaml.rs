//! Ask ledgers read as the Paseo plugin reads them, within a defined subset of YAML.
//!
//! The plugin parses ledgers with the `yaml` package (2.x):
//! `parseDocument(source).toJS()` under the YAML 1.2 core schema, and an entry
//! hash covers that JavaScript value. This module reads ledgers with yaml-rust2
//! and resolves every scalar itself, porting the package's rules, so the value,
//! and so every hash and state, is the plugin's.
//!
//! Exact parity holds inside a **subset**, the YAML that ledger writers produce
//! (`ask_ledger.py` and its `json.dumps` strings, PyYAML, the plugin itself). Outside
//! it the two parsers disagree in ways no rule over yaml-rust2 can close, so a
//! document outside the subset is **refused**, with the reason named, never
//! misread. The owner chose this on 2026-10-20 (BRO-3207); a Rust port of the
//! package's parser (BRO-3224) is the path to parity outside it.
//!
//! **Inside the subset** (exact):
//! - block mappings and sequences, flow `[...]`/`{...}`, comments;
//! - plain, single- and double-quoted scalars, single- and multi-line, with every
//!   continuation line indented past its parent collection;
//! - double-quoted escapes, `\uD83D\uDE00` surrogate pairs included;
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
//! **Outside the subset** (refused, the reason naming the feature and line):
//! - a tab, a lone carriage return, a NUL or other C0 control character, a
//!   byte-order mark past the start outside a double-quoted scalar (inside
//!   one, as `JSON.stringify` writes it, it reads), invalid UTF-8;
//! - `%` directives, `...`, a `---` past the first line, more than one document;
//! - tags (`!!int`, `!custom`, …), explicit keys (`? k`) and empty keys (`: v`);
//! - block scalars with `+` chomping or an indentation digit;
//! - whitespace-only lines, and a non-empty document not ending in a newline;
//! - a lone surrogate escape in a double-quoted scalar, and an escaped line break
//!   followed by an empty line;
//! - a continuation line indented at or before its parent collection, except
//!   a flow collection's `]` or `}` alone at the parent's column;
//! - a collection as a map key; an implicit key whose `:` is over 1024 UTF-16
//!   units past its start (its anchor, if any), as the package measures it for
//!   block keys and flow sequence pairs (refused in flow mappings too, which the
//!   package reads); a flow sequence pair whose `:` is not on its key's line; and
//!   a plain scalar opening with `|`, `>`, `@`, `` ` `` or `%`;
//! - nesting deeper than [`MAX_DEPTH`], and more than [`MAX_NODES`] values;
//! - anything yaml-rust2 itself cannot parse.

use maestro_core::js::JsValue;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser, Tag};
use yaml_rust2::scanner::{Marker, TScalarStyle};

/// The package's default `maxAliasCount`: an alias whose `count × aliasCount` passes it refuses the document.
const MAX_ALIAS_COUNT: u64 = 100;
/// Values one document may hold, every alias copy counted: far above any ledger, it bounds memory.
pub const MAX_NODES: usize = 500_000;
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
    let (text, marks) = carry_surrogate_escapes(source)?;
    let value = parse_events(&text, marks)?;
    Ok(Read { value, warnings: Vec::new() })
}

/// The first lexical feature of `source` outside the subset, if any, with its line.
fn outside_subset(source: &str) -> Option<String> {
    let mut line = 1;
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        let bad = match c {
            '\n' => {
                line += 1;
                None
            }
            '\t' => Some("a tab character"),
            '\r' if chars.peek() != Some(&'\n') => Some("a carriage return outside a CRLF line ending"),
            c if (c as u32) < 0x20 && c != '\r' => Some("a NUL or other control character"),
            _ => None,
        };
        if let Some(bad) = bad {
            return Some(format!("{bad} (line {line})"));
        }
    }
    if !source.is_empty() && !source.ends_with('\n') {
        return Some("a document that does not end with a newline".into());
    }
    for (n, raw) in source.split('\n').enumerate() {
        let l = raw.strip_suffix('\r').unwrap_or(raw);
        if l.starts_with('%') {
            return Some(format!("a directive (line {})", n + 1));
        }
        if l == "..." || l.starts_with("... ") {
            return Some(format!("a document end marker `...` (line {})", n + 1));
        }
        if (l == "---" || l.starts_with("--- ")) && n > 0 {
            return Some(format!("a `---` past the first line (line {})", n + 1));
        }
        if !l.is_empty() && l.trim_start_matches(' ').is_empty() {
            return Some(format!("a whitespace-only line (line {})", n + 1));
        }
    }
    None
}

/// A surrogate escape, carried through the parser as one private-use char: what it decodes to
/// in a double-quoted scalar (`None` for a lone half), and its text, which any other scalar keeps.
struct Carried {
    decoded: Option<char>,
    text: String,
}

/// yaml-rust2 refuses a `\uD800`–`\uDFFF` escape, which the package reads: a pair as its code
/// point, a lone half kept as is. Each such escape is replaced by a private-use char the document
/// does not hold, one per escape, and put back once the scalar it landed in is known (see
/// [`Builder::restore`]). Only an escape that begins at an odd run of backslashes is one.
fn carry_surrogate_escapes(source: &str) -> Result<(String, HashMap<char, Carried>), String> {
    if !source.contains("\\u") && !source.contains("\\U") {
        return Ok((source.to_string(), HashMap::new()));
    }
    let held: std::collections::HashSet<char> = source.chars().collect();
    let mut free = ('\u{F0000}'..='\u{FFFFD}').chain('\u{E000}'..='\u{F8FF}').filter(|c| !held.contains(c));
    let chars: Vec<char> = source.chars().collect();
    let hex4 = |at: usize| -> Option<u32> {
        let s: String = chars.get(at..at + 4)?.iter().collect();
        if s.bytes().all(|b| b.is_ascii_hexdigit()) { u32::from_str_radix(&s, 16).ok() } else { None }
    };
    let mut out = String::with_capacity(source.len());
    let mut marks = HashMap::new();
    // One marker per distinct escape text, so a repeated escape never exhausts the pool.
    let mut by_text: HashMap<String, char> = HashMap::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '\\' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let run = chars[i..].iter().take_while(|&&c| c == '\\').count();
        // All but the last backslash pair up; the last one may open an escape.
        out.extend(std::iter::repeat_n('\\', run - 1));
        i += run - 1;
        let unit = if run % 2 == 1 && chars.get(i + 1) == Some(&'u') { hex4(i + 2) } else { None };
        let Some(hi @ 0xD800..=0xDFFF) = unit else {
            out.push('\\');
            i += 1;
            continue;
        };
        let lo = (chars.get(i + 6) == Some(&'\\') && chars.get(i + 7) == Some(&'u')).then(|| hex4(i + 8)).flatten();
        let (decoded, len) = match (hi, lo) {
            (0xD800..=0xDBFF, Some(lo @ 0xDC00..=0xDFFF)) => {
                (char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)), 12)
            }
            _ => (None, 6),
        };
        let text: String = chars[i..i + len].iter().collect();
        let mark = match by_text.get(&text) {
            Some(&m) => m,
            None => {
                let m = free
                    .next()
                    .ok_or_else(|| format!("{OUTSIDE}: more distinct surrogate escapes than can be carried"))?;
                by_text.insert(text.clone(), m);
                marks.insert(m, Carried { decoded, text });
                m
            }
        };
        out.push(mark);
        i += len;
    }
    Ok((out, marks))
}

fn parse_events(source: &str, marks: HashMap<char, Carried>) -> Result<JsValue, String> {
    let builder = build(source, marks)?;
    match &builder.root {
        None => Ok(JsValue::Null),
        Some(node) => resolve(&builder, node).0,
    }
}

/// The document's nodes, as the parser's events build them.
fn build(source: &str, marks: HashMap<char, Carried>) -> Result<Builder, String> {
    let src: Vec<char> = source.chars().collect();
    let line_starts =
        std::iter::once(0).chain(src.iter().enumerate().filter(|(_, c)| **c == '\n').map(|(i, _)| i + 1)).collect();
    let mut builder = Builder { src, line_starts, marks, ..Builder::default() };
    let mut parser = Parser::new_from_str(source);
    // Drive the parser one event at a time: its own `load` recurses once per nesting level,
    // and this loop stops at the first refusal instead of parsing the rest.
    loop {
        let (event, mark) =
            parser.next_token().map_err(|e| format!("{OUTSIDE}: what this reader's YAML parser refuses ({e})"))?;
        let end = event == Event::StreamEnd;
        builder.on_event(event, mark);
        if end || builder.error.is_some() {
            break;
        }
    }
    if let Some(e) = builder.error.take() {
        return Err(e);
    }
    // A byte-order mark past the start is read alike only inside a double-quoted scalar, where
    // `JSON.stringify` (the plugin's writer) leaves it literal. Anywhere else it is refused.
    let mut spans = builder.dq_spans.iter().peekable();
    for (i, &c) in builder.src.iter().enumerate() {
        if c != '\u{feff}' {
            continue;
        }
        while spans.peek().is_some_and(|&&(_, end)| end <= i) {
            spans.next();
        }
        if !spans.peek().is_some_and(|&&(start, end)| start < i && i < end) {
            let n = builder.line_of(i) + 1;
            return Err(format!(
                "{OUTSIDE}: a byte-order mark past the start, outside a double-quoted scalar (line {n})"
            ));
        }
    }
    if builder.documents > 1 {
        return Err("Source contains multiple documents; the plugin reads one".into());
    }
    Ok(builder)
}

/// The document's value, and how many values the alias cache held to build it.
fn resolve(builder: &Builder, node: &Node) -> (Result<JsValue, String>, usize) {
    let mut cx = Cx {
        anchors: &builder.anchors,
        aliased: &builder.aliased,
        budget: MAX_NODES,
        aliases: HashMap::new(),
        cache: HashMap::new(),
    };
    let value = to_js(node, &mut cx, 0);
    (value, cx.cache.values().map(|(_, n)| n).sum())
}

#[derive(Debug)]
enum Node {
    Scalar {
        value: String,
        style: TScalarStyle,
    },
    Seq(Vec<Node>),
    Map(Vec<(Node, Node)>),
    Alias(usize),
    /// A node carrying an anchor, shared with the anchors table (never copied): converted once,
    /// and its value kept for the aliases to it.
    Anchored(usize, Rc<Node>),
}

enum Open {
    Seq(usize, Vec<Node>),
    Map(usize, Vec<(Node, Node)>, Option<Node>),
}

/// An open collection's place in the source.
struct Frame {
    flow: bool,
    /// The column its entries begin at: a block sequence's `-`, a block mapping's keys.
    entries_at: Option<usize>,
    /// The line it opens on (0-based).
    line: usize,
    /// A single-pair mapping inside a flow sequence, whose key must share a line with its `:`.
    pair: bool,
}

#[derive(Default)]
struct Builder {
    stack: Vec<Open>,
    frames: Vec<Frame>,
    anchors: HashMap<usize, Rc<Node>>,
    /// The anchors some alias names: only these are cached when resolved.
    aliased: HashSet<usize>,
    root: Option<Node>,
    documents: usize,
    error: Option<String>,
    /// The source as chars, and where each line starts.
    src: Vec<char>,
    line_starts: Vec<usize>,
    /// The surrogate escapes carried through the parser.
    marks: HashMap<char, Carried>,
    /// Where each double-quoted scalar sits in `src`, quotes included, in document order.
    dq_spans: Vec<(usize, usize)>,
}

impl Builder {
    fn push(&mut self, node: Node, anchor: usize) {
        let node = if anchor > 0 {
            let shared = Rc::new(node);
            self.anchors.insert(anchor, shared.clone());
            Node::Anchored(anchor, shared)
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

    fn refuse(&mut self, reason: impl Into<String>) {
        self.error.get_or_insert_with(|| format!("{OUTSIDE}: {}", reason.into()));
    }

    /// A mark as a char index. yaml-rust2 advances `index` (and `col`, until the line ends) by
    /// bytes over a block scalar's content lines, so after non-ASCII content the index drifts;
    /// its line and column do not, so the position is the line's start plus the column.
    fn char_index(&self, mark: &Marker) -> usize {
        let line = mark.line().saturating_sub(1);
        self.line_starts.get(line).map_or(self.src.len(), |&start| (start + mark.col()).min(self.src.len()))
    }

    fn line_of(&self, at: usize) -> usize {
        self.line_starts.partition_point(|&s| s <= at).saturating_sub(1)
    }

    /// Line `n` without its line break.
    fn line(&self, n: usize) -> &[char] {
        let start = self.line_starts[n];
        let end = self.line_starts.get(n + 1).map_or(self.src.len(), |&e| e - 1);
        let line = &self.src[start..end.max(start)];
        line.strip_suffix(&['\r']).unwrap_or(line)
    }

    fn indent(line: &[char]) -> usize {
        line.iter().take_while(|&&c| c == ' ').count()
    }

    /// The column the nearest enclosing block collection's entries begin at: every continuation
    /// line of a quoted scalar or flow collection inside it must be indented past it.
    fn parent_column(&self) -> Option<usize> {
        self.frames.iter().rev().find(|f| !f.flow).and_then(|f| f.entries_at)
    }

    /// Line `n` holds a closing `]` or `}` alone (a comment may follow). Its column needs no
    /// check here: past the parent's the continuation check passes anyway, and below it
    /// yaml-rust2 refuses the indentation itself.
    fn closes_alone(&self, n: usize) -> bool {
        let text: String = self.line(n).iter().collect();
        let code = match text.find(" #") {
            Some(i) => &text[..i],
            None => &text,
        };
        matches!(code.trim(), "]" | "}")
    }

    /// Refuse when a line in `from+1 ..= to` holding text is indented at or before `parent`.
    fn check_continuation(&mut self, from: usize, to: usize, what: &str) {
        let Some(parent) = self.parent_column() else { return };
        for n in from + 1..=to.min(self.line_starts.len().saturating_sub(1)) {
            let line = self.line(n);
            if !line.is_empty() && Self::indent(line) <= parent {
                return self
                    .refuse(format!("a {what} continuation line indented at or before its parent (line {})", n + 1));
            }
        }
    }

    /// The header of the block scalar whose content mark is `at`: the token after `|` or `>`.
    /// It sits on the mark's own line for an empty scalar, else on the last line with text
    /// before the content; a comment after it is not part of it.
    fn block_header(&self, at: usize) -> String {
        let mut n = self.line_of(at);
        if !matches!(self.src.get(at), Some('|' | '>')) {
            n = n.saturating_sub(1);
            while n > 0 && self.line(n).is_empty() {
                n -= 1;
            }
        }
        let line: String = self.line(n).iter().collect();
        let code = match line.find(" #") {
            Some(i) => &line[..i],
            None => &line,
        };
        let token = code.split_whitespace().last().unwrap_or("");
        token.trim_start_matches(['|', '>']).to_string()
    }

    /// The char index just past the closing quote of the quoted scalar opening at `at`.
    fn quoted_end(&self, at: usize, quote: char) -> usize {
        let src = &self.src;
        let mut i = at + 1;
        while i < src.len() {
            match src[i] {
                '\\' if quote == '"' => i += 2,
                c if c == quote && quote == '\'' && src.get(i + 1) == Some(&'\'') => i += 2,
                c if c == quote => return i + 1,
                _ => i += 1,
            }
        }
        src.len()
    }

    /// Carried surrogate escapes back into a scalar: decoded in a double-quoted one, else the text.
    fn restore(&mut self, value: String, style: TScalarStyle, at: usize) -> String {
        if self.marks.is_empty() || !value.chars().any(|c| self.marks.contains_key(&c)) {
            return value;
        }
        let mut out = String::with_capacity(value.len());
        for c in value.chars() {
            match self.marks.get(&c) {
                Some(m) if style == TScalarStyle::DoubleQuoted => match m.decoded {
                    Some(d) => out.push(d),
                    None => {
                        // The scalar starts at `at`; its first use of this marker is the escape's line.
                        let pos = self.src.get(at..).and_then(|rest| rest.iter().position(|&x| x == c));
                        let n = self.line_of(pos.map_or(at, |p| at + p)) + 1;
                        self.refuse(format!(
                            "a lone surrogate escape in a double-quoted scalar (`{}`, line {n})",
                            m.text
                        ));
                        return value;
                    }
                },
                Some(m) => out.push_str(&m.text),
                None => out.push(c),
            }
        }
        out
    }

    /// Whether the next node completes a mapping's key, and so is a key.
    fn awaits_key(&self) -> bool {
        matches!(self.stack.last(), Some(Open::Map(_, _, None)))
    }

    /// Checks on a node about to be read as a mapping key at char `at`.
    fn check_key(&mut self, at: usize, value: Option<(&str, TScalarStyle)>) {
        let n = self.line_of(at);
        // Look back from the key over spaces only: copying the line up to every key is quadratic
        // when many keys share one line.
        let line_start = self.line_starts[n];
        let mut j = at.min(self.src.len());
        while j > line_start && self.src[j - 1] == ' ' {
            j -= 1;
        }
        // `? k`: the `?` opens a token (line start, after `- `, or in flow after `[`, `{` or `,`).
        if j > line_start
            && self.src[j - 1] == '?'
            && (j - 1 == line_start || matches!(self.src[j - 2], ' ' | '-' | '[' | '{' | ','))
        {
            return self.refuse(format!("an explicit key `?` (line {})", n + 1));
        }
        if let Some((_, style)) = value {
            // yaml-rust2 gives an empty key as a plain `~` placed on the `:` itself; a real `~` key sits on its `~`.
            if style == TScalarStyle::Plain && self.src.get(at) == Some(&':') {
                return self.refuse(format!("an empty key (line {})", n + 1));
            }
        }
        let span = self.key_span(at, value);
        // A flow sequence pair's key is on one line with its `:`: a quoted key's closing quote
        // too. (An anchor on an earlier line is the package's to allow, and it does.)
        if self.frames.last().is_some_and(|f| f.pair) {
            let one_line = span.is_some_and(|(start, colon)| self.line_of(start) == self.line_of(colon));
            if !one_line {
                return self.refuse(format!(
                    "a flow sequence pair whose key is not on one line with its `:` (line {})",
                    n + 1
                ));
            }
        }
        if let Some(units) = span.map(|(start, colon)| self.units(start, colon))
            && units > MAX_IMPLICIT_KEY
        {
            self.refuse(format!(
                "an implicit key whose `:` is over {MAX_IMPLICIT_KEY} UTF-16 units past its start (line {})",
                n + 1
            ));
        }
    }

    /// `&a{…}` or `&a[…]`: the package wants white space after an anchor; yaml-rust2 ends the
    /// name at the bracket and reads on.
    fn glued_anchor(&self, at: usize) -> bool {
        let src = &self.src;
        match src.get(at) {
            Some('[' | '{') => at > 0 && !src[at - 1].is_whitespace(),
            Some('&') => {
                let mut i = at + 1;
                while src.get(i).is_some_and(|c| !c.is_whitespace() && !matches!(c, ',' | '[' | ']' | '{' | '}')) {
                    i += 1;
                }
                matches!(src.get(i), Some('[' | '{'))
            }
            _ => false,
        }
    }

    /// The source from `start` to `colon` in UTF-16 units: a carried escape counts as its text.
    fn units(&self, start: usize, colon: usize) -> usize {
        self.src[start..colon].iter().map(|c| self.marks.get(c).map_or(c.len_utf16(), |m| m.text.len())).sum()
    }

    /// What the package measures for `KEY_OVER_1024_CHARS`: the key's start (its anchor, if one
    /// is on the key's line) and its `:` indicator. `None` when no `:` follows the key on the
    /// line it ends on, as in `{a, b}`.
    fn key_span(&self, at: usize, value: Option<(&str, TScalarStyle)>) -> Option<(usize, usize)> {
        let src = &self.src;
        let token = |c: char| !c.is_whitespace() && !matches!(c, ',' | '[' | ']' | '{' | '}');
        let skip_spaces = |mut i: usize| {
            while src.get(i) == Some(&' ') {
                i += 1;
            }
            i
        };
        let (mut start, mut i) = (at, at);
        if src.get(i) == Some(&'&') {
            // The mark sits on the anchor: the key itself starts after it.
            while src.get(i).is_some_and(|&c| token(c)) {
                i += 1;
            }
            i = skip_spaces(i);
        } else {
            // Back over spaces on the key's line only. An anchor on an earlier line counts for the
            // package only in a flow sequence pair, which yaml-rust2 refuses itself.
            let mut j = at;
            while j > 0 && src[j - 1] == ' ' {
                j -= 1;
            }
            let mut k = j;
            while k > 0 && token(src[k - 1]) {
                k -= 1;
            }
            if k < j && src[k] == '&' {
                start = k;
            }
        }
        let end = match (src.get(i), value) {
            (Some(&q), Some((_, TScalarStyle::DoubleQuoted | TScalarStyle::SingleQuoted))) => self.quoted_end(i, q),
            (_, Some((text, _))) => i + text.chars().count(),
            (Some('*'), None) => {
                let mut e = i + 1;
                while src.get(e).is_some_and(|&c| token(c)) {
                    e += 1;
                }
                e
            }
            _ => return None,
        };
        let colon = skip_spaces(end);
        if src.get(colon) != Some(&':') {
            return None;
        }
        Some((start, colon))
    }
}

impl MarkedEventReceiver for Builder {
    fn on_event(&mut self, ev: Event, mark: Marker) {
        // Once the document is refused, build nothing more: the error is the result.
        if self.error.is_some() {
            return;
        }
        let at = self.char_index(&mark);
        let line = self.line_of(at);
        if matches!(ev, Event::SequenceStart(..) | Event::MappingStart(..)) && self.stack.len() >= MAX_DEPTH {
            return self.refuse(format!("collections nested deeper than {MAX_DEPTH} levels"));
        }
        let tag: Option<&Tag> = match &ev {
            Event::Scalar(_, _, _, t) | Event::SequenceStart(_, t) | Event::MappingStart(_, t) => t.as_ref(),
            _ => None,
        };
        if let Some(t) = tag {
            let written = if t.handle == "tag:yaml.org,2002:" {
                format!("!!{}", t.suffix)
            } else {
                format!("{}{}", t.handle, t.suffix)
            };
            return self.refuse(format!("a tag (`{written}`, line {})", line + 1));
        }
        if self.awaits_key() {
            match &ev {
                Event::Scalar(value, style, ..) => self.check_key(at, Some((value, *style))),
                Event::Alias(_) | Event::SequenceStart(..) | Event::MappingStart(..) => self.check_key(at, None),
                _ => {}
            }
            // A block mapping's entries begin where its first key does.
            if let Some(frame) = self.frames.last_mut()
                && !frame.flow
                && frame.entries_at.is_none()
            {
                frame.entries_at = Some(mark.col());
            }
            if self.error.is_some() {
                return;
            }
        }
        if let Event::Scalar(value, style, ..) = &ev {
            match style {
                TScalarStyle::Literal | TScalarStyle::Folded => {
                    let header = self.block_header(at);
                    if !matches!(header.as_str(), "" | "-") {
                        return self.refuse(format!(
                            "a block scalar header other than `|`, `|-`, `>`, `>-` (`{}{header}`, line {})",
                            if *style == TScalarStyle::Literal { '|' } else { '>' },
                            line + 1
                        ));
                    }
                }
                TScalarStyle::Plain => {
                    if let Some(c) = value.chars().next()
                        && matches!(c, '|' | '>' | '@' | '`' | '%')
                    {
                        return self.refuse(format!("a plain scalar opening with `{c}` (line {})", line + 1));
                    }
                }
                TScalarStyle::SingleQuoted | TScalarStyle::DoubleQuoted => {
                    let quote = if *style == TScalarStyle::DoubleQuoted { '"' } else { '\'' };
                    if self.src.get(at) == Some(&quote) {
                        let end = self.quoted_end(at, quote);
                        if quote == '"' {
                            self.dq_spans.push((at, end));
                        }
                        let last = self.line_of(end.saturating_sub(1));
                        // An escaped line break before an empty line: yaml-rust2 folds it otherwise.
                        if quote == '"' {
                            for n in line..last {
                                let l = self.line(n);
                                let escaped = l.iter().rev().take_while(|&&c| c == '\\').count() % 2 == 1;
                                if escaped && self.line(n + 1).is_empty() {
                                    return self.refuse(format!(
                                        "an escaped line break followed by an empty line (line {})",
                                        n + 1
                                    ));
                                }
                            }
                        }
                        self.check_continuation(line, last, "quoted scalar");
                        if self.error.is_some() {
                            return;
                        }
                    }
                }
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
                let value = if empty_block { String::new() } else { self.restore(value, style, at) };
                self.push(Node::Scalar { value, style }, anchor)
            }
            Event::Alias(id) => {
                self.aliased.insert(id);
                self.push(Node::Alias(id), 0)
            }
            Event::SequenceStart(anchor, _) => {
                if anchor > 0 && self.glued_anchor(at) {
                    return self.refuse(format!("an anchor glued to a flow collection (line {})", line + 1));
                }
                // An indentless sequence (as PyYAML writes them) is marked on its first item, past
                // its own `-`, and that item may itself open with `[` or `-`. A `-` before the mark,
                // outside any block sequence, is therefore this sequence's own: its entries begin
                // there. Inside a block sequence the `-` is the parent's entry, and the mark is ours.
                let mut r = at.min(self.src.len());
                while r > 0 && self.src[r - 1] == ' ' {
                    r -= 1;
                }
                let own_dash = (r > 0 && self.src[r - 1] == '-').then(|| r - 1);
                let in_block_seq =
                    matches!(self.stack.last(), Some(Open::Seq(..))) && self.frames.last().is_some_and(|f| !f.flow);
                let in_flow = self.frames.last().is_some_and(|f| f.flow);
                let (flow, dash) = match own_dash {
                    Some(d) if !in_block_seq && !in_flow => (false, d),
                    _ => (matches!(self.src.get(at), Some('[')), at),
                };
                let entries_at = (!flow).then(|| dash - self.line_starts[self.line_of(dash)]);
                self.frames.push(Frame { flow, entries_at, line, pair: false });
                self.stack.push(Open::Seq(anchor, Vec::new()))
            }
            Event::MappingStart(anchor, _) => {
                if anchor > 0 && self.glued_anchor(at) {
                    return self.refuse(format!("an anchor glued to a flow collection (line {})", line + 1));
                }
                let flow = matches!(self.src.get(at), Some('{'));
                // A single pair in a flow sequence (`[k: v]`) opens a mapping with no `{`.
                let pair = !flow
                    && matches!(self.stack.last(), Some(Open::Seq(..)))
                    && self.frames.last().is_some_and(|f| f.flow);
                self.frames.push(Frame { flow, entries_at: None, line, pair });
                self.stack.push(Open::Map(anchor, Vec::new(), None))
            }
            Event::SequenceEnd | Event::MappingEnd => {
                if let Some(frame) = self.frames.pop()
                    && frame.flow
                {
                    // The package lets a flow collection close on a line of its own at the parent's
                    // column, `]` or `}` alone (JSON style); any other line must be past it. An inner
                    // collection's closing line is still checked by the collection around it.
                    let last = if line > frame.line && self.closes_alone(line) { line - 1 } else { line };
                    self.check_continuation(frame.line, last, "flow collection");
                }
                match self.stack.pop() {
                    Some(Open::Seq(anchor, items)) => self.push(Node::Seq(items), anchor),
                    Some(Open::Map(anchor, pairs, pending)) => {
                        if pending.is_some() {
                            self.error.get_or_insert_with(|| "a mapping ended with a key and no value".into());
                        }
                        self.push(Node::Map(pairs), anchor)
                    }
                    None => {}
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

/// What one conversion carries: the anchors, the value budget, each anchor's alias counts, and
/// each anchored node's value with its size. An alias returns that value, as the package's
/// `Alias.resolve` returns `data.res`, so the aliases inside it are never counted twice. Every
/// node spends one as it is built, and every alias copy is charged by its size, so memory stays
/// bounded however aliases nest. Only an anchor some alias names is cached, and its cached copy
/// is charged too: nested anchors would otherwise hold depth × size in copies. An anchor no alias
/// names costs only what its value costs.
struct Cx<'a> {
    anchors: &'a HashMap<usize, Rc<Node>>,
    aliased: &'a HashSet<usize>,
    budget: usize,
    aliases: HashMap<usize, AliasData>,
    cache: HashMap<usize, (JsValue, usize)>,
}

impl Cx<'_> {
    fn spend(&mut self, n: usize) -> Result<(), String> {
        self.budget = self
            .budget
            .checked_sub(n)
            .ok_or_else(|| format!("{OUTSIDE}: a document that expands past {MAX_NODES} values"))?;
        Ok(())
    }
}

fn size(v: &JsValue) -> usize {
    1 + match v {
        JsValue::Arr(items) => items.iter().map(size).sum(),
        JsValue::Obj(map) => map.values().map(size).sum(),
        _ => 0,
    }
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
    cx.spend(1)?;
    if depth > MAX_DEPTH {
        return Err(format!("{OUTSIDE}: collections nested deeper than {MAX_DEPTH} levels once aliases are resolved"));
    }
    match node {
        Node::Scalar { value, style } => Ok(scalar(value, *style).into_value()),
        Node::Anchored(id, inner) => {
            let value = to_js(inner, cx, depth)?;
            if cx.aliased.contains(id) {
                let n = size(&value);
                cx.spend(n)?;
                cx.cache.insert(*id, (value.clone(), n));
            }
            Ok(value)
        }
        Node::Alias(id) => {
            let target = alias_target(*id, cx)?;
            match cx.cache.get(id).map(|(_, n)| *n) {
                Some(n) => {
                    cx.spend(n)?;
                    Ok(cx.cache[id].0.clone())
                }
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
    let target: &Node = cx.anchors.get(&id).ok_or("an alias names no anchor")?;
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

    /// How many values the alias cache held to read `source`.
    fn cached(source: &str) -> usize {
        let builder = build(source, HashMap::new()).unwrap();
        let (value, held) = resolve(&builder, builder.root.as_ref().unwrap());
        value.unwrap();
        held
    }

    #[test]
    fn only_an_aliased_anchor_is_cached() {
        // Nested anchors, none aliased: no copy is held, so memory is not depth × size.
        let mut doc = format!("[{}]", vec!["x"; 1000].join(","));
        for i in 0..50 {
            doc = format!("&n{i} [{doc}]");
        }
        assert_eq!(cached(&format!("big: {doc}\n")), 0);
        // An aliased anchor is held once, at its size, and that copy is charged to the budget.
        assert_eq!(cached("a: &a [x, y]\nb: *a\nc: *a\n"), 3);
        // 2/5 of the budget: value and alias copy alone fit (4/5); with the cached copy they do not.
        let n = MAX_NODES * 2 / 5;
        let held = format!("big: &a [{}]\nuse: *a\n", vec!["0"; n].join(","));
        assert!(refused(&held).contains("expands past"), "value, cached copy and alias copy: 6/5 of the budget");
    }

    #[test]
    fn alias_copies_are_charged_to_the_value_budget() {
        // An anchor of empty lists has aliasCount 0, so maxAliasCount never trips; the budget does.
        let wide =
            |n: usize, m: usize| format!("big: &a [{}]\nuse: [{}]\n", vec!["[]"; n].join(","), vec!["*a"; m].join(","));
        assert!(parse(&wide(100, 100)).is_ok());
        let started = std::time::Instant::now();
        assert!(refused(&wide(3000, 3000)).contains("expands past"));
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "refused before it grows");
        // An anchor costs what its value costs: one anchored around 60% of the budget still reads.
        let n = MAX_NODES * 3 / 5;
        let anchored = format!("big: &a [{}]\n", vec!["0"; n].join(","));
        assert!(parse(&anchored).is_ok(), "{:?}", parse(&anchored).err());
    }

    #[test]
    fn the_subset_refuses_what_it_cannot_read_exactly() {
        for (doc, why) in [
            ("a:\tb\n", "tab character (line 1)"),
            ("a: 1\nb: x\ry\n", "carriage return outside a CRLF line ending (line 2)"),
            ("a: x\u{0}\n", "control character"),
            ("a: \u{feff}x\n", "byte-order mark"),
            ("a: 1", "newline"),
            ("%YAML 1.2\n---\na: 1\n", "directive"),
            ("a: 1\n...\n", "end marker"),
            ("a: 1\n---\nb: 2\n", "`---` past the first line"),
            ("a: 1\n  \nb: 2\n", "whitespace-only"),
            ("? k\n: v\n", "explicit key `?` (line 1)"),
            ("- ? k\n  : v\n", "explicit key"),
            ("{? a: b}\n", "explicit key"),
            (": a\nb: 1\n", "empty key"),
            ("x:\n  : a\n", "empty key"),
            ("a: \"\\uD83D x\"\n", "lone surrogate escape"),
            ("a: !!int 1\n", "tag (`!!int`"),
            ("a: !custom x\n", "tag (`!custom`"),
            ("a: |+\n  x\n", "block scalar header other than"),
            ("a: |2\n   x\n", "block scalar header other than"),
            ("{[a, b]: v}\n", "collection used as a map key"),
            ("a: \"x \\\n\n  y\"\n", "escaped line break followed by an empty line"),
            ("k:\n  - \"x\n  \"\n", "quoted scalar continuation line indented at or before its parent (line 3)"),
            ("k:\n  - 'x\n  y'\n", "quoted scalar continuation"),
            ("k:\n  - id: \"x\n    y\"\n", "quoted scalar"),
            ("    x: [a, b\naken]\n", "flow collection continuation"),
            ("o: [a: [x]]\n", "parser refuses"),
        ] {
            let e = refused(doc);
            assert!(e.contains(OUTSIDE) && e.contains(why), "{doc:?}: {e}");
        }
        assert!(parse("a: @x\n").is_err(), "a reserved indicator: yaml-rust2 refuses it");
        assert!(parse(&format!("{}: v\n", "k".repeat(1025))).is_err());
        assert!(parse(&format!("{}: v\n", "k".repeat(1024))).is_ok(), "1024 chars is still an implicit key");
    }

    #[test]
    fn pyyaml_indentless_sequences_read_whatever_their_first_item() {
        // The sequence is marked on its first item; a `[` or `-` there is the item, not the sequence.
        assert_eq!(js("a:\n- [x]\nb: 1\n"), json!({"a": [["x"]], "b": 1}));
        assert_eq!(js("k:\n- - x\n- 'a\n  b'\n"), json!({"k": [["x"], "a b"]}));
        assert_eq!(js("k:\n- - a\n- []\n- 'q\n\n  r'\n"), json!({"k": [["a"], [], "q\nr"]}));
        // Inside a block sequence, the `-` before a `[` or `-` is the parent's entry, so a nested
        // sequence's entries begin at its own `-`: a continuation must pass that column.
        assert_eq!(js("k:\n  - [x]\n  - - y\n"), json!({"k": [["x"], ["y"]]}));
        assert!(refused("k:\n  - - 'a\n    b'\n").contains("continuation"));
        assert_eq!(js("k:\n  - - 'a\n     b'\n"), json!({"k": [["a b"]]}));
    }

    #[test]
    fn a_flow_collection_closes_alone_at_its_parents_column() {
        assert_eq!(js("a:\n  o: [\n    x\n  ]\nb: 1\n"), json!({"a": {"o": ["x"]}, "b": 1}));
        assert_eq!(js("a:\n  o: {\n    x: 1\n  } # done\n"), json!({"a": {"o": {"x": 1}}}));
        // Only alone, and only the outermost: `]]` there is refused, as the package refuses it.
        assert!(refused("a:\n  o: [[\n    x\n  ]]\n").starts_with(OUTSIDE));
        assert!(refused("a:\n  o: [\n    x,\n  y]\n").starts_with(OUTSIDE));
    }

    #[test]
    fn a_flow_sequence_pair_keeps_its_colon_on_the_keys_line() {
        assert!(refused("o: {f: [a\n  : b]}\n").contains("pair"));
        assert!(refused("o: [{}, n\n  : v]\n").contains("pair"));
        // A quoted key spanning lines, and an anchor on the line before the key.
        assert!(refused("o: [{},\n  \"a\n    b\": c]\n").contains("pair"));
        assert!(refused("o: [{},\n  'a\n    b': c]\n").contains("pair"));
        assert_eq!(js("o: {f: [{}, &a\n  \"x\": c]}\n"), json!({"o": {"f": [{}, {"x": "c"}]}}));
        assert_eq!(js("o: [&a \"x\": c]\n"), json!({"o": [{"x": "c"}]}));
        assert_eq!(js("o: {f: [a: b]}\n"), json!({"o": {"f": [{"a": "b"}]}}));
    }

    #[test]
    fn keys_sharing_a_line_are_checked_in_linear_time() {
        let keys: Vec<String> = (0..30_000).map(|i| format!("k{i}: 1")).collect();
        let doc = format!("x: {{{}}}\n", keys.join(", "));
        let started = std::time::Instant::now();
        assert!(parse(&doc).is_ok());
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
    }

    #[test]
    fn a_repeated_escape_shares_one_carrier() {
        // 80k uses of one escape: more than the private-use pool holds, so one carrier each would fail.
        let doc = format!("note: {}\n", "\\uD83D ".repeat(80_000));
        assert!(parse(&doc).is_ok(), "{:?}", parse(&doc).err());
        assert!(refused("a: x\nb: \"\\uD800\"\n").contains("line 2"), "the lone escape's line is named");
        // A document holding every private-use character leaves no carrier: refused, labelled.
        let held: String = ('\u{F0000}'..='\u{FFFFD}').chain('\u{E000}'..='\u{F8FF}').collect();
        assert!(refused(&format!("note: x{held} \\uD800\n")).starts_with(OUTSIDE));
    }

    #[test]
    fn a_byte_order_mark_reads_only_inside_double_quotes() {
        // The plugin's writer is `JSON.stringify`, which leaves U+FEFF literal in a double-quoted value.
        assert_eq!(js("a: \"x\u{feff}y\"\nb: \"\u{feff}\"\n"), json!({"a": "x\u{feff}y", "b": "\u{feff}"}));
        assert_eq!(js("a: \"x\n  \u{feff}y\"\n"), json!({"a": "x \u{feff}y"}));
        // Anywhere else yaml-rust2 and the package part ways (`- ` after one reads nothing): refused.
        for doc in [
            "asks:\n  \u{feff}- id: a\n",
            "---\n\u{feff}a: 1\n",
            "a: x\u{feff}\n",
            "a: 'x\u{feff}'\n",
            "\u{feff}\u{feff}a: 1\n",
        ] {
            assert!(refused(doc).contains("byte-order mark"), "{doc:?}");
        }
        assert_eq!(js("\u{feff}a: 1\n"), json!({"a": 1}), "a leading one is not content");
    }

    #[test]
    fn an_anchor_glued_to_a_flow_collection_is_refused() {
        // The package wants white space after an anchor; yaml-rust2 ends the name at the bracket.
        assert!(refused("x: &1{a: b}\n").contains("glued"));
        assert!(refused("x: &1[a]\n").contains("glued"));
        assert_eq!(js("x: &1 {a: b}\ny: *1\n"), json!({"x": {"a": "b"}, "y": {"a": "b"}}));
    }

    #[test]
    fn an_implicit_key_is_measured_as_the_package_measures_it() {
        let units = |key: &str| format!("a:\n  {key}: v\n");
        // UTF-16 units: an emoji is two, so 512 reach the limit and 513 pass it.
        assert!(parse(&units(&"😀".repeat(512))).is_ok());
        assert!(refused(&units(&"😀".repeat(513))).contains("UTF-16 units"));
        // From the anchor, not the key; and a carried escape counts as the six characters it is.
        assert!(parse(&units(&format!("&a {}", "k".repeat(1021)))).is_ok());
        assert!(refused(&units(&format!("&a {}", "k".repeat(1022)))).starts_with(OUTSIDE));
        assert!(parse(&units(&format!("\\uD83D{}", "k".repeat(1018)))).is_ok());
        assert!(refused(&units(&format!("\\uD83D{}", "k".repeat(1019)))).contains("UTF-16 units"));
        // Past BMP, where yaml-rust2's own count in chars stays short: the anchor and the quotes count.
        let e = "😀".repeat(511);
        assert!(parse(&units(&format!("&a {}k", "😀".repeat(510)))).is_ok());
        assert!(refused(&units(&format!("&a {e}"))).contains("UTF-16 units"));
        for q in ['"', '\''] {
            assert!(parse(&units(&format!("{q}{e}{q}"))).is_ok());
            assert!(refused(&units(&format!("{q}{e}k{q}"))).contains("UTF-16 units"));
        }
        // A flow sequence pair is held to it as a block key is.
        assert!(parse(&format!("a: [{}: v]\n", "k".repeat(1024))).is_ok());
        assert!(refused(&format!("a: [{}: v]\n", "k".repeat(1025))).contains("UTF-16 units"));
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
        // A comment after a block header is not the header, whatever it holds.
        assert_eq!(js("q: | # see a > b\n  text\nr: >- # x|y\n  t\n"), json!({"q": "text\n", "r": "t"}));
        // A `?` or `:` inside a scalar opens nothing.
        assert_eq!(js("q: |\n  ? not a key\n  : nor this\n"), json!({"q": "? not a key\n: nor this\n"}));
        // What the plugin's writer and json.dumps emit: LS/PS/NEL/DEL raw, surrogate pairs escaped.
        assert_eq!(js("a: \"x\u{2028}y\u{7f}\"\n"), json!({"a": "x\u{2028}y\u{7f}"}));
        assert_eq!(js("a: \"\\ud83d\\ude80 go\"\n"), json!({"a": "\u{1F680} go"}));
        assert_eq!(js("a: '\\ud83d\\ude80'\nb: x \\ud83d\n"), json!({"a": "\\ud83d\\ude80", "b": "x \\ud83d"}));
        assert_eq!(js("a: \"x\\\\uD83D\"\n"), json!({"a": "x\\uD83D"}), "an escaped backslash, then text");
        // Continuation lines indented past their parent read.
        assert_eq!(js("k:\n  - \"x\n   y\"\n  - id: 'p\n     q'\n"), json!({"k": ["x y", {"id": "p q"}]}));
        // PyYAML's indentless sequence: the `-` at the key's column, a continuation two past it.
        assert_eq!(
            js("exhausted:\n- 'read: a\n  b'\nx:\n  y:\n  - 'c\n    d'\n"),
            json!({"exhausted": ["read: a b"], "x": {"y": ["c d"]}})
        );
        assert_eq!(js("x: [a,\n  b]\n"), json!({"x": ["a", "b"]}));
        assert_eq!(js("a: \"x \\\n  y\"\n"), json!({"a": "x y"}), "an escaped line break alone reads");
        assert_eq!(js("~: a\n"), json!({"": "a"}), "a `~` key is null, not an empty key");
    }

    #[test]
    fn deep_nesting_is_refused_not_a_stack_overflow() {
        let block = |n: usize| format!("x:\n  {}v\n", "- ".repeat(n));
        assert!(parse(&block(MAX_DEPTH - 1)).is_ok(), "at the cap it reads");
        // Refused while parsing, before a 50 000-level tree is built, not only once it is resolved.
        let deep = refused(&block(50_000));
        assert!(deep.contains("deeper than") && !deep.contains("once aliases are resolved"), "{deep}");
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
