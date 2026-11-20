//! kernel-ready@1 end to end through the built binary (BRO-3842): the flow's own definition
//! (`crates/flow/flows/kernel-ready.yaml`) and check script (`scripts/kernel-ready.py`) on a
//! scratch repository and a scratch home, driven `flow start`, `claim` and `complete`, with the
//! kernel running every command.
//!
//! The contract's first clause: every check node fails closed when its subject is broken. Each
//! test below seeds one fault into one node's subject and shows the kernel refuses that node,
//! after a sound installation has carried the chain up to it. The sound run reaches the owner's
//! `ready` node and nothing past it.
//!
//! The scratch home stands in for the owner's: `~/.local/bin/maestro` is a shim that reports a
//! build commit and runs the built binary for everything else, `~/.local/bin/codex` answers
//! `hooks/list`, and the Claude Code and Codex settings wire the Stop gate.

use maestro_flow::dogfood;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const DEFINITION: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../flow/flows/kernel-ready.yaml");
const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/kernel-ready.py");
const CHAIN: [&str; 6] = ["pin", "suite", "installed", "mcp_tools", "stop_gate", "consumers"];

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

fn executable(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// How the scratch installation is broken, if at all.
#[derive(Default, Clone, Copy)]
struct Faults {
    /// The commit the installed binary reports; the head when none.
    commit: Option<&'static str>,
    /// The MCP server lists no `maestro_flow_spine`.
    mcp_missing: bool,
    /// No Stop hook for this harness.
    unwired: Option<&'static str>,
    /// Codex's Stop hook is untrusted, or disabled.
    untrusted: bool,
    disabled: bool,
    /// Codex lists its Stop hook with a command other than the gate's.
    foreign: bool,
    /// The consumer's `make maestro-check` fails.
    consumer_fails: bool,
}

struct Rig {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    first: String,
    head: String,
}

fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir_all(root.join("workspace/.broomva")).unwrap();
    std::fs::create_dir_all(root.join("no-hooks")).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join("scripts")).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "core.hooksPath", root.join("no-hooks").to_str().unwrap()]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "one"]);
    let first = git(&repo, &["rev-parse", "HEAD"]);
    std::fs::copy(SCRIPT, repo.join("scripts/kernel-ready.py")).unwrap();
    git(&repo, &["add", "scripts"]);
    git(&repo, &["commit", "-q", "-m", "two"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    // The owner's session, its codex child (another provider than the writer's, claude) and its
    // claude child (the writer's provider).
    let registry = json!([
        { "id": "d1", "cwd": repo, "token": "tok-1", "liveness": "live" },
        { "id": "kid", "cwd": repo, "token": "tok-2", "liveness": "live", "harness": "codex", "parent_id": "d1" },
        { "id": "twin", "cwd": repo, "token": "tok-3", "liveness": "live", "harness": "claude-code", "parent_id": "d1" },
    ]);
    std::fs::write(root.join("direct.json"), registry.to_string()).unwrap();
    let rig = Rig { _tmp: tmp, root, repo, first, head };
    rig.install(Faults::default());
    rig
}

fn run(cmd: &mut Command) -> (Value, Output) {
    let out = cmd.output().unwrap();
    let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!("not JSON: {}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    });
    (v, out)
}

impl Rig {
    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn consumer(&self) -> PathBuf {
        self.root.join("consumer")
    }

    /// Writes the scratch installation, broken as `f` says.
    fn install(&self, f: Faults) {
        let home = self.home();
        let maestro = home.join(".local/bin/maestro");
        let commit = f.commit.unwrap_or(&self.head);
        let mcp = if f.mcp_missing {
            // A server whose tool list lacks one flow verb.
            concat!(
                "if [ \"$1\" = mcp ]; then\n",
                "  printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}' ",
                "'{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"maestro_flow_start\"}]}}'\n",
                "  exec cat >/dev/null\n",
                "fi\n"
            )
        } else {
            ""
        };
        executable(
            &maestro,
            &format!(
                "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'maestro 0.1.0 ({commit})'; exit 0; fi\n{mcp}exec '{}' \"$@\"\n",
                env!("CARGO_BIN_EXE_maestro")
            ),
        );
        let gate = |harness: &str| format!("{} flow stop-gate --harness {harness}", maestro.display());
        for (harness, settings) in [("claude", ".claude/settings.json"), ("codex", ".codex/hooks.json")] {
            let path = home.join(settings);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let hooks = if f.unwired == Some(harness) {
                json!({ "hooks": {} })
            } else {
                json!({ "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": gate(harness) }] }] } })
            };
            std::fs::write(path, hooks.to_string()).unwrap();
        }
        let hook = json!({
            "key": format!("{}:stop:0:0", home.join(".codex/hooks.json").display()),
            "eventName": "stop",
            "command": if f.foreign { "/usr/bin/true".to_string() } else { gate("codex") },
            "enabled": !f.disabled,
            "trustStatus": if f.untrusted { "untrusted" } else { "trusted" },
        });
        let listing = json!({ "jsonrpc": "2.0", "id": 2, "result": { "data": [{ "cwd": home, "hooks": [hook] }] } });
        executable(
            &home.join(".local/bin/codex"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{}}}}' '{listing}'\nexec cat >/dev/null\n"
            ),
        );
        let recipe = if f.consumer_fails { "\t@exit 3\n" } else { "\t@\"$(MAESTRO)\" flow --help >/dev/null\n" };
        std::fs::create_dir_all(self.consumer()).unwrap();
        std::fs::write(self.consumer().join("Makefile"), format!("maestro-check:\n{recipe}")).unwrap();
    }

    /// The binary as the verified Direct session `d1`, its home the scratch one.
    fn maestro(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_maestro"));
        cmd.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.home())
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

    fn start_with(&self, head: &str, suite: &str) -> String {
        let params = json!({
            "repo": self.repo, "head": head, "suite": suite, "consumer": self.consumer(),
            "writer_provider": "claude", "surface": "cli",
        });
        let v = self.ok(&["flow", "start", "--definition", DEFINITION, "--params", &params.to_string()]);
        v["instance"].as_str().unwrap().to_string()
    }

    fn start(&self) -> String {
        self.start_with(&self.head, "true")
    }

    fn claim(&self, fi: &str, node: &str) -> String {
        self.ok(&["flow", "claim", "--instance", fi, "--node-id", node])["claim_token"].as_str().unwrap().to_string()
    }

    /// Claims `node` and completes it, naming `performer` if any: the JSON, and whether it exited 0.
    fn attempt(&self, fi: &str, node: &str, outputs: Value, performer: Option<&str>) -> (Value, bool) {
        let token = self.claim(fi, node);
        self.complete(&token, outputs, performer)
    }

    /// Completes the claim `token` (a refused completion keeps its claim while the lease holds).
    fn complete(&self, token: &str, outputs: Value, performer: Option<&str>) -> (Value, bool) {
        let outputs = outputs.to_string();
        let mut args = vec!["flow", "complete", "--claim-token", token, "--outputs", &outputs];
        if let Some(p) = performer {
            args.extend(["--performer-session", p]);
        }
        let (v, out) = run(self.maestro().args(&args));
        (v, out.status.success())
    }

    fn done(&self, fi: &str, node: &str) {
        let (v, ok) = self.attempt(fi, node, Value::Null, None);
        assert!(ok && v["ok"] == true, "{node}: {v}");
    }

    /// The chain's nodes before `node`, completed on a sound installation.
    fn up_to(&self, fi: &str, node: &str) {
        for n in CHAIN.iter().take_while(|n| **n != node) {
            self.done(fi, n);
        }
    }

    /// `node` is refused with `code`, and stays not done.
    fn refused(&self, fi: &str, node: &str, code: &str) -> Value {
        let (v, ok) = self.attempt(fi, node, Value::Null, None);
        assert!(!ok && v["ok"] == false && v["code"] == code, "{node} must be refused {code}: {v}");
        assert_ne!(self.states(fi)[node], "done", "{node}");
        v
    }

    fn states(&self, fi: &str) -> Value {
        let s = self.ok(&["flow", "status", "--instance", fi])["status"].clone();
        let nodes = s["nodes"].as_object().unwrap();
        Value::Object(nodes.iter().map(|(k, v)| (k.clone(), v["state"].clone())).collect())
    }

    /// A passing dogfood receipt for `head`, its evidence beside it; the outputs naming it.
    fn receipt(&self, head: &str) -> Value {
        let dir = self.root.join("receipts").join(&head[..8]);
        std::fs::create_dir_all(&dir).unwrap();
        let evidence = b"a fresh verify@1 instance reached terminal\n";
        std::fs::write(dir.join("run.log"), evidence).unwrap();
        let v = json!({
            "schema": dogfood::SCHEMA, "head": head, "surface": "cli", "entry": "maestro flow start",
            "driver": "shell", "evidence": { "path": "run.log", "sha256": dogfood::sha256(evidence) },
            "result": "pass", "at": "2026-11-20T12:00:00Z",
        });
        let path = dir.join("receipt.json");
        std::fs::write(&path, v.to_string()).unwrap();
        json!({ "receipt": path, "receipt_sha256": dogfood::sha256(&std::fs::read(&path).unwrap()) })
    }
}

