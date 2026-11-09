//! The flow kernel's verbs as MCP tools (BRO-3634; BRO-3624 items 4 and 9): verified writers only,
//! reads for anyone, refusals as results, the workspace rule for the flows root, and a guarded
//! `choose` through the tool. Guards run in this binary again (`guard_process_entry`).

use maestro_core::session::PeerCredentials;
use maestro_hosts::{CallerClaim, DirectRegistry, DirectSession, PaseoRegistry, SessionLiveness, SurfaceRegistry};
use maestro_hosts::{NodeId, T3Registry};
use maestro_mcp::call_with_hosts;
use maestro_mcp::flow::{ASK, CHOOSE, CLAIM, COMPLETE, HEARTBEAT, MAX_NAME, NEXT, START, STATUS};
use maestro_store::{Paths, Store, TreeSource};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[test]
fn guard_process_entry() {
    maestro_flow::kernel::guard_entry();
}

/// review (work) -> pick (choice, guarded on the review's outputs) -> ship or fix.
pub const DECIDE: &str = r#"
flow: decide
version: 1
params: [dir]
workdir: "{{params.dir}}"
nodes:
  review: {class: work, verify: [sh, -c, 'test -f ok || { echo no ok file >&2; exit 1; }']}
  pick: {class: choice, needs: [review], edges: {ship: {guard: '.nodes.review.outputs.verdict == "pass"'}, fix: {guard: '.nodes.review.outputs.verdict != "pass"'}}}
  ship: {class: pure, run: ['true'], switch: {on: pick, case: ship}}
  fix: {class: pure, run: ['true'], switch: {on: pick, case: fix}}
"#;

/// A scratch machine: a workspace with `.broomva/`, a work folder for the flow's commands, and a
/// Direct registry with two live sessions there.
struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    store: Store,
    work: PathBuf,
    hosts: SurfaceRegistry,
}

fn fx() -> Fx {
    maestro_flow::kernel::guard_entry();
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir_all(root.join("workspace/.broomva")).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let direct = DirectRegistry::new();
    for (id, token) in [("d1", "tok-1"), ("d2", "tok-2")] {
        direct.register(DirectSession {
            id: id.into(),
            cwd: work.to_string_lossy().into_owned(),
            token: token.into(),
            liveness: SessionLiveness::Live,
            harness: None,
            pending_permissions: vec![],
        });
    }
    let store = Store::new(Paths::under(&root), TreeSource::WorkingTree);
    Fx { _tmp: tmp, root, store, work, hosts: SurfaceRegistry::new(None, None, Some(direct)) }
}

impl Fx {
    fn call(&self, tool: &str, args: Value) -> Value {
        call_with_hosts(&self.store, &self.hosts, tool, args).unwrap_or_else(|e| panic!("{tool}: {}", e.message))
    }

    /// `args` as Direct session `id`, with its launch token and folder.
    fn as_direct(&self, id: &str, token: &str, mut args: Value) -> Value {
        args["session"] = json!(format!("direct:-/{id}"));
        args["token"] = json!(token);
        args["cwd"] = json!(self.work);
        args
    }

    fn d1(&self, tool: &str, args: Value) -> Value {
        self.call(tool, self.as_direct("d1", "tok-1", args))
    }

    fn begin(&self) -> String {
        let v = self.d1(START, json!({ "definition": DECIDE, "params": { "dir": self.work } }));
        assert_eq!(v["ok"], true, "{v}");
        v["instance"].as_str().unwrap().to_string()
    }

    fn claim(&self, instance: &str, node: &str) -> String {
        let v = self.d1(CLAIM, json!({ "instance": instance, "node_id": node }));
        assert_eq!(v["ok"], true, "{v}");
        v["claim_token"].as_str().unwrap().to_string()
    }

    fn tree(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        tree(&self.root)
    }
}

pub fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                out.insert(rel, Vec::new());
                stack.push(path);
            } else {
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    out
}

