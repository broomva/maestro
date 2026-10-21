//! Readers for the control plane Maestro governs.
//!
//! Every reader returns a [`Section`]: what parsed, where it came from, and
//! every file that failed. Nothing here writes; owner writes stay in the one
//! process that owns each ledger until Maestro takes that ownership over.

#[cfg(not(unix))]
compile_error!("maestro-store reads the Broomva path registry, whose reader is Unix only (BRO-3266)");

pub mod broomva_paths;
pub mod doctor;
pub mod paths;
pub mod tree;
pub mod views;
pub mod yaml;

use maestro_core::ask::AskLedger;
use maestro_core::fleet::{FleetReport, FleetSummary};
use maestro_core::grant::Preauth;
use maestro_core::initiative::Initiative;
use maestro_core::loops::LoopSpec;
use maestro_core::tick::{LedgerEvent, Tick, pair_ticks};
use maestro_core::work::{Flag, FlagKind, OrchState, WorkFrontmatter, WorkItem, WorkState};
use maestro_core::{Section, SourceError};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The tree facts the registry's rule reads.
#[derive(Debug, Clone)]
enum Layout {
    /// Working tree: the names present under both `.broomva/` and `.control/`.
    Disk { shadowed: Vec<String> },
    /// Git revision: every entry directly under the two folders, with its mode.
    Git { commit: String, children: HashMap<String, String> },
}

pub use paths::{Paths, UNFILED, WorkLayout};
pub use tree::{Tree, TreeSource};

/// The workspace names Maestro reads, as the path registry names them.
pub const ASKS: &str = "asks";
pub const INITIATIVES: &str = "initiatives";
pub const PREAUTH: &str = "preauth";
pub const LOOPS: &str = "loops";
/// The plugin's `LEDGER_LIMIT`: a ledger over this size is refused, not cut.
pub const LEDGER_LIMIT: usize = 1024 * 1024;
/// The plugin's `STREAM_TAIL` (`server/loops.ts`): an envelope stream is judged on its last
/// this many UTF-16 units, so a ledger that only grows costs the same to read every call.
pub const STREAM_TAIL: usize = 2 * 1024 * 1024;

/// Ticks read from the ledger's tail. `from` is the byte offset of the tail's first line: 0 when
/// the tail is the whole ledger, so only then does an absent tick mean the ledger holds none.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TickTail {
    pub ticks: Vec<Tick>,
    pub from: u64,
}

