//! `verify@1` end to end through the built binary (BRO-3636): the flow's own definition
//! (`crates/flow/flows/verify.yaml`) on a scratch git repository, driven `flow start`, `next`,
//! `claim` and `complete`, with the kernel running every command, `maestro dogfood verify-receipt`
//! included: the binary's folder leads PATH, which the kernel passes on to what it runs. The
//! contract's V1, V3 and V4, the verifier's exit statuses, and `flow spine --against` over real
//! runs. V2 needs the kernel's test clock and runs in the crate (`maestro_flow`'s
//! `kernel::piloted`).

use maestro_flow::dogfood;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const VERIFY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../flow/flows/verify.yaml");

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@localhost", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A scratch machine: a workspace with `.broomva/`, a git repository with two commits (the
/// session's folder), and a Direct registry with one live session there.
struct Pilot {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    /// The repository's first commit, and its HEAD.
    first: String,
    head: String,
}

fn pilot() -> Pilot {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir_all(root.join("workspace/.broomva")).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "one"]);
    let first = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "two"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let registry = json!([{ "id": "d1", "cwd": repo, "token": "tok-1", "liveness": "live" }]);
    std::fs::write(root.join("direct.json"), registry.to_string()).unwrap();
    Pilot { _tmp: tmp, root, repo, first, head }
}

/// The printed JSON, and the exit status.
fn run(cmd: &mut Command) -> (Value, Output) {
    let out = cmd.output().unwrap();
    let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!("not JSON: {}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    });
    (v, out)
}

impl Pilot {
    /// The binary as the verified Direct session `d1`, with its own folder first on PATH.
    fn maestro(&self) -> Command {
        let bin = Path::new(env!("CARGO_BIN_EXE_maestro")).parent().unwrap().to_path_buf();
        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
        )
        .unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_maestro"));
        cmd.env_clear()
            .env("PATH", path)
            .env("HOME", self.root.join("home"))
            .env("BROOMVA_WORKSPACE", self.root.join("workspace"))
            .env("BROOMVA_HOME", self.root.join("broomva-home"))
            .env("MAESTRO_FLEET_STATE", self.root.join("fleet"))
            .env("MAESTRO_TREE", "worktree")
            .env("MAESTRO_SESSION", "direct:-/d1")
            .env("MAESTRO_DIRECT_REGISTRY", self.root.join("direct.json"))
            .env("MAESTRO_DIRECT_TOKEN", "tok-1")
            .env("MAESTRO_CALLER_CWD", &self.repo);
        cmd
    }

    fn ok(&self, args: &[&str]) -> Value {
        let (v, out) = run(self.maestro().args(args));
        assert!(out.status.success() && v["ok"] == true, "{args:?}: {v}\n{}", String::from_utf8_lossy(&out.stderr));
        v
    }

    /// A refusal: exit 1, and the JSON.
    fn refused(&self, args: &[&str]) -> Value {
        let (v, out) = run(self.maestro().args(args));
        assert_eq!((out.status.code(), &v["ok"]), (Some(1), &json!(false)), "{args:?}: {v}");
        v
    }

    fn start(&self, head: &str, check: &str, surface: Option<&str>) -> String {
        let mut params = json!({ "repo": self.repo, "head": head, "check": check });
        if let Some(surface) = surface {
            params["surface"] = json!(surface);
        }
        let v = self.ok(&["flow", "start", "--definition", VERIFY, "--params", &params.to_string()]);
        v["instance"].as_str().unwrap().to_string()
    }

    fn claim(&self, fi: &str, node: &str) -> String {
        let v = self.ok(&["flow", "claim", "--instance", fi, "--node-id", node]);
        v["claim_token"].as_str().unwrap().to_string()
    }

    /// `flow complete` on `token`: the JSON, and whether it exited 0.
    fn complete(&self, token: &str, outputs: Value) -> (Value, bool) {
        let (v, out) =
            run(self.maestro().args(["flow", "complete", "--claim-token", token, "--outputs", &outputs.to_string()]));
        (v, out.status.success())
    }

    /// Claims and completes `node`, which must pass; the receipt's verify.
    fn done(&self, fi: &str, node: &str) -> Value {
        let token = self.claim(fi, node);
        let (v, ok) = self.complete(&token, Value::Null);
        assert!(ok, "{node}: {v}");
        v["verify"].clone()
    }

    fn status(&self, fi: &str) -> Value {
        self.ok(&["flow", "status", "--instance", fi])["status"].clone()
    }

    fn events(&self, fi: &str) -> Vec<Value> {
        let log = self.root.join("workspace/.broomva/flows/instances").join(fi).join("events.jsonl");
        std::fs::read_to_string(log).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    /// A dogfood receipt at `receipt.json` in a fresh folder of its own, with its evidence beside
    /// it; `edit` changes its JSON first. Its path.
    fn receipt(&self, name: &str, edit: impl FnOnce(&mut Value)) -> PathBuf {
        let dir = self.root.join("receipts").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("run.log"), "maestro flow next printed the enabled nodes\n").unwrap();
        let mut v = json!({
            "schema": dogfood::SCHEMA,
            "head": self.head,
            "surface": "cli",
            "entry": "maestro flow next",
            "driver": "shell",
            "evidence": { "path": "run.log", "sha256": dogfood::sha256(b"maestro flow next printed the enabled nodes\n") },
            "result": "pass",
            "at": "2026-11-11T12:00:00Z",
        });
        edit(&mut v);
        let path = dir.join("receipt.json");
        std::fs::write(&path, v.to_string()).unwrap();
        path
    }
}

