//! Regression tests for authoritative T3 destination resolution and linked environment discovery
//! (BRO-4229): short UUIDs, worktree labels, exact local and host-authored IDs, linked vs unlinked
//! remote environments, wrong-target isolation, and fail-closed registry reads.

use maestro_hosts::{SessionRef, Surface, T3Destination, T3Registry};
use serde_json::json;
use std::path::Path;
use std::process::Command;

const LOCAL_ENV: &str = "env-local-host";
const LINKED_ENV: &str = "env-linked-peer";
const FENCED_ENV: &str = "env-fenced-peer";
const UNKNOWN_ENV: &str = "env-unknown-peer";

const FULL_UUID_A: &str = "60f14287-1111-4222-8333-444444444444";
const SHORT_UUID_A: &str = "60f14287";
const FULL_UUID_B: &str = "d054d967-5555-4666-8777-888888888888";
const WORKTREE_LABEL_B: &str = "t3-d054d967";
const HOST_AUTHORED_ID: &str = "mcp:launch:ab%3Acd";
const COLLIDING_ID: &str = "thread-shared-name";

fn t3_ref(env: Option<&str>, id: &str) -> SessionRef {
    SessionRef { surface: Surface::T3, environment: env.map(str::to_string), id: id.to_string() }
}

fn build_json_t3(root: &Path) -> T3Registry {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("environment-id"), format!("{LOCAL_ENV}\n")).unwrap();
    let threads = json!([
        {
            "id": FULL_UUID_A,
            "cwd": "/tmp/ws-a",
            "environment_id": LOCAL_ENV,
            "provider": "claudeAgent",
            "created_at": "2026-11-20T00:00:00Z",
            "run_id": "run-a",
        },
        {
            "id": FULL_UUID_B,
            "cwd": "/tmp/t3-d054d967",
            "environment_id": LOCAL_ENV,
            "provider": "claudeAgent",
            "created_at": "2026-11-20T00:00:00Z",
            "run_id": "run-b",
        },
        {
            "id": HOST_AUTHORED_ID,
            "cwd": "/tmp/ws-c",
            "environment_id": LOCAL_ENV,
            "provider": "codex",
            "created_at": "2026-11-20T00:00:00Z",
            "run_id": "run-c",
        },
        {
            "id": COLLIDING_ID,
            "cwd": "/tmp/ws-local-collide",
            "environment_id": LOCAL_ENV,
            "provider": "codex",
            "created_at": "2026-11-20T00:00:00Z",
            "run_id": "run-d",
        },
        {
            "id": "archived-thread-1",
            "cwd": "/tmp/ws-archived",
            "environment_id": LOCAL_ENV,
            "archived_at": "2026-11-21T00:00:00Z",
        },
    ]);
    std::fs::write(root.join("threads.json"), serde_json::to_vec(&threads).unwrap()).unwrap();
    std::fs::write(
        root.join("linked-environments.json"),
        serde_json::to_vec(&json!([
            { "environment_id": LINKED_ENV, "fenced": false },
            { "environment_id": FENCED_ENV, "fenced": true },
        ]))
        .unwrap(),
    )
    .unwrap();
    T3Registry::new(root)
}

fn build_sqlite_t3(root: &Path) -> T3Registry {
    let userdata = root.join("userdata");
    std::fs::create_dir_all(&userdata).unwrap();
    std::fs::write(userdata.join("environment-id"), format!("{LOCAL_ENV}\n")).unwrap();
    let db = userdata.join("statev2.sqlite");
    let script = r#"
import json, sqlite3, sys
db_path, full_a, full_b, host_id, collide, linked_env, fenced_env = sys.argv[1:]
con = sqlite3.connect(db_path)
con.executescript("""
CREATE TABLE projection_projects (project_id TEXT PRIMARY KEY, workspace_root TEXT);
CREATE TABLE orchestration_v2_projection_threads (
    thread_id TEXT PRIMARY KEY,
    project_id TEXT,
    default_provider TEXT,
    created_at TEXT,
    updated_at TEXT,
    archived_at TEXT,
    deleted_at TEXT,
    payload_json TEXT
);
CREATE TABLE peer_environment_links (
    environment_id TEXT PRIMARY KEY,
    label TEXT,
    fenced INTEGER
);
""")
con.execute("INSERT INTO projection_projects VALUES ('p1', '/tmp/project')")
rows = [
    (full_a, 'p1', 'claudeAgent', '2026-11-20T00:00:00Z', '2026-11-20T00:00:00Z', None, None, json.dumps({"worktreePath": "/tmp/ws-a"})),
    (full_b, 'p1', 'claudeAgent', '2026-11-20T00:00:00Z', '2026-11-20T00:00:00Z', None, None, json.dumps({"worktreePath": "/tmp/t3-d054d967"})),
    (host_id, 'p1', 'codex', '2026-11-20T00:00:00Z', '2026-11-20T00:00:00Z', None, None, json.dumps({"worktreePath": "/tmp/ws-c"})),
    (collide, 'p1', 'codex', '2026-11-20T00:00:00Z', '2026-11-20T00:00:00Z', None, None, json.dumps({"worktreePath": "/tmp/ws-local-collide"})),
    ('archived-thread-1', 'p1', 'codex', '2026-11-20T00:00:00Z', '2026-11-20T00:00:00Z', '2026-11-21T00:00:00Z', None, json.dumps({"worktreePath": "/tmp/ws-archived", "archivedAt": "2026-11-21T00:00:00Z"})),
]
con.executemany("INSERT INTO orchestration_v2_projection_threads VALUES (?, ?, ?, ?, ?, ?, ?, ?)", rows)
con.execute("INSERT INTO peer_environment_links VALUES (?, 'Mac', 0)", (linked_env,))
con.execute("INSERT INTO peer_environment_links VALUES (?, 'Fenced', 1)", (fenced_env,))
con.commit()
con.close()
"#;
    let status = Command::new("python3")
        .args(["-I", "-c", script])
        .arg(&db)
        .args([FULL_UUID_A, FULL_UUID_B, HOST_AUTHORED_ID, COLLIDING_ID, LINKED_ENV, FENCED_ENV])
        .status()
        .unwrap();
    assert!(status.success(), "building fixture sqlite failed");
    T3Registry::new(root)
}

