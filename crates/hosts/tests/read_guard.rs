//! The read rule's guard (BRO-3846, joint rounds 7 and 9): registry discovery, enumeration and
//! record decoding go through `maestro_hosts::read` and `decode_permissions` only. A raw probe or
//! read (`is_dir`, `is_file`, `exists`, `read_dir`, `read_to_string`, `fs::metadata`, `fs::read`,
//! `File::open`), an error dropped by `.ok()` on a decode or in a `filter_map`, or an embedded-reader
//! `except …: continue` turns a read failure into "not there", the defect these rules end.
//!
//! It is a scoped, lexical guard and claims no more than it checks: the named functions below, and
//! the whole embedded SQLite reader, for the listed patterns, line by line. Its one reviewed
//! exception is listed in [`REVIEWED`], with why.

use std::path::Path;

/// The functions that discover a registry or enumerate it, by file.
const GUARDED: &[(&str, &[&str])] = &[
    (
        "src/lib.rs",
        &["t3_root_from", "from_env", "from_file", "rows_in", "lookup", "list_live_report", "live_permission_report"],
    ),
    (
        "src/t3.rs",
        &[
            "locate",
            "sqlite_path",
            "json_path",
            "is_connected",
            "discover",
            "environment_id",
            "native_threads",
            "lookup_in",
            "snapshot_each",
            "snapshot_rows",
            "snapshot_row",
            "from_raw",
            "take",
            "list_live_report",
        ],
    ),
    ("src/paseo.rs", &["is_connected", "lookup", "list_with", "agent_at", "parse_agent", "decode_permissions"]),
];

/// What turns a read failure into absence outside `read`.
const FORBIDDEN: &[&str] = &[
    ".is_dir()",
    ".is_file()",
    ".exists()",
    "read_dir(",
    "read_to_string(",
    "fs::metadata(",
    "fs::read(",
    "File::open(",
    "Result::ok",
    "e.ok()",
    ".flatten()",
];

/// Where a dropped decode error is a reviewed choice: `(file, fn, why)`.
const REVIEWED: &[(&str, &str, &str)] = &[(
    "src/t3.rs",
    "discover",
    "display-only runtime metadata (a version and an endpoint); identity and needs never read these files",
)];

/// A line that drops an error with `.ok()`: in a `filter_map`, or on a decode.
fn drops_an_error(line: &str) -> bool {
    line.contains(".ok()")
        && (line.contains("filter_map(") || ["from_value", "from_str", "from_slice"].iter().any(|d| line.contains(d)))
}

/// The embedded SQLite reader's `except …:` clauses whose next statement is `continue`.
fn skips_on_error(reader: &str) -> Vec<String> {
    let lines: Vec<&str> = reader.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("except") && line.trim_end().ends_with(':') {
            let next = lines[i + 1..].iter().find(|l| !l.trim().is_empty()).map(|l| l.trim());
            if next == Some("continue") {
                out.push(format!("line {}: {}", i + 1, line.trim()));
            }
        }
    }
    out
}

/// The bodies of every `fn <name>(` in `text`, from the signature to the matching closing brace.
fn bodies<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let needle = format!("fn {name}(");
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(&needle) {
        let start = from + at;
        let Some(open) = text[start..].find('{').map(|i| start + i) else { break };
        let mut depth = 0usize;
        let mut end = open;
        for (i, c) in text[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.push(&text[start..end]);
        from = end;
    }
    out
}

#[test]
fn registry_discovery_and_enumeration_read_by_the_one_rule() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = Vec::new();
    for (file, functions) in GUARDED {
        let text = std::fs::read_to_string(crate_dir.join(file)).unwrap();
        for name in *functions {
            let bodies = bodies(&text, name);
            assert!(!bodies.is_empty(), "{file}: no fn {name}: update the guard's list");
            for body in bodies {
                for pattern in FORBIDDEN {
                    if body.contains(pattern) {
                        found.push(format!("{file}: fn {name} uses `{pattern}`"));
                    }
                }
                let reviewed = REVIEWED.iter().any(|(f, n, _)| f == file && n == name);
                for line in body.lines().filter(|l| !reviewed && drops_an_error(l)) {
                    found.push(format!("{file}: fn {name} drops an error: {}", line.trim()));
                }
            }
        }
    }
    assert!(found.is_empty(), "read through maestro_hosts::read instead:\n{}", found.join("\n"));
}

/// The embedded SQLite reader skips nothing on an error: a record that does not read is a marker
/// or an exit status, never a `continue`.
#[test]
fn the_embedded_reader_never_skips_on_an_error() {
    let t3 = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/t3.rs")).unwrap();
    let start = t3.find("const SQLITE_READER_PY: &str = r#\"").expect("the embedded reader");
    let reader = &t3[start..start + t3[start..].find("\"#;").expect("its end")];
    let found = skips_on_error(reader);
    assert!(found.is_empty(), "the reader skips on an error:\n{}", found.join("\n"));
}

/// The guard's own rules see what they name.
#[test]
fn the_guards_rules_see_what_they_name() {
    assert!(drops_an_error("    .filter_map(|v| serde_json::from_value::<P>(v).ok())"));
    assert!(drops_an_error("let x = serde_json::from_str::<V>(&t).ok();"));
    assert!(!drops_an_error("let held = self.found.lock().ok();"));
    let reader = "try:\n    x = load()\nexcept Exception:\n\n    continue\nexcept E as e:\n    out.append(e)\n";
    assert_eq!(skips_on_error(reader), ["line 3: except Exception:"]);
}

/// The guard itself sees a raw probe.
#[test]
fn the_guard_catches_a_raw_probe() {
    let text = "fn locate(&self) -> X {\n    if p.is_dir() { a } else { b }\n}\nfn other() {}";
    let found = bodies(text, "locate");
    assert_eq!(found.len(), 1);
    assert!(found[0].contains(".is_dir()") && !found[0].contains("fn other"));
}