fn code(v: &Value) -> &str {
    v["code"].as_str().unwrap_or_else(|| panic!("no code: {v}"))
}

#[test]
fn a_verified_direct_session_runs_a_flow_through_the_tools() {
    let f = fx();
    let fi = f.begin();
    let next = f.call(NEXT, json!({ "instance": fi }));
    assert_eq!(next["ok"], true, "{next}");
    let enabled: Vec<_> = next["next"]["enabled"].as_array().unwrap().iter().map(|e| e["node"].clone()).collect();
    assert_eq!(enabled, [json!("review")]);
    assert!(!next.to_string().contains("token"), "a read never returns a token: {next}");

    // The claim's fields, by name: the node as node_id, its first attempt, outside any repeat.
    let claimed = f.d1(CLAIM, json!({ "instance": fi, "node_id": "review" }));
    assert_eq!(
        (&claimed["node_id"], &claimed["attempt"], &claimed["iteration"]),
        (&json!("review"), &json!(1), &json!({}))
    );
    let review = claimed["claim_token"].as_str().unwrap().to_string();
    let lease = |v: &Value| v["lease_until"].as_str().unwrap().parse::<jiff::Timestamp>().unwrap();
    let beat = f.d1(HEARTBEAT, json!({ "claim_token": review }));
    assert_eq!(beat["ok"], true, "{beat}");
    assert!(lease(&beat) > lease(&claimed), "the beat moves the lease: {beat} {claimed}");
    // Limits the kernel does not allow are refused before the command runs: the timeout is passed on.
    let limits = f.d1(COMPLETE, json!({ "claim_token": review, "timeout_secs": 0 }));
    assert_eq!((code(&limits), &limits["recorded"]), ("invalid_limits", &json!(false)), "{limits}");
    // The command fails: recorded, with its standard error, and the token kept.
    let failed = f.d1(COMPLETE, json!({ "claim_token": review, "outputs": { "verdict": "pass" } }));
    assert_eq!((code(&failed), &failed["recorded"]), ("verify_failed", &json!(true)), "{failed}");
    assert_eq!(failed["stderr"], "no ok file\n", "{failed}");
    std::fs::write(f.work.join("ok"), "").unwrap();
    let done = f.d1(COMPLETE, json!({ "claim_token": review, "outputs": { "verdict": "pass" } }));
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(done["verify"]["exit"], 0);

    // Another session owns nothing here, whatever token it holds.
    let pick = f.claim(&fi, "pick");
    let other = f.call(CHOOSE, f.as_direct("d2", "tok-2", json!({ "claim_token": pick, "edge": "ship" })));
    assert_eq!(code(&other), "not_owner", "{other}");
    // A guard that is false is recorded and keeps the claim; one that holds is chosen. Both ran
    // in the evaluator: a process that is no guard host would say guard_host_missing.
    let fix = f.d1(CHOOSE, json!({ "claim_token": pick, "edge": "fix" }));
    assert_eq!((code(&fix), &fix["recorded"]), ("guard_false", &json!(true)), "{fix}");
    let ship = f.d1(CHOOSE, json!({ "claim_token": pick, "edge": "ship" }));
    assert_eq!(ship["ok"], true, "{ship}");
    assert_eq!(ship["edge"], "ship");

    let status = f.call(STATUS, json!({ "instance": fi }));
    let state = |node: &str| status["status"]["nodes"][node]["state"].clone();
    assert_eq!(
        (state("review"), state("pick"), state("ship"), state("fix")),
        (json!("done"), json!("done"), json!("ready"), json!("blocked"))
    );
}