fn assert_registry_destination_contract(reg: &T3Registry) {
    // 1. Short UUID prefix and worktree label are NEVER inferred or resolved.
    assert!(
        matches!(reg.resolve_destination(&t3_ref(Some("local"), SHORT_UUID_A)), T3Destination::Unresolved { .. }),
        "short UUID must remain unresolved"
    );
    assert!(
        matches!(reg.resolve_destination(&t3_ref(Some("local"), WORKTREE_LABEL_B)), T3Destination::Unresolved { .. }),
        "worktree label must remain unresolved"
    );
    assert!(
        matches!(
            reg.resolve_destination(&t3_ref(Some("local"), "archived-thread-1")),
            T3Destination::Unresolved { .. }
        ),
        "archived thread must remain unresolved"
    );
    assert!(
        matches!(reg.resolve_destination(&t3_ref(Some("local"), "--flag")), T3Destination::Unresolved { .. }),
        "flag-like id must remain unresolved"
    );
    assert!(
        matches!(reg.resolve_destination(&t3_ref(Some(LINKED_ENV), "--flag")), T3Destination::Unresolved { .. }),
        "remote destination with flag-like id must remain unresolved"
    );

    // 2. Exact local UUID and host-authored percent-encoded ID canonicalize to the host's env id.
    let local_a = reg.resolve_destination(&t3_ref(Some("local"), FULL_UUID_A));
    assert_eq!(
        local_a,
        T3Destination::Local {
            session: t3_ref(Some(LOCAL_ENV), FULL_UUID_A),
            environment_id: Some(LOCAL_ENV.into()),
            thread_id: FULL_UUID_A.into(),
        }
    );
    let dash_b = reg.resolve_destination(&t3_ref(Some("-"), FULL_UUID_B));
    assert_eq!(
        dash_b,
        T3Destination::Local {
            session: t3_ref(Some(LOCAL_ENV), FULL_UUID_B),
            environment_id: Some(LOCAL_ENV.into()),
            thread_id: FULL_UUID_B.into(),
        }
    );
    let host_authored = reg.resolve_destination(&t3_ref(Some(LOCAL_ENV), HOST_AUTHORED_ID));
    assert_eq!(
        host_authored,
        T3Destination::Local {
            session: t3_ref(Some(LOCAL_ENV), HOST_AUTHORED_ID),
            environment_id: Some(LOCAL_ENV.into()),
            thread_id: HOST_AUTHORED_ID.into(),
        }
    );

    // 3. Remote linked environment is NEVER matched against a colliding local thread id.
    assert_eq!(reg.linked_environments().unwrap(), vec![LINKED_ENV.to_string()]);
    assert_eq!(
        reg.resolve_destination(&t3_ref(Some(LINKED_ENV), COLLIDING_ID)),
        T3Destination::Remote {
            session: t3_ref(Some(LINKED_ENV), COLLIDING_ID),
            environment_id: LINKED_ENV.into(),
            thread_id: COLLIDING_ID.into(),
            linked: true,
        }
    );

    // 4. Fenced and unknown remote environments are never treated as linked or local.
    assert_eq!(
        reg.resolve_destination(&t3_ref(Some(FENCED_ENV), COLLIDING_ID)),
        T3Destination::Remote {
            session: t3_ref(Some(FENCED_ENV), COLLIDING_ID),
            environment_id: FENCED_ENV.into(),
            thread_id: COLLIDING_ID.into(),
            linked: false,
        }
    );
    assert_eq!(
        reg.resolve_destination(&t3_ref(Some(UNKNOWN_ENV), COLLIDING_ID)),
        T3Destination::Remote {
            session: t3_ref(Some(UNKNOWN_ENV), COLLIDING_ID),
            environment_id: UNKNOWN_ENV.into(),
            thread_id: COLLIDING_ID.into(),
            linked: false,
        }
    );

    // 5. Disconnected registry fails closed as Unavailable.
    let disconnected = reg.clone().with_disconnected(true);
    assert!(matches!(
        disconnected.resolve_destination(&t3_ref(Some("local"), FULL_UUID_A)),
        T3Destination::Unavailable { .. }
    ));
}

#[test]
fn t3_destination_resolution_matches_contract_for_json_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let reg = build_json_t3(dir.path());
    assert_registry_destination_contract(&reg);
}

#[test]
fn t3_destination_resolution_matches_contract_for_sqlite_registry() {
    let dir = tempfile::tempdir().unwrap();
    let reg = build_sqlite_t3(dir.path());
    assert_registry_destination_contract(&reg);
}
