//! `verify@1` end to end through the built binary (BRO-3636): the flow's own definition
//! (`crates/flow/flows/verify.yaml`) on a scratch git repository, driven `flow start`, `next`,
//! `claim` and `complete`, with the kernel running every command, `maestro dogfood verify-receipt`
//! included (the kernel runs its own executable for it). The contract's V1, V3 and V4, the
//! verifier's exit statuses, the commands an agent's `PATH` cannot stand in for, and `flow spine
//! --against` over real runs. V2 needs the kernel's test clock and runs in the crate
//! (`maestro_flow`'s `kernel::piloted`).
//!
//! The repository has an empty `core.hooksPath` of its own, so no hook of the host's runs in it.

use maestro_flow::dogfood;
use serde_json::{Value, json};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

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
/// session's folder; the second adds `tracked`), and a Direct registry with one live session there.
struct Pilot {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    /// The repository's first commit, and its HEAD.
    first: String,
    head: String,
    /// Folders put first on the binary's PATH.
    before: Vec<PathBuf>,
}

fn pilot() -> Pilot {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir_all(root.join("workspace/.broomva")).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(root.join("no-hooks")).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "core.hooksPath", root.join("no-hooks").to_str().unwrap()]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "one"]);
    let first = git(&repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join("tracked"), "").unwrap();
    git(&repo, &["add", "tracked"]);
    git(&repo, &["commit", "-q", "-m", "two"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let registry = json!([{ "id": "d1", "cwd": repo, "token": "tok-1", "liveness": "live" }]);
    std::fs::write(root.join("direct.json"), registry.to_string()).unwrap();
    Pilot { _tmp: tmp, root, repo, first, head, before: Vec::new() }
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
    /// The binary as the verified Direct session `d1`, with `before` first on its PATH.
    fn maestro(&self) -> Command {
        let caller = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(self.before.iter().cloned().chain(std::env::split_paths(&caller))).unwrap();
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

    fn start_with(&self, definition: &Path, head: &str, check: &str, surface: Option<&str>) -> String {
        let mut params = json!({ "repo": self.repo, "head": head, "check": check });
        if let Some(surface) = surface {
            params["surface"] = json!(surface);
        }
        let definition = definition.to_str().unwrap();
        let v = self.ok(&["flow", "start", "--definition", definition, "--params", &params.to_string()]);
        v["instance"].as_str().unwrap().to_string()
    }

    fn start(&self, head: &str, check: &str, surface: Option<&str>) -> String {
        self.start_with(Path::new(VERIFY), head, check, surface)
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

    /// An instance at HEAD with a surface, its pin, surface and check done: exercise is ready.
    fn exercising(&self) -> String {
        let fi = self.start(&self.head, "true", Some("cli"));
        for node in ["pin", "surface", "check"] {
            assert_eq!(self.done(&fi, node)["exit"], 0, "{node}");
        }
        fi
    }
}

/// The outputs that name the receipt at `path` and its digest as it is now; only a regular file
/// is read for it.
fn named(path: &Path) -> Value {
    let regular = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file());
    let digest = if regular { dogfood::sha256(&std::fs::read(path).unwrap()) } else { dogfood::sha256(b"") };
    json!({ "receipt": path, "receipt_sha256": digest })
}

fn kinds(events: &[Value]) -> Vec<&str> {
    events.iter().map(|e| e["kind"].as_str().unwrap()).collect()
}

/// `maestro dogfood verify-receipt` with `input` on its standard input, no workspace and only PATH
/// in its environment, in the repository, as the kernel runs it: the exit status, the JSON and
/// standard error.
fn verify_receipt(p: &Pilot, input: &[u8], head: &str) -> (Option<i32>, Value, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_maestro"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .current_dir(&p.repo)
        .args(["dogfood", "verify-receipt", &format!("--head={head}"), "--surface=cli"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The verifier reads to one byte past its bound and no further, so input longer than that may
    // meet a closed pipe; what it read is what the test judges.
    if let Err(e) = child.stdin.take().unwrap().write_all(input) {
        assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe, "{e}");
    }
    let out = child.wait_with_output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    (out.status.code(), v, String::from_utf8(out.stderr).unwrap())
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
    // Failing (the head has no `passed`): refused with its exit status, never a receipt, however
    // often it is tried. A `passed` in the working tree changes nothing: the check reads the head.
    std::fs::write(p.repo.join("passed"), "").unwrap();
    let fi = p.start(&p.head, "test -f passed", None);
    p.done(&fi, "pin");
    let token = p.claim(&fi, "check");
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

    // Passing (the head has `tracked`): the receipt records the command the kernel ran and its
    // exit status, 0.
    let fi = p.start(&p.head, "test -f tracked", None);
    p.done(&fi, "pin");
    let (v, ok) = p.complete(&p.claim(&fi, "check"), Value::Null);
    assert!(ok, "{v}");
    let receipt = p.events(&fi).pop().unwrap()["body"].clone();
    assert_eq!(receipt["node"], "check");
    assert_eq!(receipt["verify"]["exit"], 0);
    assert_eq!(receipt["verify"]["argv"], json!(["sh", "-c", "test -f tracked"]));

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
    let fi = p.exercising();
    let next = p.ok(&["flow", "next", "--instance", &fi])["next"].clone();
    assert_eq!(next["enabled"].as_array().unwrap().len(), 1);
    assert_eq!(next["enabled"][0]["node"], "exercise");
    assert_eq!(next["enabled"][0]["launch"]["skill"], "dogfood");
    let token = p.claim(&fi, "exercise");

    // Each receipt the verifier refuses is refused with its exit status and reason.
    let fifo = p.root.join("receipts/fifo.json");
    std::fs::create_dir_all(fifo.parent().unwrap()).unwrap();
    assert!(Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    let good = p.receipt("good", |_| {});
    let changed = json!({ "receipt": good, "receipt_sha256": dogfood::sha256(b"another receipt") });
    let cases = [
        (json!({}), 11, "outputs_invalid"),
        (named(&p.root.join("receipts/none.json")), 3, "receipt_missing"),
        (named(&fifo), 13, "receipt_not_regular"),
        (changed, 12, "receipt_changed"),
        (named(&p.receipt("other-head", |v| v["head"] = json!(p.first))), 6, "wrong_head"),
        (named(&p.receipt("web", |v| v["surface"] = json!("web"))), 7, "wrong_surface"),
        (named(&p.receipt("failed", |v| v["result"] = json!("fail"))), 8, "result_not_pass"),
        (
            named(&p.receipt("tampered", |v| v["evidence"]["sha256"] = json!(dogfood::sha256(b"else")))),
            10,
            "evidence_mismatch",
        ),
    ];
    for (outputs, exit, code) in cases {
        let (v, ok) = p.complete(&token, outputs.clone());
        assert!(!ok, "{outputs}");
        assert_eq!(
            (&v["code"], &v["reason"]),
            (&json!("verify_failed"), &json!(format!("the command exited {exit}"))),
            "{outputs}: {v}"
        );
        assert!(v["stderr"].as_str().unwrap().contains(code), "{v}");
    }
    assert_eq!(p.status(&fi)["nodes"]["exercise"]["state"], "ready");

    // A passing receipt for the pinned head and the surface. The verify's argv names no output:
    // the outputs reached it on its standard input, and the log records them.
    let outputs = named(&good);
    let (v, ok) = p.complete(&token, outputs.clone());
    assert!(ok, "{v}");
    assert_eq!(v["verify"]["exit"], 0);
    assert_eq!(
        v["verify"]["argv"],
        json!(["maestro", "dogfood", "verify-receipt", format!("--head={}", p.head), "--surface=cli"])
    );
    assert_eq!(p.status(&fi)["nodes"]["exercise"]["state"], "done");
    let logged = p.events(&fi).pop().unwrap()["body"]["outputs"].clone();
    assert_eq!(logged, outputs);

    // The log pins the receipt's content: one edited after the completion no longer matches the
    // logged digest, and the verifier refuses it on the logged outputs.
    let mut edited: Value = serde_json::from_slice(&std::fs::read(&good).unwrap()).unwrap();
    edited["entry"] = json!("a story told afterwards");
    std::fs::write(&good, edited.to_string()).unwrap();
    assert_ne!(dogfood::sha256(&std::fs::read(&good).unwrap()), logged["receipt_sha256"].as_str().unwrap());
    let (code, v, _) = verify_receipt(&p, logged.to_string().as_bytes(), &p.head);
    assert_eq!((code, &v["code"]), (Some(12), &json!("receipt_changed")));
}

#[test]
fn a_maestro_sh_git_or_test_first_on_the_agents_path_passes_nothing() {
    // BRO-3636 round 1: an agent that runs `flow complete` with a folder of its own first on
    // PATH. Each fake exits 0 (git prints the head the instance names), so any of them run in
    // place of the real one would pass its node.
    let mut p = pilot();
    let fakes = p.root.join("fakes");
    std::fs::create_dir_all(&fakes).unwrap();
    for (name, body) in
        [("maestro", "exit 0"), ("sh", "exit 0"), ("test", "exit 0"), ("git", &format!("echo {}", p.first))]
    {
        let path = fakes.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }
    p.before = vec![fakes];
    // pin: the repository is not at `first`, whatever a git on PATH says.
    let fi = p.start(&p.first, "true", None);
    assert_eq!(p.done(&fi, "pin")["exit"], 1);
    // surface: none declared, whatever a test on PATH says.
    let fi = p.start(&p.head, "exit 1", None);
    assert_eq!(p.done(&fi, "surface")["exit"], 1);
    // check: the project's command fails, whatever an sh on PATH says.
    p.done(&fi, "pin");
    let (v, ok) = p.complete(&p.claim(&fi, "check"), Value::Null);
    assert_eq!((ok, &v["reason"]), (false, &json!("the command exited 1")), "{v}");
    // exercise: no receipt is named, whatever a maestro on PATH says.
    let fi = p.exercising();
    let (v, ok) = p.complete(&p.claim(&fi, "exercise"), json!({}));
    assert_eq!((ok, &v["reason"]), (false, &json!("the command exited 11")), "{v}");
}

#[test]
fn the_spine_compares_runs_of_one_definition_and_reports_another_on_its_own() {
    let p = pilot();
    let good = p.receipt("good", |_| {});
    // Exercise completed with the good receipt, after one refused attempt when `refused_first`.
    let exercised = |fi: &str, refused_first: bool| {
        let token = p.claim(fi, "exercise");
        if refused_first {
            assert!(!p.complete(&token, json!({})).1);
        }
        let (v, ok) = p.complete(&token, named(&good));
        assert!(ok, "{v}");
    };
    // A run refused once on the way and a clean run of the same shape are equal.
    let fi = p.exercising();
    exercised(&fi, true);
    let clean = p.exercising();
    exercised(&clean, false);
    let same = p.ok(&["flow", "spine", "--instance", &fi, "--against", &clean]);
    assert_eq!(same["equal"], true);
    let nodes: Vec<&str> =
        same["spine"]["records"].as_array().unwrap().iter().map(|r| r["node"].as_str().unwrap()).collect();
    assert_eq!(nodes, ["pin", "surface", "check", "exercise"]);

    // A run without a surface differs, and `flow spine --against` exits 1 on it.
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

    // Another definition is not compared at all: its own code, field and exit status (3).
    let other = p.root.join("verify-other.yaml");
    std::fs::write(&other, std::fs::read_to_string(VERIFY).unwrap().replace("fresh_for: 10m", "fresh_for: 11m"))
        .unwrap();
    let elsewhere = p.start_with(&other, &p.head, "true", Some("cli"));
    for node in ["pin", "surface", "check"] {
        p.done(&elsewhere, node);
    }
    exercised(&elsewhere, false);
    let (v, out) = run(p.maestro().args(["flow", "spine", "--instance", &fi, "--against", &elsewhere]));
    assert_eq!(
        (out.status.code(), &v["ok"], &v["code"]),
        (Some(3), &json!(false), &json!("definition_mismatch")),
        "{v}"
    );
    assert_eq!(v["definition_mismatch"]["flow"], "verify@1");
    assert_ne!(v["definition_mismatch"]["ir_hash"], v["definition_mismatch"]["against_ir_hash"]);
    assert!(v.get("difference").is_none(), "{v}");
}

/// The verifier on its own: the outputs on its standard input, one JSON line out, and each
/// failure's own exit status, with no workspace and only PATH in its environment.
#[test]
fn verify_receipt_exits_with_each_failures_own_status() {
    let p = pilot();
    let good = p.receipt("good", |_| {});
    let (code, v, stderr) = verify_receipt(&p, named(&good).to_string().as_bytes(), &p.head);
    assert_eq!((code, &v["ok"], stderr.as_str()), (Some(0), &json!(true), ""));
    assert_eq!(v["evidence_sha256"], dogfood::sha256(b"maestro flow next printed the enabled nodes\n"));
    assert_eq!(v["verifier"], "dogfood-receipt@1");

    let malformed = p.receipt("malformed", |v| v["at"] = json!("soon"));
    let gone = p.receipt("gone", |v| v["evidence"]["path"] = json!("missing.log"));
    // Valid outputs naming the good receipt, padded to one byte past the bound: only the size
    // refuses them, so a cap that read one byte less would take them and pass.
    let mut over = named(&good).to_string().into_bytes();
    over.resize(dogfood::MAX_OUTPUTS + 1, b' ');
    for (input, head, want, code_name) in [
        (b"not json".to_vec(), p.head.as_str(), 11, "outputs_invalid"),
        (over, p.head.as_str(), 11, "outputs_invalid"),
        (named(&p.root.join("nowhere.json")).to_string().into_bytes(), p.head.as_str(), 3, "receipt_missing"),
        (named(&p.root).to_string().into_bytes(), p.head.as_str(), 13, "receipt_not_regular"),
        (named(&malformed).to_string().into_bytes(), p.head.as_str(), 5, "receipt_malformed"),
        (named(&good).to_string().into_bytes(), p.first.as_str(), 6, "wrong_head"),
        (named(&gone).to_string().into_bytes(), p.head.as_str(), 9, "evidence_unreadable"),
    ] {
        let (code, v, stderr) = verify_receipt(&p, &input, head);
        assert_eq!((code, &v["ok"], &v["code"]), (Some(want), &json!(false), &json!(code_name)), "{code_name}");
        assert_eq!(stderr, format!("maestro: {code_name}: {}\n", v["reason"].as_str().unwrap()));
    }
    // A relative receipt is taken from the working directory, where the kernel runs it.
    std::fs::copy(&good, p.repo.join("receipt.json")).unwrap();
    std::fs::copy(good.parent().unwrap().join("run.log"), p.repo.join("run.log")).unwrap();
    let relative = json!({ "receipt": "receipt.json", "receipt_sha256": named(&good)["receipt_sha256"] });
    assert_eq!(verify_receipt(&p, relative.to_string().as_bytes(), &p.head).0, Some(0));
}
