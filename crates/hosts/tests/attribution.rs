//! Attributed session identity at the host level (BRO-3846): T3 thread ids, the registry root,
//! the identity mode, the precedence of what thread variables resolve to, resolution through
//! `statev2.sqlite`'s active provider row, the T3 reader's run status, and the audit facts.

use maestro_core::need::{NeedKind, NeedSource};
use maestro_hosts::attribution::{MAX_ANCESTORS, combine, process_facts};
use maestro_hosts::{
    Attributed, CallerClaim, IdentityMode, ProcStat, ProcessReader, RefusalReason, Resolution, Surface,
    SurfaceRegistry, T3Registry, ThreadVar, ThreadVars, VarOutcome, is_safe_t3_id,
};
use std::path::{Path, PathBuf};

fn id_of(len: usize) -> String {
    format!("mcp:{}", "x".repeat(len - 4))
}

#[test]
fn thread_ids_are_opaque_with_percent_and_up_to_512_bytes() {
    // Live ids the probes found: percent-encoded composites, 129 to 206 bytes.
    let delegated = format!("mcp:launch:{}", "0c5a6cd1910cbc2320492d4acd61c71a%3Aordinal%3A1".repeat(4));
    assert!(delegated.len() > 128 && delegated.len() <= 206, "{}", delegated.len());
    for ok in [
        "mcp:thread-a",
        "6ff25f7c-0000-4000-8000-000000000000",
        "provider-thread:provider:codex:native-thread:pending%3Arun%3Athread",
        "mcp:a%2Fb",
        delegated.as_str(),
        id_of(512).as_str(),
    ] {
        assert!(is_safe_t3_id(ok), "{ok}");
    }
    for bad in [
        id_of(513).as_str(),
        "",
        "mcp:a/b",
        "mcp:a..b",
        "mcp:a\0b",
        "%2Fstarts-with-percent",
        "-leading-dash",
        "mcp:a b",
        "mcp:a\\b",
    ] {
        assert!(!is_safe_t3_id(bad), "{bad:?}");
    }
}

#[test]
fn the_t3_root_is_maestro_t3_home_then_t3code_home_then_home_t3() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let at = |m: Option<&str>, t: Option<&str>, h: &Path| SurfaceRegistry::t3_root_from(m, t, Some(h));
    assert_eq!(at(None, None, &home), None, "no ~/.t3, no root");
    // A ~/.t3 with no registry under it (client config, logs, worktrees) is no root (Gemini's
    // joint round 7); one with a database under it is found.
    std::fs::create_dir_all(home.join(".t3/userdata")).unwrap();
    std::fs::create_dir_all(home.join(".t3/worktrees")).unwrap();
    assert_eq!(at(None, None, &home), None, "a ~/.t3 with no registry");
    std::fs::write(home.join(".t3/userdata/statev2.sqlite"), "").unwrap();
    assert_eq!(at(None, None, &home), Some((home.join(".t3"), true)));
    assert_eq!(at(None, Some("/t3code"), &home), Some((PathBuf::from("/t3code"), true)));
    assert_eq!(at(Some("/maestro"), Some("/t3code"), &home), Some((PathBuf::from("/maestro"), false)));
    // Empty is unset.
    assert_eq!(at(Some(""), Some(""), &home), Some((home.join(".t3"), true)));
    // A found root is a surface, but not a configured one.
    let found = SurfaceRegistry::new(None, Some(T3Registry::new(tmp.path())), None).with_t3_found(true);
    assert!(found.has_surface(&Surface::T3) && !found.has_configured_surface(&Surface::T3));
    let configured = SurfaceRegistry::new(None, Some(T3Registry::new(tmp.path())), None);
    assert!(configured.has_configured_surface(&Surface::T3));
}

#[test]
fn the_identity_mode_defaults_to_attributed_and_a_typo_is_strict() {
    assert_eq!(IdentityMode::parse(None), IdentityMode::Attributed);
    assert_eq!(IdentityMode::parse(Some("")), IdentityMode::Attributed);
    assert_eq!(IdentityMode::parse(Some("attributed")), IdentityMode::Attributed);
    assert_eq!(IdentityMode::parse(Some("strict")), IdentityMode::Strict);
    assert_eq!(IdentityMode::parse(Some("atributed")), IdentityMode::Strict);
    assert_eq!(SurfaceRegistry::new(None, None, None).identity_mode(), IdentityMode::Attributed);
}

fn var(name: &str) -> ThreadVar {
    ThreadVar { name: name.into(), value: "v".into(), native: name != "T3CODE_THREAD_ID" }
}

fn thread(id: &str) -> VarOutcome {
    VarOutcome::Thread { id: id.into(), environment: Some("env".into()) }
}

fn refused(a: Attributed) -> RefusalReason {
    match a {
        Attributed::Refused(r) => r.reason,
        other => panic!("not refused: {other:?}"),
    }
}

