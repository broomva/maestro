//! The read rule's guard (BRO-3846, joint rounds 7, 9 and 11): registry discovery, enumeration and
//! record decoding go through `maestro_hosts::read` and the one permission rule
//! (`permission_record`, behind `decode_permissions` and `decode_runtime_requests`) only. A raw probe or
//! read (`is_dir`, `is_file`, `exists`, `read_dir`, `read_to_string`, `fs::metadata`, `fs::read`,
//! `File::open`), an error dropped by `.ok()` on a decode or in a `filter_map`, or an embedded-reader
//! `except …: continue` turns a read failure into "not there", the defect these rules end.
//!
//! It is a scoped, lexical guard over a named list and claims no more than it checks: the functions
//! named below, for the listed patterns, matched over each statement (the text between `;`, `{` and
//! `}`, whitespace normalised, so a chain split across lines is one statement); and the whole
//! embedded SQLite reader, for a `continue` anywhere in an `except` block's body (blank and
//! comment-only lines skipped). Code outside the
//! list, and errors dropped by other means, are review's to catch. Its one reviewed exception is
//! listed in [`REVIEWED`], with why.

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
    (
        "src/paseo.rs",
        &[
            "is_connected",
            "lookup",
            "list_with",
            "agent_at",
            "parse_agent",
            "decode_permissions",
            "decode_runtime_requests",
            "permission_record",
        ],
    ),
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

/// The statements of a function body: the text between `;`, `{` and `}`, each with its whitespace
/// (newlines included) normalised to single spaces, so an expression split across lines is one.
fn statements(body: &str) -> Vec<String> {
    body.split([';', '{', '}'])
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty())
        .collect()
}

/// A statement that drops an error with `.ok()`: in a `filter_map`, or on a decode.
fn drops_an_error(statement: &str) -> bool {
    let s = statement.replace(" .", ".");
    s.contains(".ok()")
        && (s.contains("filter_map(") || ["from_value", "from_str", "from_slice"].iter().any(|d| s.contains(d)))
}

/// The embedded SQLite reader's `except …:` clauses with a `continue` anywhere in their body (the
/// lines indented deeper than the `except`, up to the first that is not; blank and comment-only
/// lines neither end the body nor count), so logging, assigning or a comment first does not hide
/// the skip. Lexical, and only this: a skip spelt otherwise (`pass`, `return None`) is review's.
fn skips_on_error(reader: &str) -> Vec<String> {
    let indent = |l: &str| l.len() - l.trim_start().len();
    let lines: Vec<&str> = reader.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !(line.trim_start().starts_with("except") && line.trim_end().ends_with(':')) {
            continue;
        }
        let depth = indent(line);
        let code = |l: &&&str| !l.trim().is_empty() && !l.trim_start().starts_with('#');
        let body = lines[i + 1..].iter().filter(code).take_while(|l| indent(l) > depth);
        if body.into_iter().any(|l| l.trim() == "continue" || l.trim().starts_with("continue ")) {
            out.push(format!("line {}: {}", i + 1, line.trim()));
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
                for statement in statements(body).iter().filter(|s| !reviewed && drops_an_error(s)) {
                    found.push(format!("{file}: fn {name} drops an error: {statement}"));
                }
            }
        }
    }
    assert!(found.is_empty(), "read through maestro_hosts::read instead:\n{}", found.join("\n"));
}

/// The embedded SQLite reader, whole, skips nothing on an error: a payload that does not read is an
/// exit status, or a listed thread with why, never a `continue`. It interprets no permission
/// payload: those are Rust's (`permission_record`, BRO-3863 round 11).
#[test]
fn the_embedded_reader_never_skips_on_an_error() {
    let t3 = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/t3.rs")).unwrap();
    let start = t3.find("const SQLITE_READER_PY: &str = r#\"").expect("the embedded reader");
    let reader = &t3[start..start + t3[start..].find("\"#;").expect("its end")];
    let found = skips_on_error(reader);
    assert!(found.is_empty(), "the reader skips on an error:\n{}", found.join("\n"));
}

/// The guard's own rules see what they name, on one line or across several, and nothing else.
#[test]
fn the_guards_rules_see_what_they_name() {
    let flagged = |body: &str| statements(body).iter().any(|s| drops_an_error(s));
    // One line.
    assert!(flagged(
        "fn f() { let p = raw.into_iter().filter_map(|v| serde_json::from_value::<P>(v).ok()).collect(); }"
    ));
    assert!(flagged("fn f() { let x = serde_json::from_str::<V>(&t).ok(); }"));
    // Across lines: the chain and the closure split up.
    assert!(flagged(
        "fn f() {\n    let p = raw\n        .into_iter()\n        .filter_map(|v| {\n            serde_json::from_value::<P>(v)\n                .ok()\n        })\n        .collect();\n}"
    ));
    assert!(flagged("fn f() {\n    let x = serde_json::from_str::<V>(\n        &t,\n    )\n    .ok();\n}"));
    // Not a decode or a filter_map: a lock, and a decode whose error is kept.
    assert!(!flagged("fn f() {\n    let held = self.found\n        .lock()\n        .ok();\n}"));
    assert!(!flagged("fn f() { let x = serde_json::from_str::<V>(&t).map_err(|e| why(e))?; let y = m.lock().ok(); }"));
    // Python: a continue right after the except, or after logging or assigning in its body.
    let direct = "try:\n    x = load()\nexcept Exception:\n\n    continue\n";
    assert_eq!(skips_on_error(direct), ["line 3: except Exception:"]);
    let later = "for r in rows:\n    try:\n        x = load(r)\n    except ValueError as e:\n        log(e)\n        bad = True\n        continue\n    use(x)\n";
    assert_eq!(skips_on_error(later), ["line 4: except ValueError as e:"]);
    // A comment, at any indent, neither ends the body nor hides the continue after it.
    let commented = "for r in rows:\n    try:\n        x = load(r)\n    except E:\n# a note at column 0\n        # an indented note\n\n        continue\n";
    assert_eq!(skips_on_error(commented), ["line 4: except E:"]);
    // Not flagged: an except that records and goes on, and a continue after the block ends.
    let kept = "for r in rows:\n    try:\n        x = load(r)\n    except E as e:\n        out.append(e)\n    if skip(r):\n        continue\n";
    assert!(skips_on_error(kept).is_empty(), "{:?}", skips_on_error(kept));
    // Nor a continue that is only in a comment, or after the block with a comment between.
    let noted = "for r in rows:\n    try:\n        x = load(r)\n    except E as e:\n        # continue would hide it\n        out.append(e)\n    # done\n    continue\n";
    assert!(skips_on_error(noted).is_empty(), "{:?}", skips_on_error(noted));
}

/// The guard itself sees a raw probe.
#[test]
fn the_guard_catches_a_raw_probe() {
    let text = "fn locate(&self) -> X {\n    if p.is_dir() { a } else { b }\n}\nfn other() {}";
    let found = bodies(text, "locate");
    assert_eq!(found.len(), 1);
    assert!(found[0].contains(".is_dir()") && !found[0].contains("fn other"));
}
