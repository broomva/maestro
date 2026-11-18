//! Attributed session identity for flow writes (BRO-3846), at the tool boundary: the spec's
//! decision matrix as one table-driven test with a row per cell in both modes, then each listed
//! refusal and acceptance, invalid credentials that never fall back to attribution, owner aliases,
//! the audit facts on events, and the Stop gate's binding.
//!
//! Every registry here is built by the test with its own thread variables
//! (`SurfaceRegistry::with_thread_vars`), so the suite is hermetic even inside a T3 shell.

use maestro_core::session::{PeerCredentials, SessionRef, VerifiedSession};
use maestro_hosts::{
    BridgeCredential, CallerClaim, DirectRegistry, DirectSession, IdentityMode, NodeId, PaseoRegistry, ProcStat,
    ProcessReader, SessionLiveness, Surface, SurfaceRegistry, T3Registry, ThreadVars,
};
use maestro_mcp::flow::{CLAIM, HEARTBEAT, OPEN, START};
use maestro_mcp::stop::{self, Gate, Harness, Outcome};
use maestro_mcp::{Ambient, MaestroServer, call_with_hosts};
use maestro_store::{Paths, Store, TreeSource};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[test]
fn guard_process_entry() {
    maestro_flow::kernel::guard_entry();
}

/// One work node, then a pure one.
const RELAY: &str = r#"
flow: relay
version: 1
params: [dir]
workdir: "{{params.dir}}"
nodes:
  one: {class: work, verify: [sh, -c, 'test -f ok']}
  two: {class: pure, run: ['true'], needs: [one]}
"#;

/// This host's T3 environment id, beside the snapshot.
const ENV: &str = "env-uuid-1";
const CREATED: &str = "2026-11-17T00:00:00Z";
const SECRET: &str = "attribution-bridge-secret";
const A: &str = "mcp:thread-a";
const B: &str = "mcp:thread-b";
const C: &str = "mcp:thread-c";

/// A percent-encoded delegated id, 180 bytes, as T3 mints them.
fn delegated() -> String {
    let mut id = String::from("mcp:launch:");
    while id.len() < 177 {
        id.push_str("ab%3A");
    }
    id.truncate(177);
    id.push_str("end");
    assert_eq!(id.len(), 180);
    id
}

/// An id of exactly `len` bytes.
fn id_of(len: usize) -> String {
    let mut id = String::from("mcp:");
    id.push_str(&"x".repeat(len - 4));
    id
}

fn thread(id: &str, native: &str) -> Value {
    json!({
        "id": id, "cwd": "/nowhere", "environment_id": ENV, "provider": "claudeAgent",
        "created_at": CREATED, "run_id": "run-1",
        "active_provider_thread_id": format!("pt:{id}"),
        "provider_threads": [{ "provider_thread_id": format!("pt:{id}"), "native_id": native }],
    })
}

/// A scratch machine: a workspace with `.broomva/` and an asks ledger whose writer of record is the
/// stub, a work folder, a T3 snapshot (and a broken one), a Paseo agent and a Direct session.
struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    paths: Paths,
    store: Store,
    work: PathBuf,
    t3: PathBuf,
    broken: PathBuf,
    agents: PathBuf,
}

fn fx() -> Fx {
    maestro_flow::kernel::guard_entry();
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let ws = root.join("workspace");
    std::fs::create_dir_all(ws.join(".broomva/asks")).unwrap();
    std::fs::create_dir_all(ws.join("scripts")).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/requests");
    std::fs::copy(fixtures.join("ledger.yaml"), ws.join(".broomva/asks/ledger.yaml")).unwrap();
    std::fs::copy(fixtures.join("ask_ledger_stub.py"), ws.join("scripts/ask_ledger.py")).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let w = work.to_string_lossy().into_owned();

    let mut threads = vec![thread(A, "native-a"), thread(B, "native-b")];
    // C, a Codex thread that switched provider: its retired row's native id never attributes.
    let mut c = thread(C, "native-c");
    c["provider"] = json!("codex");
    c["provider_threads"] = json!([
        { "provider_thread_id": "pt:c-old", "native_id": "native-c-old" },
        { "provider_thread_id": format!("pt:{C}"), "native_id": "native-c" },
    ]);
    threads.push(c);
    let mut archived = thread("mcp:thread-archived", "native-archived");
    archived["archived_at"] = json!("2026-11-17T01:00:00Z");
    let mut deleted = thread("mcp:thread-deleted", "native-deleted");
    deleted["deleted_at"] = json!("2026-11-17T01:00:00Z");
    threads.extend([archived, deleted]);
    // Two threads whose active rows carry one native id.
    threads.extend([thread("mcp:dup-1", "native-dup"), thread("mcp:dup-2", "native-dup")]);
    for id in [delegated(), id_of(512), "mcp:a%2Fb".to_string()] {
        threads.push(thread(&id, &format!("native-{}", id.len())));
    }
    // A thread with no created_at: no owner marker.
    let mut unmarked = thread("mcp:thread-unmarked", "native-unmarked");
    unmarked.as_object_mut().unwrap().remove("created_at");
    threads.push(unmarked);
    // A thread with no incarnation marker at all.
    let mut bare = thread("mcp:thread-bare", "native-bare");
    bare.as_object_mut().unwrap().remove("created_at");
    bare.as_object_mut().unwrap().remove("run_id");
    threads.push(bare);
    // Credentials are bound to the work folder: verified T3 threads run there.
    for t in &mut threads {
        t["cwd"] = json!(w);
    }
    let t3_dir = root.join("t3");
    std::fs::create_dir_all(&t3_dir).unwrap();
    let t3 = t3_dir.join("threads.json");
    std::fs::write(&t3, Value::Array(threads).to_string()).unwrap();
    std::fs::write(t3_dir.join("environment-id"), format!("{ENV}\n")).unwrap();
    let broken = root.join("broken.json");
    std::fs::write(&broken, "[{ not json").unwrap();

    let agents = root.join("agents");
    std::fs::create_dir_all(agents.join("work")).unwrap();
    let agent = json!({
        "id": "agent-p", "cwd": w, "provider": "claude", "createdAt": CREATED,
        "lastStatus": "running", "pid": 4101, "startedAt": "2026-11-17T00:00:01Z",
        "persistence": { "sessionId": "handle-1" }
    });
    std::fs::write(agents.join("work/agent-p.json"), agent.to_string()).unwrap();

    let mut paths = Paths::under(&root);
    paths.ask_locks = root.join("ask-locks");
    let store = Store::new(paths.clone(), TreeSource::WorkingTree);
    Fx { _tmp: tmp, root, paths, store, work, t3, broken, agents }
}

/// Facts about the claiming process, read from a fixed table: the uid does not read.
#[derive(Debug)]
struct FakeProcs;

impl ProcessReader for FakeProcs {
    fn read(&self, pid: u32) -> Result<ProcStat, String> {
        let stat = |ppid: u32, command: &str| ProcStat {
            ppid,
            command: command.into(),
            start_time: Ok(CREATED.into()),
            uid: Err("uid hidden in this test".into()),
        };
        match pid {
            p if p == std::process::id() => Ok(stat(4242, "maestro")),
            4242 => Ok(stat(1, "claude")),
            1 => Ok(stat(0, "launchd")),
            other => Err(format!("no process {other}")),
        }
    }
}

/// Which T3 registry a row's host reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reg {
    Good,
    Broken,
}

impl Fx {
    fn hosts(&self, mode: IdentityMode, vars: &[(&str, &str)], reg: Reg, secret: bool) -> SurfaceRegistry {
        let direct = DirectRegistry::new();
        direct.register(DirectSession {
            id: "d1".into(),
            cwd: self.work.to_string_lossy().into_owned(),
            token: "tok-1".into(),
            liveness: SessionLiveness::Live,
            harness: None,
            pending_permissions: vec![],
        });
        let t3 = T3Registry::new(match reg {
            Reg::Good => &self.t3,
            Reg::Broken => &self.broken,
        });
        let hosts = SurfaceRegistry::new(Some(PaseoRegistry::new(&self.agents)), Some(t3), Some(direct))
            .with_identity_mode(mode)
            .with_thread_vars(ThreadVars::from_pairs(vars.iter().map(|(k, v)| (*k, v.to_string()))))
            .with_process_reader(Arc::new(FakeProcs));
        if secret { hosts.with_bridge_secret(SECRET) } else { hosts }
    }

    /// A fresh signed credential for `claim`'s session, issued by `hosts`.
    fn credential(&self, hosts: &SurfaceRegistry, claim: CallerClaim, from: i64, to: i64) -> BridgeCredential {
        let peer = PeerCredentials { pid: std::process::id(), uid: 1000 };
        let verified = hosts.verify_caller(&claim.with_peer(peer)).unwrap();
        hosts.issue_bridge_credential(&verified, node(), from, to).unwrap()
    }

    fn t3_credential(&self, hosts: &SurfaceRegistry, thread: &str) -> BridgeCredential {
        let now = maestro_hosts::current_time_ms();
        let claim = CallerClaim::t3(Some(thread), Some(self.work.to_string_lossy().into_owned()));
        self.credential(hosts, claim, now - 5_000, now + 60_000)
    }