/// BRO-3624 item 4: an unverified caller is refused every write verb and still reads; a claim
/// that does not verify is refused too. Neither writes anything.
#[test]
fn an_unverified_caller_reads_and_writes_nothing() {
    let f = fx();
    let fi = f.begin();
    let token = f.claim(&fi, "review");
    let before = f.tree();
    let writes = [
        (START, json!({ "definition": DECIDE, "params": { "dir": f.work } })),
        (CLAIM, json!({ "instance": fi, "node_id": "review" })),
        (HEARTBEAT, json!({ "claim_token": token })),
        (COMPLETE, json!({ "claim_token": token })),
        (CHOOSE, json!({ "claim_token": token, "edge": "ship" })),
        (ASK, json!({ "instance": fi, "node_id": "review", "ledger": "l.yaml", "ask": { "class": "decision" } })),
    ];
    for (tool, args) in &writes {
        // A Paseo session, with no Paseo registry here and no claim: unverified.
        let mut unverified = args.clone();
        unverified["session"] = json!("paseo:-/a1");
        let v = f.call(tool, unverified);
        assert_eq!((code(&v), &v["ok"], &v["recorded"]), ("caller_unverified", &json!(false), &json!(false)), "{tool}");
        // The registered Direct session with a wrong launch token: refused.
        let v = f.call(tool, f.as_direct("d1", "not-the-token", args.clone()));
        assert_eq!(code(&v), "caller_refused", "{tool}: {v}");
        assert_eq!(v["identity"], "invalid_token", "{tool}: {v}");
        assert!(f.tree() == before, "{tool} changed the machine");
    }
    for tool in [NEXT, STATUS] {
        let v = f.call(tool, json!({ "instance": fi }));
        assert_eq!(v["ok"], true, "{tool}: {v}");
    }
}

/// Reads take the shared lock and write nothing, the projection included (BRO-3624 item 8, at
/// the surface).
#[test]
fn flow_reads_write_nothing() {
    let f = fx();
    let fi = f.begin();
    f.claim(&fi, "review");
    let before = f.tree();
    for tool in [NEXT, STATUS] {
        assert_eq!(f.call(tool, json!({ "instance": fi }))["ok"], true);
        assert!(f.tree() == before, "{tool} changed the machine");
    }
    let missing = f.call(NEXT, json!({ "instance": "fi_nothing" }));
    assert_eq!(code(&missing), "unknown_instance", "{missing}");
}

/// The flows root is `.broomva/flows` in the path registry's workspace; a workspace with no
/// `.broomva`, or one that is a linked worktree, is refused, and only `start` makes the ledger.
#[test]
fn the_flows_root_is_the_main_checkouts_and_only_start_makes_it() {
    let f = fx();
    let ws = f.root.join("workspace");
    let start = json!({ "definition": DECIDE, "params": { "dir": f.work } });

    // Every verb but start finds no ledger, and makes none.
    let before = f.tree();
    let claimed = f.d1(CLAIM, json!({ "instance": "fi_x", "node_id": "review" }));
    assert_eq!(code(&claimed), "no_ledger", "{claimed}");
    for (tool, args) in [
        (HEARTBEAT, json!({ "claim_token": "fi_x.00" })),
        (COMPLETE, json!({ "claim_token": "fi_x.00" })),
        (CHOOSE, json!({ "claim_token": "fi_x.00", "edge": "ship" })),
    ] {
        assert_eq!(code(&f.d1(tool, args)), "no_ledger", "{tool}");
    }
    assert_eq!(code(&f.call(STATUS, json!({ "instance": "fi_x" }))), "no_ledger");
    // A start whose definition does not compile is refused before the ledger is made.
    let broken = f.d1(START, json!({ "definition": "flow: x\nnodes: [", "params": {} }));
    assert_eq!(code(&broken), "definition_invalid", "{broken}");
    assert!(f.tree() == before, "a verb other than start, or a refused start, made a ledger");

    // A linked worktree (its .git is a file) is refused, reads included.
    std::fs::write(ws.join(".git"), "gitdir: /elsewhere/.git/worktrees/w1\n").unwrap();
    let before = f.tree();
    assert_eq!(code(&f.d1(START, start.clone())), "workspace_is_worktree");
    assert_eq!(code(&f.call(NEXT, json!({ "instance": "fi_x" }))), "workspace_is_worktree");
    assert!(f.tree() == before, "a worktree got a ledger");
    // The main checkout's .git is a folder: the ledger is made there, and only there.
    std::fs::remove_file(ws.join(".git")).unwrap();
    std::fs::create_dir(ws.join(".git")).unwrap();
    assert_eq!(f.d1(START, start.clone())["ok"], true);
    assert!(ws.join(".broomva/flows/instances").is_dir());
    assert!(ws.join(".broomva/flows.lock").is_file());

    // A workspace with no .broomva folder (a stray path) gets nothing.
    std::fs::remove_dir_all(ws.join(".broomva")).unwrap();
    let before = f.tree();
    assert_eq!(code(&f.d1(START, start)), "workspace_unready");
    assert!(f.tree() == before, "a stray workspace got a ledger");
}

