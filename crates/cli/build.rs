//! The commit this binary was built from, for `maestro --version` (BRO-3842). kernel-ready@1's
//! `installed` node reads it to tell whether the installed binary is the head under check.
//!
//! It is `git rev-parse HEAD` of the checkout being built. `-dirty` is added when a tracked file
//! under `crates/`, or the root `Cargo.toml` or `Cargo.lock`, differs from that commit: the
//! sources a build reads. It is `unknown` when there is no git or no repository (a build from a
//! source archive). An `unknown` or `-dirty` build never matches a head, so the check fails
//! closed. The build reruns when HEAD, the branch it names, the packed refs, the index or any of
//! those sources change, so neither a new commit nor a new edit is reported under the old value.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The sources a build reads, relative to the repository root.
const SOURCES: &[&str] = &["crates", "Cargo.toml", "Cargo.lock"];

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let head = git(&dir, &["rev-parse", "HEAD"]).filter(|h| h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()));
    let top = git(&dir, &["rev-parse", "--show-toplevel"]).map(PathBuf::from);
    let commit = match (head, top) {
        (Some(head), Some(top)) => {
            for path in ["HEAD", "packed-refs", "index"] {
                if let Some(p) = git(&dir, &["rev-parse", "--path-format=absolute", "--git-path", path]) {
                    println!("cargo:rerun-if-changed={p}");
                }
            }
            if let Some(branch) = git(&dir, &["symbolic-ref", "-q", "HEAD"])
                && let Some(p) = git(&dir, &["rev-parse", "--path-format=absolute", "--git-path", &branch])
            {
                println!("cargo:rerun-if-changed={p}");
            }
            for source in SOURCES {
                println!("cargo:rerun-if-changed={}", top.join(source).display());
            }
            let mut status =
                vec!["-C", top.to_str().unwrap_or("."), "status", "--porcelain", "--untracked-files=no", "--"];
            status.extend(SOURCES);
            match Command::new("git").args(&status).output() {
                Ok(out) if out.status.success() && out.stdout.is_empty() => head,
                _ => format!("{head}-dirty"),
            }
        }
        _ => "unknown".to_string(),
    };
    println!("cargo:rustc-env=MAESTRO_BUILD_COMMIT={commit}");
}