    fn paseo_credential(&self, hosts: &SurfaceRegistry) -> BridgeCredential {
        let now = maestro_hosts::current_time_ms();
        let claim = CallerClaim::paseo(Some("agent-p"), Some(self.work.to_string_lossy().into_owned()));
        self.credential(hosts, claim, now - 5_000, now + 60_000)
    }

    fn start_args(&self, session: Option<&str>) -> Value {
        let mut args = json!({ "definition": RELAY, "params": { "dir": self.work } });
        if let Some(s) = session {
            args["session"] = json!(s);
        }
        args
    }

    fn signed(&self, mut args: Value, cred: &BridgeCredential) -> Value {
        args["cwd"] = json!(self.work);
        args["node"] = json!(cred.node.as_str());
        args["credential"] = json!(cred);
        args
    }

    fn direct(&self, mut args: Value) -> Value {
        args["token"] = json!("tok-1");
        args["cwd"] = json!(self.work);
        args
    }

    fn call(&self, hosts: &SurfaceRegistry, tool: &str, args: Value) -> Value {
        call_with_hosts(&self.store, hosts, tool, args).unwrap_or_else(|e| panic!("{tool}: {}", e.message))
    }

    fn events(&self, fi: &str) -> Vec<Value> {
        let log = self.root.join("workspace/.broomva/flows/instances").join(fi).join("events.jsonl");
        std::fs::read_to_string(log).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn ledger(&self) -> String {
        std::fs::read_to_string(self.root.join("workspace/.broomva/asks/ledger.yaml")).unwrap()
    }

    /// Starts an instance owned by `thread`, attributed from its own variable.
    fn begin_as(&self, var: (&str, &str)) -> String {
        let hosts = self.hosts(IdentityMode::Attributed, &[var], Reg::Good, false);
        let v = self.call(&hosts, START, self.start_args(None));
        assert_eq!(v["ok"], true, "{v}");
        v["instance"].as_str().unwrap().to_string()
    }
}

fn node() -> NodeId {
    "node-1".parse().unwrap()
}

/// What a row expects: accepted, or refused with this code and this identity reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    Ok,
    Refused(&'static str, &'static str),
}

fn outcome(v: &Value) -> (bool, String, String) {
    (
        v["ok"] == json!(true),
        v["code"].as_str().unwrap_or("").to_string(),
        v["identity"].as_str().unwrap_or("").to_string(),
    )
}

fn check(row: &str, v: &Value, want: Want) {
    match want {
        Want::Ok => assert_eq!(v["ok"], true, "{row}: {v}"),
        Want::Refused(code, identity) => {
            let (ok, got, why) = outcome(v);
            assert!(!ok, "{row}: accepted, expected {code}: {v}");
            assert_eq!((got.as_str(), why.as_str()), (code, identity), "{row}: {v}");
            assert_eq!(v["recorded"], false, "{row}: a refused caller writes nothing");
        }
    }
}

/// The presented claim of a row.
#[derive(Debug, Clone, Copy)]
enum Cred {
    None,
    T3(&'static str),
    Paseo,
    DirectToken,
    BadSignature,
}

/// The transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    Stdio,
    Http,
}

struct Row {
    name: &'static str,
    mode: IdentityMode,
    via: Via,
    cred: Cred,
    session: Option<&'static str>,
    vars: &'static [(&'static str, &'static str)],
    reg: Reg,
    want: Want,
}

const T3V: &str = "T3CODE_THREAD_ID";
const CXV: &str = "CODEX_THREAD_ID";
const STRICT: IdentityMode = IdentityMode::Strict;
const ATTRIB: IdentityMode = IdentityMode::Attributed;
const SA: Option<&str> = Some("t3:env-uuid-1/mcp:thread-a");
const MISSING: Want = Want::Refused("attribution_missing", "attribution_missing");
const UNRESOLVED: Want = Want::Refused("attribution_unresolved", "attribution_unresolved");
const MISMATCH: Want = Want::Refused("thread_mismatch", "thread_mismatch");
const UNAVAILABLE: Want = Want::Refused("registry_unavailable", "registry_unavailable");
const INVALID: Want = Want::Refused("caller_refused", "invalid_token");

/// The spec's decision matrix, one row per cell, each in the mode its cell names; a cell that
/// spans modes has a row in each.
fn matrix() -> Vec<Row> {
    use Cred::*;
    use Reg::*;
    use Via::*;
    let r = |name, mode, via, cred, session, vars, reg, want| Row { name, mode, via, cred, session, vars, reg, want };
    vec![
        // strict | any | valid credential or Direct token | any (ignored) | today's result
        r("strict, T3 credential, another thread's variable", STRICT, Stdio, T3(A), SA, &[(T3V, B)], Good, Want::Ok),
        r(
            "strict, Direct token, a T3 variable",
            STRICT,
            Stdio,
            DirectToken,
            Some("direct:-/d1"),
            &[(T3V, A)],
            Good,
            Want::Ok,
        ),
        r("strict, T3 credential over HTTP", STRICT, Http, T3(A), SA, &[(T3V, B)], Good, Want::Ok),
        // strict | any | none | any (ignored) | today's result: refused
        r("strict, unsigned T3, its own variable", STRICT, Stdio, None, SA, &[(T3V, A)], Good, INVALID),
        r("strict, unsigned T3, no variable", STRICT, Stdio, None, SA, &[], Good, INVALID),
        r(
            "strict, unsigned, no session",
            STRICT,
            Stdio,
            None,
            Option::None,
            &[(T3V, A)],
            Good,
            Want::Refused("caller_refused", "missing_session_id"),
        ),
        // attributed | stdio | valid credential | absent | today's result
        r("attributed, T3 credential, no variable", ATTRIB, Stdio, T3(A), SA, &[], Good, Want::Ok),
        r(
            "attributed, Direct token, no variable",
            ATTRIB,
            Stdio,
            DirectToken,
            Some("direct:-/d1"),
            &[],
            Good,
            Want::Ok,
        ),
        r(
            "attributed, Paseo credential, no variable",
            ATTRIB,
            Stdio,
            Paseo,
            Some("paseo:-/agent-p"),
            &[],
            Good,
            Want::Ok,
        ),
        // attributed | stdio | valid credential | the claimed thread | today's result
        r("attributed, T3 credential, its own variable", ATTRIB, Stdio, T3(A), SA, &[(T3V, A)], Good, Want::Ok),
        r(
            "attributed, T3 credential, its own variable, no session",
            ATTRIB,
            Stdio,
            T3(A),
            Option::None,
            &[(T3V, A)],
            Good,
            Want::Ok,
        ),
        // attributed | stdio | valid credential | another thread, or unresolved | refused
        r(
            "attributed, T3 credential, another thread's variable",
            ATTRIB,
            Stdio,
            T3(A),
            SA,
            &[(T3V, B)],
            Good,
            MISMATCH,
        ),
        r(
            "attributed, T3 credential, an unknown variable",
            ATTRIB,
            Stdio,
            T3(A),
            SA,
            &[(T3V, "mcp:nobody")],
            Good,
            UNRESOLVED,
        ),
        // attributed | stdio | none, T3 | the claimed thread, or no session | accepted, attributed
        r("attributed, unsigned T3, its own variable", ATTRIB, Stdio, None, SA, &[(T3V, A)], Good, Want::Ok),
        r(
            "attributed, unsigned, no session, a variable",
            ATTRIB,
            Stdio,
            None,
            Option::None,
            &[(T3V, A)],
            Good,
            Want::Ok,
        ),
        // attributed | stdio | none, T3 | absent | attribution_missing
        r("attributed, unsigned T3, no variable", ATTRIB, Stdio, None, SA, &[], Good, MISSING),
        r("attributed, unsigned, no session, no variable", ATTRIB, Stdio, None, Option::None, &[], Good, MISSING),
        // attributed | stdio | none, T3 | another thread | thread_mismatch
        r("attributed, unsigned T3, another thread's variable", ATTRIB, Stdio, None, SA, &[(T3V, B)], Good, MISMATCH),
        // attributed | stdio | none, Paseo or Direct | any | today's result: refused
        r(
            "attributed, unsigned Paseo, a T3 variable",
            ATTRIB,
            Stdio,
            None,
            Some("paseo:-/agent-p"),
            &[(T3V, A)],
            Good,
            INVALID,
        ),
        r(
            "attributed, unsigned Direct, a T3 variable",
            ATTRIB,
            Stdio,
            None,
            Some("direct:-/d1"),
            &[(T3V, A)],
            Good,
            INVALID,
        ),
        // attributed | HTTP | valid credential | n/a | today's result
        r("attributed, T3 credential over HTTP", ATTRIB, Http, T3(A), SA, &[(T3V, B)], Good, Want::Ok),
        // attributed | HTTP | none | n/a | attribution_missing
        r("attributed, unsigned T3 over HTTP", ATTRIB, Http, None, SA, &[(T3V, A)], Good, MISSING),
        r("attributed, unsigned, no session, over HTTP", ATTRIB, Http, None, Option::None, &[(T3V, A)], Good, MISSING),
        // attributed | stdio | presented but invalid | any | the verifier's refusal, never attribution
        r(
            "attributed, a forged credential, its own variable",
            ATTRIB,
            Stdio,
            BadSignature,
            SA,
            &[(T3V, A)],
            Good,
            INVALID,
        ),
        // attributed | stdio | none, T3 | unresolved, or two with one unresolved | attribution_unresolved
        r(
            "attributed, unsigned T3, an unknown variable",
            ATTRIB,
            Stdio,
            None,
            SA,
            &[(T3V, "mcp:nobody")],
            Good,
            UNRESOLVED,
        ),
        r(
            "attributed, unsigned T3, one variable unknown",
            ATTRIB,
            Stdio,
            None,
            SA,
            &[(T3V, A), (CXV, "native-nobody")],
            Good,
            UNRESOLVED,
        ),
        // attributed | stdio | valid Paseo or Direct credential | a T3 variable, resolved or not | thread_mismatch
        r(
            "attributed, Paseo credential, a resolved T3 variable",
            ATTRIB,
            Stdio,
            Paseo,
            Some("paseo:-/agent-p"),
            &[(T3V, A)],
            Good,
            MISMATCH,
        ),
        r(
            "attributed, Direct token, an unresolved T3 variable",
            ATTRIB,
            Stdio,
            DirectToken,
            Some("direct:-/d1"),
            &[(T3V, "mcp:nobody")],
            Good,
            MISMATCH,
        ),
        // strict or attributed | HTTP, or strict | any | any | no registry read for attribution
        r(
            "strict, Direct token, a variable, registry broken",
            STRICT,
            Stdio,
            DirectToken,
            Some("direct:-/d1"),
            &[(T3V, A)],
            Broken,
            Want::Ok,
        ),
        r(
            "attributed, Direct token over HTTP, registry broken",
            ATTRIB,
            Http,
            DirectToken,
            Some("direct:-/d1"),
            &[(T3V, A)],
            Broken,
            Want::Ok,
        ),
        // The precedence: registry_unavailable first.
        r("attributed, unsigned T3, registry broken", ATTRIB, Stdio, None, SA, &[(T3V, A)], Broken, UNAVAILABLE),
    ]
}