#[test]
fn names_passed_to_the_kernel_are_bounded() {
    let f = fx();
    let fi = f.begin();
    let long = "n".repeat(MAX_NAME + 1);
    let v = f.d1(CLAIM, json!({ "instance": fi, "node_id": long }));
    assert_eq!(code(&v), "name_too_long", "{v}");
    let v = f.d1(ASK, json!({ "instance": fi, "node_id": long }));
    assert_eq!(code(&v), "name_too_long", "{v}");
    assert!(v.to_string().len() < MAX_NAME, "the name is not echoed back");
    // At the bound, the kernel judges it.
    let v = f.d1(CLAIM, json!({ "instance": fi, "node_id": "n".repeat(MAX_NAME) }));
    assert_eq!(code(&v), "unknown_node", "{v}");
    // A refusal the kernel returns before writing is not recorded.
    assert_eq!(v["recorded"], false, "{v}");
    let pick = {
        std::fs::write(f.work.join("ok"), "").unwrap();
        let review = f.claim(&fi, "review");
        assert_eq!(f.d1(COMPLETE, json!({ "claim_token": review, "outputs": { "verdict": "pass" } }))["ok"], true);
        f.claim(&fi, "pick")
    };
    let v = f.d1(CHOOSE, json!({ "claim_token": pick, "edge": "e".repeat(MAX_NAME + 1) }));
    assert_eq!(code(&v), "name_too_long", "{v}");
    let v = f.d1(CHOOSE, json!({ "claim_token": pick, "edge": "e".repeat(MAX_NAME) }));
    assert_eq!(code(&v), "undeclared_edge", "{v}");
}

/// Every write tool's advertised schema carries the caller's claim (a shared daemon reads it only
/// from the call's arguments), still refuses any other field, and a verified Direct call fits it.
#[test]
fn write_schemas_carry_the_callers_claim() {
    let tools = maestro_mcp::MaestroServer::tools();
    for name in [START, CLAIM, HEARTBEAT, COMPLETE, CHOOSE, ASK] {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        let schema = Value::Object((*tool.input_schema).clone());
        let props = schema["properties"].as_object().unwrap_or_else(|| panic!("{name}: {schema}"));
        for field in ["session", "cwd", "token", "node", "credential"] {
            assert!(props.contains_key(field), "{name} does not declare `{field}`: {schema}");
        }
        assert_eq!(schema["additionalProperties"], false, "{name}");
    }
    // A Direct start as a schema-enforcing client sends it: every key declared, every required
    // key present, and the call verified and written.
    let f = fx();
    let call = f.as_direct("d1", "tok-1", json!({ "definition": DECIDE, "params": { "dir": f.work } }));
    let tool = tools.iter().find(|t| t.name == START).unwrap();
    let schema = Value::Object((*tool.input_schema).clone());
    for key in call.as_object().unwrap().keys() {
        assert!(schema["properties"].get(key).is_some(), "the schema refuses `{key}`");
    }
    for key in schema["required"].as_array().unwrap() {
        assert!(call.get(key.as_str().unwrap()).is_some(), "the call lacks `{key}`");
    }
    assert_eq!(f.call(START, call)["ok"], true);
}

