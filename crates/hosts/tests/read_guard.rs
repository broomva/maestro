//! The read rule's guard (BRO-3846, Codex's joint round 7): registry discovery and enumeration go
//! through `maestro_hosts::read` only. A raw existence probe (`is_dir`, `is_file`, `exists`), a
//! raw `read_dir` or `read_to_string`, or an entry error dropped (`.ok()` / `Result::ok` /
//! `flatten` over entries) turns a read failure into "not there", the defect this rule ends. This
//! test fails if one comes back in the functions that discover a registry or enumerate it.

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
            "list_live_report",
        ],
    ),
    ("src/paseo.rs", &["is_connected", "lookup", "list_with", "agent_at", "parse_agent"]),
];

/// What turns a read failure into absence outside `read`.
const FORBIDDEN: &[&str] =
    &[".is_dir()", ".is_file()", ".exists()", "read_dir(", "read_to_string(", "Result::ok", "e.ok()", ".flatten()"];

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
            }
        }
    }
    assert!(found.is_empty(), "read through maestro_hosts::read instead:\n{}", found.join("\n"));
}

/// The guard itself sees a raw probe.
#[test]
fn the_guard_catches_a_raw_probe() {
    let text = "fn locate(&self) -> X {\n    if p.is_dir() { a } else { b }\n}\nfn other() {}";
    let found = bodies(text, "locate");
    assert_eq!(found.len(), 1);
    assert!(found[0].contains(".is_dir()") && !found[0].contains("fn other"));
}