fn kinds(events: &[Value]) -> Vec<&str> {
    events.iter().map(|e| e["kind"].as_str().unwrap()).collect()
}

#[test]
fn v1_pin_accepts_the_repository_at_its_head_and_marks_a_moved_one() {
    let p = pilot();
    let fi = p.start(&p.head, "true", None);
    let verify = p.done(&fi, "pin");
    assert_eq!(verify["exit"], 0);
    let s = p.status(&fi);
    assert_eq!((&s["nodes"]["pin"]["state"], &s["nodes"]["check"]["state"]), (&json!("done"), &json!("ready")));

    // The repository is not at the head the instance names: pin records moved, and nothing
    // downstream runs.
    let fi = p.start(&p.first, "true", None);
    assert_eq!(p.done(&fi, "pin")["exit"], 1);
    let s = p.status(&fi);
    assert_eq!(s["nodes"]["pin"]["state"], "done");
    assert_eq!(s["nodes"]["check"]["state"], "blocked");
    assert_eq!(
        s["nodes"]["check"]["unmet"],
        json!([{ "reason": "switch", "on": "pin", "took": "moved", "case": "at_head" }])
    );
    let v = p.refused(&["flow", "claim", "--instance", &fi, "--node-id", "check"]);
    assert_eq!(v["code"], "not_enabled");
}

#[test]
fn v3_the_projects_own_check_run_by_the_kernel_is_the_result() {
    let p = pilot();
    let fi = p.start(&p.head, "test -f passed", None);
    p.done(&fi, "pin");
    let token = p.claim(&fi, "check");
    // Failing: refused with its exit status, never a receipt, however often it is tried.
    for _ in 0..2 {
        let (v, ok) = p.complete(&token, Value::Null);
        assert!(!ok);
        assert_eq!((&v["code"], &v["reason"]), (&json!("verify_failed"), &json!("the command exited 1")), "{v}");
    }
    let events = p.events(&fi);
    let check: Vec<&Value> = events.iter().filter(|e| e["body"]["node"] == "check").collect();
    assert_eq!(
        check.iter().map(|e| e["kind"].as_str().unwrap()).collect::<Vec<_>>(),
        ["Claimed", "Refused", "Refused"]
    );
    assert_eq!(check[1]["body"]["reason"], "the command exited 1");
    assert_eq!(p.status(&fi)["nodes"]["check"]["state"], "ready");
    // Passing: the receipt records the command the kernel ran and its exit status, 0.
    std::fs::write(p.repo.join("passed"), "").unwrap();
    let (v, ok) = p.complete(&token, Value::Null);
    assert!(ok, "{v}");
    let receipt = p.events(&fi).pop().unwrap()["body"].clone();
    assert_eq!(receipt["node"], "check");
    assert_eq!(receipt["verify"]["exit"], 0);
    let argv = receipt["verify"]["argv"].as_array().unwrap();
    assert_eq!(argv[argv.len() - 2..], [json!(p.head), json!("test -f passed")]);

    // Another exit status is refused under it too: the status is the check's own.
    let fi = p.start(&p.head, "exit 3", None);
    p.done(&fi, "pin");
    let (v, ok) = p.complete(&p.claim(&fi, "check"), Value::Null);
    assert!(!ok);
    assert_eq!(v["reason"], "the command exited 3", "{v}");
    assert!(!kinds(&p.events(&fi)).ends_with(&["Completed"]));
}