/// A flow with no params starts with none given: they default to an empty object.
#[test]
fn missing_params_are_an_empty_object() {
    let f = fx();
    let bare = "flow: bare\nversion: 1\nnodes:\n  a: {class: pure, run: ['true']}\n";
    let v = f.d1(START, json!({ "definition": bare }));
    assert_eq!(v["ok"], true, "{v}");
}

/// The ledger's host id is the host name's first DNS label, lower case: the network suffix a Mac
/// takes from where it is (`.lan`, `.local`) is dropped.
#[test]
fn the_host_label_drops_the_network_suffix() {
    use maestro_mcp::flow::host_label;
    assert_eq!(host_label("Carloss-MBP.lan"), "carloss-mbp");
    assert_eq!(host_label("Carloss-MBP.local"), "carloss-mbp");
    assert_eq!(host_label("carloss-mbp"), "carloss-mbp");
    assert_eq!(host_label("-nuc box.tail"), "nuc-box");
}

/// Arguments outside a tool's schema are refused before anything runs: a model cannot pass a
/// field the tool would ignore, such as a raw event.
#[test]
fn unknown_arguments_are_refused() {
    let f = fx();
    let fi = f.begin();
    for (tool, args) in [
        (CLAIM, json!({ "instance": fi, "node_id": "review", "kind": "Completed", "body": {} })),
        (NEXT, json!({ "instance": fi, "seq": 0 })),
    ] {
        let e = call_with_hosts(&f.store, &f.hosts, tool, f.as_direct("d1", "tok-1", args)).unwrap_err();
        assert!(e.message.contains("unknown field"), "{tool}: {}", e.message);
    }
}

fn write_t3_thread(path: &Path, cwd: &Path) {
    let thread = json!([{
        "id": "mcp:flow-thread", "cwd": cwd, "environment_id": "env-flow", "provider": "codex",
        "created_at": "2026-11-07T00:00:00Z", "run_id": "run-1", "status": "running"
    }]);
    std::fs::write(path, thread.to_string()).unwrap();
}

fn write_paseo_agent(dir: &Path, id: &str, cwd: &Path) {
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let agent = json!({
        "id": id, "cwd": cwd, "provider": "claude", "createdAt": "2026-11-07T00:00:00Z",
        "lastStatus": "running", "pid": 4101, "startedAt": "2026-11-07T00:00:01Z",
        "persistence": { "sessionId": "handle-1" }
    });
    std::fs::write(dir.join("work").join(format!("{id}.json")), agent.to_string()).unwrap();
}

/// BRO-3624 item 4: verified T3 and Paseo sessions write, each through a signed bridge
/// credential, and the same session with no credential is refused.
#[test]
fn verified_t3_and_paseo_sessions_write() {
    let f = fx();
    let threads = f.root.join("threads.json");
    write_t3_thread(&threads, &f.work);
    let agents = f.root.join("agents");
    write_paseo_agent(&agents, "agent-flow", &f.work);
    let hosts = SurfaceRegistry::new(Some(PaseoRegistry::new(&agents)), Some(T3Registry::new(&threads)), None)
        .with_bridge_secret("flow-bridge-secret");
    let node: NodeId = "node-flow".parse().unwrap();
    let peer = PeerCredentials { pid: std::process::id(), uid: 1000 };
    let cwd = f.work.to_string_lossy().into_owned();
    for (session, claim) in [
        ("t3:env-flow/mcp:flow-thread", CallerClaim::t3(Some("mcp:flow-thread"), Some(cwd.clone()))),
        ("paseo:-/agent-flow", CallerClaim::paseo(Some("agent-flow"), Some(cwd.clone()))),
    ] {
        let verified = hosts.verify_caller(&claim.with_peer(peer)).unwrap();
        assert!(verified.owner_marker().is_some(), "{session}");
        let now = maestro_hosts::current_time_ms();
        let credential = hosts.issue_bridge_credential(&verified, node.clone(), now - 5_000, now + 60_000).unwrap();
        let start = json!({ "session": session, "definition": DECIDE, "params": { "dir": f.work } });
        let unsigned = call_with_hosts(&f.store, &hosts, START, start.clone()).unwrap();
        assert_eq!(code(&unsigned), "caller_refused", "{session}: {unsigned}");

        let mut signed = start;
        signed["cwd"] = json!(cwd);
        signed["node"] = json!(node.as_str());
        signed["credential"] = json!(credential);
        let begun = call_with_hosts(&f.store, &hosts, START, signed.clone()).unwrap();
        assert_eq!(begun["ok"], true, "{session}: {begun}");
        let mut claim_args = signed;
        claim_args
            .as_object_mut()
            .unwrap()
            .retain(|k, _| ["session", "cwd", "node", "credential"].contains(&k.as_str()));
        claim_args["instance"] = begun["instance"].clone();
        claim_args["node_id"] = json!("review");
        let claimed = call_with_hosts(&f.store, &hosts, CLAIM, claim_args).unwrap();
        assert_eq!(claimed["ok"], true, "{session}: {claimed}");
    }
}

