//! Where the control plane lives. Every path has an environment override so
//! tests and other machines never touch the owner's real files by accident.

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Paths {
    /// The workspace checkout holding `.control/` and `.broomva/loops/` (`MAESTRO_WORKSPACE`).
    pub workspace: PathBuf,
    /// Work items, `<root>/<slug>/_work.md` (`MAESTRO_WORK_ROOT`).
    pub work_root: PathBuf,
    /// The `.broomva` home holding `ledger/` and `notes/` (`BROOMVA_HOME`).
    pub broomva_home: PathBuf,
    /// fleet-reconcile state, one directory per scope (`MAESTRO_FLEET_STATE`).
    pub fleet_state: PathBuf,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key).filter(|v| !v.is_empty()).map(PathBuf::from)
}

impl Paths {
    pub fn from_env() -> Self {
        let home = home();
        let work_root =
            env_path("MAESTRO_WORK_ROOT").or_else(|| paseo_work_root(&home)).unwrap_or_else(|| home.join("maestro"));
        Self {
            workspace: env_path("MAESTRO_WORKSPACE").unwrap_or_else(|| home.join("broomva")),
            work_root,
            broomva_home: env_path("BROOMVA_HOME").unwrap_or_else(|| home.join(".broomva")),
            fleet_state: env_path("MAESTRO_FLEET_STATE").unwrap_or_else(|| home.join(".local/state/fleet-reconcile")),
        }
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

    pub fn ticks_ledger(&self) -> PathBuf {
        self.broomva_home.join("ledger/loops/ticks.jsonl")
    }
}

/// The Paseo plugin's configured work root, so both Maestros read the same items.
fn paseo_work_root(home: &std::path::Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(home.join(".config/maestro-paseo/config.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("workRoot")?.as_str().map(PathBuf::from)
}