#[test]
fn v4_without_a_surface_exercise_never_becomes_enabled() {
    let p = pilot();
    let fi = p.start(&p.head, "true", None);
    p.done(&fi, "pin");
    assert_eq!(p.done(&fi, "surface")["exit"], 1);
    p.done(&fi, "check");
    let next = p.ok(&["flow", "next", "--instance", &fi])["next"].clone();
    assert_eq!(next["enabled"], json!([]));
    assert_eq!(
        next["pending"]["exercise"]["unmet"],
        json!([{ "reason": "switch", "on": "surface", "took": "none", "case": "declared" }])
    );
    let v = p.refused(&["flow", "claim", "--instance", &fi, "--node-id", "exercise"]);
    assert_eq!(v["code"], "not_enabled");
}

#[test]
fn v4_exercise_completes_only_with_a_passing_receipt_for_the_pinned_head_and_surface() {
    let p = pilot();
    let fi = p.start(&p.head, "true", Some("cli"));
    for node in ["pin", "surface", "check"] {
        assert_eq!(p.done(&fi, node)["exit"], 0, "{node}");
    }
    let next = p.ok(&["flow", "next", "--instance", &fi])["next"].clone();
    assert_eq!(next["enabled"].as_array().unwrap().len(), 1);
    assert_eq!(next["enabled"][0]["node"], "exercise");
    assert_eq!(next["enabled"][0]["launch"]["skill"], "dogfood");
    let token = p.claim(&fi, "exercise");

    // No receipt named: the performer's to fix, refused before anything runs.
    let (v, _) = p.complete(&token, json!({}));
    assert_eq!(v["code"], "outputs_invalid", "{v}");
    // Each receipt the verifier refuses is refused with its exit status and reason.
    let other_head = p.receipt("other-head", |v| v["head"] = json!(p.first));
    let web = p.receipt("web", |v| v["surface"] = json!("web"));
    let failed = p.receipt("failed", |v| v["result"] = json!("fail"));
    let tampered = p.receipt("tampered", |v| v["evidence"]["sha256"] = json!(dogfood::sha256(b"else")));
    for (path, exit, code) in [
        (p.root.join("receipts/none.json"), 3, "receipt_missing"),
        (other_head, 6, "wrong_head"),
        (web, 7, "wrong_surface"),
        (failed, 8, "result_not_pass"),
        (tampered, 10, "evidence_mismatch"),
    ] {
        let (v, ok) = p.complete(&token, json!({ "receipt": path }));
        assert!(!ok, "{path:?}");
        assert_eq!(
            (&v["code"], &v["reason"]),
            (&json!("verify_failed"), &json!(format!("the command exited {exit}"))),
            "{path:?}: {v}"
        );
        assert!(v["stderr"].as_str().unwrap().contains(code), "{v}");
    }
    assert_eq!(p.status(&fi)["nodes"]["exercise"]["state"], "ready");

    // A passing receipt for the pinned head and the surface: the receipt names it in its verify.
    let good = p.receipt("good", |_| {});
    let (v, ok) = p.complete(&token, json!({ "receipt": good }));
    assert!(ok, "{v}");
    assert_eq!(v["verify"]["exit"], 0);
    assert_eq!(
        v["verify"]["argv"],
        json!([
            "maestro",
            "dogfood",
            "verify-receipt",
            format!("--receipt={}", good.display()),
            format!("--head={}", p.head),
            "--surface=cli"
        ])
    );
    assert_eq!(p.status(&fi)["nodes"]["exercise"]["state"], "done");

    // The accepting spine: a clean run of the same shape is equal to this one, refusals and
    // all; a run without a surface is not, and `flow spine --against` exits 1 on it.
    let clean = p.start(&p.head, "true", Some("cli"));
    for node in ["pin", "surface", "check"] {
        p.done(&clean, node);
    }
    let (v, ok) = p.complete(&p.claim(&clean, "exercise"), json!({ "receipt": good }));
    assert!(ok, "{v}");
    let same = p.ok(&["flow", "spine", "--instance", &fi, "--against", &clean]);
    assert_eq!(same["equal"], true);
    let nodes: Vec<&str> =
        same["spine"]["records"].as_array().unwrap().iter().map(|r| r["node"].as_str().unwrap()).collect();
    assert_eq!(nodes, ["pin", "surface", "check", "exercise"]);

    let bare = p.start(&p.head, "true", None);
    for node in ["pin", "surface", "check"] {
        p.done(&bare, node);
    }
    let v = p.refused(&["flow", "spine", "--instance", &fi, "--against", &bare]);
    assert_eq!(v["code"], "spine_differs");
    assert_eq!(
        v["difference"],
        json!({ "difference": "exit", "node": "surface", "iteration": {}, "index": 1, "exit": 0, "against": 1 })
    );
}