impl TickTail {
    pub fn whole(&self) -> bool {
        self.from == 0
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LedgerFile {
    pub file: String,
    pub ledger: AskLedger,
}

#[derive(Debug, Clone)]
pub struct Store {
    pub paths: Paths,
    pub tree: Tree,
    /// What the registry's rule needs to know about this store's tree, read once.
    layout: OnceLock<Result<Layout, String>>,
}

impl Store {
    /// A git revision is resolved to one commit here, so every read of this store sees the same tree.
    /// The workspace root is made absolute and its `.` and `..` resolved by the text, as the
    /// registry spells a root it reads from the environment (it refuses a `..` part).
    pub fn new(mut paths: Paths, source: TreeSource) -> Self {
        if let Ok(abs) = std::path::absolute(&paths.workspace) {
            paths.workspace = views::lexical(&abs);
        }
        let tree = Tree::resolved(paths.workspace.clone(), source);
        Self { paths, tree, layout: OnceLock::new() }
    }

    /// The workspace-relative path the registry gives `name` in this store's tree: `.broomva/<path>`
    /// when the tree has it, `.control/<path>` otherwise (on disk, or in the git revision read).
    /// A tree holding both is an error, as is one the registry refuses (a symlinked `.control`):
    /// which copy is the record is then unknown, so neither is read.
    pub fn rel(&self, name: &str) -> Result<String, String> {
        let rels = broomva_paths::repo_rels(name).map_err(|e| e.to_string())?;
        let root = self.tree.root();
        let layout = self.layout.get_or_init(|| self.read_layout()).as_ref().map_err(Clone::clone)?;
        let both = match layout {
            Layout::Disk { shadowed } => shadowed.iter().any(|n| n == name),
            Layout::Git { children, .. } => rels.len() > 1 && rels.iter().all(|r| children.contains_key(r)),
        };
        if both {
            return Err(format!(
                "both {} and {} exist in {}: the record is one of them, so neither is read until one is removed",
                rels[0],
                rels[1],
                self.tree.place()
            ));
        }
        match layout {
            Layout::Disk { .. } => {
                let path = broomva_paths::repo_path(name, root).map_err(|e| e.to_string())?;
                rels.into_iter()
                    .find(|rel| path.ends_with(rel))
                    .ok_or_else(|| format!("the registry put {name} at {}, outside {}", path.display(), root.display()))
            }
            // The registry's `tree_path`, on the one listing (a differential test holds them equal).
            Layout::Git { commit, children } => {
                for rel in &rels {
                    if let Some(mode) = children.get(rel) {
                        if !["040000", "100644", "100755"].contains(&mode.as_str()) {
                            return Err(format!("{rel} is not a folder or a file in {commit} (mode {mode})"));
                        }
                        return Ok(rel.clone());
                    }
                }
                Ok(rels[0].clone())
            }
        }
    }

    /// On disk, the registry's `shadowed` (lstats). In a git revision, one `ls-tree` of both
    /// folders: the registry's tree readers run three to four git processes per name, which put
    /// the session hook over its budget.
    fn read_layout(&self) -> Result<Layout, String> {
        let Some(commit) = self.tree.commit_to_read().map_err(|e| e.to_string())? else {
            let shadowed = broomva_paths::shadowed(self.tree.root(), None).map_err(|e| e.to_string())?;
            return Ok(Layout::Disk { shadowed });
        };
        let (new, old) = (broomva_paths::REPO_DIR, broomva_paths::REPO_LEGACY_DIR);
        let mut children = HashMap::new();
        for (mode, path) in self.tree.ls_full(commit, &[new, old, &format!("{new}/"), &format!("{old}/")])? {
            // A folder that is a tree is listed by its children; listed itself, it is not a folder.
            if path == new || path == old {
                return Err(format!("{path} is not a folder in {commit} ({mode})"));
            }
            children.insert(path, mode);
        }
        Ok(Layout::Git { commit: commit.to_string(), children })
    }

    /// Read every YAML file in a workspace directory as a document, with `parse`.
    /// A file the listing skipped (not a regular file) is an error, not a silence.
    fn yaml_docs_with<T>(
        &self,
        dir: &str,
        suffixes: &[&str],
        parse: impl Fn(&str) -> Result<(T, Vec<String>), String>,
    ) -> Section<Vec<(String, T)>> {
        let label = self.tree.label(dir);
        let listing = match self.tree.list(dir, suffixes) {
            Ok(listing) => listing,
            Err(e) => return Section::unavailable(label, e.to_string()),
        };
        let mut errors: Vec<SourceError> = listing
            .skipped
            .into_iter()
            .map(|(name, reason)| SourceError { at: format!("{dir}/{name}"), reason })
            .collect();
        let rels: Vec<String> = listing.names.iter().map(|n| format!("{dir}/{n}")).collect();
        let mut items = Vec::new();
        for (name, (rel, text)) in listing.names.into_iter().zip(self.tree.read_many(&rels)) {
            match text.map_err(|e| e.to_string()).and_then(|t| parse(&t)) {
                Ok((value, warnings)) => {
                    errors.extend(warnings.into_iter().map(|reason| SourceError { at: rel.clone(), reason }));
                    items.push((name, value));
                }
                Err(reason) => errors.push(SourceError { at: rel, reason }),
            }
        }
        Section::partial(label, items, errors)
    }

    /// Typed YAML (initiatives, loops): serde_yaml, whose reading no hash depends on.
    fn yaml_docs(&self, dir: &str) -> Section<Vec<(String, serde_json::Value)>> {
        self.yaml_docs_with(dir, &[".yaml"], |t| {
            serde_yaml::from_str(t).map(|v| (v, Vec::new())).map_err(|e| e.to_string())
        })
    }

    /// Every YAML document in the workspace folder the registry names `name`.
    fn yaml_dir<T: DeserializeOwned>(&self, name: &str) -> Section<Vec<T>> {
        let dir = match self.rel(name) {
            Ok(dir) => dir,
            Err(reason) => return Section::unavailable(self.tree.label(name), reason),
        };
        let dir = dir.as_str();
        let docs = self.yaml_docs(dir);
        let mut errors = docs.errors;
        let data = docs.data.map(|docs| {
            docs.into_iter()
                .filter_map(|(name, doc)| match serde_json::from_value::<T>(doc) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        errors.push(SourceError { at: format!("{dir}/{name}"), reason: e.to_string() });
                        None
                    }
                })
                .collect()
        });
        Section { source: docs.source, data, errors }
    }

