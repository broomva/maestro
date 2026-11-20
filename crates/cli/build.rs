//! The commit this binary was built from, for `maestro --version` (BRO-3842). kernel-ready@1's
//! `installed` node reads it to tell whether the installed binary is the head under check.
//!
//! It is `git rev-parse HEAD` of the checkout being built. `-dirty` is added when the checkout is
//! not exactly that commit: any tracked file changed, or any untracked file git does not ignore.
//! A build reads more than `crates/` (`agents/` is compiled in, P20 round 1), so the whole
//! checkout counts, and an edit anywhere is reported rather than guessed harmless. It is `unknown`
//! when there is no git or no repository (a build from a source archive). An `unknown` or
//! `-dirty` build never matches a head, so the check fails closed. The build reruns when HEAD, the
//! branch it names, the packed refs, the index or any top-level entry of the commit changes, so
//! neither a new commit nor a new edit is reported under the old value.

use std::path::{Path, PathBuf};
use std::process::Command;

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
            // Every top-level entry of the commit (a folder is watched whole), never the untracked
            // build output beside them, which would rerun this on every build.
            for entry in git(&top, &["ls-tree", "--name-only", "HEAD"]).unwrap_or_default().lines() {
                println!("cargo:rerun-if-changed={}", top.join(entry).display());
            }
            match Command::new("git").arg("-C").arg(&top).args(["status", "--porcelain"]).output() {
                Ok(out) if out.status.success() && out.stdout.is_empty() => head,
                _ => format!("{head}-dirty"),
            }
        }
        _ => "unknown".to_string(),
    };
    println!("cargo:rustc-env=MAESTRO_BUILD_COMMIT={commit}");
}
