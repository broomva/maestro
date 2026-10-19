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
//!   `"1"`). `!!binary` and `!!timestamp` resolve as the package resolves them.
//!   Any other tag (`!custom`) reads its scalar as a string.
//! - **Map keys** are strings as `toJS` makes them: null is `""` and numbers
//!   print as JavaScript prints them. Collection keys use the package's flow
//!   form, such as `[ a, b ]`. A later key that stringifies alike replaces the
//!   earlier one. Two keys with the same resolved value are an error ("Map
//!   keys must be unique"), as in the package.
//! - **Anchors and aliases** resolve to a copy of the anchored value. `<<` is
//!   an ordinary key, because YAML 1.2 has no merge keys.
//! - **More than one document** is an error, as `parseDocument` reports it.
//!
//! Known gaps, each refused loudly or reported, never silent:
//!
//! - A `%YAML 1.1` document is refused. The plugin reads it with YAML 1.1
//!   rules, which are not implemented here.
//! - A non-finite number is JSON `null`, as `JSON.stringify` prints it, so its
//!   hash matches. Its *text* differs: the plugin's `text(Infinity)` is
//!   `"Infinity"`, so `answer: .inf` is answered there and open here.
//! - A lone surrogate escape (`"\uD83D"`) cannot live in a Rust string. It is
//!   read as U+FFFD and reported, so that entry's hash differs from the plugin's.
//! - Collections nested deeper than [`MAX_DEPTH`] are refused. The plugin's limit
//!   is its engine's stack, which differs between node and bun.

use serde_json::{Map, Number, Value};
use std::collections::HashMap;
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser, Tag};
use yaml_rust2::scanner::{Marker, TScalarStyle};

const CORE: &str = "tag:yaml.org,2002:";
/// Values an alias may expand to in one document: the package's `maxAliasCount` guards the same blow-up.
const MAX_NODES: usize = 1_000_000;
/// Collections nested deeper than this are refused. yaml-rust2 caps flow nesting at 255
/// and has no cap on block nesting, and conversion recurses once per level, so an
/// unbounded block document would overflow the stack and abort the process (a 50 000-deep
/// `- - - …` did, inside yaml-rust2's own recursive `load`, which is why it is not used). The plugin's own limit is its engine's stack: node fails near 1 000
/// levels and bun near 8 000, so no exact parity exists past a few hundred.
pub const MAX_DEPTH: usize = 256;

/// A ledger read as the plugin reads it, plus anything read differently that the caller must report.
#[derive(Debug)]
pub struct Read {
    pub value: Value,
    pub warnings: Vec<String>,
}

/// Parse one YAML document to the JavaScript value `toJS()` would build.
pub fn parse(source: &str) -> Result<Read, String> {
    if let Some(version) = yaml_version(source)
        && version != "1.2"
    {
        return Err(format!(
            "a %YAML {version} document: the plugin reads it with YAML {version} rules, which Maestro does not implement"
        ));
    }
    match parse_events(source) {
        Ok(value) => Ok(Read { value, warnings: Vec::new() }),
        Err(e) if e.contains("invalid Unicode character escape") => {
            let (fixed, lone) = rewrite_surrogate_escapes(source);
            let value = parse_events(&fixed)?;
            let warnings = if lone > 0 {
                vec![format!(
                    "{lone} lone surrogate escape(s) read as U+FFFD: the plugin keeps them, so these entries hash differently"
                )]
            } else {
                Vec::new()
            };
            Ok(Read { value, warnings })
        }
        Err(e) => Err(e),
    }
}

/// The version of a leading `%YAML` directive, if the document has one.
fn yaml_version(source: &str) -> Option<String> {
    for line in source.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let rest = t.strip_prefix("%YAML")?;
        return rest.split_whitespace().next().map(str::to_string);
    }
    None
}

fn parse_events(source: &str) -> Result<Value, String> {
    let mut builder = Builder::default();
    let mut parser = Parser::new_from_str(source);
    // Drive the parser one event at a time. Its own `load` recurses once per nesting level,
    // which a deep enough document turns into a stack overflow; this loop does not, and it
    // stops at the first refusal instead of parsing the rest.
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
        None => Ok(Value::Null),
        Some(node) => {
            let mut budget = MAX_NODES;
            to_js(&node, &builder.anchors, &mut budget)
        }
    }
}

