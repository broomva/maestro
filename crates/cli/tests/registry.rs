//! The binary takes its roots from the path registry (BRO-3266): the workspace by
//! BROOMVA_WORKSPACE, then MAESTRO_WORKSPACE, then BSTACK_WORKSPACE; `$BROOMVA_HOME` refused
//! when relative; the work root by override, then config, then the registry's `machine.work`.

use serde_json::Value;
use std::path::Path;
use std::process::Command;

/// The binary with a clean, scratch environment: HOME is `home`, no workspace or work variable.
fn maestro(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_maestro"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("MAESTRO_TREE", "worktree")
        .env("MAESTRO_FLEET_STATE", home.join("fleet"));
    cmd
}

fn workspace(dir: &Path, ask: &str) {
    std::fs::create_dir_all(dir.join(".control/asks")).unwrap();
    std::fs::write(dir.join(".control/asks/a.yaml"), format!("asks:\n  - id: {ask}\n")).unwrap();
}

fn json(cmd: &mut Command) -> Value {
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn ask_ids(v: &Value) -> Vec<String> {
    v["asks"].as_array().unwrap().iter().map(|a| a["id"].as_str().unwrap().to_string()).collect()
}

#[test]
fn broomva_workspace_wins_over_the_older_names() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    for name in ["new", "maestro", "bstack"] {
        workspace(&tmp.path().join(name), name);
    }
    let asks = |vars: &[(&str, &str)]| {
        let mut cmd = maestro(&home);
        for (k, v) in vars {
            cmd.env(k, tmp.path().join(v));
        }
        ask_ids(&json(cmd.args(["asks", "--json"])))
    };
    let all = [("BROOMVA_WORKSPACE", "new"), ("MAESTRO_WORKSPACE", "maestro"), ("BSTACK_WORKSPACE", "bstack")];
    assert_eq!(asks(&all), ["new"]);
    assert_eq!(asks(&all[1..]), ["maestro"]);
    assert_eq!(asks(&all[2..]), ["bstack"]);
    // An explicit flag wins over all of them.
    let mut cmd = maestro(&home);
    cmd.env("BROOMVA_WORKSPACE", tmp.path().join("new")).arg("--workspace").arg(tmp.path().join("bstack"));
    assert_eq!(ask_ids(&json(cmd.args(["asks", "--json"]))), ["bstack"]);
}

#[test]
fn a_root_the_registry_refuses_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    for (key, value) in [("BROOMVA_HOME", "relative/home"), ("BROOMVA_WORKSPACE", "relative/ws")] {
        let out = maestro(&home).env(key, value).args(["snapshot", "--json"]).output().unwrap();
        assert!(!out.status.success(), "{key}={value} was accepted");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("the path registry") && err.contains(key), "{err}");
    }
    // An explicit --workspace does not read the workspace variables, so a bad one cannot fail it.
    let ws = tmp.path().join("ws");
    workspace(&ws, "a");
    let mut cmd = maestro(&home);
    cmd.env("BROOMVA_WORKSPACE", "relative/ws").arg("--workspace").arg(&ws);
    assert_eq!(ask_ids(&json(cmd.args(["asks", "--json"]))), ["a"]);
    // The hook path still never fails the session.
    let out = maestro(&home).env("BROOMVA_HOME", "relative").args(["context", "--format", "hook"]).output().unwrap();
    assert!(out.status.success() && out.stdout.is_empty());
}

fn item(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("_work.md"), "---\nid: x\nkind: task\nstate: proposed\nupdated: 2026-10-01\n---\n\n# x\n")
        .unwrap();
}

fn work_slugs(cmd: &mut Command) -> Vec<String> {
    let v = json(cmd.args(["work", "--json"]));
    v["items"].as_array().unwrap().iter().map(|w| w["slug"].as_str().unwrap().to_string()).collect()
}

#[test]
fn the_work_root_is_override_then_config_then_the_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let bhome = tmp.path().join("bhome");
    workspace(&tmp.path().join("ws"), "a");
    item(&bhome.join("work/alpha/registry-item"));
    item(&tmp.path().join("configured/config-item"));
    item(&tmp.path().join("paseo/paseo-item"));
    item(&tmp.path().join("override/override-item"));
    let run = || {
        let mut cmd = maestro(&home);
        cmd.env("BROOMVA_WORKSPACE", tmp.path().join("ws")).env("BROOMVA_HOME", &bhome);
        cmd
    };
    // Nothing configured: the registry's $BROOMVA_HOME/work, read by initiative.
    assert_eq!(work_slugs(&mut run()), ["registry-item"]);
    // The Paseo plugin's configured root, then Maestro's own config ahead of it.
    let paseo = home.join(".config/maestro-paseo/config.json");
    std::fs::create_dir_all(paseo.parent().unwrap()).unwrap();
    std::fs::write(&paseo, serde_json::json!({ "workRoot": tmp.path().join("paseo") }).to_string()).unwrap();
    assert_eq!(work_slugs(&mut run()), ["paseo-item"]);
    let own = bhome.join("config/maestro/config.json");
    std::fs::create_dir_all(own.parent().unwrap()).unwrap();
    std::fs::write(&own, serde_json::json!({ "workRoot": tmp.path().join("configured") }).to_string()).unwrap();
    assert_eq!(work_slugs(&mut run()), ["config-item"]);
    // The override wins over every config.
    assert_eq!(work_slugs(run().env("MAESTRO_WORK_ROOT", tmp.path().join("override"))), ["override-item"]);
}

#[test]
fn doctor_exits_non_zero_unless_the_reader_is_known_current() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    workspace(&tmp.path().join("ws"), "a");
    let out = maestro(&home)
        .env("BROOMVA_WORKSPACE", tmp.path().join("ws"))
        .args(["doctor", "--json", "--bstack"])
        .arg(tmp.path().join("no-bstack"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let statuses: Vec<_> = v["checks"].as_array().unwrap().iter().map(|c| c["status"].as_str().unwrap()).collect();
    assert_eq!(statuses, ["unread", "unread"]);
}
