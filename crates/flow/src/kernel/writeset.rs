//! Case-folded POSIX segment-DP write-set glob intersection and 6-surface post-write verification
//! for Maestro Flow Kernel IR v10 (`ready_when: disjoint_writes`).
//!
//! Every concrete path `p` is treated as `p ∪ p/**` so a directory claim collides with files under
//! it. Patterns are normalized to lowercase POSIX segments so case-insensitive filesystems (macOS
//! APFS) cannot alias `Foo.rs` and `foo.rs`.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// Result of verifying all 6 write surfaces of a child lane worktree against its declared
/// `write_set`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteVerification {
    pub checked_paths: Vec<String>,
    pub violations: Vec<String>,
}

impl WriteVerification {
    pub fn is_ok(&self) -> bool {
        self.violations.is_empty()
    }
}

/// Complete Unicode Case Folding (Unicode CaseFolding.txt `C + F` mappings across all 0..=0x10FFFF
/// scalar values). Every code point where official Unicode default full case folding differs from
/// `char::to_lowercase()` is listed explicitly below (including `ß`/`ẞ` -> `ss`, `ϐ` -> `β`,
/// `ς` -> `σ`, Cherokee, Greek extended, ligatures, and standard `İ` -> `i\u{0307}` via `to_lowercase`).
fn unicode_case_fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\u{00B5}' => out.push('\u{03BC}'),
            '\u{00DF}' => out.push_str("\u{0073}\u{0073}"),
            '\u{0149}' => out.push_str("\u{02BC}\u{006E}"),
            '\u{017F}' => out.push('\u{0073}'),
            '\u{01F0}' => out.push_str("\u{006A}\u{030C}"),
            '\u{0345}' => out.push('\u{03B9}'),
            '\u{0390}' => out.push_str("\u{03B9}\u{0308}\u{0301}"),
            '\u{03B0}' => out.push_str("\u{03C5}\u{0308}\u{0301}"),
            '\u{03C2}' => out.push('\u{03C3}'),
            '\u{03D0}' => out.push('\u{03B2}'),
            '\u{03D1}' => out.push('\u{03B8}'),
            '\u{03D5}' => out.push('\u{03C6}'),
            '\u{03D6}' => out.push('\u{03C0}'),
            '\u{03F0}' => out.push('\u{03BA}'),
            '\u{03F1}' => out.push('\u{03C1}'),
            '\u{03F5}' => out.push('\u{03B5}'),
            '\u{0587}' => out.push_str("\u{0565}\u{0582}"),
            '\u{13A0}'..='\u{13F5}' => out.push(ch),
            '\u{13F8}'..='\u{13FD}' => {
                if let Some(c) = char::from_u32(ch as u32 - 8) {
                    out.push(c);
                }
            }
            '\u{1C80}' => out.push('\u{0432}'),
            '\u{1C81}' => out.push('\u{0434}'),
            '\u{1C82}' => out.push('\u{043E}'),
            '\u{1C83}' => out.push('\u{0441}'),
            '\u{1C84}' | '\u{1C85}' => out.push('\u{0442}'),
            '\u{1C86}' => out.push('\u{044A}'),
            '\u{1C87}' => out.push('\u{0463}'),
            '\u{1C88}' => out.push('\u{A64B}'),
            '\u{1E96}' => out.push_str("\u{0068}\u{0331}"),
            '\u{1E97}' => out.push_str("\u{0074}\u{0308}"),
            '\u{1E98}' => out.push_str("\u{0077}\u{030A}"),
            '\u{1E99}' => out.push_str("\u{0079}\u{030A}"),
            '\u{1E9A}' => out.push_str("\u{0061}\u{02BE}"),
            '\u{1E9B}' => out.push('\u{1E61}'),
            '\u{1E9E}' => out.push_str("\u{0073}\u{0073}"),
            '\u{1F50}' => out.push_str("\u{03C5}\u{0313}"),
            '\u{1F52}' => out.push_str("\u{03C5}\u{0313}\u{0300}"),
            '\u{1F54}' => out.push_str("\u{03C5}\u{0313}\u{0301}"),
            '\u{1F56}' => out.push_str("\u{03C5}\u{0313}\u{0342}"),
            '\u{1F80}'..='\u{1F87}' => {
                if let Some(c) = char::from_u32(0x1F00 + (ch as u32 - 0x1F80)) {
                    out.push(c);
                }
                out.push('\u{03B9}');
            }
            '\u{1F88}'..='\u{1F8F}' => {
                if let Some(c) = char::from_u32(0x1F00 + (ch as u32 - 0x1F88)) {
                    out.push(c);
                }
                out.push('\u{03B9}');
            }
            '\u{1F90}'..='\u{1F97}' => {
                if let Some(c) = char::from_u32(0x1F20 + (ch as u32 - 0x1F90)) {
                    out.push(c);
                }
                out.push('\u{03B9}');
            }
            '\u{1F98}'..='\u{1F9F}' => {
                if let Some(c) = char::from_u32(0x1F20 + (ch as u32 - 0x1F98)) {
                    out.push(c);
                }
                out.push('\u{03B9}');
            }
            '\u{1FA0}'..='\u{1FA7}' => {
                if let Some(c) = char::from_u32(0x1F60 + (ch as u32 - 0x1FA0)) {
                    out.push(c);
                }
                out.push('\u{03B9}');
            }
            '\u{1FA8}'..='\u{1FAF}' => {
                if let Some(c) = char::from_u32(0x1F60 + (ch as u32 - 0x1FA8)) {
                    out.push(c);
                }
                out.push('\u{03B9}');
            }
            '\u{1FB2}' => out.push_str("\u{1F70}\u{03B9}"),
            '\u{1FB3}' | '\u{1FBC}' => out.push_str("\u{03B1}\u{03B9}"),
            '\u{1FB4}' => out.push_str("\u{03AC}\u{03B9}"),
            '\u{1FB6}' => out.push_str("\u{03B1}\u{0342}"),
            '\u{1FB7}' => out.push_str("\u{03B1}\u{0342}\u{03B9}"),
            '\u{1FBE}' => out.push('\u{03B9}'),
            '\u{1FC2}' => out.push_str("\u{1F74}\u{03B9}"),
            '\u{1FC3}' | '\u{1FCC}' => out.push_str("\u{03B7}\u{03B9}"),
            '\u{1FC4}' => out.push_str("\u{03AE}\u{03B9}"),
            '\u{1FC6}' => out.push_str("\u{03B7}\u{0342}"),
            '\u{1FC7}' => out.push_str("\u{03B7}\u{0342}\u{03B9}"),
            '\u{1FD2}' => out.push_str("\u{03B9}\u{0308}\u{0300}"),
            '\u{1FD3}' => out.push_str("\u{03B9}\u{0308}\u{0301}"),
            '\u{1FD6}' => out.push_str("\u{03B9}\u{0342}"),
            '\u{1FD7}' => out.push_str("\u{03B9}\u{0308}\u{0342}"),
            '\u{1FE2}' => out.push_str("\u{03C5}\u{0308}\u{0300}"),
            '\u{1FE3}' => out.push_str("\u{03C5}\u{0308}\u{0301}"),
            '\u{1FE4}' => out.push_str("\u{03C1}\u{0313}"),
            '\u{1FE6}' => out.push_str("\u{03C5}\u{0342}"),
            '\u{1FE7}' => out.push_str("\u{03C5}\u{0308}\u{0342}"),
            '\u{1FF2}' => out.push_str("\u{1F7C}\u{03B9}"),
            '\u{1FF3}' | '\u{1FFC}' => out.push_str("\u{03C9}\u{03B9}"),
            '\u{1FF4}' => out.push_str("\u{03CE}\u{03B9}"),
            '\u{1FF6}' => out.push_str("\u{03C9}\u{0342}"),
            '\u{1FF7}' => out.push_str("\u{03C9}\u{0342}\u{03B9}"),
            '\u{AB70}'..='\u{ABBF}' => {
                if let Some(c) = char::from_u32(ch as u32 - 0x97D0) {
                    out.push(c);
                }
            }
            '\u{FB00}' => out.push_str("ff"),
            '\u{FB01}' => out.push_str("fi"),
            '\u{FB02}' => out.push_str("fl"),
            '\u{FB03}' => out.push_str("ffi"),
            '\u{FB04}' => out.push_str("ffl"),
            '\u{FB05}' | '\u{FB06}' => out.push_str("st"),
            '\u{FB13}' => out.push_str("\u{0574}\u{0576}"),
            '\u{FB14}' => out.push_str("\u{0574}\u{0565}"),
            '\u{FB15}' => out.push_str("\u{0574}\u{056B}"),
            '\u{FB16}' => out.push_str("\u{057E}\u{0576}"),
            '\u{FB17}' => out.push_str("\u{0574}\u{056D}"),
            _ => {
                for lc in ch.to_lowercase() {
                    out.push(lc);
                }
            }
        }
    }
    out
}

