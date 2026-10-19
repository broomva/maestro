//! Readers for the control plane Maestro governs.
//!
//! Every reader returns a [`Section`]: what parsed, where it came from, and
//! every file that failed. Nothing here writes; owner writes stay in the one
//! process that owns each ledger until Maestro takes that ownership over.

pub mod paths;
pub mod tree;
pub mod views;

use maestro_core::ask::AskLedger;
use maestro_core::fleet::{FleetReport, FleetSummary};
use maestro_core::grant::Preauth;
use maestro_core::initiative::Initiative;
use maestro_core::loops::LoopSpec;
use maestro_core::tick::{LedgerEvent, Tick, pair_ticks};
use maestro_core::work::{WorkFrontmatter, WorkItem, WorkState};
use maestro_core::{Section, SourceError};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::path::Path;

pub use paths::Paths;
pub use tree::{Tree, TreeSource};

pub const ASKS_DIR: &str = ".control/asks";
pub const INITIATIVES_DIR: &str = ".control/initiatives";
pub const PREAUTH_FILE: &str = ".control/preauth.yaml";
pub const LOOPS_DIR: &str = ".broomva/loops";

#[derive(Debug, Clone, Serialize)]
pub struct LedgerFile {
    pub file: String,
    pub ledger: AskLedger,
}

#[derive(Debug, Clone)]
pub struct Store {
    pub paths: Paths,
    pub tree: Tree,
}

impl Store {
    pub fn new(paths: Paths, source: TreeSource) -> Self {
        let tree = Tree::new(paths.workspace.clone(), source);
        Self { paths, tree }
    }

    /// Parse every YAML file in a workspace directory into `T`.
    fn yaml_dir<T: DeserializeOwned>(&self, dir: &str) -> Section<Vec<(String, T)>> {
        let label = self.tree.label(dir);
        let names = match self.tree.list(dir, ".yaml") {
            Ok(names) => names,
            Err(e) => return Section::unavailable(label, e.to_string()),
        };
        let rels: Vec<String> = names.iter().map(|n| format!("{dir}/{n}")).collect();
        let mut items = Vec::new();
        let mut errors = Vec::new();
        for (name, (rel, text)) in names.into_iter().zip(self.tree.read_many(&rels)) {
            match text.map_err(|e| e.to_string()).and_then(|t| serde_yaml::from_str::<T>(&t).map_err(|e| e.to_string()))
            {
                Ok(value) => items.push((name, value)),
                Err(reason) => errors.push(SourceError { at: rel, reason }),
            }
        }
        Section::partial(label, items, errors)
    }

    pub fn asks(&self) -> Section<Vec<LedgerFile>> {
        self.yaml_dir::<AskLedger>(ASKS_DIR)
            .map(|v| v.into_iter().map(|(file, ledger)| LedgerFile { file, ledger }).collect())
    }

    pub fn initiatives(&self) -> Section<Vec<Initiative>> {
        self.yaml_dir::<Initiative>(INITIATIVES_DIR).map(|v| v.into_iter().map(|(_, i)| i).collect())
    }

    pub fn loops(&self) -> Section<Vec<LoopSpec>> {
        self.yaml_dir::<LoopSpec>(LOOPS_DIR).map(|v| v.into_iter().map(|(_, l)| l).collect())
    }

    pub fn preauth(&self) -> Section<Preauth> {
        let label = self.tree.label(PREAUTH_FILE);
        match self.tree.read(PREAUTH_FILE) {
            Ok(text) => match serde_yaml::from_str(&text) {
                Ok(p) => Section::ok(label, p),
                Err(e) => Section::unavailable(label, e.to_string()),
            },
            Err(e) => Section::unavailable(label, e.to_string()),
        }
    }

