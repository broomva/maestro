//! Read workspace files either from the working tree or from a git revision.
//!
//! The control plane's home rule is that main's copy wins, so the default is
//! `origin/main`. A working-tree read is explicit and labelled as such.

use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeSource {
    WorkingTree,
    GitRev(String),
}

impl fmt::Display for TreeSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkingTree => f.write_str("worktree"),
            Self::GitRev(rev) => write!(f, "git:{rev}"),
        }
    }
}

impl std::str::FromStr for TreeSource {
    type Err = String;
    /// A revision may not start with `-`: git would read it as an option.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "worktree" | "working-tree" | "fs" => Self::WorkingTree,
            rev => {
                let rev = rev.strip_prefix("git:").unwrap_or(rev);
                if rev.is_empty() || rev.starts_with('-') {
                    return Err(format!("`{s}` is not a revision: a tree is `worktree` or a git revision"));
                }
                Self::GitRev(rev.to_string())
            }
        })
    }
}

/// The most any one read takes in. Ledgers stop at a quarter of this (`LEDGER_LIMIT`).
pub const MAX_READ: usize = 4 * 1024 * 1024;

/// A directory listing: the files to read, and entries skipped with the reason.
#[derive(Debug, Default)]
pub struct Listing {
    pub names: Vec<String>,
    pub skipped: Vec<(String, String)>,
}

/// One file of a batch read: its path and its contents or the reason it failed.
pub type FileRead = (String, Result<String, TreeError>);

#[derive(Debug, thiserror::Error)]
pub enum TreeError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("git {args}: {stderr}")]
    Git { args: String, stderr: String },
    /// A file that is not UTF-8. The plugin decodes it with replacement chars; Maestro reads
    /// UTF-8 only, so its byte size is the size the plugin's ledger limit measures.
    #[error("{rel} is not valid UTF-8")]
    NotUtf8 { rel: String },
    /// A file past [`MAX_READ`]: not read, so no file can make a read allocate without bound.
    #[error("{rel} is {size} bytes, over the {MAX_READ}-byte read limit")]
    TooLarge { rel: String, size: u64 },
    /// The revision named no commit, so nothing is read from it: not the raw name, not a guess.
    #[error("revision `{rev}` did not resolve: {reason}")]
    Unresolved { rev: String, reason: String },
}

#[derive(Debug, Clone)]
pub struct Tree {
    pub root: PathBuf,
    pub source: TreeSource,
    /// The commit a `GitRev` resolved to. Every read uses it, so a fetch mid-read cannot mix trees.
    pub commit: Option<String>,
    /// Why a `GitRev` did not resolve. Every read then fails with it.
    pub unresolved: Option<String>,
}

impl Tree {
    pub fn new(root: impl Into<PathBuf>, source: TreeSource) -> Self {
        Self { root: root.into(), source, commit: None, unresolved: None }
    }

    /// Like [`Tree::new`], resolving a git revision to its commit once.
    pub fn resolved(root: impl Into<PathBuf>, source: TreeSource) -> Self {
        let mut tree = Self::new(root, source);
        if let TreeSource::GitRev(rev) = &tree.source {
            if rev.starts_with('-') {
                tree.unresolved = Some("a revision may not start with `-`".into());
                return tree;
            }
            let spec = format!("{rev}^{{commit}}");
            // No `--end-of-options` (git >= 2.30): the leading-dash check above already keeps
            // the revision from reading as an option, and older git would refuse the flag.
            match tree.git(&["rev-parse", "--verify", "--quiet", &spec]) {
                Ok(out) if !out.trim().is_empty() => tree.commit = Some(out.trim().to_string()),
                // `--quiet` fails with no message when the revision names nothing.
                Ok(_) | Err(TreeError::Git { .. }) => {
                    tree.unresolved = Some(format!("no commit is named `{rev}` in {}", tree.root.display()))
                }
                Err(e) => tree.unresolved = Some(e.to_string()),
            }
        }
        tree
    }

    /// The commit to read. A revision that did not resolve is an error, never read by name.
    fn rev(&self) -> Result<&str, TreeError> {
        match (&self.source, &self.commit) {
            (TreeSource::GitRev(_), Some(commit)) => Ok(commit),
            (TreeSource::GitRev(rev), None) => Err(TreeError::Unresolved {
                rev: rev.clone(),
                reason: self.unresolved.clone().unwrap_or_else(|| "never resolved (use Tree::resolved)".into()),
            }),
            (TreeSource::WorkingTree, _) => Ok(""),
        }
    }

    pub fn label(&self, rel: &str) -> String {
        match (&self.source, &self.commit) {
            (TreeSource::GitRev(rev), Some(c)) => format!("git:{rev}@{}:{rel}", &c[..c.len().min(12)]),
            (TreeSource::GitRev(rev), None) => format!("git:{rev}@unresolved:{rel}"),
            _ => format!("{}:{rel}", self.source),
        }
    }