#[test]
fn a_sound_installation_reaches_the_owner_and_nothing_else() {
    let r = rig();
    let fi = r.start();
    r.up_to(&fi, "exercise");
    let (v, ok) = r.attempt(&fi, "exercise", r.receipt(&r.head), Some("direct:-/kid"));
    assert!(ok && v["ok"] == true, "exercise by a codex child: {v}");
    let states = r.states(&fi);
    for n in CHAIN.iter().chain(["exercise"].iter()) {
        assert_eq!(states[*n], "done", "{n}: {states}");
    }
    assert_eq!(states["ready"], "waiting", "the owner's node waits for the owner: {states}");
    let spine = r.ok(&["flow", "spine", "--instance", &fi]);
    let nodes: Vec<&str> =
        spine["spine"]["records"].as_array().unwrap().iter().map(|x| x["node"].as_str().unwrap()).collect();
    assert_eq!(nodes, ["pin", "suite", "installed", "mcp_tools", "stop_gate", "consumers", "exercise"], "{spine}");
}

#[test]
fn pin_refuses_another_head_and_a_tracked_change() {
    let r = rig();
    let fi = r.start_with(&r.first, "true");
    r.refused(&fi, "pin", "exit_unmapped");
    let fi = r.start();
    std::fs::write(r.repo.join("scripts/kernel-ready.py"), "import sys; sys.exit(0)\n").unwrap();
    r.refused(&fi, "pin", "exit_unmapped");
}