fn run_row(f: &Fx, row: &Row) -> Value {
    let hosts = f.hosts(row.mode, row.vars, row.reg, true);
    let args = f.start_args(row.session);
    let args = match row.cred {
        Cred::None => args,
        Cred::T3(t) => {
            let cred = f.t3_credential(&hosts, t);
            f.signed(args, &cred)
        }
        Cred::Paseo => {
            let cred = f.paseo_credential(&hosts);
            f.signed(args, &cred)
        }
        Cred::DirectToken => f.direct(args),
        Cred::BadSignature => {
            let mut cred = f.t3_credential(&hosts, A);
            cred.signature = "00".repeat(32);
            f.signed(args, &cred)
        }
    };
    match row.via {
        Via::Stdio => f.call(&hosts, START, args),
        Via::Http => {
            let server = MaestroServer::new(f.store.clone()).with_hosts(hosts).shared();
            server.call(START, args).unwrap_or_else(|e| panic!("{}: {}", row.name, e.message))
        }
    }
}

#[test]
fn the_decision_matrix_one_row_per_cell() {
    let f = fx();
    let rows = matrix();
    assert!(rows.iter().any(|r| r.mode == STRICT) && rows.iter().any(|r| r.mode == ATTRIB));
    for row in &rows {
        let v = run_row(&f, row);
        check(row.name, &v, row.want);
        if row.want == Want::Ok {
            let fi = v["instance"].as_str().unwrap();
            let started = &f.events(fi)[0]["body"];
            let mode = &started["audit"]["identity"]["mode"];
            let want = match row.cred {
                Cred::None => "t3-attributed",
                Cred::DirectToken => "direct-token",
                _ => "bridge-credential",
            };
            assert_eq!(mode, want, "{}: {started}", row.name);
        }
    }
}

/// Acceptance, attributed mode, refused: 0 of 15 accepted.
#[test]
fn attributed_mode_refuses_each_listed_case() {
    let f = fx();
    let at = |vars: &[(&str, &str)], session: Option<&str>, reg: Reg| {
        let hosts = f.hosts(ATTRIB, vars, reg, true);
        f.call(&hosts, START, f.start_args(session))
    };
    let tid = |id: String| format!("t3:-/{id}");
    // 1. No variable.
    check("1", &at(&[], SA, Reg::Good), MISSING);
    // 2. An unknown T3CODE_THREAD_ID.
    check("2", &at(&[(T3V, "mcp:nobody")], SA, Reg::Good), UNRESOLVED);
    // 3. An archived thread.
    check("3", &at(&[(T3V, "mcp:thread-archived")], None, Reg::Good), UNRESOLVED);
    // 4. The variable's thread is not the claimed session.
    check("4", &at(&[(T3V, B)], SA, Reg::Good), MISMATCH);
    // 5. T3CODE_THREAD_ID and CODEX_THREAD_ID name different threads.
    check("5", &at(&[(T3V, A), (CXV, "native-b")], None, Reg::Good), MISMATCH);
    // 6. A CODEX_THREAD_ID that matches only a retired provider row.
    check("6", &at(&[(CXV, "native-c-old")], None, Reg::Good), UNRESOLVED);
    // 7 and 8. A mismatch beside a valid signed credential, and beside a Direct token.
    let hosts = f.hosts(ATTRIB, &[(T3V, B)], Reg::Good, true);
    let cred = f.t3_credential(&hosts, A);
    check("7", &f.call(&hosts, START, f.signed(f.start_args(SA), &cred)), MISMATCH);
    check("8", &f.call(&hosts, START, f.direct(f.start_args(Some("direct:-/d1")))), MISMATCH);
    // 9. The registry does not read.
    check("9", &at(&[(T3V, A)], None, Reg::Broken), UNAVAILABLE);
    // 10. A shared daemon's caller with no credential.
    let server = MaestroServer::new(f.store.clone()).with_hosts(f.hosts(ATTRIB, &[(T3V, A)], Reg::Good, true)).shared();
    check("10", &server.call(START, f.start_args(SA)).unwrap(), MISSING);
    // 11. A deleted thread.
    check("11", &at(&[(T3V, "mcp:thread-deleted")], None, Reg::Good), UNRESOLVED);
    // 12. Two threads' active rows carry the native id.
    check("12", &at(&[(CXV, "native-dup")], None, Reg::Good), UNRESOLVED);
    // 13. A session of another T3 environment.
    check("13", &at(&[(T3V, A)], Some("t3:env-other/mcp:thread-a"), Reg::Good), MISMATCH);
    // 14. Ids at 513 bytes, or with `/`, `..` or NUL.
    for bad in [id_of(513), "mcp:a/b".into(), "mcp:a..b".into(), "mcp:a\0b".into()] {
        check(&format!("14 {:?}", &bad[..bad.len().min(12)]), &at(&[(T3V, bad.as_str())], None, Reg::Good), UNRESOLVED);
        // Named as the session with the thread's own variable, it is the thread, not the name.
        check("14 session", &at(&[(T3V, A)], Some(&tid(bad.clone())), Reg::Good), MISMATCH);
    }
    // 15. An unresolved variable while the registry read fails: registry_unavailable wins.
    check("15", &at(&[(T3V, &id_of(513)), (CXV, "native-a")], None, Reg::Broken), UNAVAILABLE);
    // None of them wrote anything.
    assert!(!f.root.join("workspace/.broomva/flows").exists(), "a refusal made the ledger");
}

/// Acceptance, attributed mode, accepted: 8 of 8.
#[test]
fn attributed_mode_accepts_each_listed_case() {
    let f = fx();
    let at = |vars: &[(&str, &str)], session: Option<&str>| {
        let hosts = f.hosts(ATTRIB, vars, Reg::Good, true);
        let v = f.call(&hosts, START, f.start_args(session));
        assert_eq!(v["ok"], true, "{vars:?} {session:?}: {v}");
        let fi = v["instance"].as_str().unwrap().to_string();
        let owner = f.events(&fi)[0]["body"]["owner"]["session"].clone();
        let owner = serde_json::from_value::<SessionRef>(owner).unwrap().to_string();
        (fi, owner)
    };
    // 1. T3CODE_THREAD_ID with no session: the session is derived, canonical.
    let (_, owner) = at(&[(T3V, A)], None);
    assert_eq!(owner, format!("t3:{ENV}/{A}"));
    // 2. The same with t3:local/<id>, canonicalized.
    assert_eq!(at(&[(T3V, A)], Some("t3:local/mcp:thread-a")).1, owner);
    // 3. CODEX_THREAD_ID through the active provider row.
    assert_eq!(at(&[(CXV, "native-c")], None).1, format!("t3:{ENV}/{C}"));
    // 4. A percent-encoded 180-byte delegated id.
    let long = delegated();
    assert_eq!(at(&[(T3V, &long)], None).1, format!("t3:{ENV}/{long}"));
    // 6. t3:-/<id> and t3:<registry-uuid>/<id> are one owner: an instance started under one
    // takes the other's writes.
    let (fi, _) = at(&[(T3V, A)], Some("t3:-/mcp:thread-a"));
    let hosts = f.hosts(ATTRIB, &[(T3V, A)], Reg::Good, true);
    let claim = json!({ "session": format!("t3:{ENV}/{A}"), "instance": fi, "node_id": "one" });
    let claimed = f.call(&hosts, CLAIM, claim);
    assert_eq!(claimed["ok"], true, "{claimed}");
    // 7. A 512-byte id and a `%2F` id, kept opaque (never decoded into a path).
    for id in [id_of(512), "mcp:a%2Fb".to_string()] {
        assert_eq!(at(&[(T3V, &id)], None).1, format!("t3:{ENV}/{id}"));
    }
    assert!(!f.root.join("workspace/.broomva/flows/instances/b").exists());
}

