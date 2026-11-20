//! BRO-3828: a performer session an owner names is attested by the registry, as the owner's own
//! child on this host, before the flow kernel compares its provider. Nothing it says about itself
//! is read.

use maestro_core::session::{Harness, RefusalReason, SessionLiveness, SessionRef, Surface, VerifiedSession};
use maestro_hosts::{DirectRegistry, DirectSession, SurfaceRegistry, T3Registry};
use serde_json::json;

fn owner_t3() -> VerifiedSession {
    VerifiedSession::new("t3:env-mac/owner".parse().unwrap(), "/tmp", Some(Harness::ClaudeCode), None, vec![])
}

fn t3(rows: serde_json::Value) -> (tempfile::TempDir, SurfaceRegistry) {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("threads.json");
    std::fs::write(&file, rows.to_string()).unwrap();
    (tmp, SurfaceRegistry::new(None, Some(T3Registry::new(&file)), None))
}

fn row(id: &str, parent: Option<&str>, extra: serde_json::Value) -> serde_json::Value {
    let mut r = json!({
        "id": id, "cwd": "/tmp", "environment_id": "env-mac", "provider": "gemini-vertex",
        "created_at": "2026-11-19T00:00:00Z", "status": "running", "parent_id": parent,
        "providers": ["gemini-vertex"],
    });
    r.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    r
}

fn named(text: &str) -> SessionRef {
    text.parse().unwrap()
}

fn refused(r: Result<VerifiedSession, maestro_core::session::IdentityRefusal>) -> RefusalReason {
    r.map(|s| panic!("attested {s:?}")).unwrap_err().reason
}

#[test]
fn the_owners_child_thread_is_attested_with_its_own_harness_even_settled() {
    let (_tmp, reg) = t3(json!([
        row("child", Some("owner"), json!({})),
        row("done", Some("owner"), json!({"settled_at": "2026-11-19T01:00:00Z", "status": "idle"})),
    ]));
    let me = owner_t3();
    let child = reg.performer(&me, &named("t3:env-mac/child")).unwrap();
    assert_eq!(child.harness(), Some(&Harness::Gemini));
    assert_eq!(child.session().id, "child");
    // A local alias names the same thread; a settled child still attests its work.
    assert!(reg.performer(&me, &named("t3:-/child")).is_ok());
    assert!(reg.performer(&me, &named("t3:-/done")).is_ok());
}

#[test]
fn a_thread_that_is_not_the_owners_child_is_refused() {
    let (_tmp, reg) = t3(json!([row("stranger", Some("someone-else"), json!({})), row("orphan", None, json!({}))]));
    let me = owner_t3();
    assert_eq!(refused(reg.performer(&me, &named("t3:env-mac/stranger"))), RefusalReason::PerformerUnverified);
    assert_eq!(refused(reg.performer(&me, &named("t3:env-mac/orphan"))), RefusalReason::PerformerUnverified);
}

#[test]
fn a_closed_missing_or_remote_child_is_refused() {
    let (_tmp, reg) = t3(json!([
        row("gone", Some("owner"), json!({"deleted_at": "2026-11-19T02:00:00Z"})),
        row("child", Some("owner"), json!({}))
    ]));
    let me = owner_t3();
    assert_eq!(refused(reg.performer(&me, &named("t3:env-mac/gone"))), RefusalReason::PerformerUnverified);
    assert_eq!(refused(reg.performer(&me, &named("t3:env-mac/nobody"))), RefusalReason::PerformerUnverified);
    // Named on another host: the id is found here, but it is not that host's thread.
    assert_eq!(refused(reg.performer(&me, &named("t3:env-vps/child"))), RefusalReason::PerformerUnverified);
}

#[test]
fn a_performer_on_another_surface_than_the_owners_is_refused() {
    let (_tmp, reg) = t3(json!([row("child", Some("owner"), json!({}))]));
    assert_eq!(refused(reg.performer(&owner_t3(), &named("direct:-/child"))), RefusalReason::PerformerUnverified);
}

fn direct(id: &str, parent: Option<&str>, liveness: SessionLiveness) -> DirectSession {
    DirectSession {
        id: id.into(),
        cwd: "/tmp".into(),
        token: "t".into(),
        liveness,
        harness: Some(Harness::Codex),
        parent_id: parent.map(str::to_string),
        pending_permissions: vec![],
    }
}

