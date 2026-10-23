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

/// `fixtures/delegation/cli-golden.json`: the workspace's delegation.py and fleet_economy.py CLIs
/// run on scratch repos (written by `scripts/parity-delegation.py --write`). Each case is replayed
/// through this binary on the same layout: the exit code, stderr and stdout must be the Python's (an
/// uncaught Python error as the same exception kind; JSON compared as values).
#[test]
fn the_cli_matches_the_python_clis() {
    let golden: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/delegation/cli-golden.json"))
            .unwrap(),
    )
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("repos");
    let root_s = root.to_str().unwrap().to_string();
    let layout = &golden["layout"];
    for d in layout["dirs"].as_array().unwrap() {
        std::fs::create_dir_all(root.join(d.as_str().unwrap())).unwrap();
    }
    for (rel, text) in layout["files"].as_object().unwrap() {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text.as_str().unwrap().replace("{ROOT}", &root_s)).unwrap();
    }
    for (link, target) in layout["symlinks"].as_object().unwrap() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join(target.as_str().unwrap()), root.join(link)).unwrap();
    }
    let cases = golden["cases"].as_array().unwrap();
    assert!(cases.len() > 150, "the CLI golden lost cases: {}", cases.len());
    let mut bad = Vec::new();
    for c in cases {
        let args: Vec<String> =
            c["rs"].as_array().unwrap().iter().map(|a| a.as_str().unwrap().replace("{ROOT}", &root_s)).collect();
        let out = Command::new(env!("CARGO_BIN_EXE_maestro"))
            .current_dir(root.join(c["cwd"].as_str().unwrap()))
            .args(&args)
            .output()
            .unwrap();
        let (code, stdout) = (out.status.code(), String::from_utf8_lossy(&out.stdout).replace(&root_s, "{ROOT}"));
        let stderr = String::from_utf8_lossy(&out.stderr).replace(&root_s, "{ROOT}");
        let want = &c["want"];
        let want_code = want["code"].as_i64().map(|c| c as i32);
        let ok = if want["crash"].as_bool().unwrap() {
            code == Some(1)
                && stderr.contains(&format!("\n  {}: ", want["stderr"].as_str().unwrap()))
                && stdout == want["stdout"].as_str().unwrap()
        } else {
            let out_ok = if c["json"].as_bool().unwrap() && want_code == Some(0) {
                serde_json::from_str::<serde_json::Value>(&stdout).ok()
                    == serde_json::from_str::<serde_json::Value>(want["stdout"].as_str().unwrap()).ok()
            } else {
                stdout == want["stdout"].as_str().unwrap()
            };
            code == want_code && stderr == want["stderr"].as_str().unwrap() && out_ok
        };
        if !ok {
            bad.push(format!(
                "{}\n  want {:?} {}{}\n  got  {code:?} {stderr}{stdout}",
                c["name"], want_code, want["stderr"], want["stdout"]
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} CLI cases differ:\n{}",
        bad.len(),
        cases.len(),
        bad[..bad.len().min(5)].join("\n")
    );
}