/// Acceptance 5: a registry found through `T3CODE_HOME`, `MAESTRO_T3_HOME` unset, attributes; and
/// in strict mode (approved fix) verifies a credential.
#[test]
fn a_registry_found_through_t3code_home_verifies_in_both_modes() {
    let f = fx();
    let home = f.root.join("t3home");
    std::fs::create_dir_all(home.join("userdata")).unwrap();
    std::fs::copy(&f.t3, home.join("threads.json")).unwrap();
    let (root, found) = SurfaceRegistry::t3_root_from(None, Some(home.to_str().unwrap()), None).unwrap();
    assert_eq!((root.as_path(), found), (home.as_path(), true));
    let (explicit, found) =
        SurfaceRegistry::t3_root_from(Some("/configured"), Some(home.to_str().unwrap()), None).unwrap();
    assert_eq!((explicit.as_path(), found), (Path::new("/configured"), false));
    for mode in [STRICT, ATTRIB] {
        let hosts = SurfaceRegistry::new(None, Some(T3Registry::new(&root)), None)
            .with_t3_found(found)
            .with_bridge_secret(SECRET)
            .with_identity_mode(mode)
            .with_thread_vars(ThreadVars::from_pairs([(T3V, A.to_string())]));
        let cred = f.t3_credential(&hosts, A);
        let v = f.call(&hosts, START, f.signed(f.start_args(SA), &cred));
        assert_eq!(v["ok"], true, "{mode:?}: {v}");
        if mode == ATTRIB {
            let v = f.call(&hosts, START, f.start_args(None));
            assert_eq!(v["ok"], true, "{v}");
        }
    }
}

/// Strict mode, approved fix 1: a percent-encoded long id verifies with its credential; and the
/// thread variable is ignored.
#[test]
fn strict_mode_takes_a_percent_encoded_long_id_and_ignores_the_variable() {
    let f = fx();
    let long = delegated();
    let hosts = f.hosts(STRICT, &[(T3V, B)], Reg::Good, true);
    let cred = f.t3_credential(&hosts, &long);
    let v = f.call(&hosts, START, f.signed(f.start_args(Some(&format!("t3:{ENV}/{long}"))), &cred));
    assert_eq!(v["ok"], true, "{v}");
    let started = &f.events(v["instance"].as_str().unwrap())[0]["body"];
    assert_eq!(started["audit"]["identity"]["variable"], Value::Null, "strict reads no variable: {started}");
}

/// A presented but invalid credential with a matching variable is the verifier's refusal, never
/// attribution: bad signature, expired, stale generation, wrong node, with and without a bridge
/// secret.
#[test]
fn an_invalid_credential_is_never_attributed() {
    let f = fx();
    let vars = [(T3V, A)];
    let now = maestro_hosts::current_time_ms();
    let w = f.work.to_string_lossy().into_owned();
    type Args<'a> = Box<dyn Fn(&SurfaceRegistry) -> Value + 'a>;
    let cases: Vec<(&str, Args, &str)> = vec![
        (
            "bad signature",
            Box::new(|h| {
                let mut c = f.t3_credential(h, A);
                c.signature = "00".repeat(32);
                f.signed(f.start_args(SA), &c)
            }),
            "invalid_token",
        ),
        (
            "expired",
            Box::new(|h| {
                let c = f.credential(h, CallerClaim::t3(Some(A), Some(w.clone())), now - 120_000, now - 60_000);
                f.signed(f.start_args(SA), &c)
            }),
            "stale_credential",
        ),
        (
            "stale generation",
            Box::new(|h| {
                let c = f.t3_credential(h, A);
                h.bump_generation(&Surface::T3, A);
                f.signed(f.start_args(SA), &c)
            }),
            "id_reused",
        ),
        (
            "wrong node",
            Box::new(|h| {
                let c = f.t3_credential(h, A);
                let mut args = f.signed(f.start_args(SA), &c);
                args["node"] = json!("node-2");
                args
            }),
            "wrong_node",
        ),
    ];
    for (name, args, reason) in &cases {
        let hosts = f.hosts(ATTRIB, &vars, Reg::Good, true);
        let v = f.call(&hosts, START, args(&hosts));
        check(name, &v, Want::Refused("caller_refused", reason));
        // Without the credential the same caller is attributed: the refusal above is the credential's.
        assert_eq!(f.call(&hosts, START, f.start_args(SA))["ok"], true, "{name}");
    }
    // No bridge secret here: a presented credential cannot verify, and is not attributed either.
    let signer = f.hosts(ATTRIB, &vars, Reg::Good, true);
    let cred = f.t3_credential(&signer, A);
    let hosts = f.hosts(ATTRIB, &vars, Reg::Good, false);
    check("no secret", &f.call(&hosts, START, f.signed(f.start_args(SA), &cred)), INVALID);
    // A forged credential, no secret, no session named.
    check("no secret, no session", &f.call(&hosts, START, f.signed(f.start_args(None), &cred)), INVALID);
}

/// The bridge-secret guard lets through only an attributed T3 claim; node, generation,
/// incarnation and cross-surface checks still apply to it (host level: `attribution` is set here
/// directly, as only the caller check sets it in production).
#[test]
fn an_attributed_claim_still_meets_every_host_check() {
    let f = fx();
    let hosts = f.hosts(ATTRIB, &[], Reg::Good, true);
    let attributed = |thread: &str| {
        let mut c = CallerClaim::t3(Some(thread), None::<String>);
        c.attribution = Some(maestro_hosts::Attribution { variable: T3V.into(), thread: thread.into() });
        c
    };
    // A configured bridge secret: accepted, with the T3 owner marker.
    let v = hosts.verify_caller(&attributed(A)).unwrap();
    assert_eq!(v.owner_marker(), Some(format!("t3-owner:{A}:{CREATED}").as_str()));
    // The same claim without its attribution meets the guard.
    let plain = CallerClaim::t3(Some(A), None::<String>);
    assert_eq!(hosts.verify_caller(&plain).unwrap_err().reason, maestro_hosts::RefusalReason::InvalidToken);
    // Wrong node.
    let pinned = f.hosts(ATTRIB, &[], Reg::Good, true).with_expected_node(node());
    let wrong = attributed(A).with_node("node-2".parse().unwrap());
    assert_eq!(pinned.verify_caller(&wrong).unwrap_err().reason, maestro_hosts::RefusalReason::WrongNode);
    // A stale generation.
    let stale = attributed(A).with_generation(7);
    assert_eq!(hosts.verify_caller(&stale).unwrap_err().reason, maestro_hosts::RefusalReason::IdReused);
    // No incarnation marker.
    let bare = attributed("mcp:thread-bare");
    assert_eq!(hosts.verify_caller(&bare).unwrap_err().reason, maestro_hosts::RefusalReason::InvalidToken);
    // Cross-surface: a Paseo session's id claimed as a T3 thread.
    let cross = attributed("agent-p");
    assert_eq!(hosts.verify_caller(&cross).unwrap_err().reason, maestro_hosts::RefusalReason::CrossSurface);
    // An attribution whose thread is not the claimed session.
    let mut other = attributed(A);
    other.session_id = Some(B.into());
    assert_eq!(hosts.verify_caller(&other).unwrap_err().reason, maestro_hosts::RefusalReason::ThreadMismatch);
    // The guard's exception is for a T3 claim only: an attribution on a Paseo claim meets it.
    let mut paseo = CallerClaim::paseo(Some("agent-p"), Some(f.work.to_string_lossy().into_owned()));
    paseo.attribution = Some(maestro_hosts::Attribution { variable: T3V.into(), thread: A.into() });
    assert_eq!(hosts.verify_caller(&paseo).unwrap_err().reason, maestro_hosts::RefusalReason::InvalidToken);
    // Never read from arguments: serde skips it both ways.
    let parsed: CallerClaim = serde_json::from_value(json!({
        "surface": "t3", "session_id": A, "attribution": { "variable": T3V, "thread": A }
    }))
    .unwrap();
    assert_eq!(parsed.attribution, None);
    assert!(serde_json::to_value(attributed(A)).unwrap().get("attribution").is_none());
}

/// A thread with no created_at verifies, but owns nothing: writes are `no_owner_marker`, as today.
#[test]
fn a_thread_without_created_at_writes_nothing() {
    let f = fx();
    let hosts = f.hosts(ATTRIB, &[(T3V, "mcp:thread-unmarked")], Reg::Good, true);
    let v = f.call(&hosts, START, f.start_args(None));
    assert_eq!(v["code"], "no_owner_marker", "{v}");
}

