//! `maestro delegation plan` binds a graph's repos as the workspace's delegation.py does: a
//! `repos` that is not a mapping stops the run (the Python's AttributeError, exit 1), it never
//! falls back to a single-repo graph and dispatches.

use std::process::Command;

fn plan(dir: &std::path::Path, graph: &str) -> std::process::Output {
    let path = dir.join("g.yaml");
    std::fs::write(&path, graph).unwrap();
    Command::new(env!("CARGO_BIN_EXE_maestro"))
        .current_dir(dir)
        .args(["delegation", "plan"])
        .arg(&path)
        .arg("--skip-absent-repos")
        .output()
        .unwrap()
}

const NODES: &str = "nodes:\n  - id: a\n    done: make test\n    signals: {paths: [a.rs]}\n";

#[test]
fn a_repos_list_is_refused_not_planned() {
    let dir = tempfile::tempdir().unwrap();
    let out = plan(dir.path(), &format!("repo: w\nrepos: [w]\n{NODES}"));
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("AttributeError: 'list' object has no attribute 'items'"));
}

#[test]
fn a_single_repo_graph_plans() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_maestro"))
        .current_dir(dir.path())
        .args(["delegation", "plan", "--root", "."])
        .arg({
            std::fs::write(dir.path().join("g.yaml"), NODES).unwrap();
            dir.path().join("g.yaml")
        })
        .output()
        .unwrap();
    // No policy file here: the Python's unreadable-policy error, against the node.
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("[error] a: "), "{}", String::from_utf8_lossy(&out.stderr));
    std::fs::create_dir_all(dir.path().join(".control")).unwrap();
    std::fs::write(dir.path().join(".control/policy.yaml"), "auto_merge: {rules: []}\n").unwrap();
    let out = plan(dir.path(), NODES);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("a "));
}
