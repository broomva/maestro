//! Attributed session identity at the host level (BRO-3846): T3 thread ids, the registry root,
//! the identity mode, the precedence of what thread variables resolve to, resolution through
//! `statev2.sqlite`'s active provider row, the T3 reader's run status, and the audit facts.

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
    std::fs::create_dir_all(home.join(".t3")).unwrap();
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
# A provider row that names an active id but belongs to another thread never attributes it.
thread("mcp:borrower", "pt-lent"); provider("pt-lent", "mcp:switched", "native-lent")
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
    assert!(unresolved(t3.resolve("native-lent", true).unwrap()).contains("no T3 thread"));
    // Liveness for attribution: archived_at and deleted_at, never settled_at.
    assert_eq!(resolved(t3.resolve("native-settled", true).unwrap()), "mcp:settled");
    assert_eq!(resolved(t3.resolve("mcp:settled", false).unwrap()), "mcp:settled");
    assert!(unresolved(t3.resolve("native-arch", true).unwrap()).contains("archived"));
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