/// Normalizes a repo-relative write-set pattern or path to full Unicode case-folded POSIX segments.
/// Refuses empty strings, null bytes, leading `/`, and `..` traversal segments.
pub fn normalize_pattern(raw: &str) -> Result<String, String> {
    if raw.is_empty() || raw.contains('\0') {
        return Err(format!("invalid empty or NUL write_set pattern: {raw:?}"));
    }
    let slash = raw.replace('\\', "/");
    if slash.starts_with('/') {
        return Err(format!("absolute path forbidden in write_set: {raw:?}"));
    }
    let mut parts = Vec::new();
    for seg in slash.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return Err(format!("parent traversal (..) forbidden in write_set: {raw:?}"));
        }
        parts.push(unicode_case_fold(seg));
    }
    if parts.is_empty() {
        return Err(format!("write_set pattern resolves to empty path: {raw:?}"));
    }
    Ok(parts.join("/"))
}

/// Validates and normalizes every entry in a declared `write_set`.
pub fn validate_write_set(patterns: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::with_capacity(patterns.len());
    for p in patterns {
        out.push(normalize_pattern(p)?);
    }
    Ok(out)
}

/// Wildcard match on a single path segment (`*` matches 0+ Unicode chars, `?` matches 1 Unicode char).
fn seg_match(pat: &[char], target: &[char]) -> bool {
    let (m, n) = (pat.len(), target.len());
    let mut dp = vec![vec![false; n + 1]; m + 1];
    dp[0][0] = true;
    for i in 1..=m {
        if pat[i - 1] == '*' {
            dp[i][0] = dp[i - 1][0];
        }
    }
    for i in 1..=m {
        for j in 1..=n {
            if pat[i - 1] == '*' {
                dp[i][j] = dp[i - 1][j] || dp[i][j - 1];
            } else if pat[i - 1] == '?' || pat[i - 1] == target[j - 1] {
                dp[i][j] = dp[i - 1][j - 1];
            }
        }
    }
    dp[m][n]
}