#[test]
fn precedence_is_unavailable_then_unresolved_then_mismatch() {
    let (t3, cx) = (var("T3CODE_THREAD_ID"), var("CODEX_THREAD_ID"));
    let unread = || VarOutcome::Unreadable("db locked".into());
    let unresolved = || VarOutcome::Unresolved("no thread".into());
    assert_eq!(combine(&[]), Attributed::Absent);
    for (each, want) in [
        (vec![(t3.clone(), unresolved()), (cx.clone(), unread())], RefusalReason::RegistryUnavailable),
        (vec![(t3.clone(), thread("a")), (cx.clone(), unread())], RefusalReason::RegistryUnavailable),
        (vec![(t3.clone(), thread("a")), (cx.clone(), unresolved())], RefusalReason::AttributionUnresolved),
        (vec![(t3.clone(), thread("a")), (cx.clone(), thread("b"))], RefusalReason::ThreadMismatch),
    ] {
        assert_eq!(refused(combine(&each)), want, "{each:?}");
    }
    match combine(&[(t3.clone(), thread("a")), (cx.clone(), thread("a"))]) {
        Attributed::Resolved { attribution, environment } => {
            assert_eq!((attribution.variable.as_str(), attribution.thread.as_str()), ("T3CODE_THREAD_ID", "a"));
            assert_eq!(environment.as_deref(), Some("env"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn with_no_t3_registry_a_variable_is_unavailable_never_absent() {
    let hosts = SurfaceRegistry::new(None, None, None)
        .with_thread_vars(ThreadVars::from_pairs([("T3CODE_THREAD_ID", "a".into())]));
    assert_eq!(refused(hosts.attribution()), RefusalReason::RegistryUnavailable);
    let none = SurfaceRegistry::new(None, None, None)
        .with_thread_vars(ThreadVars::from_pairs([("T3CODE_THREAD_ID", String::new())]));
    assert_eq!(none.attribution(), Attributed::Absent, "an empty variable is none");
}

/// A `statev2.sqlite` with the live schema's columns: threads, provider-thread rows, runs.
fn sqlite(dir: &Path) -> PathBuf {
    let userdata = dir.join("userdata");
    std::fs::create_dir_all(&userdata).unwrap();
    std::fs::write(userdata.join("environment-id"), "env-sqlite\n").unwrap();
    let db = userdata.join("statev2.sqlite");
    let script = r#"
import json, sqlite3, sys
conn = sqlite3.connect(sys.argv[1])
conn.executescript("""
CREATE TABLE orchestration_v2_projection_threads (
  thread_id TEXT PRIMARY KEY, project_id TEXT, title TEXT, default_provider TEXT, runtime_mode TEXT,
  interaction_mode TEXT, active_provider_thread_id TEXT, created_at TEXT, updated_at TEXT,
  archived_at TEXT, deleted_at TEXT, payload_json TEXT, provider_instance_id TEXT);
CREATE TABLE orchestration_v2_projection_provider_threads (
  provider_thread_id TEXT PRIMARY KEY, thread_id TEXT, owner_node_id TEXT, provider TEXT,
  provider_session_id TEXT, status TEXT, first_run_ordinal INTEGER, last_run_ordinal INTEGER,
  updated_at TEXT, payload_json TEXT, driver TEXT, provider_instance_id TEXT);
CREATE TABLE orchestration_v2_projection_runs (
  run_id TEXT PRIMARY KEY, thread_id TEXT, ordinal INTEGER, provider TEXT, provider_thread_id TEXT,
  status TEXT, requested_at TEXT, completed_at TEXT, payload_json TEXT, provider_instance_id TEXT);
""")
def thread(tid, active, archived=None, deleted=None, settled=None):
    payload = {"worktreePath": "/work"}
    if settled:
        payload["settledAt"] = settled
    conn.execute("INSERT INTO orchestration_v2_projection_threads VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (tid, "p", "t", "codex", "full", "default", active, "2026-11-17T00:00:00Z", "2026-11-17T00:00:00Z",
         archived, deleted, json.dumps(payload), "codex"))
def provider(ptid, tid, native):
    conn.execute("INSERT INTO orchestration_v2_projection_provider_threads VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
        (ptid, tid, None, "codex", None, "active", 1, 1, "2026-11-17T00:00:00Z",
         json.dumps({"nativeThreadRef": {"driver": "codex", "nativeId": native, "strength": "strong"}}), "codex", "codex"))
def run(tid, ordinal, status):
    conn.execute("INSERT INTO orchestration_v2_projection_runs VALUES (?,?,?,?,?,?,?,?,?,?)",
        (f"run:{tid}:{ordinal}", tid, ordinal, "codex", None, status, "2026-11-17T00:00:00Z", None, "{}", "codex"))
# A switched provider: the retired row's native id never attributes.
thread("mcp:switched", "pt-new")
provider("pt-old", "mcp:switched", "native-old")
provider("pt-new", "mcp:switched", "native-new")
# Settled threads still resolve; archived and deleted ones do not.
thread("mcp:settled", "pt-settled", settled="2026-11-17T02:00:00Z")
provider("pt-settled", "mcp:settled", "native-settled")
thread("mcp:archived", "pt-arch", archived="2026-11-17T01:00:00Z")
provider("pt-arch", "mcp:archived", "native-arch")
thread("mcp:deleted", "pt-del", deleted="2026-11-17T01:00:00Z")
provider("pt-del", "mcp:deleted", "native-del")
# Two active rows with one native id.
thread("mcp:dup-1", "pt-d1"); provider("pt-d1", "mcp:dup-1", "native-dup")
thread("mcp:dup-2", "pt-d2"); provider("pt-d2", "mcp:dup-2", "native-dup")
# Runs: the latest by ordinal is the status, whatever the insert order.
run("mcp:switched", 2, "completed"); run("mcp:switched", 3, "running"); run("mcp:switched", 1, "failed")
conn.commit()
"#;
    let out = std::process::Command::new("python3").arg("-I").arg("-c").arg(script).arg(&db).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    dir.to_path_buf()
}

fn resolved(r: Resolution) -> String {
    match r {
        Resolution::Thread(t) => t.id,
        Resolution::Unresolved(why) => panic!("unresolved: {why}"),
    }
}

fn unresolved(r: Resolution) -> String {
    match r {
        Resolution::Unresolved(why) => why,
        Resolution::Thread(t) => panic!("resolved to {}", t.id),
    }
}

#[test]
fn native_ids_resolve_through_the_active_provider_row_only() {
    let tmp = tempfile::tempdir().unwrap();
    let t3 = T3Registry::new(sqlite(tmp.path()));
    assert_eq!(resolved(t3.resolve("native-new", true).unwrap()), "mcp:switched");
    assert!(unresolved(t3.resolve("native-old", true).unwrap()).contains("no T3 thread"));
    assert!(unresolved(t3.resolve("native-dup", true).unwrap()).contains("2 active provider rows"));
    // Liveness for attribution: archived_at and deleted_at, never settled_at.
    assert_eq!(resolved(t3.resolve("native-settled", true).unwrap()), "mcp:settled");
    assert_eq!(resolved(t3.resolve("mcp:settled", false).unwrap()), "mcp:settled");
    // An archived thread's rows are not scanned at all: its native id names no live thread.
    assert!(unresolved(t3.resolve("native-arch", true).unwrap()).contains("no T3 thread"));
    assert!(unresolved(t3.resolve("mcp:archived", false).unwrap()).contains("archived"));
    assert!(unresolved(t3.resolve("mcp:deleted", false).unwrap()).contains("deleted"));
    assert!(unresolved(t3.resolve("mcp:nobody", false).unwrap()).contains("no T3 thread"));
    assert!(unresolved(t3.resolve(&id_of(513), false).unwrap()).contains("not a T3 thread id"));
    // The environment id beside the database is the canonical environment.
    assert_eq!(t3.environment_id().as_deref(), Some("env-sqlite"));
    // A registry that does not read is unavailable, never a fallback.
    let broken = tmp.path().join("broken");
    std::fs::create_dir_all(broken.join("userdata")).unwrap();
    std::fs::write(broken.join("userdata/statev2.sqlite"), "not a database").unwrap();
    let e = T3Registry::new(&broken).resolve("native-new", true).unwrap_err();
    assert_eq!(e.reason, RefusalReason::RegistryUnavailable);
    let e = T3Registry::new(&broken).resolve("mcp:switched", false).unwrap_err();
    assert_eq!(e.reason, RefusalReason::RegistryUnavailable);
}

/// The T3 reader reads the latest run of `orchestration_v2_projection_runs`, by ordinal.
#[test]
fn the_reader_takes_the_latest_run_by_ordinal() {
    let tmp = tempfile::tempdir().unwrap();
    let t3 = T3Registry::new(sqlite(tmp.path()));
    let thread = t3.find("mcp:switched").unwrap();
    assert_eq!(thread.status.as_deref(), Some("running"));
    assert_eq!(thread.run_id.as_deref(), Some("run:mcp:switched:3"));
}

/// `verify_attributed` sets the same owner marker `verify` sets, and ignores `settled_at`.
#[test]
fn verify_attributed_sets_the_t3_owner_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let t3 = T3Registry::new(sqlite(tmp.path()));
    let v = t3.verify_attributed("mcp:settled", None).unwrap();
    assert_eq!(v.owner_marker(), Some("t3-owner:mcp:settled:2026-11-17T00:00:00Z"));
    assert_eq!(v.session().to_string(), "t3:env-sqlite/mcp:settled");
    let claim = CallerClaim::t3(Some("mcp:settled"), Some("/work"));
    let marker = t3.verify(&claim);
    // `verify` keeps its own liveness rule: a settled thread with no running run is not live.
    assert_eq!(marker.unwrap_err().reason, RefusalReason::NotLive);
    assert_eq!(t3.verify_attributed("mcp:archived", None).unwrap_err().reason, RefusalReason::AttributionUnresolved);
}

#[derive(Debug)]
struct Procs(Vec<(u32, Result<ProcStat, String>)>);

impl ProcessReader for Procs {
    fn read(&self, pid: u32) -> Result<ProcStat, String> {
        self.0.iter().find(|(p, _)| *p == pid).map(|(_, s)| s.clone()).unwrap_or(Err(format!("no {pid}")))
    }
}

fn stat(ppid: u32, command: &str) -> Result<ProcStat, String> {
    Ok(ProcStat { ppid, command: command.into(), start_time: Ok("t".into()), uid: Ok(501) })
}

#[test]
fn process_facts_record_unread_facts_as_null_with_a_reason() {
    // The process itself does not read: every fact is null, each with the reason.
    let facts = process_facts(&Procs(vec![]), 10, vec!["T3CODE_THREAD_ID".into()]);
    assert_eq!((facts.start_time, facts.uid, facts.ancestors), (None, None, None));
    assert_eq!(facts.unread.len(), 3, "{:?}", facts.unread);
    assert_eq!(facts.variables, ["T3CODE_THREAD_ID"]);
    // A chain longer than the bound is cut at MAX_ANCESTORS.
    let chain: Vec<_> = (10..30).map(|p| (p, stat(p + 1, &format!("p{p}")))).collect();
    let facts = process_facts(&Procs(chain), 10, vec![]);
    let ancestors = facts.ancestors.unwrap();
    assert_eq!(ancestors.len(), MAX_ANCESTORS);
    assert_eq!((ancestors[0].pid, ancestors[0].command.as_deref()), (11, Some("p11")));
    assert_eq!(facts.uid, Some(501));
    // An ancestor that does not read ends the chain, null with its reason.
    let facts = process_facts(&Procs(vec![(10, stat(11, "me")), (11, stat(12, "parent"))]), 10, vec![]);
    let ancestors = facts.ancestors.unwrap();
    assert_eq!(ancestors.last().map(|a| (a.pid, a.command.clone())), Some((12, None)));
    assert!(facts.unread["ancestors"].contains("process 12"), "{:?}", facts.unread);
}

/// A `statev2.sqlite` with the threads and provider tables (`provider: false` leaves the provider
/// table out), then `inserts`, Python run with `thread(tid, active, payload)` and
/// `provider(ptid, tid, payload_json)` in scope.
fn partial_db(dir: &Path, provider: bool, inserts: &str) -> PathBuf {
    let userdata = dir.join("userdata");
    std::fs::create_dir_all(&userdata).unwrap();
    let db = userdata.join("statev2.sqlite");
    let provider_table = if provider {
        "CREATE TABLE orchestration_v2_projection_provider_threads (provider_thread_id TEXT PRIMARY KEY, thread_id TEXT, payload_json TEXT);"
    } else {
        ""
    };
    let script = format!(
        r#"
import json, sqlite3, sys
conn = sqlite3.connect(sys.argv[1])
conn.executescript("""
CREATE TABLE orchestration_v2_projection_threads (
  thread_id TEXT PRIMARY KEY, project_id TEXT, title TEXT, default_provider TEXT,
  active_provider_thread_id TEXT, created_at TEXT, updated_at TEXT, archived_at TEXT,
  deleted_at TEXT, payload_json TEXT);
{provider_table}
""")
def thread(tid, active, payload):
    conn.execute("INSERT INTO orchestration_v2_projection_threads VALUES (?,?,?,?,?,?,?,?,?,?)",
        (tid, "p", "t", "codex", active, "2026-11-17T00:00:00Z", "2026-11-17T00:00:00Z", None, None, json.dumps(payload)))
def provider(ptid, tid, payload_json):
    conn.execute("INSERT INTO orchestration_v2_projection_provider_threads VALUES (?,?,?)", (ptid, tid, payload_json))
def native(n):
    return json.dumps({{"nativeThreadRef": {{"driver": "codex", "nativeId": n}}}})
{inserts}
conn.commit()
"#
    );
    let out = std::process::Command::new("python3").arg("-I").arg("-c").arg(script).arg(&db).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    dir.to_path_buf()
}

fn unavailable(r: Result<Resolution, maestro_hosts::IdentityRefusal>) -> String {
    match r {
        Err(e) if e.reason == RefusalReason::RegistryUnavailable => e.message,
        other => panic!("not registry_unavailable: {other:?}"),
    }
}

/// Uniqueness of a native id needs every active provider row read (BRO-3846 review): a missing
/// provider table, an active row whose payload does not parse or whose `nativeThreadRef` is
/// malformed, or an active pointer with no row is `registry_unavailable`, even beside a match.
#[test]
fn a_partial_read_of_the_active_rows_is_registry_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = partial_db(&tmp.path().join("missing"), false, r#"thread("mcp:a", "pt-a", {"worktreePath": "/w"})"#);
    let why = unavailable(T3Registry::new(missing).resolve("native-a", true));
    assert!(why.contains("missing table(s) orchestration_v2_projection_provider_threads"), "{why}");
    let good = r#"
thread("mcp:a", "pt-a", {"worktreePath": "/w"}); provider("pt-a", "mcp:a", native("native-a"))
"#;
    for (name, bad, says) in [
        ("unparsed", r#"thread("mcp:b", "pt-b", {}); provider("pt-b", "mcp:b", "{not json")"#, "does not parse"),
        ("empty", r#"thread("mcp:b", "pt-b", {}); provider("pt-b", "mcp:b", "")"#, "is empty"),
        ("blank", r#"thread("mcp:b", "pt-b", {}); provider("pt-b", "mcp:b", " ")"#, "is empty"),
        ("null", r#"thread("mcp:b", "pt-b", {}); provider("pt-b", "mcp:b", None)"#, "is empty"),
        ("not an object", r#"thread("mcp:b", "pt-b", {}); provider("pt-b", "mcp:b", "[1]")"#, "is not an object"),
        (
            "malformed ref",
            r#"thread("mcp:b", "pt-b", {}); provider("pt-b", "mcp:b", json.dumps({"nativeThreadRef": "native-a"}))"#,
            "has a malformed nativeThreadRef",
        ),
        (
            "no native id",
            r#"thread("mcp:b", "pt-b", {}); provider("pt-b", "mcp:b", json.dumps({"nativeThreadRef": {"nativeId": 7}}))"#,
            "has a malformed nativeThreadRef",
        ),
        ("dangling", r#"thread("mcp:b", "pt-b", {})"#, "is missing"),
        // An active pointer at another thread's row never attributes that row: it is dangling.
        ("borrowed", r#"thread("mcp:b", "pt-a", {})"#, "is missing"),
    ] {
        let db = partial_db(&tmp.path().join(name), true, &format!("{good}\n{bad}"));
        let why = unavailable(T3Registry::new(db).resolve("native-a", true));
        assert!(why.contains(&format!("the active provider row of thread mcp:b {says}")), "{name}: {why}");
    }
    // A row with no `nativeThreadRef` at all has no native id yet: not a partial read.
    let fresh = partial_db(
        &tmp.path().join("fresh"),
        true,
        &format!("{good}\n{}", r#"thread("mcp:b", "pt-b", {}); provider("pt-b", "mcp:b", "{}")"#),
    );
    assert_eq!(resolved(T3Registry::new(fresh).resolve("native-a", true).unwrap()), "mcp:a");
}

/// The JSON snapshot: an active pointer with no provider row is a partial read too.
#[test]
fn a_snapshot_active_pointer_with_no_row_is_registry_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("threads.json");
    let rows = serde_json::json!([
        { "id": "mcp:a", "cwd": "/w", "active_provider_thread_id": "pt-a",
          "provider_threads": [{ "provider_thread_id": "pt-a", "native_id": "native-a" }] },
        { "id": "mcp:b", "cwd": "/w", "active_provider_thread_id": "pt-b", "provider_threads": [] },
    ]);
    std::fs::write(&path, rows.to_string()).unwrap();
    unavailable(T3Registry::new(&path).resolve("native-a", true));
}

/// A live thread with no usable folder (no `worktreePath`, no project root) is still the thread its
/// variable names (BRO-3846 review); the credentialed reader still needs the folder.
#[test]
fn a_live_thread_with_no_cwd_is_attributed() {
    let tmp = tempfile::tempdir().unwrap();
    let db = partial_db(
        tmp.path(),
        true,
        r#"thread("mcp:nocwd", "pt-n", {}); provider("pt-n", "mcp:nocwd", native("native-n"))"#,
    );
    let t3 = T3Registry::new(db);
    assert_eq!(resolved(t3.resolve("mcp:nocwd", false).unwrap()), "mcp:nocwd");
    assert_eq!(resolved(t3.resolve("native-n", true).unwrap()), "mcp:nocwd");
    let v = t3.verify_attributed("mcp:nocwd", None).unwrap();
    assert_eq!(v.owner_marker(), Some("t3-owner:mcp:nocwd:2026-11-17T00:00:00Z"));
    assert!(t3.find("mcp:nocwd").is_none(), "the credentialed reader needs a folder");
    // The JSON snapshot: a row with no cwd at all.
    let path = tmp.path().join("threads.json");
    let rows = serde_json::json!([{ "id": "mcp:nocwd", "created_at": "2026-11-17T00:00:00Z", "run_id": "r" }]);
    std::fs::write(&path, rows.to_string()).unwrap();
    let t3 = T3Registry::new(&path);
    assert_eq!(resolved(t3.resolve("mcp:nocwd", false).unwrap()), "mcp:nocwd");
    assert!(t3.verify_attributed("mcp:nocwd", None).is_ok());
    assert!(t3.find("mcp:nocwd").is_none(), "the credentialed reader needs a folder in a snapshot too");
}

/// The legacy `projection_threads` schema: a thread with no folder is attributed there too; and a
/// database with no thread table at all is unavailable, not "no such thread".
#[test]
fn the_legacy_schema_attributes_and_no_thread_table_is_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let make = |name: &str, sql: &str| {
        let dir = tmp.path().join(name).join("userdata");
        std::fs::create_dir_all(&dir).unwrap();
        let script =
            "import sqlite3, sys\nc = sqlite3.connect(sys.argv[1])\nc.executescript(sys.argv[2])\nc.commit()\n";
        let db = dir.join("statev2.sqlite");
        let out = std::process::Command::new("python3").args(["-I", "-c", script]).arg(&db).arg(sql).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        T3Registry::new(tmp.path().join(name))
    };
    let legacy = make(
        "legacy",
        "CREATE TABLE projection_threads (thread_id TEXT, project_id TEXT, worktree_path TEXT, created_at TEXT, archived_at TEXT, deleted_at TEXT, settled_at TEXT); INSERT INTO projection_threads VALUES ('mcp:old', 'p', NULL, '2026-11-17T00:00:00Z', NULL, NULL, NULL);",
    );
    assert_eq!(resolved(legacy.resolve("mcp:old", false).unwrap()), "mcp:old");
    let empty = make("empty", "CREATE TABLE other (x TEXT);");
    let why = unavailable(empty.resolve("mcp:old", false));
    assert!(why.contains("no thread table"), "{why}");
}

/// Gemini's joint review, MINOR 1: for attribution a thread payload that does not parse, or is not
/// an object, is `registry_unavailable`, never `{}` (which drops archivedAt, deletedAt, node); and
/// a snapshot row's folder defaults only when missing or null.
#[test]
fn a_corrupt_payload_is_unavailable_for_attribution() {
    let tmp = tempfile::tempdir().unwrap();
    let raw = |tid: &str, payload: &str| {
        format!(
            r#"conn.execute("INSERT INTO orchestration_v2_projection_threads VALUES (?,?,?,?,?,?,?,?,?,?)", ("{tid}", "p", "t", "codex", None, "2026-11-17T00:00:00Z", "2026-11-17T00:00:00Z", None, None, {payload}))"#
        )
    };
    for (name, payload, says) in [
        ("unparsed", r#""{bad""#, "does not parse"),
        ("list", r#""[1]""#, "is not an object"),
        // Codex's joint round 3: empty, blank and NULL are no payload, never `{}`.
        ("empty", r#""""#, "is empty"),
        ("blank", r#""  ""#, "is empty"),
        ("null", "None", "is empty"),
    ] {
        let db = partial_db(&tmp.path().join(name), true, &raw("mcp:bad", payload));
        let t3 = T3Registry::new(db);
        let why = unavailable(t3.resolve("mcp:bad", false));
        assert!(why.contains(&format!("thread mcp:bad's payload {says}")), "{name}: {why}");
        // The credentialed reader keeps its `{}` fallback: no folder, so no thread, never an error.
        assert!(t3.find("mcp:bad").is_none(), "{name}");
    }
    let path = tmp.path().join("threads.json");
    // A snapshot row that is not an object is a bad row, never skipped (Codex's joint round 4).
    std::fs::write(&path, serde_json::json!([7, { "id": "mcp:a", "cwd": "/w" }]).to_string()).unwrap();
    assert!(unavailable(T3Registry::new(&path).resolve("mcp:a", false)).contains("a row that is not an object"));
    for (cwd, ok) in
        [(serde_json::json!(7), false), (serde_json::Value::Null, true), (serde_json::json!(["/w"]), false)]
    {
        let rows =
            serde_json::json!([{ "id": "mcp:a", "cwd": cwd, "created_at": "2026-11-17T00:00:00Z", "run_id": "r" }]);
        std::fs::write(&path, rows.to_string()).unwrap();
        let got = T3Registry::new(&path).resolve("mcp:a", false);
        if ok {
            assert_eq!(resolved(got.unwrap()), "mcp:a");
        } else {
            assert!(unavailable(got).contains("cwd that is not a string"), "{cwd}");
        }
    }
}

/// Gemini's joint review, MINOR 2: native resolution reads only live threads' active rows, so an
/// archived or deleted thread whose provider row was pruned breaks nothing; a `nativeId` not yet
/// assigned (null) matches nothing; a malformed row of a live thread is still unavailable.
#[test]
fn native_resolution_reads_only_live_threads_and_skips_unassigned_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let live = r#"thread("mcp:a", "pt-a", {"worktreePath": "/w"}); provider("pt-a", "mcp:a", native("native-a"))"#;
    let gone = |column: &str| {
        format!(
            r#"conn.execute("INSERT INTO orchestration_v2_projection_threads VALUES (?,?,?,?,?,?,?,?,?,?)", ("mcp:gone", "p", "t", "codex", "pt-pruned", "c", "u", {}, {}, "{{}}"))"#,
            if column == "archived" { r#""2026-11-17T01:00:00Z""# } else { "None" },
            if column == "deleted" { r#""2026-11-17T01:00:00Z""# } else { "None" },
        )
    };
    for case in ["archived", "deleted"] {
        let db = partial_db(&tmp.path().join(case), true, &format!("{live}\n{}", gone(case)));
        assert_eq!(resolved(T3Registry::new(db).resolve("native-a", true).unwrap()), "mcp:a", "{case}");
    }
    let unassigned = r#"thread("mcp:b", "pt-b", {}); provider("pt-b", "mcp:b", json.dumps({"nativeThreadRef": {"driver": "codex", "nativeId": None}}))"#;
    let db = partial_db(&tmp.path().join("unassigned"), true, &format!("{live}\n{unassigned}"));
    assert_eq!(resolved(T3Registry::new(db).resolve("native-a", true).unwrap()), "mcp:a");
    let malformed = r#"thread("mcp:b", "pt-b", {}); provider("pt-b", "mcp:b", json.dumps({"nativeThreadRef": []}))"#;
    let db = partial_db(&tmp.path().join("malformed"), true, &format!("{live}\n{malformed}"));
    unavailable(T3Registry::new(db).resolve("native-a", true));
    // The snapshot: an archived row with a pruned provider row breaks nothing either.
    let path = tmp.path().join("threads.json");
    let rows = serde_json::json!([
        { "id": "mcp:a", "cwd": "/w", "active_provider_thread_id": "pt-a",
          "provider_threads": [{ "provider_thread_id": "pt-a", "native_id": "native-a" }] },
        { "id": "mcp:gone", "cwd": "/w", "archived_at": "2026-11-17T01:00:00Z",
          "active_provider_thread_id": "pt-pruned", "provider_threads": [] },
    ]);
    std::fs::write(&path, rows.to_string()).unwrap();
    assert_eq!(resolved(T3Registry::new(&path).resolve("native-a", true).unwrap()), "mcp:a");
}

/// Gemini's joint round 3: a thread archived or deleted only in its payload (`archivedAt`,
/// `deletedAt`, as the thread reader honours them) is no live thread to the native scan: its
/// native id collides with no live thread's, and its pruned provider row breaks nothing. A live
/// thread's own payload that does not read is `registry_unavailable`.
#[test]
fn a_thread_archived_in_its_payload_only_is_not_scanned() {
    let tmp = tempfile::tempdir().unwrap();
    let live = r#"thread("mcp:a", "pt-a", {"worktreePath": "/w"}); provider("pt-a", "mcp:a", native("native-n"))"#;
    for (name, key) in [("archived", "archivedAt"), ("deleted", "deletedAt")] {
        let old = format!(
            r#"thread("mcp:old", "pt-old", {{"{key}": "2026-11-17T01:00:00Z"}}); provider("pt-old", "mcp:old", native("native-n"))"#
        );
        let db = partial_db(&tmp.path().join(name), true, &format!("{live}\n{old}"));
        assert_eq!(resolved(T3Registry::new(db).resolve("native-n", true).unwrap()), "mcp:a", "{name}");
        let pruned = format!(r#"thread("mcp:old", "pt-gone", {{"{key}": "2026-11-17T01:00:00Z"}})"#);
        let db = partial_db(&tmp.path().join(format!("{name}-pruned")), true, &format!("{live}\n{pruned}"));
        assert_eq!(resolved(T3Registry::new(db).resolve("native-n", true).unwrap()), "mcp:a", "{name} pruned");
    }
    for (name, payload, says) in
        [("empty", r#""""#, "is empty"), ("null", "None", "is empty"), ("corrupt", r#""{x""#, "does not parse")]
    {
        let other = format!(
            r#"conn.execute("INSERT INTO orchestration_v2_projection_threads VALUES (?,?,?,?,?,?,?,?,?,?)", ("mcp:b", "p", "t", "codex", "pt-b", "c", "u", None, None, {payload})); provider("pt-b", "mcp:b", native("native-b"))"#
        );
        let db = partial_db(&tmp.path().join(format!("thread-{name}")), true, &format!("{live}\n{other}"));
        let why = unavailable(T3Registry::new(db).resolve("native-n", true));
        assert!(why.contains(&format!("thread mcp:b's payload {says}")), "{name}: {why}");
    }
    // The snapshot: a row archived under the payload's own key is not scanned either.
    let path = tmp.path().join("threads.json");
    let rows = serde_json::json!([
        { "id": "mcp:a", "cwd": "/w", "active_provider_thread_id": "pt-a",
          "provider_threads": [{ "provider_thread_id": "pt-a", "native_id": "native-n" }] },
        { "id": "mcp:old", "cwd": "/w", "archivedAt": "2026-11-17T01:00:00Z", "active_provider_thread_id": "pt-old",
          "provider_threads": [{ "provider_thread_id": "pt-old", "native_id": "native-n" }] },
    ]);
    std::fs::write(&path, rows.to_string()).unwrap();
    assert_eq!(resolved(T3Registry::new(&path).resolve("native-n", true).unwrap()), "mcp:a");
}

/// Codex's joint round 4: the credentialed reader reads a thread's payload by the one rule too. A
/// credential minted while the payload read must stop verifying once it is emptied or NULLed (the
/// project root would otherwise still give the thread a folder, and the payload's `archivedAt`
/// would be lost); a thread archived in its payload only is not live.
#[test]
fn a_credentialed_claim_fails_closed_on_an_unreadable_payload() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("t3");
    let work = std::fs::canonicalize(tmp.path()).unwrap().join("work");
    std::fs::create_dir_all(root.join("userdata")).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let db = root.join("userdata/statev2.sqlite");
    let sql = |statement: &str, arg: &str| {
        let script = "import sqlite3, sys\nc = sqlite3.connect(sys.argv[1])\nc.executescript(sys.argv[2].replace('@', sys.argv[3]))\nc.commit()\n";
        let out = std::process::Command::new("python3")
            .args(["-I", "-c", script])
            .arg(&db)
            .arg(statement)
            .arg(arg)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    };
    sql(
        "CREATE TABLE projection_projects (project_id TEXT, workspace_root TEXT);
         INSERT INTO projection_projects VALUES ('p', '@');
         CREATE TABLE orchestration_v2_projection_threads (thread_id TEXT, project_id TEXT, default_provider TEXT,
           created_at TEXT, updated_at TEXT, archived_at TEXT, deleted_at TEXT, payload_json TEXT);
         INSERT INTO orchestration_v2_projection_threads VALUES
           ('mcp:c', 'p', 'codex', '2026-11-17T00:00:00Z', '2026-11-17T00:00:00Z', NULL, NULL, '{}');
         CREATE TABLE orchestration_v2_projection_runs (run_id TEXT, thread_id TEXT, ordinal INTEGER, status TEXT);
         INSERT INTO orchestration_v2_projection_runs VALUES ('run-1', 'mcp:c', 1, 'running');",
        work.to_str().unwrap(),
    );
    let hosts = SurfaceRegistry::new(None, Some(T3Registry::new(&root)), None)
        .with_bridge_secret("round-4-secret")
        .with_identity_mode(IdentityMode::Strict);
    let w = work.to_string_lossy().into_owned();
    let peer = maestro_hosts::PeerCredentials { pid: std::process::id(), uid: 1000 };
    let verified = hosts.verify_caller(&CallerClaim::t3(Some("mcp:c"), Some(w.clone())).with_peer(peer)).unwrap();
    assert_eq!(verified.cwd(), w, "the folder is the project root");
    let node: maestro_hosts::NodeId = "node-1".parse().unwrap();
    let now = maestro_hosts::current_time_ms();
    let cred = hosts.issue_bridge_credential(&verified, node.clone(), now - 5_000, now + 60_000).unwrap();
    let signed = CallerClaim::t3(Some("mcp:c"), Some(w.clone())).with_credential(cred).with_node(node);
    hosts.verify_caller(&signed).unwrap();
    for (name, payload) in [("empty", "''"), ("blank", "'  '"), ("null", "NULL"), ("corrupt", "'{x'")] {
        sql(&format!("UPDATE orchestration_v2_projection_threads SET payload_json = {payload}"), "");
        let e = hosts.verify_caller(&signed).unwrap_err();
        assert_eq!(e.reason, RefusalReason::RegistryUnavailable, "{name}: {e:?}");
        assert!(e.message.contains("thread mcp:c's payload"), "{name}: {}", e.message);
    }
    // Archived in its payload only: the credentialed reader sees it, so the thread is not live.
    sql(
        "UPDATE orchestration_v2_projection_threads SET payload_json = '{\"archivedAt\": \"2026-11-17T01:00:00Z\"}'",
        "",
    );
    assert_eq!(hosts.verify_caller(&signed).unwrap_err().reason, RefusalReason::NotLive);
}

/// A `statev2.sqlite` whose thread `mcp:bad` has a payload that does not read, whose thread
/// `mcp:good` reads, and whose thread `mcp:nofolder` reads but has no folder; each has one pending
/// permission prompt, stored apart from the payload.
fn prompts_db(dir: &Path) -> PathBuf {
    partial_db(
        dir,
        true,
        r#"
conn.executescript("""CREATE TABLE orchestration_v2_projection_runtime_requests (
  runtime_request_id TEXT, thread_id TEXT, kind TEXT, status TEXT, created_at TEXT, payload_json TEXT);""")
thread("mcp:good", None, {"worktreePath": "/w"})
thread("mcp:nofolder", None, {})
conn.execute("INSERT INTO orchestration_v2_projection_threads VALUES (?,?,?,?,?,?,?,?,?,?)",
    ("mcp:bad", "p", "t", "codex", None, "2026-11-17T00:00:00Z", "2026-11-17T00:00:00Z", None, None, "{corrupt"))
for tid, cmd in [("mcp:good", "cargo test"), ("mcp:bad", "git push"), ("mcp:nofolder", "ls")]:
    conn.execute("INSERT INTO orchestration_v2_projection_runtime_requests VALUES (?,?,?,?,?,?)",
        (f"req-{tid}", tid, "approval", "pending", "2026-11-17T00:00:00Z",
         json.dumps({"toolName": "Bash", "input": {"command": cmd}})))
"#,
    )
}

/// Codex's joint round 5: the permission-prompt listing fails open. A thread whose payload does not
/// read keeps its pending prompts, which are stored apart from the payload.
#[test]
fn a_thread_whose_payload_does_not_read_keeps_its_prompts() {
    let tmp = tempfile::tempdir().unwrap();
    let hosts = SurfaceRegistry::new(None, Some(T3Registry::new(prompts_db(tmp.path()))), None);
    let needs = hosts.live_permission_needs(None, None);
    let keys: Vec<&str> = needs.iter().map(|n| n.key.as_str()).collect();
    assert!(keys.iter().any(|k| k.contains("mcp:bad")), "the unreadable thread's prompt is listed: {keys:?}");
    assert!(keys.iter().any(|k| k.contains("mcp:good")), "{keys:?}");
    assert!(keys.iter().any(|k| k.contains("mcp:nofolder")), "a prompt needs no folder: {keys:?}");
}

/// The listing says what it could not read: the unreadable thread, by id and reason; and the
/// identity readers stay fail-closed on the same thread.
#[test]
fn the_permission_listing_names_what_it_could_not_read() {
    let tmp = tempfile::tempdir().unwrap();
    let t3 = T3Registry::new(prompts_db(tmp.path()));
    let report = t3.list_live_report();
    assert_eq!(report.unreadable.len(), 1, "{:?}", report.unreadable);
    let (at, why) = &report.unreadable[0];
    assert!(at.contains("mcp:bad") && why.contains("payload does not parse"), "{at}: {why}");
    let bad = report.threads.iter().find(|t| t.id == "mcp:bad").expect("listed from what is known");
    assert_eq!((bad.cwd.as_str(), bad.pending_permissions.len()), ("", 1));
    let hosts = SurfaceRegistry::new(None, Some(t3.clone()), None);
    let live = hosts.live_permission_report(None, None);
    assert_eq!((live.needs.len(), live.unreadable.len()), (3, 1));
    // A database that does not read at all is named, never an empty healthy list.
    let broken = tmp.path().join("broken");
    std::fs::create_dir_all(broken.join("userdata")).unwrap();
    std::fs::write(broken.join("userdata/statev2.sqlite"), "not a database").unwrap();
    let report = T3Registry::new(&broken).list_live_report();
    assert!(report.threads.is_empty() && report.unreadable.len() == 1, "{report:?}");
    assert!(report.unreadable[0].1.contains("unreadable"), "{report:?}");
    // Identity is untouched: the same thread is still refused.
    assert!(unavailable(t3.resolve("mcp:bad", false)).contains("payload does not parse"));
    assert!(t3.find("mcp:bad").is_none());
}

/// One snapshot row that does not read hides no other thread's prompts, and is named.
#[test]
fn one_bad_snapshot_row_hides_no_other_prompts() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("threads.json");
    let rows = serde_json::json!([
        7,
        { "id": "mcp:cwd", "cwd": 7 },
        { "id": "mcp:good", "cwd": "/w", "created_at": "2026-11-17T00:00:00Z", "status": "running",
          "pending_permissions": [{ "name": "Bash", "input": { "command": "cargo test" } }] },
    ]);
    std::fs::write(&path, rows.to_string()).unwrap();
    let hosts = SurfaceRegistry::new(None, Some(T3Registry::new(&path)), None);
    assert_eq!(hosts.live_permission_needs(None, None).len(), 1, "mcp:good's prompt is listed");
    let live = hosts.live_permission_report(None, None);
    assert_eq!((live.needs.len(), live.unreadable.len()), (1, 2), "{:?}", live.unreadable);
    // A snapshot that does not read at all is named too, never an empty healthy list.
    std::fs::write(&path, "[{ not json").unwrap();
    let live = hosts.live_permission_report(None, None);
    assert_eq!((live.needs.len(), live.unreadable.len()), (0, 1));
}

/// BRO-3863 (folded into round 9): a pending permission record that does not decode is an unread
/// item of the listing, tagged with its surface and session, never dropped; a well-formed
/// neighbour is still listed; identity is unaffected (the session's prompts are the decoded ones).
fn unread_reasons(hosts: &SurfaceRegistry) -> (usize, Vec<String>) {
    let live = hosts.live_permission_report(None, None);
    (
        live.needs.len(),
        live.unreadable.iter().map(|u| format!("{} {}: {}", u.surface.as_str(), u.error.at, u.error.reason)).collect(),
    )
}

#[test]
fn a_paseo_permission_record_that_does_not_decode_is_unread() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("work")).unwrap();
    let agent = serde_json::json!({ "id": "agent-a", "cwd": "/w", "lastStatus": "running",
        "pendingPermissions": [{ "bad_entry_missing_name": 42 }, { "name": "Bash", "input": { "command": "ls" } }] });
    std::fs::write(tmp.path().join("work/agent-a.json"), agent.to_string()).unwrap();
    let paseo = maestro_hosts::PaseoRegistry::new(tmp.path());
    let (needs, unread) =
        unread_reasons(&SurfaceRegistry::new(Some(maestro_hosts::PaseoRegistry::new(tmp.path())), None, None));
    assert_eq!(needs, 1, "the well-formed prompt is listed");
    assert_eq!(unread.len(), 1, "{unread:?}");
    assert!(
        unread[0].starts_with("paseo ")
            && unread[0].contains("agent-a.json")
            && unread[0].contains("permission record 0 names no tool"),
        "{unread:?}"
    );
    // Identity: the agent is found as before, with its decoded prompt only.
    let found = paseo.find("agent-a").unwrap();
    assert_eq!((found.pending_permissions.len(), found.unread_permissions.len()), (1, 1));
}

#[test]
fn a_t3_snapshot_permission_record_that_does_not_decode_is_unread() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("threads.json");
    let rows = serde_json::json!([{ "id": "mcp:a", "cwd": "/w", "created_at": "2026-11-17T00:00:00Z", "status": "running",
        "pending_permissions": [7, { "name": "Bash", "input": { "command": "ls" } }] }]);
    std::fs::write(&path, rows.to_string()).unwrap();
    let t3 = T3Registry::new(&path);
    let (needs, unread) = unread_reasons(&SurfaceRegistry::new(None, Some(t3.clone()), None));
    assert_eq!(needs, 1);
    assert_eq!(unread.len(), 1, "{unread:?}");
    assert!(
        unread[0].starts_with("t3 t3 thread mcp:a") && unread[0].contains("permission record 0 is not an object"),
        "{unread:?}"
    );
    let v = t3.verify_attributed("mcp:a", None).unwrap();
    assert_eq!(v.pending_permissions().len(), 1, "identity carries the decoded prompt only");
}

#[test]
fn a_t3_runtime_request_that_does_not_decode_is_unread() {
    let tmp = tempfile::tempdir().unwrap();
    let db = partial_db(
        tmp.path(),
        true,
        r#"
conn.executescript("""CREATE TABLE orchestration_v2_projection_runtime_requests (
  runtime_request_id TEXT, thread_id TEXT, kind TEXT, status TEXT, created_at TEXT, payload_json TEXT);""")
thread("mcp:a", None, {"worktreePath": "/w"})
for rid, payload in [("req-bad", "{bad"), ("req-null", None), ("req-list", "[1]"), ("req-tool", json.dumps({"toolName": 7})), ("req-blank", json.dumps({"toolName": "   "})),
                     ("req-good", json.dumps({"toolName": "Bash", "input": {"command": "ls"}}))]:
    conn.execute("INSERT INTO orchestration_v2_projection_runtime_requests VALUES (?,?,?,?,?,?)",
        (rid, "mcp:a", "approval", "pending", "2026-11-17T00:00:00Z", payload))
"#,
    );
    let t3 = T3Registry::new(db);
    let (needs, unread) = unread_reasons(&SurfaceRegistry::new(None, Some(t3.clone()), None));
    assert_eq!(needs, 1, "req-good is listed");
    assert_eq!(unread.len(), 5, "{unread:?}");
    for (rid, says) in [
        ("req-bad", "does not parse"),
        ("req-null", "is empty"),
        ("req-list", "is not an object"),
        ("req-tool", "has a tool name that is not a non-blank string"),
        ("req-blank", "has a tool name that is not a non-blank string"),
    ] {
        assert!(
            unread.iter().any(|u| u.contains(&format!("permission request {rid} of thread mcp:a {says}"))),
            "{rid}: {unread:?}"
        );
    }
    let v = t3.verify_attributed("mcp:a", None).unwrap();
    assert_eq!(v.pending_permissions().len(), 1, "identity carries the decoded prompt only");
}

/// BRO-3863 round 11: one rule, in Rust, for every surface. Each payload is put, beside a
/// well-formed neighbour, through a T3 runtime request (SQLite, kind `approval`), a Paseo agent
/// file and a T3 snapshot row; each surface gives the expected answer, the neighbour is listed, and
/// identity still finds the session. Non-JSON text exists only as a runtime request's payload.
#[derive(Clone, Copy, Debug)]
enum Outcome {
    Prompt(&'static str),
    Unread(&'static str),
}

/// A prompt's title, or why the record is unread.
fn outcome_of(hosts: &SurfaceRegistry) -> Result<String, String> {
    let live = hosts.live_permission_report(None, None);
    let titles: Vec<&str> = live.needs.iter().map(|n| n.title.as_str()).collect();
    assert!(titles.contains(&"Neighbour"), "the well-formed neighbour is listed: {titles:?}");
    match (live.needs.len(), live.unreadable.len()) {
        (2, 0) => match titles.iter().find(|t| **t != "Neighbour") {
            Some(t) => Ok(t.to_string()),
            None => panic!("{titles:?}"),
        },
        (1, 1) => Err(live.unreadable[0].error.reason.clone()),
        other => panic!("{other:?}: {titles:?} {:?}", live.unreadable),
    }
}

fn runtime_outcome(payload: Option<&str>) -> Result<String, String> {
    let tmp = tempfile::tempdir().unwrap();
    let payload = match payload {
        Some(text) => serde_json::to_string(text).unwrap(),
        None => "None".to_string(),
    };
    let db = partial_db(
        tmp.path(),
        true,
        &format!(
            r#"
conn.executescript("""CREATE TABLE orchestration_v2_projection_runtime_requests (
  runtime_request_id TEXT, thread_id TEXT, kind TEXT, status TEXT, created_at TEXT, payload_json TEXT);""")
thread("mcp:a", None, {{"worktreePath": "/w"}})
for rid, payload in [("req-case", {payload}), ("req-neighbour", json.dumps({{"toolName": "Neighbour"}}))]:
    conn.execute("INSERT INTO orchestration_v2_projection_runtime_requests VALUES (?,?,?,?,?,?)",
        (rid, "mcp:a", "approval", "pending", "2026-11-17T00:00:00Z", payload))
"#
        ),
    );
    let t3 = T3Registry::new(db);
    let outcome = outcome_of(&SurfaceRegistry::new(None, Some(t3.clone()), None));
    assert!(t3.verify_attributed("mcp:a", None).is_ok(), "identity is unaffected");
    outcome
}

fn paseo_outcome(record: &serde_json::Value) -> Result<String, String> {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("work")).unwrap();
    let agent = serde_json::json!({ "id": "agent-a", "cwd": "/w", "lastStatus": "running",
        "pendingPermissions": [record, { "name": "Neighbour" }] });
    std::fs::write(tmp.path().join("work/agent-a.json"), agent.to_string()).unwrap();
    let outcome = outcome_of(&SurfaceRegistry::new(Some(maestro_hosts::PaseoRegistry::new(tmp.path())), None, None));
    assert!(maestro_hosts::PaseoRegistry::new(tmp.path()).find("agent-a").is_some(), "identity is unaffected");
    outcome
}

fn snapshot_outcome(record: &serde_json::Value) -> Result<String, String> {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("threads.json");
    let rows = serde_json::json!([{ "id": "mcp:a", "cwd": "/w", "created_at": "2026-11-17T00:00:00Z", "status": "running",
        "pending_permissions": [record, { "toolName": "Neighbour" }] }]);
    std::fs::write(&path, rows.to_string()).unwrap();
    let t3 = T3Registry::new(&path);
    let outcome = outcome_of(&SurfaceRegistry::new(None, Some(t3.clone()), None));
    assert!(t3.verify_attributed("mcp:a", None).is_ok(), "identity is unaffected");
    outcome
}

#[test]
fn one_permission_rule_answers_alike_on_every_surface() {
    use Outcome::{Prompt, Unread};
    const TOOL: &str = "has a tool name that is not a non-blank string (`toolName`)";
    const NAME: &str = "has a tool name that is not a non-blank string (`name`)";
    // (payload, the answer of a record surface (Paseo, T3 snapshot), a runtime request's answer).
    let cases: [(Option<&str>, Option<Outcome>, Outcome); 20] = [
        (Some(r#"{"toolName": ""}"#), Some(Unread(TOOL)), Unread(TOOL)),
        (Some(r#"{"toolName": 0}"#), Some(Unread(TOOL)), Unread(TOOL)),
        (Some(r#"{"toolName": false}"#), Some(Unread(TOOL)), Unread(TOOL)),
        (Some(r#"{"toolName": null}"#), Some(Unread(TOOL)), Unread(TOOL)),
        (Some(r#"{"toolName": []}"#), Some(Unread(TOOL)), Unread(TOOL)),
        (Some(r#"{"toolName": 7}"#), Some(Unread(TOOL)), Unread(TOOL)),
        (Some(r#"{"toolName": "   "}"#), Some(Unread(TOOL)), Unread(TOOL)),
        (Some(r#"{"name": ""}"#), Some(Unread(NAME)), Unread(NAME)),
        (Some(r#"{"name": null}"#), Some(Unread(NAME)), Unread(NAME)),
        // The first present key governs: an invalid `toolName` is not rescued by a valid `name`.
        (Some(r#"{"toolName": 7, "name": "Deploy"}"#), Some(Unread(TOOL)), Unread(TOOL)),
        (Some(r#"{"toolName": "Deploy", "name": "Other"}"#), Some(Prompt("Deploy")), Prompt("Deploy")),
        (Some(r#"{"toolName": "Deploy"}"#), Some(Prompt("Deploy")), Prompt("Deploy")),
        (Some(r#"{"name": "Deploy"}"#), Some(Prompt("Deploy")), Prompt("Deploy")),
        // Optional fields of a wrong type are ignored, never unread.
        (
            Some(r#"{"toolName": "Deploy", "id": [1], "title": 7, "description": false, "input": "x"}"#),
            Some(Prompt("Deploy")),
            Prompt("Deploy"),
        ),
        (Some(r#"{"toolName": "Deploy", "title": "Ship it"}"#), Some(Prompt("Ship it")), Prompt("Ship it")),
        // Neither key: a runtime request is a prompt named by its kind; a record has no kind.
        (Some("{}"), Some(Unread("names no tool")), Prompt("approval")),
        (Some("[1]"), Some(Unread("is not an object")), Unread("is not an object")),
        (None, Some(Unread("is not an object")), Unread("is empty")),
        (Some("   "), None, Unread("is empty")),
        (Some("{bad"), None, Unread("does not parse")),
    ];
    let same = |got: &Result<String, String>, want: Outcome| match (got, want) {
        (Ok(g), Prompt(w)) => g == w,
        (Err(g), Unread(w)) => g.ends_with(w),
        _ => false,
    };
    for (payload, records, runtime) in cases {
        let got = runtime_outcome(payload);
        assert!(same(&got, runtime), "runtime {payload:?}: {got:?}, want {runtime:?}");
        if let Some(want) = records {
            let record: serde_json::Value =
                payload.map_or(serde_json::Value::Null, |p| serde_json::from_str(p).unwrap());
            for (surface, got) in [("paseo", paseo_outcome(&record)), ("snapshot", snapshot_outcome(&record))] {
                assert!(same(&got, want), "{surface} {payload:?}: {got:?}, want {want:?}");
            }
        }
    }
}

/// BRO-3936: a `statev2.sqlite` whose thread `mcp:q` waits on six AskUserQuestion cards (one
/// readable with two questions, one with no card item, one whose card holds no questions, one
/// with a 200-character question, two whose questions hold nothing readable) and holds one
/// answered card and one permission prompt; and whose thread `mcp:qbad`, with a payload that does
/// not read, waits on one readable card (P20 r1).
fn questions_db(dir: &Path) -> PathBuf {
    partial_db(
        dir,
        true,
        r#"
conn.executescript("""CREATE TABLE orchestration_v2_projection_runtime_requests (
  runtime_request_id TEXT, thread_id TEXT, kind TEXT, status TEXT, created_at TEXT, payload_json TEXT);
CREATE TABLE orchestration_v2_projection_turn_items (
  turn_item_id TEXT, thread_id TEXT, type TEXT, payload_json TEXT);""")
thread("mcp:q", None, {"worktreePath": "/w"})
def req(rid, kind, status, at, payload=None):
    conn.execute("INSERT INTO orchestration_v2_projection_runtime_requests VALUES (?,?,?,?,?,?)",
        (rid, "mcp:q", kind, status, at, json.dumps(payload or {"id": rid, "kind": kind})))
def card(rid, questions):
    conn.execute("INSERT INTO orchestration_v2_projection_turn_items VALUES (?,?,?,?)",
        ("item-" + rid, "mcp:q", "user_input_request", json.dumps({"requestId": rid, "questions": questions})))
req("q-wait", "user_input", "pending", "2026-11-19T10:00:00Z")
card("q-wait", [{"header": "Deploy", "question": "Ship tonight?"}, {"header": "Font", "question": "Which font?"}])
req("q-done", "user_input", "resolved", "2026-11-19T09:00:00Z")
card("q-done", [{"header": "Old", "question": "Answered already?"}])
req("q-nocard", "user_input", "pending", "2026-11-19T10:01:00Z")
req("q-empty", "user_input", "pending", "2026-11-19T10:02:00Z")
card("q-empty", [])
req("q-long", "user_input", "pending", "2026-11-19T10:03:00Z")
card("q-long", [{"question": "x" * 200}])
req("perm", "approval", "pending", "2026-11-19T10:04:00Z", {"toolName": "Bash", "input": {"command": "ls"}})
req("q-noq", "user_input", "pending", "2026-11-19T10:05:00Z")
card("q-noq", [7, {}])
req("q-blank", "user_input", "pending", "2026-11-19T10:06:00Z")
card("q-blank", [{"header": "Scope", "question": "   "}])
conn.execute("INSERT INTO orchestration_v2_projection_threads VALUES (?,?,?,?,?,?,?,?,?,?)",
    ("mcp:qbad", "p", "t", "codex", None, "2026-11-17T00:00:00Z", "2026-11-17T00:00:00Z", None, None, "{corrupt"))
conn.execute("INSERT INTO orchestration_v2_projection_runtime_requests VALUES (?,?,?,?,?,?)",
    ("q-bad-thread", "mcp:qbad", "user_input", "pending", "2026-11-19T10:07:00Z", "{}"))
conn.execute("INSERT INTO orchestration_v2_projection_turn_items VALUES (?,?,?,?)",
    ("item-q-bad-thread", "mcp:qbad", "user_input_request",
     json.dumps({"requestId": "q-bad-thread", "questions": [{"header": "Kept", "question": "Still listed?"}]})))
"#,
    )
}

/// BRO-3936: every waiting AskUserQuestion card is a need on the owner, an ask whose source is the
/// thread, listed beside the thread's permission prompts; an answered card is not.
#[test]
fn a_waiting_question_card_is_a_need() {
    let tmp = tempfile::tempdir().unwrap();
    let hosts = SurfaceRegistry::new(None, Some(T3Registry::new(questions_db(tmp.path()))), None);
    let live = hosts.live_permission_report(None, None);
    let questions: Vec<_> = live.needs.iter().filter(|n| n.key.starts_with("question:")).collect();
    let keys: Vec<&str> = questions.iter().map(|n| n.key.as_str()).collect();
    assert_eq!(questions.len(), 7, "{keys:?}");
    assert!(!keys.iter().any(|k| k.contains("q-done")), "an answered card is not a need: {keys:?}");
    assert_eq!(live.needs.len(), 8, "the permission prompt still lists: {:?}", live.needs);
    // A thread whose payload does not read keeps its waiting cards, as it keeps its prompts (P20 r1).
    let kept = questions.iter().find(|n| n.key.ends_with("#q-bad-thread")).expect("listed from what is known");
    assert_eq!(kept.title, "Question: Kept: Still listed?");
    let wait = questions.iter().find(|n| n.key.ends_with("#q-wait")).unwrap();
    assert_eq!((wait.kind, wait.source), (NeedKind::Ask, NeedSource::Questions));
    assert_eq!(wait.title, "Question: Deploy · Font: Ship tonight? (2 questions)");
    assert_eq!(wait.since.as_deref(), Some("2026-11-19T10:00:00Z"), "when the card was asked");
    assert!(wait.ask.is_none(), "not a ledger ask");
    assert_eq!(wait.unblocks, vec![wait.key.trim_start_matches("question:").split('#').next().unwrap().to_string()]);
    let long = questions.iter().find(|n| n.key.ends_with("#q-long")).unwrap();
    assert_eq!(long.title, format!("Question: question: {}…", "x".repeat(160)), "cut at 160 characters");
}

/// BRO-3936: a waiting card that does not read is still a need, titled so, and the listing names
/// it unread with why; a request with no id is unread too. Nothing is dropped.
#[test]
fn a_question_card_that_does_not_read_is_listed_and_named() {
    let tmp = tempfile::tempdir().unwrap();
    let hosts = SurfaceRegistry::new(None, Some(T3Registry::new(questions_db(tmp.path()))), None);
    let live = hosts.live_permission_report(None, None);
    for id in ["q-nocard", "q-empty", "q-noq", "q-blank"] {
        let n = live.needs.iter().find(|n| n.key.ends_with(&format!("#{id}"))).expect(id);
        assert_eq!(n.title, "Question waiting (its card did not read)", "{id}");
    }
    let why: Vec<String> = live.errors().iter().map(|e| e.reason.clone()).collect();
    // Four cards, and mcp:qbad's payload.
    assert_eq!(why.len(), 5, "{why:?}");
    assert!(why.iter().any(|w| w.contains("q-nocard") && w.contains("has no card")), "{why:?}");
    assert!(why.iter().any(|w| w.contains("q-empty") && w.contains("no questions")), "{why:?}");
    for id in ["q-noq", "q-blank"] {
        assert!(why.iter().any(|w| w.contains(id) && w.contains("no readable question")), "{id}: {why:?}");
    }
    // A JSON snapshot carries the same raw records: a card that does not parse, and one with no id.
    let path = tmp.path().join("threads.json");
    // P20 r1: records that do not decode (a number, a non-string id) are unread items; they never
    // fail the thread's row, so its permission prompt still lists and its identity still resolves.
    let rows = serde_json::json!([{ "id": "mcp:j", "cwd": "/w", "created_at": "2026-11-19T00:00:00Z", "status": "running",
        "pending_permissions": [{ "name": "Bash", "input": { "command": "ls" } }],
        "pending_questions": [
            { "id": "j1", "created_at": "2026-11-19T11:00:00Z",
              "item_payload_json": "{\"questions\": [{\"header\": \"H\", \"question\": \"Q?\"}]}" },
            { "id": "j2", "item_payload_json": "{not json" },
            { "item_payload_json": "{}" },
            7,
            { "id": 9 } ] }]);
    std::fs::write(&path, rows.to_string()).unwrap();
    let t3 = T3Registry::new(&path);
    let live = SurfaceRegistry::new(None, Some(t3.clone()), None).live_permission_report(None, None);
    let titles: Vec<&str> = live.needs.iter().map(|n| n.title.as_str()).collect();
    assert_eq!(titles.len(), 3, "{titles:?}");
    assert!(
        titles.contains(&"Question: H: Q?") && titles.contains(&"Question waiting (its card did not read)"),
        "{titles:?}"
    );
    let why: Vec<String> = live.errors().iter().map(|e| e.reason.clone()).collect();
    assert_eq!(why.len(), 4, "{why:?}");
    assert!(why.iter().any(|w| w.contains("j2") && w.contains("does not parse")), "{why:?}");
    assert!(why.iter().any(|w| w.contains("has no id")), "{why:?}");
    assert_eq!(why.iter().filter(|w| w.contains("does not decode")).count(), 2, "{why:?}");
    assert!(t3.find("mcp:j").is_some(), "identity is untouched by an undecodable question record");
}