/// The claimless agent request stays unverified once the T3 root is found rather than configured;
/// a configured root keeps refusing it, as before.
#[test]
fn a_claimless_agent_request_stays_unverified() {
    let f = fx();
    let args = json!({ "session": format!("t3:-/{A}"), "summary": "done" });
    let kind = maestro_core::request::RequestKind::Handback;
    for mode in [STRICT, ATTRIB] {
        let found = f.hosts(mode, &[(T3V, A)], Reg::Good, true).with_t3_found(true);
        let v = maestro_mcp::agent_request_with_hosts(&f.store, &found, kind, args.clone()).unwrap();
        assert_eq!(v["request"]["verified"], false, "{mode:?}: {v}");
        let configured = f.hosts(mode, &[(T3V, A)], Reg::Good, true);
        let e = maestro_mcp::agent_request_with_hosts(&f.store, &configured, kind, args.clone()).unwrap_err();
        assert!(e.message.contains("authenticated bridge credential or launch token is required"), "{}", e.message);
    }
}

/// Every event type a write appends carries `identity` and the audit facts, and an unreadable fact
/// is null with its reason.
#[test]
fn events_carry_identity_and_audit_facts() {
    let f = fx();
    let hosts = f.hosts(ATTRIB, &[(T3V, A)], Reg::Good, true);
    let v = f.call(&hosts, START, f.start_args(None));
    let fi = v["instance"].as_str().unwrap().to_string();
    let c = f.call(&hosts, CLAIM, json!({ "instance": fi, "node_id": "one" }));
    let token = c["claim_token"].as_str().unwrap().to_string();
    assert_eq!(f.call(&hosts, HEARTBEAT, json!({ "claim_token": token }))["ok"], true);
    // No `ok` file yet: a refused completion, then a receipt.
    let refused = f.call(&hosts, maestro_mcp::flow::COMPLETE, json!({ "claim_token": token }));
    assert_eq!(refused["recorded"], true, "{refused}");
    std::fs::write(f.work.join("ok"), "").unwrap();
    let done = f.call(&hosts, maestro_mcp::flow::COMPLETE, json!({ "claim_token": token }));
    assert_eq!(done["ok"], true, "{done}");
    let events = f.events(&fi);
    let kinds: Vec<&str> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    for kind in ["Started", "Claimed", "Heartbeat", "Refused", "Completed"] {
        assert!(kinds.contains(&kind), "{kinds:?}");
    }
    for e in events
        .iter()
        .filter(|e| ["Started", "Claimed", "Heartbeat", "Refused", "Completed"].contains(&e["kind"].as_str().unwrap()))
    {
        let audit = &e["body"]["audit"];
        assert_eq!(audit["identity"], json!({ "mode": "t3-attributed", "variable": T3V, "thread": A }), "{e}");
        let p = &audit["process"];
        assert_eq!(p["pid"], json!(std::process::id()));
        assert_eq!(p["start_time"], json!(CREATED));
        assert_eq!(p["uid"], Value::Null, "an unread fact is null: {p}");
        assert_eq!(p["unread"]["uid"], "uid hidden in this test");
        assert_eq!(p["ancestors"], json!([{ "pid": 4242, "command": "claude" }, { "pid": 1, "command": "launchd" }]));
        assert_eq!(p["variables"], json!([T3V]));
    }
    // A choice's edge, `Chosen`, carries them too.
    let pick = "flow: pick\nversion: 1\nnodes:\n  pick: {class: choice, edges: {go: {guard: 'true'}}}\n  go: {class: pure, run: ['true'], switch: {on: pick, case: go}}\n";
    let v = f.call(&hosts, START, json!({ "definition": pick }));
    let pi = v["instance"].as_str().unwrap().to_string();
    let c = f.call(&hosts, CLAIM, json!({ "instance": pi, "node_id": "pick" }));
    let chosen = f.call(&hosts, maestro_mcp::flow::CHOOSE, json!({ "claim_token": c["claim_token"], "edge": "go" }));
    assert_eq!(chosen["ok"], true, "{chosen}");
    let last = f.events(&pi).into_iter().find(|e| e["kind"] == "Chosen").unwrap();
    assert_eq!(last["body"]["audit"]["identity"]["mode"], "t3-attributed", "{last}");
    // The spine ignores them: equal to the spine of a run with no audit facts.
    let spine = f.call(&hosts, maestro_mcp::flow::SPINE, json!({ "instance": fi }));
    assert!(!spine.to_string().contains("audit"), "{spine}");
}

/// Acceptance 8: a pre-existing instance owned as `t3:local/<id>` is written as `t3:<uuid>/<id>`
/// and gated by the Stop hook.
#[test]
fn an_instance_owned_under_an_alias_is_written_and_gated() {
    let f = fx();
    let root = maestro_mcp::flow::flows_root(&f.paths.workspace).unwrap();
    let host = maestro_mcp::flow::host_id().unwrap();
    let mut ledger = maestro_flow::Ledger::open(&root, &host, maestro_flow::Options::default()).unwrap();
    let alias: SessionRef = format!("t3:local/{A}").parse().unwrap();
    let old = VerifiedSession::new(alias, f.work.to_string_lossy(), None, None, vec![])
        .with_owner_marker(format!("t3-owner:{A}:{CREATED}"));
    let flow = maestro_flow::kernel::compile(RELAY).unwrap();
    let begun = maestro_flow::kernel::start(&mut ledger, &old, &flow, json!({ "dir": f.work })).unwrap();
    drop(ledger);
    let fi = begun.instance.as_str().to_string();
    assert_eq!(
        f.events(&fi)[0]["body"]["owner"]["session"],
        json!({ "surface": "t3", "environment": "local", "id": A })
    );
    // Written as the registry's own form.
    let hosts = f.hosts(ATTRIB, &[(T3V, A)], Reg::Good, true);
    let open = f.call(&hosts, OPEN, json!({ "session": format!("t3:{ENV}/{A}") }));
    assert_eq!(open["refuses_stop"], true, "{open}");
    // Gated by the Stop hook.
    let out = gate(&f, &hosts, Harness::Claude, &claude_stop());
    assert!(out.blocked, "{out:?}");
    assert!(out.stdout.contains(&fi), "{out:?}");
    // An instance owned by the same thread id in another T3 environment is not this thread's: a
    // non-alias environment is never canonicalized away.
    let foreign: SessionRef = format!("t3:env-other/{A}").parse().unwrap();
    let theirs = VerifiedSession::new(foreign, f.work.to_string_lossy(), None, None, vec![])
        .with_owner_marker(format!("t3-owner:{A}:{CREATED}"));
    let mut ledger = maestro_flow::Ledger::open(&root, &host, maestro_flow::Options::default()).unwrap();
    let begun = maestro_flow::kernel::start(&mut ledger, &theirs, &flow, json!({ "dir": f.work })).unwrap();
    drop(ledger);
    let claim = json!({ "instance": begun.instance.as_str(), "node_id": "one" });
    let v = f.call(&hosts, CLAIM, claim);
    assert_eq!(v["code"], "not_owner", "{v}");
}

fn claude_stop() -> Vec<u8> {
    json!({ "session_id": "abc123", "hook_event_name": "Stop", "stop_hook_active": false, "cwd": "/tmp" })
        .to_string()
        .into_bytes()
}

fn codex_stop(session_id: &str) -> Vec<u8> {
    json!({
        "cwd": "/tmp", "hook_event_name": "Stop", "last_assistant_message": "Done.", "model": "gpt",
        "permission_mode": "bypassPermissions", "session_id": session_id, "stop_hook_active": false,
        "transcript_path": null, "turn_id": "01a2",
    })
    .to_string()
    .into_bytes()
}

/// The gate with no session named, as a hook runs it in a T3 thread.
fn gate(f: &Fx, hosts: &SurfaceRegistry, harness: Harness, input: &[u8]) -> Outcome {
    let g = Gate { harness, cannot_block: false, session: None, alert_ledger: "ledger.yaml".into() };
    stop::gate(&f.paths, hosts, Ambient::Caller, &json!({}), &g, input)
}

fn allowed(out: &Outcome) {
    assert!(!out.blocked, "{out:?}");
    assert_eq!(out.stdout, "{}");
}

/// Stop gate 1: Claude's variable with open work refuses.
#[test]
fn stop_gate_claude_variable_with_open_work_refuses() {
    let f = fx();
    let fi = f.begin_as((T3V, A));
    let hosts = f.hosts(ATTRIB, &[(T3V, A)], Reg::Good, false);
    let out = gate(&f, &hosts, Harness::Claude, &claude_stop());
    assert!(out.blocked && out.stdout.contains(&fi), "{out:?}");
    // Claude's own stdin session_id is not a binding: with no variable the stop goes through.
    let bare = f.hosts(ATTRIB, &[], Reg::Good, false);
    allowed(&gate(&f, &bare, Harness::Claude, &codex_stop("native-a")));
}

/// Stop gate 2: Codex's stdin `session_id` with open work refuses.
#[test]
fn stop_gate_codex_stdin_session_with_open_work_refuses() {
    let f = fx();
    let fi = f.begin_as((CXV, "native-c"));
    let hosts = f.hosts(ATTRIB, &[], Reg::Good, false);
    let out = gate(&f, &hosts, Harness::Codex, &codex_stop("native-c"));
    assert!(out.blocked && out.stdout.contains(&fi), "{out:?}");
    // Strict mode binds nothing.
    let strict = f.hosts(STRICT, &[], Reg::Good, false);
    allowed(&gate(&f, &strict, Harness::Codex, &codex_stop("native-c")));
}