#[derive(Debug, Clone)]
enum Node {
    Scalar { value: String, style: TScalarStyle, tag: Option<Tag> },
    Seq(Vec<Node>),
    Map(Vec<(Node, Node)>),
    Alias(usize),
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
}

impl Builder {
    fn push(&mut self, node: Node, anchor: usize) {
        if anchor > 0 {
            self.anchors.insert(anchor, node.clone());
        }
        match self.stack.last_mut() {
            None => self.root = Some(node),
            Some(Open::Seq(_, items)) => items.push(node),
            Some(Open::Map(_, pairs, key)) => match key.take() {
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

fn spend(budget: &mut usize) -> Result<(), String> {
    *budget = budget.checked_sub(1).ok_or("Excessive alias count: the document expands past its limit")?;
    Ok(())
}

fn to_js(node: &Node, anchors: &HashMap<usize, Node>, budget: &mut usize) -> Result<Value, String> {
    spend(budget)?;
    match node {
        Node::Scalar { value, style, tag } => Ok(scalar(value, *style, tag.as_ref())),
        Node::Alias(id) => {
            let target = anchors.get(id).ok_or("an alias names no anchor")?;
            to_js(target, anchors, budget)
        }
        Node::Seq(items) => items.iter().map(|n| to_js(n, anchors, budget)).collect::<Result<_, _>>().map(Value::Array),
        Node::Map(pairs) => {
            let mut out = Map::new();
            let mut seen: Vec<Value> = Vec::new();
            for (k, v) in pairs {
                let key_value = to_js(k, anchors, budget)?;
                if is_scalar(k, anchors) {
                    if seen.iter().any(|s| same_scalar(s, &key_value)) {
                        return Err("Map keys must be unique".into());
                    }
                    seen.push(key_value.clone());
                }
                let key = key_string(&key_value, k, anchors)?;
                out.insert(key, to_js(v, anchors, budget)?);
            }
            Ok(Value::Object(out))
        }
    }
}

fn is_scalar(node: &Node, anchors: &HashMap<usize, Node>) -> bool {
    match node {
        Node::Scalar { .. } => true,
        Node::Alias(id) => anchors.get(id).is_some_and(|n| is_scalar(n, anchors)),
        _ => false,
    }
}

/// `a.value === b.value` for two resolved scalars. Strings, booleans and null
/// compare by value. Numbers compare as doubles, so 0 and -0 are equal; a
/// non-finite number is `null` here and never equal (NaN !== NaN).
fn same_scalar(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Null, Value::Null) => false,
        _ => a == b,
    }
}

/// The package's `stringifyKey`: null is "", a scalar is `String(value)`, a collection its flow form.
fn key_string(value: &Value, node: &Node, anchors: &HashMap<usize, Node>) -> Result<String, String> {
    if is_scalar(node, anchors) {
        return Ok(match value {
            Value::Null => String::new(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => maestro_core::ask::js_number(n),
            Value::String(s) => s.clone(),
            // A tagged scalar that resolved to bytes (`!!binary`) as a key: no plain form to give it.
            _ => return Err("a map key that is not a plain value".into()),
        });
    }
    flow(value).ok_or_else(|| "a collection map key the plugin would stringify in a form Maestro does not".into())
}

/// The flow form the package writes for a collection key: `[ a, b ]`, `{ a: 1 }`, `[]`, `{}`.
/// Only plain-safe scalars are written; anything that would need quoting returns None.
fn flow(value: &Value) -> Option<String> {
    match value {
        Value::Array(items) if items.is_empty() => Some("[]".into()),
        Value::Object(map) if map.is_empty() => Some("{}".into()),
        Value::Array(items) => {
            let parts: Option<Vec<String>> = items.iter().map(flow).collect();
            Some(format!("[ {} ]", parts?.join(", ")))
        }
        Value::Object(map) => {
            let parts: Option<Vec<String>> =
                map.iter().map(|(k, v)| Some(format!("{}: {}", plain_safe(k)?, flow(v)?))).collect();
            Some(format!("{{ {} }}", parts?.join(", ")))
        }
        Value::Null => Some("null".into()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(maestro_core::ask::js_number(n)),
        Value::String(s) => plain_safe(s).map(str::to_string),
    }
}

/// A string the package writes unquoted in flow context: letters, digits and `_-./`, starting
/// with a letter, and not one the core schema would read back as something else.
fn plain_safe(s: &str) -> Option<&str> {
    let ok = s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "_-./".contains(c))
        && matches!(plain(s), Value::String(_));
    ok.then_some(s)
}

fn scalar(value: &str, style: TScalarStyle, tag: Option<&Tag>) -> Value {
    match tag {
        Some(tag) if tag.handle == CORE => tagged(value, &tag.suffix),
        // A non-specific `!` or any local/unknown tag: the scalar is its text.
        Some(_) => Value::String(value.to_string()),
        None if style == TScalarStyle::Plain => plain(value),
        None => Value::String(value.to_string()),
    }
}

/// A plain scalar under the core schema, in the schema's own test order.
fn plain(s: &str) -> Value {
    null_of(s).or_else(|| bool_of(s)).or_else(|| int_of(s)).or_else(|| float_of(s)).unwrap_or_else(|| s.into())
}

fn tagged(s: &str, suffix: &str) -> Value {
    let resolved = match suffix {
        "str" => Some(Value::String(s.to_string())),
        "null" => null_of(s),
        "bool" => bool_of(s),
        "int" => int_of(s),
        "float" => float_of(s),
        "binary" => Some(binary(s)),
        "timestamp" => timestamp(s),
        _ => None,
    };
    // A failed test falls back to the string tag, as the package does (with a warning).
    resolved.unwrap_or_else(|| Value::String(s.to_string()))
}

fn null_of(s: &str) -> Option<Value> {
    matches!(s, "" | "~" | "null" | "Null" | "NULL").then_some(Value::Null)
}

fn bool_of(s: &str) -> Option<Value> {
    match s {
        "true" | "True" | "TRUE" => Some(Value::Bool(true)),
        "false" | "False" | "FALSE" => Some(Value::Bool(false)),
        _ => None,
    }
}

/// `0o[0-7]+`, `[-+]?[0-9]+`, `0x[0-9a-fA-F]+`, each through `parseInt`.
fn int_of(s: &str) -> Option<Value> {
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
    s.parse::<f64>().ok().map(js_num)
}

fn radix(digits: &str, base: u32) -> Option<Value> {
    if digits.is_empty() {
        return None;
    }
    let mut acc = 0f64;
    for c in digits.chars() {
        acc = acc * f64::from(base) + f64::from(c.to_digit(base)?);
    }
    Some(js_num(acc))
}

/// `.inf`/`.nan` forms, then `[-+]?(\.d+|d+(\.d*)?)[eE][-+]?d+`, then `[-+]?(\.d+|d+\.d*)`, through `parseFloat`.
fn float_of(s: &str) -> Option<Value> {
    let unsigned = s.strip_prefix(['-', '+']).unwrap_or(s);
    if matches!(unsigned, ".inf" | ".Inf" | ".INF") || matches!(s, ".nan" | ".NaN" | ".NAN") {
        return Some(Value::Null); // JSON.stringify(Infinity | NaN)
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
    s.parse::<f64>().ok().map(js_num)
}

/// A JavaScript number as JSON: integral doubles within 2^53 as integers, a non-finite one as null.
fn js_num(x: f64) -> Value {
    const SAFE: f64 = 9_007_199_254_740_992.0;
    if !x.is_finite() {
        return Value::Null;
    }
    if x.fract() == 0.0 && x.abs() <= SAFE {
        return Value::from(x as i64); // -0 becomes 0, as JSON.stringify prints it
    }
    Number::from_f64(x).map_or(Value::Null, Value::Number)
}

/// `!!binary`: base64 decoded leniently (non-alphabet bytes skipped, as Node's
/// `Buffer.from(s, 'base64')`), then shaped as `canonical` shapes a byte array:
/// an object of index keys.
fn binary(s: &str) -> Value {
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
    Value::Object(bytes.into_iter().enumerate().map(|(i, b)| (i.to_string(), Value::from(b))).collect())
}

/// `!!timestamp`: the YAML 1.1 forms the package accepts, as `Date#toISOString()`.
fn timestamp(s: &str) -> Option<Value> {
    let s = s.trim();
    let (date, time) = match s.find(['t', 'T', ' ', '\t']) {
        Some(i) => (&s[..i], Some(s[i + 1..].trim_start())),
        None => (s, None),
    };
    let mut d = date.split('-');
    let (y, m, day) = (d.next()?, d.next()?, d.next()?);
    if d.next().is_some() || y.len() != 4 || !(1..=2).contains(&m.len()) || !(1..=2).contains(&day.len()) {
        return None;
    }
    let date = jiff::civil::Date::new(y.parse().ok()?, m.parse().ok()?, day.parse().ok()?).ok()?;
    let ts = match time {
        None => date.to_zoned(jiff::tz::TimeZone::UTC).ok()?.timestamp(),
        Some(t) => {
            // `hh:mm:ss(.f)?` then an optional `Z` or `±hh(:mm)?` offset, spaces allowed before it.
            let split = t.find(['Z', 'z', '+', '-']).unwrap_or(t.len());
            let (clock, zone) = (t[..split].trim_end(), t[split..].trim());
            let mut c = clock.split(':');
            let (h, mi, sec) = (c.next()?, c.next()?, c.next()?);
            let (sec, frac) = sec.split_once('.').unwrap_or((sec, ""));
            let nanos: i32 = format!("{frac:0<9}").get(..9)?.parse().ok()?;
            let time = jiff::civil::Time::new(h.parse().ok()?, mi.parse().ok()?, sec.parse().ok()?, nanos).ok()?;
            let offset_secs = match zone {
                "" | "Z" | "z" => 0,
                z => {
                    let sign = if z.starts_with('-') { -1 } else { 1 };
                    let (hh, mm) = z[1..].split_once(':').unwrap_or((&z[1..], "0"));
                    sign * (hh.parse::<i32>().ok()? * 3600 + mm.parse::<i32>().ok()? * 60)
                }
            };
            let offset = jiff::tz::Offset::from_seconds(offset_secs).ok()?;
            offset.to_timestamp(date.to_datetime(time)).ok()?
        }
    };
    // toISOString: millisecond precision, always UTC with `Z`.
    let ms = ts.as_millisecond();
    let utc = jiff::Timestamp::from_millisecond(ms.div_euclid(1000) * 1000).ok()?;
    Some(Value::String(format!("{}.{:03}Z", utc.strftime("%Y-%m-%dT%H:%M:%S"), ms.rem_euclid(1000))))
}

/// JSON text with each lone surrogate escape replaced by `\uFFFD`; valid pairs are kept.
pub fn lone_surrogates_to_replacement(text: &str) -> String {
    let (fixed, _) = rewrite_surrogate_escapes(text);
    // The rewrite spells a valid pair as YAML's `\U` escape, which JSON lacks: put pairs back.
    let mut out = String::with_capacity(fixed.len());
    let mut rest = fixed.as_str();
    while let Some(i) = rest.find("\\U") {
        out.push_str(&rest[..i]);
        let at = &rest[i..];
        match at.get(2..10).and_then(|h| u32::from_str_radix(h, 16).ok()).and_then(char::from_u32) {
            Some(c) => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{u:04x}"));
                }
                rest = &at[10..];
            }
            None => {
                out.push_str("\\U");
                rest = &at[2..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Make a source with lone surrogate escapes parseable: a `\uD8xx\uDCxx` pair
/// becomes the `\UXXXXXXXX` escape of its code point, and a lone half becomes
/// `�`. Returns the source and how many halves were lone.
fn rewrite_surrogate_escapes(source: &str) -> (String, usize) {
    let unit = |s: &str| -> Option<u32> {
        let hex = s.strip_prefix("\\u")?.get(..4)?;
        u32::from_str_radix(hex, 16).ok()
    };
    let mut out = String::with_capacity(source.len());
    let mut lone = 0;
    let mut rest = source;
    while let Some(i) = rest.find("\\u") {
        out.push_str(&rest[..i]);
        let at = &rest[i..];
        match unit(at) {
            Some(hi @ 0xD800..=0xDBFF) => match at.get(6..).and_then(unit) {
                Some(lo @ 0xDC00..=0xDFFF) => {
                    let cp = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                    out.push_str(&format!("\\U{cp:08X}"));
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
        let v = js("0x10: hex\n1.0: float\n~: n\ntrue: b\n? [a, b]\n: v\n");
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["", "1", "16", "[ a, b ]", "true"]);
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
    }

    #[test]
    fn aliases_copy_and_merge_keys_are_plain_keys() {
        assert_eq!(
            js("b: &b {x: 1}\nr: *b\nm:\n  <<: *b\n"),
            json!({"b": {"x": 1}, "r": {"x": 1}, "m": {"<<": {"x": 1}}})
        );
    }
}