/// Returns true if two single-segment glob patterns can both match at least one common non-empty
/// segment string (exact DP over `*` and `?` on Unicode scalar chars).
fn seg_patterns_intersect(a: &[char], b: &[char]) -> bool {
    let (m, n) = (a.len(), b.len());
    let mut dp = vec![vec![false; n + 1]; m + 1];
    dp[0][0] = true;
    for i in 1..=m {
        if a[i - 1] == '*' {
            dp[i][0] = dp[i - 1][0];
        }
    }
    for j in 1..=n {
        if b[j - 1] == '*' {
            dp[0][j] = dp[0][j - 1];
        }
    }
    for i in 1..=m {
        for j in 1..=n {
            let ca = a[i - 1];
            let cb = b[j - 1];
            if ca == '*' && cb == '*' {
                dp[i][j] = dp[i - 1][j] || dp[i][j - 1] || dp[i - 1][j - 1];
            } else if ca == '*' || cb == '*' {
                dp[i][j] = dp[i - 1][j] || dp[i][j - 1];
            } else if ca == '?' || cb == '?' || ca == cb {
                dp[i][j] = dp[i - 1][j - 1];
            }
        }
    }
    dp[m][n]
}

fn match_segments(pat: &[&str], path: &[&str]) -> bool {
    let (m, n) = (pat.len(), path.len());
    let mut dp = vec![vec![false; n + 1]; m + 1];
    dp[0][0] = true;
    for i in 1..=m {
        if pat[i - 1] == "**" {
            dp[i][0] = dp[i - 1][0];
        }
    }
    for i in 1..=m {
        for j in 1..=n {
            if pat[i - 1] == "**" {
                dp[i][j] = dp[i - 1][j] || dp[i][j - 1];
            } else {
                let pc: Vec<char> = pat[i - 1].chars().collect();
                let tc: Vec<char> = path[j - 1].chars().collect();
                if seg_match(&pc, &tc) {
                    dp[i][j] = dp[i - 1][j - 1];
                }
            }
        }
    }
    dp[m][n]
}