/// Stop gate 3: no variable lets the stop through silently.
#[test]
fn stop_gate_no_variable_goes_through_silently() {
    let f = fx();
    f.begin_as((T3V, A));
    let hosts = f.hosts(ATTRIB, &[], Reg::Good, false);
    let out = gate(&f, &hosts, Harness::Claude, &claude_stop());
    allowed(&out);
    assert!(out.alerts.is_empty() && out.recorded.is_empty(), "{out:?}");
}

/// Stop gate 4: disagreeing variables let the stop through, append `StopUnverified` to each
/// candidate's unfinished instance, and file the alert as an unverified caller.
#[test]
fn stop_gate_disagreeing_variables_record_and_alert() {
    let f = fx();
    let a = f.begin_as((T3V, A));
    let b = f.begin_as((T3V, B));
    let c = f.begin_as((T3V, C));
    let hosts = f.hosts(ATTRIB, &[(T3V, A), (CXV, "native-b")], Reg::Good, false);
    let out = gate(&f, &hosts, Harness::Codex, &codex_stop("native-b"));
    allowed(&out);
    assert_eq!(out.alerts.len(), 1, "{out:?}");
    assert!(out.notes.iter().any(|n| n.contains("binding_unavailable: thread_mismatch")), "{out:?}");
    for fi in [&a, &b] {
        let last = f.events(fi).last().cloned().unwrap();
        assert_eq!(last["kind"], "StopUnverified", "{fi}: {last}");
        let t3 = |id: &str| json!({ "surface": "t3", "environment": ENV, "id": id });
        assert_eq!(last["body"]["candidates"], json!([t3(A), t3(B)]));
        assert_eq!(last["body"]["audit"]["identity"]["mode"], "unbound");
    }
    assert_eq!(f.events(&c).last().unwrap()["kind"], "Started", "not a candidate's");
    let ledger = f.ledger();
    assert!(ledger.contains(&out.alerts[0]) && ledger.contains("StopUnverified on"), "{ledger}");
    // The request journal records the alert as unverified.
    let journal = std::fs::read_to_string(maestro_store::requests::journal(&f.paths)).unwrap();
    assert!(journal.contains("\"verified\":false"), "{journal}");
    // StopUnverified changes nothing: the instance still reads, with the same work.
    let status = f.call(&hosts, maestro_mcp::flow::STATUS, json!({ "instance": a }));
    assert_eq!(status["ok"], true, "{status}");
}

/// Stop gate 5: a registry read error lets the stop through and files the alert.
#[test]
fn stop_gate_read_error_records_and_alerts() {
    let f = fx();
    f.begin_as((T3V, A));
    let hosts = f.hosts(ATTRIB, &[(T3V, A)], Reg::Broken, false);
    let out = gate(&f, &hosts, Harness::Claude, &claude_stop());
    allowed(&out);
    assert_eq!(out.alerts.len(), 1, "{out:?}");
    assert!(out.notes.iter().any(|n| n.contains("registry_unavailable")), "{out:?}");
    assert!(f.ledger().contains("t3:-/unverified") || f.ledger().contains(&out.alerts[0]));
}