    /// Ask ledgers. A ledger that does not parse, and each entry inside one that does not, is an error.
    /// Ledgers are named as the plugin's `LEDGER` names them (`.yaml` or `.yml`) and read
    /// with [`yaml::parse`], so every value, and so every entry hash, is the plugin's.
    pub fn asks(&self) -> Section<Vec<LedgerFile>> {
        let dir = match self.rel(ASKS) {
            Ok(dir) => dir,
            Err(reason) => return Section::unavailable(self.tree.label(ASKS), reason),
        };
        let docs = self.yaml_docs_with(&dir, &[".yaml", ".yml"], |t| {
            if t.len() > LEDGER_LIMIT {
                return Err(format!("it is over {LEDGER_LIMIT} bytes"));
            }
            yaml::parse(t).map(|r| (r.value, r.warnings))
        });
        // Invalid UTF-8 is outside the subset too: say so in the same words as every other refusal.
        let mut docs = docs;
        for e in &mut docs.errors {
            if e.reason.contains("not valid UTF-8") {
                e.reason = format!("{}: {}", yaml::OUTSIDE, e.reason);
            }
        }
        let mut errors = docs.errors;
        let data = docs.data.map(|docs| {
            let mut out = Vec::new();
            for (file, doc) in docs {
                match AskLedger::from_value(doc) {
                    Ok((ledger, bad)) => {
                        for (i, reason) in bad {
                            errors.push(SourceError { at: format!("{dir}/{file}#{i}"), reason });
                        }
                        out.push(LedgerFile { file, ledger });
                    }
                    Err(reason) => errors.push(SourceError { at: format!("{dir}/{file}"), reason }),
                }
            }
            out
        });
        Section { source: docs.source, data, errors }
    }

    pub fn initiatives(&self) -> Section<Vec<Initiative>> {
        self.yaml_dir(INITIATIVES)
    }

    pub fn loops(&self) -> Section<Vec<LoopSpec>> {
        self.yaml_dir(LOOPS)
    }

    pub fn preauth(&self) -> Section<Preauth> {
        let file = match self.rel(PREAUTH) {
            Ok(file) => file,
            Err(reason) => return Section::unavailable(self.tree.label(PREAUTH), reason),
        };
        let label = self.tree.label(&file);
        match self.tree.read(&file) {
            Ok(text) => match serde_yaml::from_str(&text) {
                Ok(p) => Section::ok(label, p),
                Err(e) => Section::unavailable(label, e.to_string()),
            },
            Err(e) => Section::unavailable(label, e.to_string()),
        }
    }