fn segment_lists_intersect(a: &[&str], b: &[&str]) -> bool {
    let (m, n) = (a.len(), b.len());
    let mut dp = vec![vec![false; n + 1]; m + 1];
    dp[0][0] = true;
    for i in 1..=m {
        if a[i - 1] == "**" {
            dp[i][0] = dp[i - 1][0];
        }
    }
    for j in 1..=n {
        if b[j - 1] == "**" {
            dp[0][j] = dp[0][j - 1];
        }
    }
    for i in 1..=m {
        for j in 1..=n {
            let sa = a[i - 1];
            let sb = b[j - 1];
            if sa == "**" && sb == "**" {
                dp[i][j] = dp[i - 1][j] || dp[i][j - 1] || dp[i - 1][j - 1];
            } else if sa == "**" || sb == "**" {
                dp[i][j] = dp[i - 1][j] || dp[i][j - 1];
            } else {
                let ca: Vec<char> = sa.chars().collect();
                let cb: Vec<char> = sb.chars().collect();
                if seg_patterns_intersect(&ca, &cb) {
                    dp[i][j] = dp[i - 1][j - 1];
                }
            }
        }
    }
    dp[m][n]
}

/// Returns true if concrete repo-relative `rel_path` falls within `pattern` (treating `p` as
/// `p ∪ p/**`).
pub fn path_matches_pattern(rel_path: &str, pattern: &str) -> Result<bool, String> {
    let p_norm = normalize_pattern(rel_path)?;
    let g_norm = normalize_pattern(pattern)?;
    let p_segs: Vec<&str> = p_norm.split('/').collect();
    let g_segs: Vec<&str> = g_norm.split('/').collect();
    if match_segments(&g_segs, &p_segs) {
        return Ok(true);
    }
    if g_segs.last() != Some(&"**") {
        let mut ext = g_segs;
        ext.push("**");
        return Ok(match_segments(&ext, &p_segs));
    }
    Ok(false)
}

/// Returns true if two declared write-set patterns can match any common path, treating every
/// pattern `p` as `p ∪ p/**`.
pub fn patterns_intersect(a: &str, b: &str) -> Result<bool, String> {
    let na = normalize_pattern(a)?;
    let nb = normalize_pattern(b)?;
    let sa: Vec<&str> = na.split('/').collect();
    let sb: Vec<&str> = nb.split('/').collect();
    let mut ea = sa.clone();
    if ea.last() != Some(&"**") {
        ea.push("**");
    }
    let mut eb = sb.clone();
    if eb.last() != Some(&"**") {
        eb.push("**");
    }
    Ok(segment_lists_intersect(&sa, &sb)
        || segment_lists_intersect(&ea, &sb)
        || segment_lists_intersect(&sa, &eb)
        || segment_lists_intersect(&ea, &eb))
}