#[test]
fn a_direct_child_is_attested_only_with_the_owner_as_its_parent() {
    let reg = DirectRegistry::new();
    reg.register(direct("kid", Some("owner"), SessionLiveness::Live));
    reg.register(direct("other", Some("not-owner"), SessionLiveness::Live));
    reg.register(direct("none", None, SessionLiveness::Live));
    reg.register(direct("shut", Some("owner"), SessionLiveness::Closed));
    let surfaces = SurfaceRegistry::new(None, None, Some(reg));
    let me = VerifiedSession::new(
        SessionRef { surface: Surface::Direct, environment: None, id: "owner".into() },
        "/tmp",
        Some(Harness::ClaudeCode),
        None,
        vec![],
    );
    assert_eq!(surfaces.performer(&me, &named("direct:-/kid")).unwrap().harness(), Some(&Harness::Codex));
    for id in ["other", "none", "shut", "absent"] {
        assert_eq!(
            refused(surfaces.performer(&me, &named(&format!("direct:-/{id}")))),
            RefusalReason::PerformerUnverified,
            "{id}"
        );
    }
}

#[test]
fn a_child_attests_only_one_provider_it_ran_never_its_default() {
    // P20 round 1 (BLOCKER): the default provider is not who did the work.
    let (_tmp, reg) = t3(json!([
        row("switched", Some("owner"), json!({"provider": "codex", "providers": ["codex", "claudeAgent"]})),
        row("unrecorded", Some("owner"), json!({"provider": "codex", "providers": null})),
        row("none", Some("owner"), json!({"provider": "codex", "providers": []})),
        row("blank", Some("owner"), json!({"provider": "codex", "providers": [""]})),
        row("moved", Some("owner"), json!({"provider": "codex", "providers": ["claudeAgent"]})),
    ]));
    let me = owner_t3();
    for id in ["switched", "unrecorded", "none", "blank"] {
        let r = reg.performer(&me, &named(&format!("t3:env-mac/{id}")));
        assert_eq!(refused(r), RefusalReason::PerformerUnverified, "{id}");
    }
    let moved = reg.performer(&me, &named("t3:env-mac/moved")).unwrap();
    assert_eq!(moved.harness(), Some(&Harness::ClaudeCode), "the provider it ran, not its default");
}