    /// Paired ticks from the loops ledger's tail ([`STREAM_TAIL`]), oldest first. A tick that
    /// started before the tail is not read, as the plugin does not read it.
    pub fn ticks(&self) -> Section<TickTail> {
        let path = self.paths.ticks_ledger();
        let label = format!("fs:{}", path.display());
        let (text, from) = match read_tail(&path, STREAM_TAIL) {
            Ok(t) => t,
            Err(e) => return Section::unavailable(label, e.to_string()),
        };
        // Line numbers count from the tail's first line, whose byte offset the label says.
        let label = if from == 0 { label } else { format!("{label} from byte {from}") };
        let mut events = Vec::new();
        let mut errors = Vec::new();
        for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            match serde_json::from_str::<LedgerEvent>(line) {
                Ok(e) => events.push(e),
                Err(e) => errors.push(SourceError { at: format!("{label}:{}", n + 1), reason: e.to_string() }),
            }
        }
        Section::partial(label, TickTail { ticks: pair_ticks(&events), from }, errors)
    }

    /// The latest fleet-reconcile report per scope.
    pub fn fleet(&self) -> Section<Vec<FleetSummary>> {
        let root = &self.paths.fleet_state;
        let label = format!("fs:{}", root.display());
        let mut errors = Vec::new();
        let mut scope_dirs = match entries(root, &mut errors) {
            Ok(dirs) => dirs,
            Err(e) => return Section::unavailable(label, e),
        };
        scope_dirs.sort();
        let mut out = Vec::new();
        for dir in scope_dirs {
            let ticks = dir.join("ticks");
            if !ticks.is_dir() {
                continue;
            }
            match latest_report(&ticks, &mut errors) {
                Ok(Some(report)) => out.push(FleetSummary::from(&report)),
                Ok(None) => {}
                Err(reason) => errors.push(SourceError { at: ticks.display().to_string(), reason }),
            }
        }
        Section::partial(label, out, errors)
    }

    /// Work items under the work root, in its [`WorkLayout`]: `<root>/<slug>/_work.md`, or
    /// `<root>/<initiative>/<slug>/_work.md` under `$BROOMVA_HOME/work`.
    pub fn work(&self) -> Section<Vec<WorkItem>> {
        let root = &self.paths.work_root;
        let label = format!("fs:{}", root.display());
        let mut errors = Vec::new();
        let top = match entries(root, &mut errors) {
            Ok(dirs) => dirs,
            Err(e) => return Section::unavailable(label, e),
        };
        let mut dirs = Vec::new();
        match self.paths.work_layout() {
            WorkLayout::Flat => dirs.extend(top.into_iter().map(|d| (None, d))),
            WorkLayout::ByInitiative => {
                for bucket in top.into_iter().filter(|d| d.is_dir()) {
                    let name = bucket.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if name.starts_with('.') {
                        continue;
                    }
                    // A bucket that cannot be listed hides its items: recorded, never an empty bucket.
                    match entries(&bucket, &mut errors) {
                        Ok(items) => {
                            let initiative = (name != UNFILED).then_some(name);
                            dirs.extend(items.into_iter().map(|d| (initiative.clone(), d)));
                        }
                        Err(reason) => errors.push(SourceError { at: bucket.display().to_string(), reason }),
                    }
                }
            }
        }
        let mut items = Vec::new();
        for (initiative, dir) in dirs {
            let file = dir.join("_work.md");
            if !file.is_file() {
                continue;
            }
            match read_work(&file) {
                Ok((mut item, warnings)) => {
                    item.filed_under = initiative;
                    items.push(item);
                    errors.extend(warnings);
                }
                Err(reason) => errors.push(SourceError { at: file.display().to_string(), reason }),
            }
        }
        items.sort_by(|a, b| b.front.updated.cmp(&a.front.updated).then(a.slug.cmp(&b.slug)));
        Section::partial(label, items, errors)
    }
}

/// The plugin's `readTail`: `text.slice(text.indexOf("\n", text.length - limit) + 1)` when the
/// file is over `limit` UTF-16 units, else the whole file, with the byte offset in the file of the
/// returned text's first byte. The bytes decode as the plugin's `readFile(path, "utf8")` decodes them: an invalid sequence
/// is U+FFFD. JavaScript measures in UTF-16 units and a unit is at most 3 bytes of UTF-8, so the cut
/// lies in the last `3 * limit + 3` bytes and only those are read, except when no line break
/// follows the cut: then the tail is the whole file, as `indexOf` giving -1 makes it there.
pub fn read_tail(path: &Path, limit: usize) -> std::io::Result<(String, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    // Plus 3: the character the window starts inside is skipped, and the rest still holds `limit` units.
    let window = (limit as u64).saturating_mul(3).saturating_add(3).min(size);
    let start = size - window;
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity(window as usize);
    file.take(window).read_to_end(&mut bytes)?;
    // A window that starts mid-file may start inside a character: skip its continuation bytes.
    let skip = if start == 0 { 0 } else { bytes.iter().take_while(|b| (**b & 0xC0) == 0x80).count().min(3) };
    let raw = &bytes[skip..];
    let text = String::from_utf8_lossy(raw);
    let units: usize = text.chars().map(char::len_utf16).sum();
    if start == 0 && units <= limit {
        return Ok((text.into_owned(), 0));
    }
    // `units` counts the window only; the cut is `limit` units before the end either way.
    let target = units - limit;
    let mut seen = 0;
    let at = text
        .char_indices()
        .find(|(_, c)| {
            let here = seen;
            seen += c.len_utf16();
            here >= target
        })
        .map_or(text.len(), |(i, _)| i);
    match text[at..].find('\n') {
        Some(nl) => {
            let cut = at + nl + 1;
            // A line break decodes from one 0x0A byte and nothing else does: the cut in the raw
            // bytes is after as many of them.
            let breaks = text[..cut].bytes().filter(|b| *b == b'\n').count();
            let raw_cut =
                raw.iter().enumerate().filter(|(_, b)| **b == b'\n').nth(breaks - 1).map_or(0, |(i, _)| i + 1);
            Ok((text[cut..].to_string(), start + (skip + raw_cut) as u64))
        }
        None if start == 0 => Ok((text.into_owned(), 0)),
        None => Ok((String::from_utf8_lossy(&std::fs::read(path)?).into_owned(), 0)),
    }
}