    /// Paired ticks from the loops ledger, oldest first.
    pub fn ticks(&self) -> Section<Vec<Tick>> {
        let path = self.paths.ticks_ledger();
        let label = format!("fs:{}", path.display());
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => return Section::unavailable(label, e.to_string()),
        };
        let mut events = Vec::new();
        let mut errors = Vec::new();
        for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            match serde_json::from_str::<LedgerEvent>(line) {
                Ok(e) => events.push(e),
                Err(e) => errors.push(SourceError { at: format!("{label}:{}", n + 1), reason: e.to_string() }),
            }
        }
        Section::partial(label, pair_ticks(&events), errors)
    }

    /// The latest fleet-reconcile report per scope.
    pub fn fleet(&self) -> Section<Vec<FleetSummary>> {
        let root = &self.paths.fleet_state;
        let label = format!("fs:{}", root.display());
        let scopes = match std::fs::read_dir(root) {
            Ok(rd) => rd,
            Err(e) => return Section::unavailable(label, e.to_string()),
        };
        let mut out = Vec::new();
        let mut errors = Vec::new();
        let mut scope_dirs: Vec<_> = scopes.filter_map(Result::ok).map(|e| e.path()).collect();
        scope_dirs.sort();
        for dir in scope_dirs {
            let ticks = dir.join("ticks");
            if !ticks.is_dir() {
                continue;
            }
            match latest_report(&ticks) {
                Ok(Some(report)) => out.push(FleetSummary::from(&report)),
                Ok(None) => {}
                Err(reason) => errors.push(SourceError { at: ticks.display().to_string(), reason }),
            }
        }
        Section::partial(label, out, errors)
    }

    /// Work items under the work root (`<root>/<slug>/_work.md`).
    pub fn work(&self) -> Section<Vec<WorkItem>> {
        let root = &self.paths.work_root;
        let label = format!("fs:{}", root.display());
        let entries = match std::fs::read_dir(root) {
            Ok(rd) => rd,
            Err(e) => return Section::unavailable(label, e.to_string()),
        };
        let mut items = Vec::new();
        let mut errors = Vec::new();
        for entry in entries.filter_map(Result::ok) {
            let file = entry.path().join("_work.md");
            if !file.is_file() {
                continue;
            }
            match read_work(&file) {
                Ok(item) => items.push(item),
                Err(reason) => errors.push(SourceError { at: file.display().to_string(), reason }),
            }
        }
        items.sort_by(|a, b| b.front.updated.cmp(&a.front.updated).then(a.slug.cmp(&b.slug)));
        Section::partial(label, items, errors)
    }
}

fn latest_report(ticks: &Path) -> Result<Option<FleetReport>, String> {
    let mut best: Option<(u64, std::path::PathBuf)> = None;
    for entry in std::fs::read_dir(ticks).map_err(|e| e.to_string())?.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(n) = name.parse::<u64>() else { continue };
        let report = entry.path().join("report.json");
        if report.is_file() && best.as_ref().is_none_or(|(b, _)| n > *b) {
            best = Some((n, report));
        }
    }
    let Some((_, path)) = best else { return Ok(None) };
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map(Some).map_err(|e| format!("{}: {e}", path.display()))
}

/// Split `---\n<yaml>\n---\n<body>` and parse the frontmatter.
pub fn read_work(file: &Path) -> Result<WorkItem, String> {
    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    let rest = text.strip_prefix("---\n").ok_or("no frontmatter")?;
    let (yaml, body) = rest.split_once("\n---").ok_or("unterminated frontmatter")?;
    let front: WorkFrontmatter = serde_yaml::from_str(yaml).map_err(|e| e.to_string())?;
    let slug = file.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let title = body.lines().find_map(|l| l.strip_prefix("# ")).map(str::trim).unwrap_or(&slug).to_string();
    let last_event_at = std::fs::read_to_string(file.with_file_name("_events.jsonl"))
        .ok()
        .and_then(|t| t.lines().rev().find(|l| !l.trim().is_empty()).map(str::to_string))
        .and_then(|l| serde_json::from_str::<serde_json::Value>(&l).ok())
        .and_then(|v| v.get("ts").and_then(|t| t.as_str()).map(str::to_string));
    // The quiet flag lives in the plugin's live state, not on disk; phase 0 reports Running.
    let work_state = WorkState::of(front.state, false);
    Ok(WorkItem { slug, title, work_state, front, last_event_at })
}
