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