/// Returns true if any pattern in `a` intersects any pattern in `b`.
pub fn write_sets_intersect(a: &[String], b: &[String]) -> Result<bool, String> {
    for pa in a {
        for pb in b {
            if patterns_intersect(pa, pb)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn git_lines(root: &Path, args: &[&str]) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run git {args:?}: {e}"))?;
    if !out.status.success() {
        return Err(format!("git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn walk_dir_entries(root: &Path, dir: &Path, tracked: &BTreeSet<String>, out: &mut BTreeSet<String>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read_dir entry: {e}"))?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).map_err(|e| format!("lstat {}: {e}", path.display()))?;
        let rel = path
            .strip_prefix(root)
            .map_err(|e| format!("strip_prefix: {e}"))?
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        if meta.file_type().is_symlink() {
            out.insert(rel);
        } else if meta.is_dir() {
            walk_dir_entries(root, &path, tracked, out)?;
        } else if meta.is_file() && !tracked.contains(&rel) {
            out.insert(rel);
        }
    }
    Ok(())
}

/// Fail-closed wrapper for kernel completion/finalization sites: resolves `head_sha` from
/// `git rev-parse HEAD` in `worktree_root`, REQUIRES an explicit pinned `base_sha` (never
/// defaulting to current `HEAD`, which would hide committed changes), and runs the full 6-surface
/// [`verify_worktree_writes`] check.
pub fn verify_worktree_writes_auto(
    worktree_root: &Path,
    base_sha_opt: Option<&str>,
    declared_write_set: &[String],
) -> Result<WriteVerification, String> {
    let base_sha = base_sha_opt
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing pinned base_sha for fail-closed 6-surface worktree write_set verification".to_string())?;
    let head_lines = git_lines(worktree_root, &["rev-parse", "HEAD"])?;
    let head_sha = head_lines
        .first()
        .map(String::as_str)
        .ok_or_else(|| "git rev-parse HEAD returned empty output".to_string())?;
    verify_worktree_writes(worktree_root, base_sha, head_sha, declared_write_set)
}

/// 1. Committed diff (`base_sha..head_sha`)
/// 2. Staged index (`git diff --cached --name-only`)
/// 3. Unstaged modifications (`git diff --name-only`)
/// 4. Untracked AND `.gitignore`d files (`git ls-files --others` without `--exclude-standard`,
///    cross-checked against a full filesystem walk excluding `.git/`)
/// 5. Deleted files (`git diff --diff-filter=D --name-only base_sha..head_sha`)
/// 6. Symlink target check (`lstat` + canonical `realpath` inside worktree root & declared `write_set`)
pub fn verify_worktree_writes(
    worktree_root: &Path,
    base_sha: &str,
    head_sha: &str,
    declared_write_set: &[String],
) -> Result<WriteVerification, String> {
    let normalized_ws = validate_write_set(declared_write_set)?;
    let range = format!("{base_sha}..{head_sha}");
    let committed = git_lines(worktree_root, &["diff", "--name-only", &range])?;
    let staged = git_lines(worktree_root, &["diff", "--cached", "--name-only"])?;
    let unstaged = git_lines(worktree_root, &["diff", "--name-only"])?;
    // Deliberately omit `--exclude-standard` so newly created `.gitignore`d files are caught.
    let untracked_and_ignored = git_lines(worktree_root, &["ls-files", "--others"])?;
    let deleted = git_lines(worktree_root, &["diff", "--diff-filter=D", "--name-only", &range])?;
    let tracked_at_base: BTreeSet<String> =
        git_lines(worktree_root, &["ls-tree", "-r", "--name-only", base_sha])?.into_iter().collect();

    let mut all_paths: BTreeSet<String> = BTreeSet::new();
    for list in [&committed, &staged, &unstaged, &untracked_and_ignored, &deleted] {
        for p in list {
            all_paths.insert(p.clone());
        }
    }
    walk_dir_entries(worktree_root, worktree_root, &tracked_at_base, &mut all_paths)?;

    let root_canon = std::fs::canonicalize(worktree_root)
        .map_err(|e| format!("canonicalize worktree root: {e}"))?;
    let mut violations = Vec::new();
    let checked_paths: Vec<String> = all_paths.iter().cloned().collect();

    for rel in &checked_paths {
        // Check symlink safety along every component of `rel`.
        let mut cur = worktree_root.to_path_buf();
        let mut is_sym = false;
        for comp in Path::new(rel).components() {
            cur.push(comp);
            if let Ok(meta) = std::fs::symlink_metadata(&cur)
                && meta.file_type().is_symlink()
            {
                is_sym = true;
                match std::fs::canonicalize(&cur) {
                    Ok(target) => {
                        if let Ok(target_rel) = target.strip_prefix(&root_canon) {
                            let target_str = target_rel.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
                            let target_allowed = normalized_ws
                                .iter()
                                .any(|pat| path_matches_pattern(&target_str, pat).unwrap_or(false));
                            if !target_allowed {
                                violations.push(format!("symlink_target_outside_write_set:{rel}->{target_str}"));
                            }
                        } else {
                            violations.push(format!("symlink_escape_root:{rel}"));
                        }
                    }
                    Err(_) => violations.push(format!("dangling_or_escaping_symlink:{rel}")),
                }
                break;
            }
        }
        let allowed = normalized_ws
            .iter()
            .any(|pat| path_matches_pattern(rel, pat).unwrap_or(false));
        if !allowed {
            let tag = if is_sym { "symlink_outside_write_set" } else { "path_outside_write_set" };
            violations.push(format!("{tag}:{rel}"));
        }
    }

    Ok(WriteVerification { checked_paths, violations })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_folded_and_directory_intersection() {
        assert!(patterns_intersect("src/Foo.rs", "src/foo.rs").unwrap());
        assert!(patterns_intersect("crates/flow", "crates/flow/src/ir.rs").unwrap());
        assert!(patterns_intersect("crates/*/src/*.rs", "crates/flow/src/writeset.rs").unwrap());
        assert!(!patterns_intersect("crates/flow/**", "crates/mcp/**").unwrap());
        assert!(normalize_pattern("../escape").is_err());
        assert!(normalize_pattern("/abs/path").is_err());
    }

    #[test]
    fn six_surface_check_catches_ignored_file_and_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("wt");
        let outside = dir.path().join("outside.txt");
        std::fs::create_dir_all(root.join("src/allowed")).unwrap();
        std::fs::write(&outside, "secret").unwrap();
        std::fs::write(root.join(".gitignore"), "*.ignored
").unwrap();
        std::fs::write(root.join("src/allowed/a.rs"), "fn a() {}
").unwrap();

        let git = |args: &[&str]| {
            let st = Command::new("git").arg("-C").arg(&root).args(args).status().unwrap();
            assert!(st.success(), "git {args:?} failed");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@broomva.tech"]);
        git(&["config", "user.name", "Test"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "base"]);
        let base = git_lines(&root, &["rev-parse", "HEAD"]).unwrap().pop().unwrap();

        // In-scope edit passes.
        std::fs::write(root.join("src/allowed/a.rs"), "fn a() { let _ = 1; }
").unwrap();
        git(&["add", "src/allowed/a.rs"]);
        git(&["commit", "-q", "-m", "in-scope"]);
        let head = git_lines(&root, &["rev-parse", "HEAD"]).unwrap().pop().unwrap();
        let ok = verify_worktree_writes(&root, &base, &head, &["src/allowed/**".into()]).unwrap();
        assert!(ok.is_ok(), "unexpected violations: {:?}", ok.violations);

        // Newly created .gitignore'd file outside declared write_set MUST fail!
        std::fs::write(root.join("leak.ignored"), "hidden").unwrap();
        let bad_ign = verify_worktree_writes(&root, &base, &head, &["src/allowed/**".into()]).unwrap();
        assert!(!bad_ign.is_ok());
        assert!(bad_ign.violations.iter().any(|v| v.contains("leak.ignored")));
        std::fs::remove_file(root.join("leak.ignored")).unwrap();

        // Escaping symlink inside allowed dir MUST fail!
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, root.join("src/allowed/link.rs")).unwrap();
            let bad_sym = verify_worktree_writes(&root, &base, &head, &["src/allowed/**".into()]).unwrap();
            assert!(!bad_sym.is_ok());
            assert!(bad_sym.violations.iter().any(|v| v.contains("symlink_escape_root")));
        }
    }
}