/// A `statev2.sqlite` as T3 writes it: a child whose default provider is codex while its provider
/// rows say otherwise, read through the registry's own reader.
#[test]
fn the_sqlite_reader_reports_every_provider_a_child_ran() {
    let tmp = tempfile::tempdir().unwrap();
    let userdata = tmp.path().join("userdata");
    std::fs::create_dir_all(&userdata).unwrap();
    std::fs::write(userdata.join("environment-id"), "env-mac\n").unwrap();
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
""")
def thread(tid, active):
    payload = {"worktreePath": "/work", "lineage": {"parentThreadId": "owner"}}
    conn.execute("INSERT INTO orchestration_v2_projection_threads VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (tid, "p", "t", "codex", "full", "default", active, "2026-11-19T00:00:00Z", "2026-11-19T00:00:00Z",
         None, None, json.dumps(payload), "codex"))
def provider(ptid, tid, name, status):
    conn.execute("INSERT INTO orchestration_v2_projection_provider_threads VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
        (ptid, tid, None, name, None, status, 1, 1, "2026-11-19T00:00:00Z", "{}", name, name))
thread("moved", "pt-m"); provider("pt-m", "moved", "claudeAgent", "active")
thread("nullrow", "pt-n"); provider("pt-n", "nullrow", None, "active")
thread("switched", "pt-s2"); provider("pt-s1", "switched", "codex", "retired"); provider("pt-s2", "switched", "claudeAgent", "active")
conn.commit()
"#;
    let db = userdata.join("statev2.sqlite");
    let out = std::process::Command::new("python3").arg("-I").arg("-c").arg(script).arg(&db).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let reg = SurfaceRegistry::new(None, Some(T3Registry::new(tmp.path())), None);
    let me = owner_t3();
    let moved = reg.performer(&me, &named("t3:env-mac/moved")).unwrap();
    assert_eq!(moved.harness(), Some(&Harness::ClaudeCode));
    assert_eq!(refused(reg.performer(&me, &named("t3:env-mac/switched"))), RefusalReason::PerformerUnverified);
    // P20 round 2: a NULL provider reads (the registry is not unavailable) and confirms nobody.
    assert_eq!(refused(reg.performer(&me, &named("t3:env-mac/nullrow"))), RefusalReason::PerformerUnverified);
}

#[test]
fn a_direct_child_the_registry_file_no_longer_holds_is_not_attested() {
    // CodeRabbit (maestro#55): the file is the registry; the rows loaded from it earlier are not.
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("direct.json");
    let row = |id: &str| json!({"id": id, "cwd": "/tmp", "token": "t", "liveness": "live", "harness": "codex", "parent_id": "owner"});
    std::fs::write(&file, json!([row("kid"), row("gone")]).to_string()).unwrap();
    let reg = DirectRegistry::from_file(&file);
    std::fs::write(&file, json!([row("kid")]).to_string()).unwrap();
    let surfaces = SurfaceRegistry::new(None, None, Some(reg));
    let me = VerifiedSession::new(
        SessionRef { surface: Surface::Direct, environment: None, id: "owner".into() },
        "/tmp",
        Some(Harness::ClaudeCode),
        None,
        vec![],
    );
    assert!(surfaces.performer(&me, &named("direct:-/kid")).is_ok());
    assert_eq!(refused(surfaces.performer(&me, &named("direct:-/gone"))), RefusalReason::PerformerUnverified);
}

#[test]
fn a_child_thread_bound_to_another_node_is_not_attested() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("threads.json");
    let rows = json!([
        row("here", Some("owner"), json!({"node": "mac-1"})),
        row("there", Some("owner"), json!({"node": "nuc-1"}))
    ]);
    std::fs::write(&file, rows.to_string()).unwrap();
    let t3 = T3Registry::new(&file).with_expected_node("mac-1".parse().unwrap());
    let reg = SurfaceRegistry::new(None, Some(t3), None);
    let me = owner_t3();
    assert!(reg.performer(&me, &named("t3:env-mac/here")).is_ok());
    assert_eq!(refused(reg.performer(&me, &named("t3:env-mac/there"))), RefusalReason::PerformerUnverified);
}

#[test]
fn a_verified_t3_session_runs_the_provider_its_rows_name_not_its_default() {
    // CodeRabbit (maestro#55, round 4): an owner completing a routed node itself was judged by its
    // default provider. A verified T3 session's harness is the one its provider rows name.
    let thread = |providers: serde_json::Value| -> maestro_hosts::T3Thread {
        serde_json::from_value(json!({"id": "t", "cwd": "/tmp", "provider": "codex", "providers": providers})).unwrap()
    };
    assert_eq!(thread(json!(["claudeAgent"])).session_harness(), Some(Harness::ClaudeCode));
    assert_eq!(thread(json!(["codex", "claudeAgent"])).session_harness(), None, "switched");
    assert_eq!(thread(json!([""])).session_harness(), None, "a NULL row");
    assert_eq!(thread(json!([])).session_harness(), None, "no rows yet");
    assert_eq!(thread(json!(null)).session_harness(), Some(Harness::Codex), "no history reported: the default");
}

#[test]
fn a_child_on_another_node_than_its_owner_is_not_attested_without_an_expected_node() {
    // P20 round 4 (MAJOR): with no expected node configured, the child's node is still the owner's.
    let (_tmp, reg) = t3(json!([
        row("here", Some("owner"), json!({"node": "mac-1"})),
        row("there", Some("owner"), json!({"node": "nuc-1"}))
    ]));
    let me = owner_t3().with_node_and_generation(Some("mac-1".parse().unwrap()), 0);
    assert!(reg.performer(&me, &named("t3:env-mac/here")).is_ok());
    assert_eq!(refused(reg.performer(&me, &named("t3:env-mac/there"))), RefusalReason::PerformerUnverified);
}

#[test]
fn with_no_node_known_for_this_host_a_child_that_names_one_is_not_attested() {
    // CodeRabbit (round 6): neither the owner's node nor an expected node is known, so a child
    // that names a node names one this host cannot confirm; a child that names none claims no
    // other host.
    let (_tmp, reg) =
        t3(json!([row("named", Some("owner"), json!({"node": "nuc-1"})), row("plain", Some("owner"), json!({}))]));
    assert_eq!(refused(reg.performer(&owner_t3(), &named("t3:env-mac/named"))), RefusalReason::PerformerUnverified);
    assert!(reg.performer(&owner_t3(), &named("t3:env-mac/plain")).is_ok());
}

/// BRO-4018 (found by the kernel-ready@1 dogfood): the provider rows name a T3 provider *instance*,
/// which the owner names (`codex_team`), so the instance's driver, as T3's settings record it,
/// decides the harness. An `acpRegistry` instance is attested only when it is one whose agent is
/// known; an unknown driver attests none; without settings the instance id is all there is.
#[test]
fn a_t3_instance_runs_the_harness_its_driver_names() {
    let thread = |instance: &str, drivers: &[(&str, &str)]| -> maestro_hosts::T3Thread {
        let mut t: maestro_hosts::T3Thread =
            serde_json::from_value(json!({"id": "t", "cwd": "/tmp", "provider": instance, "providers": [instance]}))
                .unwrap();
        t.drivers = drivers.iter().map(|(i, d)| (i.to_string(), d.to_string())).collect();
        t
    };
    let mac = [
        ("codex_team", "codex"),
        ("codex_carlos_personal_20261029", "codex"),
        ("claudeAgent", "claudeAgent"),
        ("antigravity-personal", "antigravity"),
        ("gemini-vertex", "acpRegistry"),
        ("holon", "acpRegistry"),
        ("muse", "muse"),
    ];
    let harness = |instance: &str| thread(instance, &mac).session_harness();
    assert_eq!(harness("codex_team"), Some(Harness::Codex));
    assert_eq!(harness("codex_carlos_personal_20261029"), Some(Harness::Codex));
    assert_eq!(harness("claudeAgent"), Some(Harness::ClaudeCode));
    assert_eq!(harness("antigravity-personal"), Some(Harness::Agy));
    assert_eq!(harness("gemini-vertex"), Some(Harness::Gemini));
    // holon is an acpRegistry instance that runs Claude models: its agent is not known here.
    assert_eq!(harness("holon"), Some(Harness::Other("acpRegistry:holon".into())));
    assert_eq!(harness("muse"), Some(Harness::Other("muse:muse".into())));
    // The driver wins over the id: an instance named like a provider runs what its driver says.
    assert_eq!(
        thread("codex", &[("codex", "acpRegistry")]).session_harness(),
        Some(Harness::Other("acpRegistry:codex".into()))
    );
    assert_eq!(thread("gemini-vertex", &[("gemini-vertex", "codex")]).session_harness(), Some(Harness::Codex));
    // No settings read: the instance id alone, so a custom name attests nothing.
    assert_eq!(thread("codex_team", &[]).session_harness(), Some(Harness::Other("codex_team".into())));
    assert_eq!(thread("codex", &[]).session_harness(), Some(Harness::Codex));
}

/// The registry reads the drivers from `settings.json` beside it, so a child thread on a custom
/// Codex instance is attested as Codex; without that file it is not.
#[test]
fn a_child_on_a_custom_codex_instance_is_attested_from_t3_settings() {
    let child = row("kid", Some("owner"), json!({"provider": "codex_team", "providers": ["codex_team"]}));
    let (tmp, reg) = t3(json!([child]));
    let me = owner_t3();
    let attested = reg.performer(&me, &named("t3:env-mac/kid")).unwrap();
    assert_eq!(attested.harness(), Some(&Harness::Other("codex_team".into())), "no settings: the id alone");
    let settings = json!({"providerInstances": {"codex_team": {"driver": "codex", "enabled": true}}});
    std::fs::write(tmp.path().join("settings.json"), settings.to_string()).unwrap();
    let attested = reg.performer(&me, &named("t3:env-mac/kid")).unwrap();
    assert_eq!(attested.harness(), Some(&Harness::Codex));
    // Settings that do not decode read as none: nothing more is attested.
    std::fs::write(tmp.path().join("settings.json"), "{not json").unwrap();
    let attested = reg.performer(&me, &named("t3:env-mac/kid")).unwrap();
    assert_eq!(attested.harness(), Some(&Harness::Other("codex_team".into())));
}