    /// Regular files directly under `dir` ending in one of `suffixes`, sorted, and
    /// the matching names that are not regular files (a symlink, a directory named
    /// `x.yaml`). Dot files are never listed.
    pub fn list(&self, dir: &str, suffixes: &[&str]) -> Result<Listing, TreeError> {
        let wanted = |n: &str| suffixes.iter().any(|s| n.ends_with(s)) && !n.starts_with('.');
        let mut listing = Listing::default();
        match &self.source {
            TreeSource::WorkingTree => {
                for entry in std::fs::read_dir(self.root.join(dir))? {
                    let entry = entry?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !wanted(&name) {
                        continue;
                    }
                    // As the plugin's `lstat`: anything but a regular file, a symlink or a
                    // directory named like a ledger included, is reported, never skipped.
                    if entry.file_type()?.is_file() {
                        listing.names.push(name);
                    } else {
                        listing.skipped.push((name, "not a regular file".into()));
                    }
                }
            }
            TreeSource::GitRev(_) => {
                let spec = format!("{}:{}", self.rev()?, dir.trim_end_matches('/'));
                // -z: names come back verbatim, never quoted, whatever bytes they hold.
                // Each record is `<mode> <type> <oid>\t<name>`. A regular file is a blob of
                // mode 100644 or 100755; 120000 is a symlink, and a tree is a directory.
                let out = self.git(&["ls-tree", "-z", &spec])?;
                for record in out.split('\0') {
                    let Some((meta, name)) = record.split_once('\t') else { continue };
                    if !wanted(name) {
                        continue;
                    }
                    let mut fields = meta.split(' ');
                    match (fields.next(), fields.next()) {
                        (Some("100644" | "100755"), Some("blob")) => listing.names.push(name.to_string()),
                        _ => listing.skipped.push((name.to_string(), "not a regular file".into())),
                    }
                }
            }
        }
        listing.names.sort();
        listing.skipped.sort();
        Ok(listing)
    }