/// The verifier on its own: one JSON line, and each failure's own exit status, with no workspace
/// and only PATH in its environment, as the kernel runs it.
#[test]
fn verify_receipt_exits_with_each_failures_own_status() {
    let p = pilot();
    let verify = |receipt: &Path, head: &str| {
        let out = Command::new(env!("CARGO_BIN_EXE_maestro"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .current_dir(&p.repo)
            .args(["dogfood", "verify-receipt"])
            .arg(format!("--receipt={}", receipt.display()))
            .arg(format!("--head={head}"))
            .arg("--surface=cli")
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        (out.status.code(), v, String::from_utf8(out.stderr).unwrap())
    };
    let good = p.receipt("good", |_| {});
    let (code, v, stderr) = verify(&good, &p.head);
    assert_eq!((code, &v["ok"], stderr.as_str()), (Some(0), &json!(true), ""));
    assert_eq!(v["evidence_sha256"], dogfood::sha256(b"maestro flow next printed the enabled nodes\n"));
    assert_eq!(v["verifier"], "dogfood-receipt@1");

    let malformed = p.receipt("malformed", |v| v["at"] = json!("soon"));
    let gone = p.receipt("gone", |v| v["evidence"]["path"] = json!("missing.log"));
    for (receipt, head, want, code_name) in [
        (p.root.join("nowhere.json"), p.head.as_str(), 3, "receipt_missing"),
        (p.root.clone(), p.head.as_str(), 4, "receipt_unreadable"),
        (malformed, p.head.as_str(), 5, "receipt_malformed"),
        (good.clone(), p.first.as_str(), 6, "wrong_head"),
        (gone, p.head.as_str(), 9, "evidence_unreadable"),
    ] {
        let (code, v, stderr) = verify(&receipt, head);
        assert_eq!((code, &v["ok"], &v["code"]), (Some(want), &json!(false), &json!(code_name)), "{receipt:?}");
        assert_eq!(stderr, format!("maestro: {code_name}: {}\n", v["reason"].as_str().unwrap()));
    }
    // A relative receipt is taken from the working directory, where the kernel runs it.
    std::fs::copy(&good, p.repo.join("receipt.json")).unwrap();
    std::fs::copy(good.parent().unwrap().join("run.log"), p.repo.join("run.log")).unwrap();
    assert_eq!(verify(Path::new("receipt.json"), &p.head).0, Some(0));
}