#[test]
fn suite_refuses_a_failing_suite() {
    let r = rig();
    let fi = r.start_with(&r.head, "false");
    r.up_to(&fi, "suite");
    r.refused(&fi, "suite", "verify_failed");
}

#[test]
fn installed_refuses_another_commit_a_dirty_build_and_none() {
    let r = rig();
    let dirty = format!("{}-dirty", r.head);
    for commit in [r.first.as_str(), dirty.as_str(), "unknown"] {
        let fi = r.start();
        r.up_to(&fi, "installed");
        r.install(Faults { commit: Some(commit.to_string().leak()), ..Faults::default() });
        let v = r.refused(&fi, "installed", "exit_unmapped");
        assert!(v["stderr"].as_str().unwrap().contains(&format!("was built from {commit}")), "{commit}: {v}");
        r.install(Faults::default());
    }
}

#[test]
fn mcp_tools_refuses_a_server_missing_a_flow_verb() {
    let r = rig();
    let fi = r.start();
    r.up_to(&fi, "mcp_tools");
    r.install(Faults { mcp_missing: true, ..Faults::default() });
    let v = r.refused(&fi, "mcp_tools", "exit_unmapped");
    assert!(v["stderr"].as_str().unwrap().contains("maestro_flow_spine"), "{v}");
}

#[test]
fn stop_gate_refuses_an_unwired_harness_and_a_codex_hook_codex_will_not_run() {
    let r = rig();
    let faults = [
        (Faults { unwired: Some("claude"), ..Faults::default() }, "not wired for claude"),
        (Faults { unwired: Some("codex"), ..Faults::default() }, "not wired for codex"),
        (Faults { untrusted: true, ..Faults::default() }, "disabled or untrusted"),
        (Faults { disabled: true, ..Faults::default() }, "disabled or untrusted"),
        (Faults { foreign: true, ..Faults::default() }, "lists no Stop hook running the gate"),
    ];
    for (fault, reason) in faults {
        let fi = r.start();
        r.up_to(&fi, "stop_gate");
        r.install(fault);
        let v = r.refused(&fi, "stop_gate", "exit_unmapped");
        assert!(v["stderr"].as_str().unwrap().contains(reason), "{reason}: {v}");
        r.install(Faults::default());
    }
}

#[test]
fn consumers_refuses_a_failing_consumer_check() {
    let r = rig();
    let fi = r.start();
    r.up_to(&fi, "consumers");
    r.install(Faults { consumer_fails: true, ..Faults::default() });
    let v = r.refused(&fi, "consumers", "exit_unmapped");
    assert!(v["stderr"].as_str().unwrap().contains("make maestro-check in"), "{v}");
}

#[test]
fn exercise_refuses_the_writers_provider_and_another_heads_receipt() {
    let r = rig();
    let fi = r.start();
    r.up_to(&fi, "exercise");
    let token = r.claim(&fi, "exercise");
    let (v, ok) = r.complete(&token, r.receipt(&r.head), Some("direct:-/twin"));
    assert!(!ok && v["code"] == "routing_violated", "a claude performer for a claude writer: {v}");
    let (v, ok) = r.complete(&token, r.receipt(&r.first), Some("direct:-/kid"));
    assert!(!ok && v["code"] == "verify_failed", "a receipt for another head: {v}");
    assert_ne!(r.states(&fi)["exercise"], "done");
}

/// `maestro --version` names the commit it was built from (`build.rs`), with `-dirty` exactly when
/// a tracked build source differs from it, which is what `installed` compares with the head.
#[test]
fn the_version_names_the_build_commit() {
    let out = Command::new(env!("CARGO_BIN_EXE_maestro")).arg("--version").output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let head = git(dir, &["rev-parse", "HEAD"]);
    let top = git(dir, &["rev-parse", "--show-toplevel"]);
    let changed = git(
        Path::new(&top),
        &["status", "--porcelain", "--untracked-files=no", "--", "crates", "Cargo.toml", "Cargo.lock"],
    );
    let commit = if changed.is_empty() { head } else { format!("{head}-dirty") };
    assert_eq!(text.trim(), format!("maestro {} ({commit})", env!("CARGO_PKG_VERSION")));
}