/// A waiting human node's question is relayed as data (BRO-3635, ADR decisions 4 and 8): the
/// question and, once filed, its ask; a T3 owner also gets prompt links that open its thread with
/// a draft naming the ask and `maestro_flow_ask`, and carrying no answer. Another surface, an ask
/// not yet linked, or a T3 owner with no environment gets no link.
#[test]
fn a_waiting_human_node_is_relayed_toward_its_t3_thread() {
    use maestro_flow::kernel::{NodeStatus, Reason, State};
    let waiting = |ask_id: Option<&str>, linked: bool| {
        let unmet = vec![Reason::Owner {
            ask: "Ship it?".into(),
            ask_id: ask_id.map(str::to_string),
            ledger: ask_id.map(|_| "arc.yaml".to_string()),
            linked,
        }];
        let s = NodeStatus {
            state: State::Waiting,
            class: "human".into(),
            iteration: BTreeMap::new(),
            unmet,
            inputs: BTreeMap::new(),
        };
        BTreeMap::from([("owner".to_string(), s)])
    };
    let t3: maestro_core::session::SessionRef = "t3:env-1/thread-1".parse().unwrap();
    let relay = maestro_mcp::flow::relay("fi_1", Some(&t3), &waiting(Some("flow-owner-1"), true));
    assert_eq!(relay.len(), 1);
    assert_eq!((&relay[0]["question"], &relay[0]["ask_id"]), (&json!("Ship it?"), &json!("flow-owner-1")));
    let app = relay[0]["t3"]["app"].as_str().unwrap();
    assert!(app.starts_with("t3code://threads/env-1/thread-1?prompt="), "{app}");
    assert!(app.contains("flow-owner-1") && app.contains("maestro_flow_ask"), "{app}");
    assert!(relay[0]["t3"]["web"].as_str().unwrap().starts_with("https://app.t3.codes/env-1/thread-1?prompt="));
    let none = |owner: &str, ask_id: Option<&str>, linked: bool| {
        let owner: maestro_core::session::SessionRef = owner.parse().unwrap();
        let relay = maestro_mcp::flow::relay("fi_1", Some(&owner), &waiting(ask_id, linked));
        assert_eq!(relay[0]["question"], "Ship it?");
        relay[0].get("t3").is_none()
    };
    assert!(none("paseo:-/a1", Some("flow-owner-1"), true), "not a T3 owner");
    assert!(none("t3:-/thread-1", Some("flow-owner-1"), true), "no environment");
    assert!(none("t3:env-1/thread-1", Some("flow-owner-1"), false), "pinned, not filed");
    assert!(none("t3:env-1/thread-1", None, false), "no ask yet");
}