/// Directory entries of `dir`. A failed entry is recorded, not skipped silently.
fn entries(dir: &Path, errors: &mut Vec<SourceError>) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        match entry {
            Ok(e) => out.push(e.path()),
            Err(e) => errors.push(SourceError { at: dir.display().to_string(), reason: e.to_string() }),
        }
    }
    Ok(out)
}

fn latest_report(ticks: &Path, errors: &mut Vec<SourceError>) -> Result<Option<FleetReport>, String> {
    let mut best: Option<(u64, PathBuf)> = None;
    for path in entries(ticks, errors)? {
        let Some(n) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.parse::<u64>().ok()) else { continue };
        let report = path.join("report.json");
        if report.is_file() && best.as_ref().is_none_or(|(b, _)| n > *b) {
            best = Some((n, report));
        }
    }
    let Some((_, path)) = best else { return Ok(None) };
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map(Some).map_err(|e| format!("{}: {e}", path.display()))
}

/// Split `---\n<yaml>\n---\n<body>`. Delimiters are lines that are exactly `---`.
fn split_frontmatter(text: &str) -> Result<(&str, &str), String> {
    let rest = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")).ok_or("no frontmatter")?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == "---" {
            return Ok((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    Err("unterminated frontmatter".into())
}

/// The plugin's `parseEvents`: one JSON object per line, kept when it has a
/// string `ts` and `type`. Blank lines are skipped; anything else counts as bad.
///
/// `JSON.parse` accepts what serde_json refuses: a number past f64 range, and a
/// lone surrogate escape. A line is first split into raw members, which checks
/// syntax without parsing numbers, so a huge number never makes a line bad.
/// Each member is then parsed on its own, a lone surrogate read as U+FFFD, and
/// a member that still fails is null. A later duplicate key wins, as in JavaScript.
pub fn parse_events(text: &str) -> (Vec<serde_json::Value>, usize) {
    let mut events = Vec::new();
    let mut bad = 0;
    for line in text.split('\n').filter(|l| !l.trim().is_empty()) {
        match event_of(line) {
            Some(v) if v.get("ts").is_some_and(|t| t.is_string()) && v.get("type").is_some_and(|t| t.is_string()) => {
                events.push(v)
            }
            _ => bad += 1,
        }
    }
    (events, bad)
}

fn event_of(line: &str) -> Option<serde_json::Value> {
    use serde_json::value::RawValue;
    let members: std::collections::BTreeMap<String, Box<RawValue>> = serde_json::from_str(line)
        .ok()
        .or_else(|| serde_json::from_str(&yaml::lone_surrogates_to_replacement(line)).ok())?;
    let object = members
        .into_iter()
        .map(|(k, raw)| {
            let value = serde_json::from_str(raw.get())
                .or_else(|_| serde_json::from_str(&yaml::lone_surrogates_to_replacement(raw.get())))
                .unwrap_or(serde_json::Value::Null);
            (k, value)
        })
        .collect();
    Some(serde_json::Value::Object(object))
}

/// The loop's current flag: the last parsed event, when it is `loop.flagged`
/// (the plugin's `flagOf` over `parseEvents`, so a torn tail never hides a flag).
fn flag_of(events: &[serde_json::Value]) -> Option<Flag> {
    let last = events.last()?;
    if last.get("type")?.as_str()? != "loop.flagged" {
        return None;
    }
    let kind = match last.get("kind") {
        Some(serde_json::Value::String(k)) => serde_json::from_value(serde_json::Value::String(k.clone())).ok()?,
        Some(other) => FlagKind::Other(other.to_string()),
        None => FlagKind::Other(String::new()),
    };
    let text = |k: &str| last.get(k).and_then(|x| x.as_str()).map(str::to_string);
    Some(Flag { kind, reason: text("reason"), since: text("since") })
}

/// Read one work item: frontmatter, title, last event and the loop's flag.
/// The second value holds failures that leave the item readable, such as an
/// `_events.jsonl` that exists but cannot be read: then the flag is unknown, and that is reported.
pub fn read_work(file: &Path) -> Result<(WorkItem, Vec<SourceError>), String> {
    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    let (yaml, body) = split_frontmatter(&text)?;
    let front: WorkFrontmatter = serde_yaml::from_str(yaml).map_err(|e| e.to_string())?;
    let slug = file.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let title = body.lines().find_map(|l| l.strip_prefix("# ")).map(str::trim).unwrap_or(&slug).to_string();
    let events_file = file.with_file_name("_events.jsonl");
    let mut warnings = Vec::new();
    let raw = match std::fs::read_to_string(&events_file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            warnings.push(SourceError { at: events_file.display().to_string(), reason: e.to_string() });
            String::new()
        }
    };
    let (events, _bad) = parse_events(&raw);
    let last_event_at = events.last().and_then(|v| v.get("ts")).and_then(|t| t.as_str()).map(str::to_string);
    let flag = flag_of(&events);
    let quiet = flag.as_ref().is_some_and(|f| f.kind == FlagKind::Quiet);
    let work_state = WorkState::of(front.state, quiet);
    let finished = front.state == OrchState::Review;
    Ok((WorkItem { slug, title, work_state, front, last_event_at, flag, finished, filed_under: None }, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_delimiter_is_a_whole_line() {
        let (yaml, body) = split_frontmatter("---\nid: a\nnote: ----\n---\n# T\n").unwrap();
        assert_eq!(yaml, "id: a\nnote: ----\n");
        assert_eq!(body, "# T\n");
        assert!(split_frontmatter("---\nid: a\n----\n").is_err());
    }

    fn flag(text: &str) -> Option<Flag> {
        flag_of(&parse_events(text).0)
    }

    #[test]
    fn only_a_trailing_flag_counts() {
        let flagged = "{\"ts\":\"1\",\"type\":\"state\"}\n{\"ts\":\"2\",\"type\":\"loop.flagged\",\"kind\":\"yours\",\"reason\":\"r\"}\n";
        assert_eq!(flag(flagged).unwrap().kind, FlagKind::Yours);
        let cleared = "{\"ts\":\"1\",\"type\":\"loop.flagged\",\"kind\":\"yours\"}\n{\"ts\":\"2\",\"type\":\"note\"}\n";
        assert!(flag(cleared).is_none());
    }

    #[test]
    fn a_torn_or_untimed_tail_does_not_hide_the_flag() {
        let flagged = "{\"ts\":\"1\",\"type\":\"loop.flagged\",\"kind\":\"yours\"}\n";
        for tail in ["{\"ts\":\"2\",\"type\":\"no", "{\"type\":\"note\"}", "[1,2]", "{\"ts\":3,\"type\":\"note\"}"] {
            let text = format!("{flagged}{tail}\n");
            assert_eq!(flag(&text).map(|f| f.kind), Some(FlagKind::Yours), "tail {tail}");
            assert_eq!(parse_events(&text).1, 1);
        }
    }

    #[test]
    fn a_line_json_parse_accepts_is_an_event_here_too() {
        // JSON.parse keeps both tails as the last event, so the earlier flag is cleared.
        let flagged = "{\"ts\":\"1\",\"type\":\"loop.flagged\",\"kind\":\"yours\"}\n";
        for tail in [
            "{\"ts\":\"2\",\"type\":\"note\",\"text\":\"cut \\ud83d\"}",
            "{\"ts\":\"2\",\"type\":\"note\",\"n\":1e400}",
            "{\"ts\":\"2\",\"type\":\"loop.flagged\",\"kind\":\"yours\",\"type\":\"note\"}",
        ] {
            let text = format!("{flagged}{tail}\n");
            assert!(flag(&text).is_none(), "tail {tail}");
            assert_eq!(parse_events(&text), (parse_events(&text).0, 0), "tail {tail}");
        }
        let (events, _) = parse_events("{\"ts\":\"\\ud83d\",\"type\":\"x\"}");
        assert_eq!(events[0]["ts"], "\u{FFFD}", "a lone surrogate in ts is still a string");
    }

    #[test]
    fn an_unreadable_events_file_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("item/_work.md");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "---\nid: x\nkind: task\nstate: running\n---\n# T\n").unwrap();
        assert!(read_work(&file).unwrap().1.is_empty(), "no events file is not an error");
        std::fs::create_dir(file.with_file_name("_events.jsonl")).unwrap(); // exists, cannot be read as a file
        let (item, warnings) = read_work(&file).unwrap();
        assert_eq!(item.title, "T");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }
}
