//! Read workspace files either from the working tree or from a git revision.
//!
//! The control plane's home rule is that main's copy wins, so the default is
//! `origin/main`. A working-tree read is explicit and labelled as such.

use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
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
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "worktree" | "working-tree" | "fs" => Self::WorkingTree,
            rev => Self::GitRev(rev.strip_prefix("git:").unwrap_or(rev).to_string()),
        })
    }
}

/// One file of a batch read: its path and its contents or the reason it failed.
pub type FileRead = (String, Result<String, TreeError>);

#[derive(Debug, thiserror::Error)]
pub enum TreeError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("git {args}: {stderr}")]
    Git { args: String, stderr: String },
}

#[derive(Debug, Clone)]
pub struct Tree {
    pub root: PathBuf,
    pub source: TreeSource,
    /// The commit a `GitRev` resolved to. Every read uses it, so a fetch mid-read cannot mix trees.
    pub commit: Option<String>,
}

impl Tree {
    pub fn new(root: impl Into<PathBuf>, source: TreeSource) -> Self {
        Self { root: root.into(), source, commit: None }
    }

    /// Like [`Tree::new`], resolving a git revision to its commit once.
    pub fn resolved(root: impl Into<PathBuf>, source: TreeSource) -> Self {
        let mut tree = Self::new(root, source);
        if let TreeSource::GitRev(rev) = &tree.source {
            let spec = format!("{rev}^{{commit}}");
            tree.commit = tree.git(&["rev-parse", "--verify", "--quiet", &spec]).ok().map(|s| s.trim().to_string());
        }
        tree
    }

    /// The revision to read: the resolved commit, else the name as given (which then fails visibly).
    fn rev(&self) -> Option<&str> {
        match &self.source {
            TreeSource::WorkingTree => None,
            TreeSource::GitRev(rev) => Some(self.commit.as_deref().unwrap_or(rev)),
        }
    }

    pub fn label(&self, rel: &str) -> String {
        match (&self.source, &self.commit) {
            (TreeSource::GitRev(rev), Some(c)) => format!("git:{rev}@{}:{rel}", &c[..c.len().min(12)]),
            _ => format!("{}:{rel}", self.source),
        }
    }

    /// File names (not paths) directly under `dir` ending in `suffix`, sorted.
    pub fn list(&self, dir: &str, suffix: &str) -> Result<Vec<String>, TreeError> {
        let mut names = match &self.source {
            TreeSource::WorkingTree => {
                let mut out = Vec::new();
                for entry in std::fs::read_dir(self.root.join(dir))? {
                    let entry = entry?;
                    if entry.file_type()?.is_file() {
                        out.push(entry.file_name().to_string_lossy().into_owned());
                    }
                }
                out
            }
            TreeSource::GitRev(_) => {
                let spec = format!("{}:{}", self.rev().unwrap_or_default(), dir.trim_end_matches('/'));
                // -z: names come back verbatim, never quoted, whatever bytes they hold.
                let out = self.git(&["ls-tree", "-z", "--name-only", &spec])?;
                out.split('\0').filter(|n| !n.is_empty()).map(str::to_string).collect()
            }
        };
        names.retain(|n| n.ends_with(suffix) && !n.starts_with('.'));
        names.sort();
        Ok(names)
    }

    pub fn read(&self, rel: &str) -> Result<String, TreeError> {
        match &self.source {
            TreeSource::WorkingTree => Ok(std::fs::read_to_string(self.root.join(rel))?),
            TreeSource::GitRev(_) => self.git(&["show", &format!("{}:{rel}", self.rev().unwrap_or_default())]),
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
            TreeSource::GitRev(_) => match self.cat_file_batch(self.rev().unwrap_or_default(), rels) {
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
        let mut reader = BufReader::new(child.stdout.take().expect("piped stdout"));
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
                let _ = child.kill();
                let _ = child.wait();
                return Err(TreeError::Git {
                    args: "cat-file --batch".into(),
                    stderr: format!("unexpected header `{header}`"),
                });
            };
            let mut buf = vec![0u8; size + 1]; // trailing newline
            reader.read_exact(&mut buf)?;
            buf.pop();
            out.push((rel.clone(), Ok(String::from_utf8_lossy(&buf).into_owned())));
        }
        let _ = writer.join();
        let _ = child.wait();
        Ok(out)
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