    pub fn read(&self, rel: &str) -> Result<String, TreeError> {
        match &self.source {
            TreeSource::WorkingTree => {
                // Read through `take`, so a file that grows after it is opened is still bounded.
                let file = std::fs::File::open(self.root.join(rel))?;
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut std::io::Read::take(file, MAX_READ as u64 + 1), &mut bytes)?;
                if bytes.len() > MAX_READ {
                    return Err(TreeError::TooLarge { rel: rel.to_string(), size: bytes.len() as u64 });
                }
                utf8(rel, bytes)
            }
            TreeSource::GitRev(_) => self.git(&["show", &format!("{}:{rel}", self.rev()?)]),
        }
    }

    /// Read many files at once. For a git revision this is one `cat-file --batch` process.
    pub fn read_many(&self, rels: &[String]) -> Vec<FileRead> {
        match &self.source {
            TreeSource::WorkingTree => rels.iter().map(|r| (r.clone(), self.read(r))).collect(),
            // The batch protocol is line-based; a path holding a newline is read on its own.
            TreeSource::GitRev(_) if rels.iter().any(|r| r.contains('\n')) => {
                rels.iter().map(|r| (r.clone(), self.read(r))).collect()
            }
            TreeSource::GitRev(_) => match self.rev().and_then(|rev| self.cat_file_batch(rev, rels)) {
                Ok(results) => results,
                Err(e) => {
                    let reason = e.to_string();
                    rels.iter()
                        .map(|r| (r.clone(), Err(TreeError::Git { args: "cat-file".into(), stderr: reason.clone() })))
                        .collect()
                }
            },
        }
    }

    fn cat_file_batch(&self, rev: &str, rels: &[String]) -> Result<Vec<FileRead>, TreeError> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["--no-pager", "cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let request: String = rels.iter().map(|r| format!("{rev}:{r}\n")).collect();
        // Write on a thread so a large batch cannot deadlock against a full stdout pipe.
        let writer = std::thread::spawn(move || stdin.write_all(request.as_bytes()));
        let result = read_batch(BufReader::new(child.stdout.take().expect("piped stdout")), rev, rels);
        reap(child, writer, result.is_err());
        result
    }

    fn git(&self, args: &[&str]) -> Result<String, TreeError> {
        let output = Command::new("git").arg("-C").arg(&self.root).arg("--no-pager").args(args).output()?;
        if !output.status.success() {
            return Err(TreeError::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn utf8(rel: &str, bytes: Vec<u8>) -> Result<String, TreeError> {
    String::from_utf8(bytes).map_err(|_| TreeError::NotUtf8 { rel: rel.to_string() })
}

/// Read `cat-file --batch` answers for `rels`, in order.
fn read_batch(mut reader: impl BufRead, rev: &str, rels: &[String]) -> Result<Vec<FileRead>, TreeError> {
    let mut out = Vec::with_capacity(rels.len());
    for rel in rels {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        let header = header.trim_end();
        if header == format!("{rev}:{rel} missing") {
            out.push((
                rel.clone(),
                Err(TreeError::Git { args: format!("show {rev}:{rel}"), stderr: "missing".into() }),
            ));
            continue;
        }
        // `<oid> <type> <size>`. Anything else means the stream is out of step: stop, do not guess.
        let parts: Vec<&str> = header.split(' ').collect();
        let size = match parts.as_slice() {
            [_, _, size] => size.parse::<usize>().ok(),
            _ => None,
        };
        let Some(size) = size else {
            return Err(TreeError::Git {
                args: "cat-file --batch".into(),
                stderr: format!("unexpected header `{header}`"),
            });
        };
        if size > MAX_READ {
            // Skip the blob to stay in step with the stream, without holding it.
            std::io::copy(&mut std::io::Read::take(&mut reader, size as u64 + 1), &mut std::io::sink())?;
            out.push((rel.clone(), Err(TreeError::TooLarge { rel: rel.clone(), size: size as u64 })));
            continue;
        }
        let mut buf = vec![0u8; size + 1]; // trailing newline
        reader.read_exact(&mut buf)?;
        buf.pop();
        out.push((rel.clone(), utf8(rel, buf)));
    }
    Ok(out)
}

/// End a batch process on every path: killed first when the read failed (it may be
/// blocked writing), then always waited for, so no exit leaves a zombie behind.
fn reap(mut child: std::process::Child, writer: std::thread::JoinHandle<std::io::Result<()>>, failed: bool) {
    if failed {
        let _ = child.kill();
    }
    let _ = child.wait();
    let _ = writer.join();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_revision_never_starts_with_a_dash() {
        assert!("--output=/tmp/x".parse::<TreeSource>().is_err());
        assert!("git:-x".parse::<TreeSource>().is_err());
        assert!("".parse::<TreeSource>().is_err());
        assert_eq!("origin/main".parse::<TreeSource>(), Ok(TreeSource::GitRev("origin/main".into())));
        assert_eq!("worktree".parse::<TreeSource>(), Ok(TreeSource::WorkingTree));
    }

    #[test]
    fn an_unresolved_revision_reads_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let ok = Command::new("git").arg("-C").arg(tmp.path()).args(["init", "-q"]).status().unwrap();
        assert!(ok.success());
        for rev in ["no-such-branch", "-x"] {
            let tree = Tree::resolved(tmp.path(), TreeSource::GitRev(rev.into()));
            assert!(tree.commit.is_none() && tree.unresolved.is_some(), "{rev}");
            assert!(matches!(tree.list(".control/asks", &[".yaml"]), Err(TreeError::Unresolved { .. })), "{rev}");
            assert!(tree.unresolved.as_deref().is_some_and(|r| !r.trim().is_empty()), "a reason, not an empty one");
            assert!(matches!(tree.read("a.yaml"), Err(TreeError::Unresolved { .. })), "{rev}");
            let many = tree.read_many(&["a.yaml".to_string()]);
            assert!(many[0].1.as_ref().is_err_and(|e| e.to_string().contains("did not resolve")), "{rev}");
        }
    }

    /// A source built directly, not parsed, is held to the same rule: git never sees the dash.
    #[test]
    fn a_dash_revision_is_refused_before_git_runs() {
        let tmp = tempfile::tempdir().unwrap();
        for rev in ["-x", "--output=/tmp/maestro-must-not-exist"] {
            let tree = Tree::resolved(tmp.path(), TreeSource::GitRev(rev.into()));
            assert_eq!(tree.unresolved.as_deref(), Some("a revision may not start with `-`"), "{rev}");
        }
    }

    #[test]
    fn an_out_of_step_batch_stream_is_an_error() {
        let rels = vec!["a.yaml".to_string()];
        assert!(read_batch(std::io::Cursor::new(b"garbage\n".to_vec()), "c", &rels).is_err());
        assert!(read_batch(std::io::Cursor::new(Vec::new()), "c", &rels).is_err(), "EOF mid-batch");
        let good = read_batch(std::io::Cursor::new(b"oid blob 2\nhi\n".to_vec()), "c", &rels).unwrap();
        assert_eq!(good[0].1.as_ref().unwrap(), "hi");
    }

    #[test]
    fn a_failed_batch_leaves_no_process_behind() {
        let mut child = Command::new("sleep").arg("30").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let pid = child.id().to_string();
        let stdin = child.stdin.take().unwrap();
        let writer = std::thread::spawn(move || {
            drop(stdin);
            Ok(())
        });
        let started = std::time::Instant::now();
        reap(child, writer, true);
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "killed, not waited out");
        // `kill -0` succeeds on a zombie, so a failure here means the process was reaped.
        let alive = Command::new("kill").args(["-0", &pid]).stderr(Stdio::null()).status().unwrap().success();
        assert!(!alive, "process {pid} is still in the table");
    }
}