/// Stop gate 6: an append that fails still lets the stop through, and the alert names it.
#[test]
fn stop_gate_failed_append_is_named_in_the_alert() {
    let f = fx();
    let a = f.begin_as((T3V, A));
    let log = f.root.join("workspace/.broomva/flows/instances").join(&a).join("events.jsonl");
    std::fs::set_permissions(&log, std::os::unix::fs::PermissionsExt::from_mode(0o444)).unwrap();
    let hosts = f.hosts(ATTRIB, &[(T3V, A), (CXV, "native-b")], Reg::Good, false);
    let out = gate(&f, &hosts, Harness::Claude, &claude_stop());
    std::fs::set_permissions(&log, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
    allowed(&out);
    assert!(out.recorded.is_empty(), "{out:?}");
    assert_eq!(out.alerts.len(), 1, "{out:?}");
    assert!(f.ledger().contains(&format!("StopUnverified could not be recorded on: {a}")), "{}", f.ledger());
}

/// The generated driver skill says how each harness writes.
#[test]
fn the_driver_skill_says_how_codex_and_gemini_write() {
    let flow = maestro_flow::kernel::compile(RELAY).unwrap();
    let skill = maestro_flow::driver::generate(&flow).unwrap();
    for needle in ["Codex writes through the CLI", "BRO-3848", "Gemini", "attribution_missing"] {
        assert!(skill.contains(needle), "{needle}");
    }
}

/// The gate with `session` named (`--session` / `MAESTRO_SESSION`), unsigned.
fn gate_named(f: &Fx, hosts: &SurfaceRegistry, harness: Harness, session: &str, input: &[u8]) -> Outcome {
    let g = Gate { harness, cannot_block: false, session: Some(session.into()), alert_ledger: "ledger.yaml".into() };
    stop::gate(&f.paths, hosts, Ambient::Caller, &json!({}), &g, input)
}

fn last_kind(f: &Fx, fi: &str) -> String {
    f.events(fi).last().unwrap()["kind"].as_str().unwrap().to_string()
}

/// BRO-3846 review, finding 1: a named session is bound through a Codex hook's stdin `session_id`
/// too, and compared with the canonical named session. Matching: the stop is refused over open
/// work. Mismatching or unresolved: the binding is unavailable, recorded and alerted.
#[test]
fn a_named_session_is_bound_through_a_codex_hooks_stdin() {
    let f = fx();
    let c = f.begin_as((CXV, "native-c"));
    let a = f.begin_as((T3V, A));
    let hosts = f.hosts(ATTRIB, &[], Reg::Good, false);
    for named in [format!("t3:{ENV}/{C}"), format!("t3:local/{C}")] {
        let out = gate_named(&f, &hosts, Harness::Codex, &named, &codex_stop("native-c"));
        assert!(out.blocked && out.stdout.contains(&c), "{named}: {out:?}");
    }
    // Mismatching: the named thread and the stdin's are both candidates.
    let out = gate_named(&f, &hosts, Harness::Codex, &format!("t3:{ENV}/{A}"), &codex_stop("native-c"));
    allowed(&out);
    assert!(out.notes.iter().any(|n| n.contains("binding_unavailable: thread_mismatch")), "{out:?}");
    assert_eq!((last_kind(&f, &a), last_kind(&f, &c)), ("StopUnverified".into(), "StopUnverified".into()));
    assert_eq!(out.alerts.len(), 1, "{out:?}");
    // Unresolved: the named thread is the candidate.
    let out = gate_named(&f, &hosts, Harness::Codex, &format!("t3:{ENV}/{A}"), &codex_stop("native-nobody"));
    allowed(&out);
    assert!(out.notes.iter().any(|n| n.contains("binding_unavailable: attribution_unresolved")), "{out:?}");
    let last = f.events(&a).pop().unwrap();
    assert!(last["body"]["reason"].as_str().unwrap().starts_with("attribution_unresolved"), "{last}");
    assert_eq!(out.alerts.len(), 1, "{out:?}");
}

/// Beside a named Direct session, a Codex hook's stdin `session_id` is Codex's own id: it binds
/// nothing unless it resolves to a T3 thread, which the Direct session can never be.
#[test]
fn a_direct_sessions_codex_stdin_contradicts_it_only_as_a_t3_thread() {
    let f = fx();
    let hosts = f.hosts(ATTRIB, &[], Reg::Good, false);
    let v = f.call(&hosts, START, f.direct(f.start_args(Some("direct:-/d1"))));
    let d = v["instance"].as_str().unwrap().to_string();
    let g = Gate {
        harness: Harness::Codex,
        cannot_block: false,
        session: Some("direct:-/d1".into()),
        alert_ledger: "ledger.yaml".into(),
    };
    let claim = json!({ "token": "tok-1", "cwd": f.work });
    let out = stop::gate(&f.paths, &hosts, Ambient::Shared, &claim, &g, &codex_stop("01a1-not-t3"));
    assert!(out.blocked && out.stdout.contains(&d), "{out:?}");
    let out = stop::gate(&f.paths, &hosts, Ambient::Shared, &claim, &g, &codex_stop("native-c"));
    allowed(&out);
    assert!(out.notes.iter().any(|n| n.contains("binding_unavailable: thread_mismatch")), "{out:?}");
}

/// BRO-3846 review, finding 2: a failure after resolution, a named session's attribution failure,
/// and a registry failure each make the binding unavailable: `StopUnverified` on the known
/// candidates, and the alert filed as an unverified caller.
#[test]
fn every_attribution_failure_of_the_gate_is_a_binding_unavailable() {
    let f = fx();
    let a = f.begin_as((T3V, A));
    let unverified = |f: &Fx| {
        let journal = std::fs::read_to_string(maestro_store::requests::journal(&f.paths)).unwrap_or_default();
        journal.lines().filter(|l| l.contains("\"verified\":false")).count()
    };
    // Resolved, then the verification fails: the thread has no incarnation marker.
    let hosts = f.hosts(ATTRIB, &[(T3V, "mcp:thread-bare")], Reg::Good, false);
    let out = gate(&f, &hosts, Harness::Claude, &claude_stop());
    allowed(&out);
    assert!(out.notes.iter().any(|n| n.contains("binding_unavailable: invalid_token")), "{out:?}");
    assert_eq!((out.alerts.len(), unverified(&f)), (1, 1), "{out:?}");
    // A named T3 session that does not verify (no variable, no credential).
    let bare = f.hosts(ATTRIB, &[], Reg::Good, false);
    let out = gate_named(&f, &bare, Harness::Claude, &format!("t3:{ENV}/{A}"), &claude_stop());
    allowed(&out);
    assert!(out.notes.iter().any(|n| n.contains("binding_unavailable: attribution_missing")), "{out:?}");
    assert_eq!(last_kind(&f, &a), "StopUnverified");
    assert_eq!((out.alerts.len(), unverified(&f)), (1, 2), "{out:?}");
    // A named session whose registry does not read.
    let broken = f.hosts(ATTRIB, &[(T3V, A)], Reg::Broken, false);
    let out = gate_named(&f, &broken, Harness::Claude, &format!("t3:{ENV}/{A}"), &claude_stop());
    allowed(&out);
    assert!(out.notes.iter().any(|n| n.contains("binding_unavailable: registry_unavailable")), "{out:?}");
    let last = f.events(&a).pop().unwrap();
    assert!(last["body"]["reason"].as_str().unwrap().starts_with("registry_unavailable"), "{last}");
    assert_eq!((out.alerts.len(), unverified(&f)), (1, 3), "{out:?}");
}

/// On a host with no T3 registry, a Codex hook's stdin `session_id` is no T3 thread's: with no
/// session named, the stop goes through silently, as before BRO-3846.
#[test]
fn without_t3_a_codex_hooks_stdin_binds_nothing() {
    let f = fx();
    let hosts = SurfaceRegistry::new(None, None, None).with_identity_mode(ATTRIB);
    let out = gate(&f, &hosts, Harness::Codex, &codex_stop("01a1"));
    allowed(&out);
    assert!(out.alerts.is_empty() && out.recorded.is_empty(), "{out:?}");
}

/// A named T3 session that presents a credential is judged by it alone: a bad one, beside its own
/// matching variable, fails open as a credential refusal, never attributed; and one checked
/// against a registry that does not read is a binding that is unavailable.
#[test]
fn a_named_sessions_credential_is_final_at_the_gate() {
    let f = fx();
    let a = f.begin_as((T3V, A));
    let hosts = f.hosts(ATTRIB, &[(T3V, A)], Reg::Good, true);
    let mut cred = f.t3_credential(&hosts, A);
    cred.signature = "00".repeat(32);
    let g = Gate {
        harness: Harness::Claude,
        cannot_block: false,
        session: Some(format!("t3:{ENV}/{A}")),
        alert_ledger: "ledger.yaml".into(),
    };
    let claim = json!({ "cwd": f.work, "node": "node-1", "credential": cred });
    let out = stop::gate(&f.paths, &hosts, Ambient::Caller, &claim, &g, &claude_stop());
    allowed(&out);
    assert!(out.notes.iter().any(|n| n.contains("caller_refused") && n.contains("InvalidToken")), "{out:?}");
    assert_eq!(last_kind(&f, &a), "Started", "a credential refusal records nothing");
    let good = f.t3_credential(&hosts, A);
    let broken = f.hosts(ATTRIB, &[], Reg::Broken, true);
    let claim = json!({ "cwd": f.work, "node": "node-1", "credential": good });
    let out = stop::gate(&f.paths, &broken, Ambient::Caller, &claim, &g, &claude_stop());
    allowed(&out);
    assert!(out.notes.iter().any(|n| n.contains("binding_unavailable: registry_disconnected")), "{out:?}");
    assert_eq!(last_kind(&f, &a), "StopUnverified");
}

/// Gemini's joint review: every Stop-gate alert path files as an unverified caller, never through
/// a caller check, so an alert whose session's claim does not verify still lands. `fail_open`: a
/// Direct session with a wrong launch token. `report_unread` and `unblockable`: an attributed T3
/// session on a host whose T3 registry is configured, whose claim a caller check refuses.
#[test]
fn every_stop_gate_alert_lands_when_the_claim_does_not_verify() {
    let landed = |f: &Fx, out: &Outcome, path: &str| {
        assert_eq!(out.alerts.len(), 1, "{path}: {out:?}");
        assert!(f.ledger().contains(&out.alerts[0]), "{path}: the alert is not in the ledger");
        let journal = std::fs::read_to_string(maestro_store::requests::journal(&f.paths)).unwrap();
        let line =
            journal.lines().find(|l| l.contains(&out.alerts[0])).unwrap_or_else(|| panic!("{path}: no journal line"));
        assert!(line.contains("\"verified\":false"), "{path}: {line}");
    };
    // fail_open: the named Direct session's token is wrong, so its claim is refused.
    let f = fx();
    let hosts = f.hosts(ATTRIB, &[], Reg::Good, false);
    let g = Gate {
        harness: Harness::Claude,
        cannot_block: false,
        session: Some("direct:-/d1".into()),
        alert_ledger: "ledger.yaml".into(),
    };
    let wrong = json!({ "token": "not-the-token", "cwd": f.work });
    let out = stop::gate(&f.paths, &hosts, Ambient::Shared, &wrong, &g, &claude_stop());
    allowed(&out);
    assert!(out.notes.iter().any(|n| n.contains("caller_refused")), "{out:?}");
    landed(&f, &out, "fail_open");

    // report_unread: thread A's stop, beside an instance whose log does not read.
    let f = fx();
    f.begin_as((T3V, A));
    let torn = f.begin_as((T3V, B));
    let log = f.root.join("workspace/.broomva/flows/instances").join(&torn).join("events.jsonl");
    let mut text = std::fs::read_to_string(&log).unwrap();
    text.push_str("{\"torn\n");
    std::fs::write(&log, text).unwrap();
    let hosts = f.hosts(ATTRIB, &[(T3V, A)], Reg::Good, false);
    let out = gate(&f, &hosts, Harness::Claude, &claude_stop());
    assert!(out.blocked, "A still has work: {out:?}");
    assert!(out.notes.iter().any(|n| n.contains("instances_unread")), "{out:?}");
    landed(&f, &out, "report_unread");

    // unblockable: thread A's harness cannot refuse the stop.
    let f = fx();
    let a = f.begin_as((T3V, A));
    let hosts = f.hosts(ATTRIB, &[(T3V, A)], Reg::Good, false);
    let g = Gate { harness: Harness::Claude, cannot_block: true, session: None, alert_ledger: "ledger.yaml".into() };
    let out = stop::gate(&f.paths, &hosts, Ambient::Caller, &json!({}), &g, &claude_stop());
    allowed(&out);
    assert_eq!(last_kind(&f, &a), "StopUnblockable");
    landed(&f, &out, "unblockable");
}

/// Gemini's joint round 2, MAJOR 1(a): a presented credential that does not parse is presented
/// all the same. Its parse error refuses the caller; the matching thread variable never attributes
/// it, at the Stop gate (named or not) or at a flow write.
#[test]
fn a_malformed_credential_is_refused_never_attributed() {
    let f = fx();
    let a = f.begin_as((T3V, A));
    let hosts = f.hosts(ATTRIB, &[(T3V, A)], Reg::Good, true);
    let claim = json!({ "credential": "not a credential" });
    for session in [Some(format!("t3:{ENV}/{A}")), None] {
        let g = Gate {
            harness: Harness::Claude,
            cannot_block: false,
            session: session.clone(),
            alert_ledger: "ledger.yaml".into(),
        };
        let out = stop::gate(&f.paths, &hosts, Ambient::Caller, &claim, &g, &claude_stop());
        allowed(&out);
        assert!(
            out.notes.iter().any(|n| n.contains("caller_refused") && n.contains("`credential`")),
            "{session:?}: {out:?}"
        );
    }
    assert_eq!(last_kind(&f, &a), "Started", "nothing recorded as A");
    let mut args = f.start_args(SA);
    args["credential"] = json!("not a credential");
    let v = f.call(&hosts, START, args);
    assert_eq!((v["ok"].clone(), v["code"].clone()), (json!(false), json!("caller_refused")), "{v}");
    assert!(v["reason"].as_str().unwrap().contains("`credential`"), "{v}");
}

/// Gemini's joint round 2, MAJOR 1(b): a named Paseo or Direct session whose hook carries a T3
/// thread variable is a binding that is unavailable, with that thread a candidate: unsigned, or
/// with its launch token. A registry that does not read for the stdin `session_id` is unavailable.
#[test]
fn a_named_non_t3_session_inside_a_t3_thread_is_unbound() {
    let f = fx();
    let a = f.begin_as((T3V, A));
    let hosts = f.hosts(ATTRIB, &[(T3V, A)], Reg::Good, false);
    let token = json!({ "token": "tok-1", "cwd": f.work });
    for (claim, n) in [(json!({}), 1), (token.clone(), 2)] {
        let g = Gate {
            harness: Harness::Claude,
            cannot_block: false,
            session: Some("direct:-/d1".into()),
            alert_ledger: "ledger.yaml".into(),
        };
        let out = stop::gate(&f.paths, &hosts, Ambient::Shared, &claim, &g, &claude_stop());
        allowed(&out);
        assert!(out.notes.iter().any(|n| n.contains("binding_unavailable: thread_mismatch")), "{claim}: {out:?}");
        // Recorded on A, or found already recorded on A (the same reason and candidates).
        let on_a = out.recorded.iter().any(|(i, _)| *i == a)
            || out.notes.iter().any(|x| x.contains(&format!("{a} has not moved since its StopUnverified")));
        assert!(on_a, "run {n} ({claim}): {out:?}");
        assert_eq!(last_kind(&f, &a), "StopUnverified");
    }
    // The stdin session_id against a T3 registry that does not read.
    let broken = f.hosts(ATTRIB, &[], Reg::Broken, false);
    let v = f.call(&f.hosts(ATTRIB, &[], Reg::Good, false), START, f.direct(f.start_args(Some("direct:-/d1"))));
    assert_eq!(v["ok"], true, "{v}");
    let g = Gate {
        harness: Harness::Codex,
        cannot_block: false,
        session: Some("direct:-/d1".into()),
        alert_ledger: "ledger.yaml".into(),
    };
    let out = stop::gate(&f.paths, &broken, Ambient::Shared, &token, &g, &codex_stop("native-c"));
    allowed(&out);
    assert!(out.notes.iter().any(|n| n.contains("binding_unavailable: registry_unavailable")), "{out:?}");
}

/// Gemini's joint round 2, MINOR 3: a fail-open alert is keyed by the workspace, the code and the
/// day, never the caller's own session string: two sessions that do not verify file one alert.
#[test]
fn fail_open_alerts_do_not_multiply_with_the_named_session() {
    let f = fx();
    let hosts = f.hosts(ATTRIB, &[], Reg::Good, false);
    let mut ids = Vec::new();
    for fake in ["direct:-/fake-1", "direct:-/fake-2"] {
        let g = Gate {
            harness: Harness::Claude,
            cannot_block: false,
            session: Some(fake.into()),
            alert_ledger: "ledger.yaml".into(),
        };
        let out = stop::gate(&f.paths, &hosts, Ambient::Shared, &json!({}), &g, &claude_stop());
        allowed(&out);
        assert_eq!(out.alerts.len(), 1, "{fake}: {out:?}");
        ids.push(out.alerts[0].clone());
    }
    assert_eq!(ids[0], ids[1]);
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &target);
        } else {
            std::fs::copy(e.path(), target).unwrap();
        }
    }
}

