//! The shared checkout for Maestro's ledger writes.
use std::path::{Path, PathBuf};
use std::process::Command;

pub const BRANCH: &str = "maestro/ledger";

#[derive(Debug, Clone)]
pub struct Writer {
    pub workspace: PathBuf,
    pub checkout: PathBuf,
    pub owner_dir: PathBuf,
    pub remote: bool,
}

fn clean_git_env(command: &mut Command) {
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
}

fn git_command(dir: &Path) -> Command {
    let mut command = Command::new(crate::tree::git());
    clean_git_env(&mut command);
    command.arg("-C").arg(dir).env("GIT_TERMINAL_PROMPT", "0");
    command
}

fn gh_command(dir: &Path) -> Command {
    let mut command = Command::new("gh");
    clean_git_env(&mut command);
    command.env_remove("GH_REPO").env_remove("GH_HOST").current_dir(dir);
    command
}

pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = git_command(dir).args(args).output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// The open landing PR's number, if one is open.
fn landing_pr(dir: &Path) -> Result<Option<String>, String> {
    let out = gh_command(dir)
        .args([
            "pr",
            "list",
            "--head",
            BRANCH,
            "--base",
            "main",
            "--state",
            "open",
            "--json",
            "number",
            "--jq",
            ".[0].number // empty",
        ])
        .output()
        .map_err(|e| format!("gh: {e}"))?;
    if !out.status.success() {
        return Err(format!("gh pr list: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let n = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((!n.is_empty()).then_some(n))
}

/// Resolve existing ancestors too, so a new checkout cannot enter the workspace through a symlink.
fn checkout_target(path: &Path) -> Result<PathBuf, String> {
    let absolute = crate::views::lexical(&std::path::absolute(path).map_err(|e| e.to_string())?);
    let mut ancestor = absolute.as_path();
    while !ancestor.exists() {
        ancestor = ancestor.parent().ok_or("checkout has no existing ancestor")?;
    }
    let resolved = ancestor.canonicalize().map_err(|e| e.to_string())?;
    Ok(resolved.join(absolute.strip_prefix(ancestor).map_err(|e| e.to_string())?))
}

/// Maestro's checkout, made if missing, reset to a clean tree (nothing staged, nothing untracked)
/// at the base the answer goes on: `maestro/ledger` with main merged in when the branch exists,
/// so an answer it holds that main lacks is kept, never pushed over; else main itself. Returns
/// the branch's tip when it exists.
pub fn prepare(w: &Writer) -> Result<Option<String>, String> {
    let workspace = w.workspace.canonicalize().map_err(|e| format!("workspace: {e}"))?;
    let checkout = checkout_target(&w.checkout).map_err(|e| format!("checkout: {e}"))?;
    if checkout.starts_with(&workspace) || workspace.starts_with(&checkout) {
        return Err("Maestro's checkout must be separate from the workspace".into());
    }
    if !w.checkout.join(".git").exists() {
        if let Some(parent) = w.checkout.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        git(&w.workspace, &["worktree", "add", "--detach", &w.checkout.to_string_lossy(), "HEAD"])?;
    }
    let common = |root: &Path| -> Result<PathBuf, String> {
        let path = PathBuf::from(git(root, &["rev-parse", "--git-common-dir"])?);
        let path = if path.is_absolute() { path } else { root.join(path) };
        path.canonicalize().map_err(|e| format!("git common directory: {e}"))
    };
    if common(&w.checkout)? != common(&w.workspace)? {
        return Err("Maestro's checkout belongs to another repository".into());
    }
    let listed = git(&w.workspace, &["worktree", "list", "--porcelain", "-z"])?;
    let registered = listed
        .split('\0')
        .filter_map(|line| line.strip_prefix("worktree "))
        .filter_map(|path| Path::new(path).canonicalize().ok())
        .any(|path| path == checkout);
    if !registered {
        return Err("Maestro's checkout is not a registered worktree".into());
    }
    let actual = PathBuf::from(git(&w.checkout, &["rev-parse", "--show-toplevel"])?);
    let actual = actual.canonicalize().map_err(|e| format!("checkout root: {e}"))?;
    if actual != checkout {
        return Err("Maestro's checkout resolves to another working tree".into());
    }
    // Whatever another process left here, staged or not, is dropped before anything is written.
    let _ = git(&w.checkout, &["merge", "--abort"]);
    git(&w.checkout, &["reset", "-q", "--hard"])?;
    git(&w.checkout, &["clean", "-q", "-f", "-d", "-x"])?;
    let tip = if w.remote {
        git(&w.checkout, &["fetch", "-q", "origin", "main"])?;
        if git(&w.checkout, &["ls-remote", "--heads", "origin", BRANCH])?.is_empty() {
            None
        } else {
            git(
                &w.checkout,
                &["fetch", "-q", "origin", &format!("+refs/heads/{BRANCH}:refs/remotes/origin/{BRANCH}")],
            )?;
            Some(git(&w.checkout, &["rev-parse", &format!("refs/remotes/origin/{BRANCH}")])?)
        }
    } else {
        None
    };
    let main = if w.remote { "refs/remotes/origin/main" } else { "HEAD" };
    match &tip {
        Some(tip) => {
            git(&w.checkout, &["checkout", "-q", "--detach", tip])?;
            let merged = git(
                &w.checkout,
                &[
                    "-c",
                    "user.name=Maestro",
                    "-c",
                    "user.email=maestro@broomva.local",
                    "merge",
                    "-q",
                    "--no-edit",
                    main,
                ],
            );
            if let Err(e) = merged {
                let _ = git(&w.checkout, &["merge", "--abort"]);
                return Err(format!("main does not merge into {BRANCH}: {e}"));
            }
        }
        None => {
            let at = git(&w.checkout, &["rev-parse", main])?;
            git(&w.checkout, &["checkout", "-q", "--detach", &at])?;
        }
    }
    Ok(tip)
}

/// One write at a time across every Maestro process: an exclusive lock on a file beside the checkout.
pub fn exclusive(w: &Writer) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(&w.owner_dir).map_err(|e| e.to_string())?;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(w.owner_dir.join("write.lock"))
        .map_err(|e| format!("write lock: {e}"))?;
    f.lock().map_err(|e| format!("write lock: {e}"))?;
    Ok(f)
}

/// Commit exactly one ledger file, verify its bytes, and push by fast-forward.
/// The caller must hold the checkout lock.
pub fn commit_and_push(w: &Writer, rel: &str, message: &str, bytes: &[u8]) -> Result<String, String> {
    git(&w.checkout, &["add", "--", rel])?;
    // A pathspec commit takes this one file whatever else is staged.
    git(
        &w.checkout,
        &[
            "-c",
            "user.name=Maestro",
            "-c",
            "user.email=maestro@broomva.local",
            "commit",
            "-q",
            "-m",
            message,
            "--",
            rel,
        ],
    )?;
    // The commit holds exactly the signed edit: one file, these bytes.
    let changed = git(&w.checkout, &["diff", "--name-only", "HEAD~1", "HEAD"])?;
    let committed = git_command(&w.checkout)
        .args(["cat-file", "blob", &format!("HEAD:{rel}")])
        .output()
        .map_err(|e| format!("git cat-file: {e}"))?
        .stdout;
    if changed != rel || committed != bytes {
        return Err(format!("the commit is not exactly the intended ledger edit ({changed}); nothing was pushed"));
    }
    let commit = git(&w.checkout, &["rev-parse", "HEAD"])?;
    if w.remote {
        // A fast-forward only: the base holds the branch's tip, so nothing on it is pushed over.
        if let Err(e) = git(&w.checkout, &["push", "-q", "origin", &format!("HEAD:refs/heads/{BRANCH}")]) {
            // The remote may have taken it before the reply was lost: its tip decides.
            let remote =
                git(&w.checkout, &["ls-remote", "origin", &format!("refs/heads/{BRANCH}")]).unwrap_or_default();
            if remote.split_whitespace().next() != Some(commit.as_str()) {
                return Err(e);
            }
        }
    }
    Ok(commit)
}

/// Open the landing PR if none is open, and set it to merge when its checks pass.
pub fn land(w: &Writer) -> String {
    let dir = &w.checkout;
    let number = match landing_pr(dir) {
        Ok(Some(n)) => n,
        Ok(None) => {
            let out = gh_command(dir)
                .args([
                    "pr",
                    "create",
                    "--head",
                    BRANCH,
                    "--base",
                    "main",
                    "--title",
                    "ledger: asks and owner answers",
                    "--body",
                    "Asks filed by Maestro and answers signed by the owner. This PR brings the ledger to main.",
                ])
                .output();
            match out {
                Ok(o) if o.status.success() => {
                    String::from_utf8_lossy(&o.stdout).trim().rsplit('/').next().unwrap_or("").to_string()
                }
                Ok(o) => return format!("no landing PR: {}", String::from_utf8_lossy(&o.stderr).trim()),
                Err(e) => return format!("no landing PR: {e}"),
            }
        }
        Err(e) => return format!("no landing PR: {e}"),
    };
    let merged = gh_command(dir).args(["pr", "merge", &number, "--auto", "--squash"]).output();
    match merged {
        Ok(o) if o.status.success() => format!("landing PR #{number}, set to merge when its checks pass"),
        Ok(o) => format!("landing PR #{number}; auto-merge not set: {}", String::from_utf8_lossy(&o.stderr).trim()),
        Err(e) => format!("landing PR #{number}; auto-merge not set: {e}"),
    }
}
