//! The commit this binary was built from, for `maestro --version` (BRO-3842). kernel-ready@1's
//! `installed` node reads it to tell whether the installed binary is the head under check.
//!
//! It is `git rev-parse HEAD` of the checkout being built. `-dirty` is added when the build could
//! have read something that commit does not hold: a tracked file changed anywhere, or an untracked
//! file (one git does not ignore) inside any top-level entry of the commit. A build reads more than
//! `crates/` (`agents/` is compiled in, P20 round 1), so every entry counts. It is `unknown` when
//! there is no git or no repository (a build from a source archive). An `unknown` or `-dirty`
//! build never matches a head, so the check fails closed.
//!
//! The build reruns when HEAD, the branch it names, the packed refs, the index or anything inside
//! a top-level entry of the commit changes, and the dirty state is read over exactly those
//! entries, so what it reports is never older than what it watches (P20 round 2).
//!
//! What it names is the sources, not the whole build. Configuration Cargo reads from outside the
//! commit is not recorded: a `.cargo/config.toml` at the top level or above it or in `CARGO_HOME`,
//! `RUSTFLAGS` and the like, and the toolchain (P20 round 3). Whoever can set those can also
//! replace `~/.local/bin/maestro` itself, the user-level write the owner accepted as residual risk
//! (BRO-3635), so the check claims the commit the binary's sources came from, and no more.

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
            // NUL-separated, so a name git would quote (non-ASCII, say) stays exact.
            let listed = git(&top, &["ls-tree", "-z", "--name-only", "HEAD"]);
            let entries: Vec<&str> =
                listed.as_deref().map_or_else(Vec::new, |l| l.split('\0').filter(|e| !e.is_empty()).collect());
            for entry in &entries {
                println!("cargo:rerun-if-changed={}", top.join(entry).display());
            }
            let mut status = Command::new("git");
            // Untracked files are listed whatever the user's status.showUntrackedFiles says.
            status
                .arg("-C")
                .arg(&top)
                .args(["--literal-pathspecs", "status", "--porcelain", "--untracked-files=normal", "--"])
                .args(&entries);
            match (listed.is_some() && !entries.is_empty()).then(|| status.output()) {
                Some(Ok(out)) if out.status.success() && out.stdout.is_empty() => head,
                _ => format!("{head}-dirty"),
            }
        }
        _ => "unknown".to_string(),
    };
    println!("cargo:rustc-env=MAESTRO_BUILD_COMMIT={commit}");
}
