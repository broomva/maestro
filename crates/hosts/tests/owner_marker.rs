use maestro_hosts::{CallerClaim, DirectRegistry, DirectSession, Harness, PaseoRegistry, SessionLiveness, T3Registry};
use serde_json::json;
use std::fs;
use std::path::Path;
use std::process::Command;

fn write_t3_thread(path: &Path, cwd: &Path, provider: &str, run_id: &str) {
    fs::write(
        path,
        serde_json::to_string_pretty(&json!([{
            "id": "mcp:owner-thread",
            "cwd": cwd,
            "environment_id": "env-owner",
            "provider": provider,
            "created_at": "2026-11-07T00:00:00Z",
            "run_id": run_id,
            "status": "running"
        }]))
        .unwrap(),
    )
    .unwrap();
}

fn write_paseo_agent(path: &Path, id: &str, cwd: &Path, handle_id: Option<&str>) {
    let persistence = handle_id.map_or_else(|| json!({}), |handle_id| json!({ "sessionId": handle_id }));
    fs::write(
        path,
        serde_json::to_string_pretty(&json!({
            "id": id,
            "cwd": cwd,
            "provider": "claude",
            "createdAt": "2026-11-07T00:00:00Z",
            "lastStatus": "idle",
            "persistence": persistence
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn t3_owner_marker_survives_turns_but_not_legacy_rows_without_creation_time() {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let cwd = root.join("work");
    fs::create_dir_all(&cwd).unwrap();
    let threads = root.join("threads.json");
    write_t3_thread(&threads, &cwd, "codex", "run-v1");

    let registry = T3Registry::new(&threads);
    let claim = CallerClaim::t3(Some("mcp:owner-thread"), Some(cwd.to_string_lossy().into_owned()));
    let first = registry.verify(&claim).unwrap();

    write_t3_thread(&threads, &cwd, "gemini-vertex", "run-v2");
    let second = registry.verify(&claim).unwrap();
    assert_eq!(first.owner_marker(), second.owner_marker());
    assert_ne!(first.incarnation(), second.incarnation());

    let legacy = root.join("legacy.sqlite");
    let script = r#"
import sqlite3, sys
conn = sqlite3.connect(sys.argv[1])
conn.executescript("""
CREATE TABLE projection_threads (
  thread_id TEXT PRIMARY KEY, project_id TEXT NOT NULL, worktree_path TEXT,
  created_at TEXT, archived_at TEXT, deleted_at TEXT, settled_at TEXT
);
CREATE TABLE projection_thread_sessions (
  thread_id TEXT PRIMARY KEY, provider_name TEXT, status TEXT, session_id TEXT
);
""")
conn.execute("INSERT INTO projection_threads VALUES ('mcp:legacy-owner', 'proj', ?, NULL, NULL, NULL, NULL)", (sys.argv[2],))
conn.execute("INSERT INTO projection_thread_sessions VALUES ('mcp:legacy-owner', 'codex', 'running', 'session-v1')")
conn.commit()
"#;
    assert!(Command::new("python3").arg("-c").arg(script).arg(&legacy).arg(&cwd).status().unwrap().success());
    let legacy_session = T3Registry::new(&legacy)
        .verify(&CallerClaim::t3(Some("mcp:legacy-owner"), Some(cwd.to_string_lossy().into_owned())))
        .unwrap();
    assert_eq!(legacy_session.owner_marker(), None);
}

#[test]
fn paseo_owner_marker_changes_with_persistence_handle_and_requires_one() {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let cwd = root.join("work");
    let agents = root.join("agents");
    let folder = agents.join("work");
    fs::create_dir_all(&folder).unwrap();
    let path = folder.join("agent-owner.json");
    let claim = CallerClaim::paseo(Some("agent-owner"), Some(cwd.to_string_lossy().into_owned()));

    write_paseo_agent(&path, "agent-owner", &cwd, Some("handle-v1"));
    let first = PaseoRegistry::new(&agents).verify(&claim).unwrap();
    write_paseo_agent(&path, "agent-owner", &cwd, Some("handle-v2"));
    let second = PaseoRegistry::new(&agents).verify(&claim).unwrap();
    assert_ne!(first.owner_marker(), second.owner_marker());

    write_paseo_agent(&path, "agent-owner", &cwd, None);
    let missing = PaseoRegistry::new(&agents).verify(&claim).unwrap();
    assert_eq!(missing.owner_marker(), None);
}

#[test]
fn direct_owner_marker_is_bound_to_the_token_not_the_working_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let cwd_one = root.join("one");
    let cwd_two = root.join("two");
    let cwd_three = root.join("three");
    for cwd in [&cwd_one, &cwd_two, &cwd_three] {
        fs::create_dir_all(cwd).unwrap();
    }

    let registry = DirectRegistry::new();
    for (id, cwd, token) in [
        ("direct-one", &cwd_one, "same-token"),
        ("direct-two", &cwd_two, "same-token"),
        ("direct-three", &cwd_three, "different-token"),
    ] {
        registry.register(DirectSession {
            id: id.to_string(),
            cwd: cwd.to_string_lossy().into_owned(),
            token: token.to_string(),
            liveness: SessionLiveness::Live,
            harness: Some(Harness::Codex),
            pending_permissions: vec![],
        });
    }

    let first = registry
        .verify(&CallerClaim::direct(
            Some("direct-one"),
            Some(cwd_one.to_string_lossy().into_owned()),
            Some("same-token"),
        ))
        .unwrap();
    let second = registry
        .verify(&CallerClaim::direct(
            Some("direct-two"),
            Some(cwd_two.to_string_lossy().into_owned()),
            Some("same-token"),
        ))
        .unwrap();
    let different = registry
        .verify(&CallerClaim::direct(
            Some("direct-three"),
            Some(cwd_three.to_string_lossy().into_owned()),
            Some("different-token"),
        ))
        .unwrap();
    assert_eq!(first.owner_marker(), second.owner_marker());
    assert_ne!(first.owner_marker(), different.owner_marker());
}
