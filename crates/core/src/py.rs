//! Python's spelling of values, for the modules ported from the workspace's Python.
//!
//! `delegation.rs` and `economy.rs` port `scripts/delegation.py` and
//! `scripts/fleet_economy.py`, whose messages are f-strings. A message that
//! names a value spells it as Python does (`repr`, `str`, `format(x, "g")`),
//! and a rounded number is rounded as `round(x, n)` rounds it, so the two
//! implementations agree to the byte (`scripts/parity-delegation.py`).
//! Values arrive as JSON, the shape a YAML graph or a usage row loads into.

use serde_json::Value;

/// Python truthiness of a loaded value.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `str.isspace()`: Rust's White_Space and the four separators U+001C..U+001F Python adds.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()` with no argument.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `a or b` on loaded values: `a` when truthy, else `b`.
pub fn or<'a>(a: Option<&'a Value>, b: Option<&'a Value>) -> Option<&'a Value> {
    if truthy(a) { a } else { b }
}

/// `isinstance(x, (int, float)) and not isinstance(x, bool)`.
pub fn num(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

/// `repr(s)` for a string.
pub fn repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !printable(c) => {
                let n = c as u32;
                out.push_str(&match n {
                    0..=0xff => format!("\\x{n:02x}"),
                    0x100..=0xffff => format!("\\u{n:04x}"),
                    _ => format!("\\U{n:08x}"),
                });
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `str.isprintable` as the reference Python reads it: its Unicode tables, generated into
/// `py_printable.rs` and checked by `scripts/parity-delegation.py`.
fn printable(c: char) -> bool {
    let n = c as u32;
    let i = crate::py_printable::NONPRINTABLE.partition_point(|&(_, end)| end < n);
    crate::py_printable::NONPRINTABLE.get(i).is_none_or(|&(start, _)| n < start)
}

/// `repr(x)` for a float.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let a = f.abs();
    if a == 0.0 || (1e-4..1e16).contains(&a) {
        let s = format!("{f:?}");
        // Rust's Debug is the shortest round-trip digits, like repr, and keeps the `.0`; it
        // switches to an exponent at other bounds than Python, so only the fixed range uses it.
        if s.contains('e') { fixed_from_exp(&s) } else { s }
    } else {
        // Shortest digits in scientific form: `1e+16`, `1.5e-05`.
        let s = format!("{f:e}");
        let (mant, exp) = s.split_once('e').expect("{:e} has an exponent");
        let exp: i32 = exp.parse().expect("integer exponent");
        format!("{mant}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

fn fixed_from_exp(s: &str) -> String {
    // Defensive: Debug never prints an exponent inside [1e-4, 1e16) today.
    let f: f64 = s.parse().expect("a float Debug parses back");
    let mut out = format!("{f}");
    if !out.contains('.') {
        out.push_str(".0");
    }
    out
}

/// `str(x)` for a loaded value, as an f-string interpolates it.
pub fn str_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => repr(other),
    }
}

/// `repr(x)` for a loaded value.
pub fn repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.to_string(),
            (None, Some(u)) => u.to_string(),
            _ => float_repr(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", repr_str(k), repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `format(x, "g")`: six significant digits, an exponent below 1e-4 or from 1e6.
pub fn fmt_g(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let s = format!("{f:.5e}");
    let (mant, exp) = s.split_once('e').expect("{:e} has an exponent");
    let exp: i32 = exp.parse().expect("integer exponent");
    if (-4..6).contains(&exp) {
        let decimals = (5 - exp).max(0) as usize;
        strip_zeros(format!("{f:.decimals$}"))
    } else {
        format!("{}e{}{:02}", strip_zeros(mant.to_string()), if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

fn strip_zeros(s: String) -> String {
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

/// `round(x, n)`: the correctly rounded decimal, ties to even on the exact binary value.
pub fn round(f: f64, n: usize) -> f64 {
    if !f.is_finite() {
        return f;
    }
    format!("{f:.n$}").parse().expect("a formatted float parses back")
}

/// `os.path.normpath` on POSIX.
pub fn normpath(p: &str) -> String {
    if p.is_empty() {
        return ".".into();
    }
    let lead = p.len() - p.trim_start_matches('/').len();
    let prefix = match lead {
        0 => "",
        2 => "//",
        _ => "/",
    };
    let mut parts: Vec<&str> = Vec::new();
    for comp in p.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp != ".." || (prefix.is_empty() && parts.is_empty()) || parts.last() == Some(&"..") {
            parts.push(comp);
        } else if !parts.is_empty() {
            parts.pop();
        }
    }
    let out = format!("{prefix}{}", parts.join("/"));
    if out.is_empty() { ".".into() } else { out }
}

/// `os.path.join(a, b)` on POSIX, for a relative `b`.
pub fn join(a: &str, b: &str) -> String {
    if b.starts_with('/') {
        b.to_string()
    } else if a.is_empty() || a.ends_with('/') {
        format!("{a}{b}")
    } else {
        format!("{a}/{b}")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn strip_is_pythons() {
        // Python: "\x1cx\x1f".strip() == "x"; U+001C..U+001F are whitespace to str.isspace.
        assert_eq!(super::strip("\u{1c} x\u{1f}\u{3000}"), "x");
    }

    use super::*;

    #[test]
    fn repr_escapes_what_python_escapes() {
        assert_eq!(repr_str("a\u{e9}\u{2192}"), "'a\u{e9}\u{2192}'");
        assert_eq!(repr_str("\u{378}"), "'\\u0378'"); // unassigned
        assert_eq!(repr_str("\u{e0041}"), "'\\U000e0041'"); // a tag character (Cf)
        assert_eq!(repr_str("\u{ad}\u{a0}"), "'\\xad\\xa0'");
        assert_eq!(repr_str("\u{10ffff}"), "'\\U0010ffff'");
        assert_eq!(repr_str(" "), "' '");
    }
    use serde_json::json;

    #[test]
    fn repr_matches_python() {
        assert_eq!(repr_str("a"), "'a'");
        assert_eq!(repr_str("it's"), "\"it's\"");
        assert_eq!(repr_str("both ' \""), "'both \\' \"'");
        assert_eq!(repr_str("a\nb\\"), "'a\\nb\\\\'");
        assert_eq!(repr(&json!([1, "x", null, true, 1.5])), "[1, 'x', None, True, 1.5]");
        assert_eq!(repr(&json!({"a": 1})), "{'a': 1}");
    }

    #[test]
    fn floats_print_as_python_prints_them() {
        assert_eq!(float_repr(12.0), "12.0");
        assert_eq!(float_repr(5.3), "5.3");
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(float_repr(1.5e-5), "1.5e-05");
        assert_eq!(float_repr(0.0001), "0.0001");
        assert_eq!(fmt_g(96.0), "96");
        assert_eq!(fmt_g(96.5), "96.5");
        assert_eq!(fmt_g(1234567.0), "1.23457e+06");
        assert_eq!(fmt_g(0.00001), "1e-05");
        assert_eq!(fmt_g(99.99999), "100");
        assert_eq!(round(2.675, 2), 2.67);
        assert_eq!(round(0.125, 2), 0.12);
    }

    #[test]
    fn normpath_matches_posixpath() {
        for (p, want) in [
            ("", "."),
            ("a/./b/", "a/b"),
            ("../a", "../a"),
            ("a/../..", ".."),
            ("/../a", "/a"),
            ("//a", "//a"),
            ("///a/b/../c", "/a/c"),
            ("a/..", "."),
        ] {
            assert_eq!(normpath(p), want, "{p}");
        }
    }
}
