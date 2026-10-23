//! `maestro delegation plan` binds a graph's repos as the workspace's delegation.py does: a
//! `repos` that is not a mapping stops the run (outside the schema, exit 1), it never
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
    assert!(String::from_utf8_lossy(&out.stderr).contains("graph.repos: a mapping, not a list"));
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
    assert!(cases.len() > 140, "the CLI golden lost cases: {}", cases.len());
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
        let ok = if want["refused"].as_bool() == Some(true) {
            // Outside the schema: refused, naming the field (the golden's stderr).
            code == Some(1)
                && stdout.is_empty()
                && stderr.contains(&format!("{}: ", want["stderr"].as_str().unwrap()))
                && stderr.contains(maestro_core::schema::OUTSIDE)
        } else if want["crash"].as_bool().unwrap() {
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

/// Anchors, aliases, tags and merge keys are outside the YAML subset the CLI reads as PyYAML does:
/// a policy using one is unreadable (assess refuses, exit 3; plan reports it against the node) and a
/// graph using one is refused with the reason named (exit 1). Never misread, never planned.
#[test]
fn yaml_outside_the_subset_is_refused() {
    let assess = |policy: &str| {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".control")).unwrap();
        std::fs::write(dir.path().join(".control/policy.yaml"), policy).unwrap();
        Command::new(env!("CARGO_BIN_EXE_maestro"))
            .current_dir(dir.path())
            .args(["delegation", "assess", "--root", ".", "--kind", "implement", "--paths", "deploy/x"])
            .output()
            .unwrap()
    };
    for (policy, why) in [
        ("auto_merge: {rules: [!rule {action: require_human, path_touched: deploy/}]}\n", "a tag"),
        ("r: &r {action: require_human}\nauto_merge: {rules: [*r]}\n", "an anchor"),
        ("auto_merge: {rules: [{<<: {action: require_human}, path_touched: deploy/}]}\n", "a merge key"),
    ] {
        let out = assess(policy);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(3), "{policy}: {err}");
        assert!(err.contains("outside the YAML subset") && err.contains(why), "{policy}: {err}");
    }
    let dir = tempfile::tempdir().unwrap();
    for (graph, why) in [
        ("nodes:\n  - !node {id: a, done: t}\n", "a tag"),
        ("x: &x {done: t}\nnodes:\n  - {id: a, signals: {paths: [a.rs]}}\n", "an anchor"),
        ("x: &a {id: a, done: t}\nnodes:\n  - *a\n", "an anchor"),
        ("nodes:\n  - {<<: {kind: review}, id: r, done: t, tier: standard}\n", "a merge key"),
        // YAML 1.1 (PyYAML) and 1.2 (serde_yaml) read these plain scalars differently.
        ("nodes:\n  - {id: a, done: yes}\n", "a plain scalar 'yes'"),
        ("nodes:\n  - {id: a, done: t, signals: {est_loc: 0250}}\n", "a plain scalar '0250'"),
        ("nodes:\n  - {id: a, done: t, signals: {est_loc: 1e3}}\n", "a plain scalar '1e3'"),
        ("nodes:\n  - {id: a, done: 2026-10-23}\n", "a plain scalar '2026-10-23'"),
        ("nodes:\n  - {id: 1:20, done: t}\n", "a plain scalar '1:20'"),
        ("repos: {7: .}\nnodes:\n  - {id: a, done: t}\n", "a mapping key '7' that is not a string"),
        // Inside the YAML subset, outside the schema: refused naming the field.
        ("nodes:\n  - {id: 1, done: t}\n", "graph.nodes[0].id: a string, not an integer"),
        ("nodes:\n  - {id: a, done: t, signals: {est_loc: '300'}}\n", "graph.nodes[0].signals.est_loc"),
    ] {
        let out = plan(dir.path(), graph);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{graph}: {err}");
        assert!(out.stdout.is_empty(), "{graph} was planned");
        assert!(err.contains(why), "{graph}: {err}");
    }
}

fn capacity(dir: &std::path::Path, usage: &str) -> std::process::Output {
    let path = dir.join("usage.json");
    std::fs::write(&path, usage).unwrap();
    Command::new(env!("CARGO_BIN_EXE_maestro"))
        .args(["economy", "capacity", "--now", "2026-10-23T00:00:00Z", "--usage"])
        .arg(&path)
        .output()
        .unwrap()
}

const ACTIVE: &str = r#"{"email":"a@x.io","fiveHourUtil":10,"sevenDayUtil":10,"sevenDayResetsAt":"2026-10-28T00:00:00Z","fiveHourResetsAt":"2026-10-23T01:00:00Z","isActive":true,"isRateLimited":false,"stale":false}"#;

/// Where the Python raises, `maestro economy capacity` prints no mode: a malformed usage row, a
/// runway past datetime's years, or a usage that is not a list never reads as spendable capacity
/// (P20 port arc 2, Stratum A).
#[test]
fn malformed_usage_is_refused_not_read_as_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let tiny = ACTIVE.replace(r#""sevenDayUtil":10"#, r#""sevenDayUtil":0.000001"#).replace("2026-10-28", "2026-10-29");
    for (usage, why) in [
        (format!("[{ACTIVE},null]"), "usage[1]: a mapping, not null (outside the schema"),
        (format!("[{}]", ACTIVE.replace("10,", "100000000000000000000,")), "does not fit 64 bits"),
        (format!("[{}]", ACTIVE.replace("10,", "1e400,")), "past f64's range"),
        (format!("[{tiny}]"), "OverflowError: date value out of range"),
        (format!("{{\"a\": {ACTIVE}}}"), "--usage is a dict, not a list of rows"),
    ] {
        let out = capacity(dir.path(), &usage);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{usage}: {err}");
        assert!(out.stdout.is_empty(), "{usage}: {}", String::from_utf8_lossy(&out.stdout));
        assert!(err.contains(why), "{usage}: {err}");
    }
    // The control: the same active row alone is read.
    let out = capacity(dir.path(), &format!("[{ACTIVE}]"));
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"mode\""));
}