/// The machine fixture without its two deliberately malformed ledgers: a workspace whose needs
/// read complete, so a view's `needs_complete` flips only for what a test does.
fn complete_store(tmp: &Path) -> Store {
    let root = tmp.join("machine");
    copy_tree(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/machine"), &root);
    for ledger in ["broken.yaml", "alt-key.yaml"] {
        std::fs::remove_file(root.join("workspace/.control/asks").join(ledger)).unwrap();
    }
    Store::new(Paths::under(&root), TreeSource::WorkingTree)
}

/// Codex's joint round 5: the owner's views never show live permission prompts as complete when
/// some did not read. A snapshot row that does not read is an error of the needs section in
/// `maestro_snapshot` and `maestro_needs_list`, and an unread source of `maestro_context`, whose
/// needs are then incomplete; the prompts that did read are still listed. The workspace is the
/// machine fixture without its two deliberately malformed ledgers, so its needs read complete.
#[test]
fn the_owners_views_say_when_prompts_did_not_read() {
    let tmp = tempfile::tempdir().unwrap();
    let store = complete_store(tmp.path());
    let path = tmp.path().join("threads.json");
    let good = json!({ "id": "mcp:good", "cwd": "/w", "created_at": CREATED, "status": "running",
        "pending_permissions": [{ "name": "Bash", "input": { "command": "cargo test" } }] });
    let hosts = SurfaceRegistry::new(None, Some(T3Registry::new(&path)), None);
    let call = |tool: &str| call_with_hosts(&store, &hosts, tool, json!({})).unwrap();
    // Every row reads: the views are complete.
    std::fs::write(&path, json!([good.clone()]).to_string()).unwrap();
    let context = call("maestro_context");
    assert_eq!(context["packet"]["needs_complete"], true, "{context}");
    assert!(call("maestro_snapshot")["needs"].get("errors").is_none());
    // One row does not read: the good prompt is still listed, and every view says so.
    std::fs::write(&path, json!([7, good]).to_string()).unwrap();
    for (tool, section) in [("maestro_snapshot", "/needs"), ("maestro_needs_list", "")] {
        let v = call(tool);
        let needs = v.pointer(section).unwrap();
        assert!(needs["data"].to_string().contains("mcp:good"), "{tool}: the good prompt is listed: {needs}");
        let errors = needs["errors"].as_array().unwrap_or_else(|| panic!("{tool}: no errors: {needs}"));
        assert!(errors.iter().any(|e| e["reason"].as_str().unwrap().contains("not an object")), "{tool}: {needs}");
    }
    let context = call("maestro_context");
    assert_eq!(context["packet"]["needs_complete"], false, "{context}");
    assert!(context["packet"]["degraded"].to_string().contains("t3 permission prompts (1 unread)"), "{context}");
    assert!(context["markdown"].as_str().unwrap().contains("t3 permission prompts"), "{context}");
}

/// Codex's joint round 6: `live_permission_report` is the one boundary where every read failure of
/// a configured registry becomes an unread source: a T3 registry missing, inaccessible,
/// disconnected or not a JSON array; a Paseo agents folder missing or an agent file that does not
/// parse; a Direct registry file missing. Each leaves the context's needs incomplete, naming the
/// surface. A host with no registry configured has nothing to read and stays complete.
#[test]
fn a_configured_registry_that_does_not_read_is_never_empty_and_complete() {
    let tmp = tempfile::tempdir().unwrap();
    let store = complete_store(tmp.path());
    let context = |hosts: &SurfaceRegistry| {
        let v = call_with_hosts(&store, hosts, "maestro_context", json!({})).unwrap();
        (v["packet"]["needs_complete"].clone(), v["packet"]["degraded"].clone())
    };
    let snapshot_errors = |hosts: &SurfaceRegistry| {
        let v = call_with_hosts(&store, hosts, "maestro_snapshot", json!({})).unwrap();
        v["needs"]["errors"].to_string()
    };
    // Nothing configured: complete.
    let none = SurfaceRegistry::new(None, None, None);
    assert_eq!(context(&none), (json!(true), Value::Null));
    let t3 = |registry: T3Registry| SurfaceRegistry::new(None, Some(registry), None);
    let missing = tmp.path().join("no-such-t3");
    let not_array = tmp.path().join("object.json");
    std::fs::write(&not_array, "{}").unwrap();
    let locked = tmp.path().join("locked.json");
    std::fs::write(&locked, "[]").unwrap();
    std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
    let cases = [
        ("missing", t3(T3Registry::new(&missing)), "no T3 registry database or snapshot"),
        ("disconnected", t3(T3Registry::new(&not_array).with_disconnected(true)), "disconnected"),
        ("not an array", t3(T3Registry::new(&not_array)), "is malformed"),
        ("inaccessible", t3(T3Registry::new(&locked)), "could not be read"),
    ];
    for (name, hosts, says) in &cases {
        if *name == "inaccessible" && std::fs::read(&locked).is_ok() {
            continue; // Running as root: a mode-000 file still reads.
        }
        assert_eq!(context(hosts), (json!(false), json!(["t3 permission prompts (1 unread)"])), "{name}");
        let errors = snapshot_errors(hosts);
        assert!(errors.contains("t3 registry") && errors.contains(says), "{name}: {errors}");
    }
    std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    // Paseo: an agents folder that is not there, and an agent file that does not parse.
    let paseo = |dir: &Path| SurfaceRegistry::new(Some(PaseoRegistry::new(dir)), None, None);
    let gone = paseo(&tmp.path().join("no-such-agents"));
    assert_eq!(context(&gone), (json!(false), json!(["paseo permission prompts (1 unread)"])));
    let agents = tmp.path().join("agents");
    std::fs::create_dir_all(agents.join("work")).unwrap();
    std::fs::write(agents.join("work/agent-x.json"), "{ not json").unwrap();
    assert_eq!(context(&paseo(&agents)), (json!(false), json!(["paseo permission prompts (1 unread)"])));
    assert!(snapshot_errors(&paseo(&agents)).contains("does not parse"));
    // A disconnected Paseo registry, and an agent folder that does not read.
    let off = SurfaceRegistry::new(Some(PaseoRegistry::new(&agents).with_disconnected(true)), None, None);
    assert!(snapshot_errors(&off).contains("the Paseo registry is disconnected"));
    std::fs::remove_file(agents.join("work/agent-x.json")).unwrap();
    std::fs::create_dir_all(agents.join("shut")).unwrap();
    std::fs::set_permissions(agents.join("shut"), std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
    let blocked = std::fs::read_dir(agents.join("shut")).is_err(); // not as root
    let shut = snapshot_errors(&paseo(&agents));
    std::fs::set_permissions(agents.join("shut"), std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    if blocked {
        assert!(shut.contains("the agent folder does not read"), "{shut}");
    }
    // Direct: a configured registry file that is not there.
    let direct = SurfaceRegistry::new(None, None, Some(DirectRegistry::from_file(tmp.path().join("no-such.json"))));
    assert_eq!(context(&direct), (json!(false), json!(["direct permission prompts (1 unread)"])));
}
