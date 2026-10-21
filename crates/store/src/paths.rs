//! Where the control plane lives, from the Broomva path registry (BRO-3216, BRO-3266).
//!
//! Every workspace and machine path comes from [`crate::broomva_paths`], the generated
//! reader vendored from bstack: the workspace root and `$BROOMVA_HOME` by its rules, and
//! each name inside them by its tables. `MAESTRO_WORK_ROOT` and `MAESTRO_FLEET_STATE`
//! stay as overrides (the registry has no fleet-reconcile entry), so tests and other
//! machines never touch the owner's real files by accident.

use crate::broomva_paths as registry;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Paths {
    /// The workspace checkout holding `.broomva/` (legacy `.control/`): the registry's
    /// workspace root (`BROOMVA_WORKSPACE`, then `MAESTRO_WORKSPACE`, then `BSTACK_WORKSPACE`).
    pub workspace: PathBuf,
    /// Work items: the registry's `machine.work`, unless configured (`MAESTRO_WORK_ROOT`,
    /// then `workRoot` in Maestro's config, then in the Paseo plugin's).
    pub work_root: PathBuf,
    /// The `.broomva` home (`BROOMVA_HOME`): `config/`, `ledger/`, `work/`, ...
    pub broomva_home: PathBuf,
    /// fleet-reconcile state, one directory per scope (`MAESTRO_FLEET_STATE`).
    pub fleet_state: PathBuf,
}

/// How work items sit under a work root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkLayout {
    /// `<root>/<slug>/_work.md`: the legacy root (`~/maestro`) and any configured one.
    Flat,
    /// `<root>/<initiative>/<slug>/_work.md`, with [`UNFILED`] for work in no initiative:
    /// the registry's `$BROOMVA_HOME/work` (owner decision 2026-10-19, BRO-2909).
    ByInitiative,
}

/// The bucket under `$BROOMVA_HOME/work` for work filed under no initiative.
pub const UNFILED: &str = "_unfiled";

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// `name`'s path under the machine root, from the registry's table (new location only).
fn machine_rel(name: &str) -> &'static str {
    registry::MACHINE
        .iter()
        .find(|(n, ..)| *n == name)
        .map(|&(_, path, _)| path)
        .unwrap_or_else(|| panic!("`{name}` is not a machine name in the vendored registry"))
}

impl Paths {
    /// Paths from the environment by the registry's rules. A root the registry refuses
    /// (a relative `BROOMVA_HOME`, the workspace at `$HOME`) is an error, not a guess.
    pub fn from_env() -> Result<Self, registry::Error> {
        let broomva_home = registry::machine_root()?;
        let config = broomva_home.join(machine_rel("config")).join("maestro");
        let work_root = match env_path("MAESTRO_WORK_ROOT")
            .or_else(|| configured_work_root(&config.join("config.json")))
            .or_else(|| home().and_then(|h| configured_work_root(&h.join(".config/maestro-paseo/config.json"))))
        {
            Some(root) => root,
            None => registry::machine_path("work")?,
        };
        let fleet_state = match env_path("MAESTRO_FLEET_STATE") {
            Some(p) => p,
            None => home().unwrap_or_else(|| PathBuf::from(".")).join(".local/state/fleet-reconcile"),
        };
        Ok(Self { workspace: registry::workspace_root()?, work_root, broomva_home, fleet_state })
    }

    /// All paths under one root, laid out like a real machine. Used by tests and fixtures.
    pub fn under(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            workspace: root.join("workspace"),
            work_root: root.join("work"),
            broomva_home: root.join("broomva-home"),
            fleet_state: root.join("fleet-reconcile"),
        }
    }

    /// Maestro's own configuration: `$BROOMVA_HOME/config/maestro`.
    pub fn config_dir(&self) -> PathBuf {
        self.broomva_home.join(machine_rel("config")).join("maestro")
    }

    /// One ledger stream: `$BROOMVA_HOME/ledger/<stream>`. A stream is one plain folder name.
    pub fn ledger(&self, stream: &str) -> Option<PathBuf> {
        let plain = !stream.is_empty() && !stream.starts_with('.') && !stream.contains(['/', '\\', '\0']);
        plain.then(|| self.broomva_home.join(machine_rel("ledger")).join(stream))
    }

    pub fn ticks_ledger(&self) -> PathBuf {
        self.broomva_home.join(machine_rel("loop_ticks"))
    }

    /// The layout is the location's: `$BROOMVA_HOME/work` is by initiative, any other root flat.
    pub fn work_layout(&self) -> WorkLayout {
        if same_dir(&self.work_root, &self.broomva_home.join(machine_rel("work"))) {
            WorkLayout::ByInitiative
        } else {
            WorkLayout::Flat
        }
    }
}

fn home() -> Option<PathBuf> {
    env_path("HOME")
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// A config file's `workRoot`, so Maestro and the Paseo plugin read the same items.
fn configured_work_root(file: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(file).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("workRoot")?.as_str().filter(|s| !s.is_empty()).map(PathBuf::from)
}
