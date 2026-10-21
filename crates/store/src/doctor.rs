//! Self-checks for `maestro doctor`.
//!
//! The vendored path reader ([`crate::broomva_paths`]) is generated in bstack from
//! `references/paths.toml`. It goes stale when the registry or its template changes there
//! and nobody re-vendors it: Maestro would then read last month's layout. The check
//! compares both against a bstack checkout, read like the workspace (a git revision by default).

use crate::broomva_paths::{REGISTRY_SHA256, sha256_hex};
use crate::tree::{Tree, TreeSource};
use serde::Serialize;
use std::path::Path;

/// The vendored reader, byte for byte as it was compiled in.
pub const VENDORED_READER: &str = include_str!("broomva_paths.rs");
pub const REGISTRY_REL: &str = "references/paths.toml";
pub const READER_REL: &str = "scripts/lib/broomva_paths.rs";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Stale,
    /// The bstack copy could not be read, so nothing is known: not a pass.
    Unread,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
}

/// The sha256 the registry's readers embed, of `bytes` (hex).
pub fn sha256(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}

/// Is the vendored path reader the one bstack generates from its current registry?
pub fn registry(bstack: &Path, source: TreeSource) -> Vec<Check> {
    let tree = Tree::resolved(bstack, source);
    let registry = match tree.read(REGISTRY_REL) {
        Ok(text) => {
            let sha = sha256_hex(text.as_bytes());
            if sha == REGISTRY_SHA256 {
                ok(
                    "registry",
                    format!("{} sha256 {sha} is the one the vendored reader embeds", tree.label(REGISTRY_REL)),
                )
            } else {
                stale(
                    "registry",
                    format!(
                        "{} sha256 is {sha}, the vendored reader embeds {REGISTRY_SHA256}: re-vendor with scripts/vendor-paths.sh",
                        tree.label(REGISTRY_REL)
                    ),
                )
            }
        }
        Err(e) => unread("registry", format!("{}: {e}", tree.label(REGISTRY_REL))),
    };
    let reader = match tree.read(READER_REL) {
        Ok(text) if text == VENDORED_READER => {
            ok("reader", format!("crates/store/src/broomva_paths.rs is {}", tree.label(READER_REL)))
        }
        Ok(_) => stale(
            "reader",
            format!(
                "crates/store/src/broomva_paths.rs differs from {}: re-vendor with scripts/vendor-paths.sh",
                tree.label(READER_REL)
            ),
        ),
        Err(e) => unread("reader", format!("{}: {e}", tree.label(READER_REL))),
    };
    vec![registry, reader]
}

fn ok(name: &'static str, detail: String) -> Check {
    Check { name, status: Status::Ok, detail }
}

fn stale(name: &'static str, detail: String) -> Check {
    Check { name, status: Status::Stale, detail }
}

fn unread(name: &'static str, detail: String) -> Check {
    Check { name, status: Status::Unread, detail }
}
