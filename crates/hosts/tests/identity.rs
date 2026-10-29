//! Host identity integration, golden-parity, spoof-refusal, bridge-credential,
//! MCP wiring, and permission-need tests (BRO-3268).

use maestro_core::authority::Principal;
use maestro_core::need::NeedKind;
use maestro_core::request::{Disposition, RequestKind};
use maestro_hosts::{
    BridgeTransport, CallerClaim, DirectRegistry, DirectSession, Harness, NodeId, PARENT_AGENT_LABEL, PaseoRegistry,
    PeerCredentials, PermissionLike, PermissionPrompt, RefusalReason, SessionError, SessionLiveness, SessionUpdate,
    Surface, SurfaceRegistry, T3Registry, describe_permission, verified_identity, within,
};
use maestro_mcp::{agent_request_with_hosts, call_with_hosts};
use maestro_store::{Paths, Store, TreeSource};
use serde::Deserialize;
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

fn folder_of(cwd: &Path) -> String {
    cwd.to_string_lossy().trim_start_matches('/').replace('/', "-")
}

#[allow(clippy::too_many_arguments)]
fn write_paseo_record(
    agents_dir: &Path,
    id: &str,
    cwd: &Path,
    archived_at: Option<&str>,
    last_status: Option<&str>,
    parent: Option<&str>,
    session_id: Option<&str>,
    pending_permissions: Value,
) -> PathBuf {
    let folder = agents_dir.join(folder_of(cwd));
    fs::create_dir_all(&folder).unwrap();
    let path = folder.join(format!("{id}.json"));
    let record = build_paseo_json(
        id,
        cwd,
        archived_at,
        last_status,
        parent,
        session_id.unwrap_or("claude-session-id"),
        pending_permissions,
    );
    fs::write(&path, serde_json::to_string_pretty(&record).unwrap()).unwrap();
    path
}

fn build_paseo_json(
    id: &str,
    cwd: &Path,
    archived_at: Option<&str>,
    last_status: Option<&str>,
    parent: Option<&str>,
    session_id: &str,
    pending_permissions: Value,
) -> Value {
    let mut labels = serde_json::Map::new();
    if let Some(p) = parent {
        labels.insert(PARENT_AGENT_LABEL.to_string(), Value::String(p.to_string()));
    }
    labels.insert("paseo.open-agent-tab.cid_1".to_string(), Value::String("true".to_string()));
    json!({
        "id": id,
        "provider": "claude",
        "cwd": cwd.to_string_lossy(),
        "workspaceId": "wks_0123456789abcdef",
        "createdAt": "2026-10-09T15:01:04.239Z",
        "updatedAt": "2026-10-09T15:01:04.239Z",
        "lastActivityAt": "2026-10-09T15:01:04.239Z",
        "lastUserMessageAt": "2026-10-09T15:01:04.239Z",
        "title": "A session",
        "labels": labels,
        "lastStatus": last_status.unwrap_or("idle"),
        "lastModeId": "bypassPermissions",
        "persistence": {
            "provider": "claude",
            "sessionId": session_id,
            "nativeHandle": session_id,
            "metadata": {
                "provider": "claude",
                "cwd": cwd.to_string_lossy(),
                "mcpServers": {
                    "paseo": {
                        "type": "http",
                        "url": "http://127.0.0.1:6767/mcp/agents",
                        "headers": {
                            "Authorization": "Bearer secret-paseo-token-do-not-read"
                        }
                    }
                }
            }
        },
        "archivedAt": archived_at,
        "pendingPermissions": pending_permissions,
    })
}

#[derive(Deserialize)]
struct GoldenFile {
    plugin_rev: String,
    verify: Vec<GoldenVerifyCase>,
    started: Vec<GoldenStartedCase>,
    permissions: Vec<GoldenPermissionCase>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoldenVerifyCase {
    name: String,
    agent_id: Option<String>,
    cwd: Option<String>,
    ok: Option<GoldenVerified>,
    error: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoldenVerified {
    agent_id: String,
    cwd: String,
}

#[derive(Deserialize)]
struct GoldenStartedCase {
    name: String,
    parent: String,
    child: String,
    started: bool,
}

#[derive(Deserialize)]
struct GoldenPermissionCase {
    name: String,
    request: PermissionLike,
    cwd: Option<String>,
    result: GoldenPermissionResult,
}

#[derive(Deserialize)]
struct GoldenPermissionResult {
    title: String,
    detail: Option<String>,
}

#[test]
fn paseo_caller_and_permission_decisions_match_plugin_golden() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let plugin_rev = fs::read_to_string(repo_root.join("fixtures/parity/plugin-rev")).unwrap().trim().to_string();
    let golden_text = fs::read_to_string(repo_root.join("fixtures/hosts/callers-golden.json")).unwrap();
    let golden: GoldenFile = serde_json::from_str(&golden_text).unwrap();
    assert_eq!(golden.plugin_rev, plugin_rev);

    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let agents_dir = root.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    let callers = PaseoRegistry::new(&agents_dir);

    let mk_folder = |rel: &str| -> PathBuf {
        let p = root.join(rel);
        fs::create_dir_all(&p).unwrap();
        p
    };

    let live_cwd = mk_folder("work/a");
    let _deep_cwd = mk_folder("work/a/server/deep");
    let other_cwd = mk_folder("work/other");
    let _prefix_cwd = mk_folder("work/ab");
    let real_dir = mk_folder("real/arc-a");
    let link_base = root.join("link");
    symlink(root.join("real"), &link_base).unwrap();
    let link_dir = link_base.join("arc-a");

    let live_id = "11111111-1111-4111-8111-111111111111";
    let live_session_id = "99999999-9999-4999-8999-999999999999";
    let archived_id = "22222222-2222-4222-8222-222222222222";
    let closed_id = "33333333-3333-4333-8333-333333333333";
    let running_id = "44444444-4444-4444-8444-444444444444";
    let error_id = "55555555-5555-4555-8555-555555555555";
    let sym_target_id = "66666666-6666-4666-8666-666666666666";
    let sym_reg_id = "77777777-7777-4777-8777-777777777777";
    let victim_id = "88888888-8888-4888-8888-888888888888";
    let child_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let grandchild_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let archived_child_id = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    let closed_child_id = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";

    let live_path =
        write_paseo_record(&agents_dir, live_id, &live_cwd, None, Some("idle"), None, Some(live_session_id), json!([]));
    write_paseo_record(
        &agents_dir,
        archived_id,
        &live_cwd,
        Some("2026-10-09T17:30:23.134Z"),
        Some("idle"),
        None,
        None,
        json!([]),
    );
    write_paseo_record(&agents_dir, closed_id, &live_cwd, None, Some("closed"), None, None, json!([]));
    write_paseo_record(&agents_dir, running_id, &live_cwd, None, Some("running"), None, None, json!([]));
    write_paseo_record(&agents_dir, error_id, &live_cwd, None, Some("error"), None, None, json!([]));
    write_paseo_record(&agents_dir, sym_target_id, &real_dir, None, Some("idle"), None, None, json!([]));
    write_paseo_record(&agents_dir, sym_reg_id, &link_dir, None, Some("idle"), None, None, json!([]));

    let victim_path = live_path.parent().unwrap().join(format!("{victim_id}.json"));
    fs::write(
        &victim_path,
        serde_json::to_string_pretty(&build_paseo_json(live_id, &live_cwd, None, Some("idle"), None, "s", json!([])))
            .unwrap(),
    )
    .unwrap();

    let outside_dir = mk_folder("outside");
    let hostile_outside = "../../outside/stolen";
    fs::write(
        outside_dir.join("stolen.json"),
        serde_json::to_string_pretty(&build_paseo_json(
            hostile_outside,
            &live_cwd,
            None,
            Some("idle"),
            None,
            "s",
            json!([]),
        ))
        .unwrap(),
    )
    .unwrap();

    write_paseo_record(&agents_dir, child_id, &other_cwd, None, Some("running"), Some(live_id), None, json!([]));
    write_paseo_record(&agents_dir, grandchild_id, &other_cwd, None, Some("idle"), Some(child_id), None, json!([]));
    write_paseo_record(
        &agents_dir,
        archived_child_id,
        &other_cwd,
        Some("2026-10-09T17:30:23.134Z"),
        Some("idle"),
        Some(live_id),
        None,
        json!([]),
    );
    write_paseo_record(&agents_dir, closed_child_id, &other_cwd, None, Some("closed"), Some(live_id), None, json!([]));

    let root_str = root.to_string_lossy().into_owned();
    for case in &golden.verify {
        let cwd = case.cwd.as_ref().map(|c| c.replace("$ROOT", &root_str));
        let claim = CallerClaim::paseo(case.agent_id.clone(), cwd);
        let outcome = callers.verify(&claim);
        match (&case.ok, &case.error) {
            (Some(want_ok), None) => {
                let verified = outcome.unwrap_or_else(|e| panic!("case {}: unexpected error {e}", case.name));
                assert_eq!(verified.agent_id(), want_ok.agent_id, "case {}", case.name);
                assert_eq!(verified.cwd().replace(&root_str, "$ROOT"), want_ok.cwd, "case {}", case.name);
                let serialized = serde_json::to_string(&verified).unwrap();
                assert!(!serialized.contains("Bearer"), "bearer token leaked in {serialized}");
            }
            (None, Some(want_err)) => {
                let err = outcome.expect_err(&format!("case {} unexpectedly succeeded", case.name));
                let got_err = err.message.replace(&root_str, "$ROOT");
                assert_eq!(&got_err, want_err, "case {}", case.name);
            }
            _ => panic!("malformed golden case {}", case.name),
        }
    }

    for sc in &golden.started {
        assert_eq!(callers.started(&sc.parent, &sc.child), sc.started, "started case {}", sc.name);
    }

    for pc in &golden.permissions {
        let (title, detail) = describe_permission(&pc.request, pc.cwd.as_deref());
        assert_eq!(title, pc.result.title, "permission title {}", pc.name);
        assert_eq!(detail, pc.result.detail, "permission detail {}", pc.name);
    }

    // Verify cache invalidation when a session file moves folders or is removed
    let movable_id = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
    let p1 = write_paseo_record(&agents_dir, movable_id, &live_cwd, None, Some("idle"), None, None, json!([]));
    assert!(
        callers.verify(&CallerClaim::paseo(Some(movable_id), Some(live_cwd.to_string_lossy().into_owned()))).is_ok()
    );
    fs::remove_file(&p1).unwrap();
    assert!(
        callers.verify(&CallerClaim::paseo(Some(movable_id), Some(live_cwd.to_string_lossy().into_owned()))).is_err()
    );
    write_paseo_record(&agents_dir, movable_id, &other_cwd, None, Some("idle"), None, None, json!([]));
    assert!(
        callers.verify(&CallerClaim::paseo(Some(movable_id), Some(other_cwd.to_string_lossy().into_owned()))).is_ok()
    );
}

#[test]
fn four_of_four_spoof_cases_and_symlink_dotdot_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let paseo_dir = root.join("paseo-agents");
    let t3_json = root.join("threads.json");
    let session_cwd = root.join("work/session-a");
    let sibling_cwd = root.join("work/session-ab");
    let archived_cwd = root.join("work/archived");
    let t3_cwd = root.join("work/t3-thread");
    let outside_deep = root.join("outside/deep");
    for d in [&paseo_dir, &session_cwd, &sibling_cwd, &archived_cwd, &t3_cwd, &outside_deep] {
        fs::create_dir_all(d).unwrap();
    }

    // Symlink inside session_cwd pointing outside to outside/deep
    let escape_link = session_cwd.join("link");
    symlink(&outside_deep, &escape_link).unwrap();

    let live_paseo_id = "paseo-live-11111111";
    let ended_paseo_id = "paseo-ended-22222222";
    let live_t3_id = "mcp:23d4cc42-50a3-4e50-9d05-5e6fe71e9e6d";
    let uuid_t3_id = "23d4cc42-50a3-4e50-9d05-5e6fe71e9e6d";

    write_paseo_record(&paseo_dir, live_paseo_id, &session_cwd, None, Some("running"), None, None, json!([]));
    write_paseo_record(
        &paseo_dir,
        ended_paseo_id,
        &archived_cwd,
        Some("2026-10-25T00:00:00Z"),
        Some("idle"),
        None,
        None,
        json!([]),
    );

    fs::write(
        &t3_json,
        serde_json::to_string_pretty(&json!([
            {
                "id": live_t3_id,
                "cwd": t3_cwd.to_string_lossy(),
                "environment_id": "env-nuc",
                "provider": "gemini-vertex",
                "created_at": "2026-10-25T00:00:00Z",
                "status": "running"
            },
            {
                "id": uuid_t3_id,
                "cwd": t3_cwd.to_string_lossy(),
                "environment_id": "env-nuc",
                "provider": "codex",
                "created_at": "2026-10-25T00:00:00Z",
                "status": "running"
            }
        ]))
        .unwrap(),
    )
    .unwrap();

    let registry = SurfaceRegistry::new(
        Some(PaseoRegistry::new(&paseo_dir)),
        Some(T3Registry::new(&t3_json)),
        Some(DirectRegistry::new()),
    );

    // Control: valid Paseo and T3 claims succeed
    let ok_paseo = registry
        .verify_caller(&CallerClaim::paseo(Some(live_paseo_id), Some(session_cwd.to_string_lossy().into_owned())))
        .unwrap();
    assert_eq!(ok_paseo.session().to_string(), format!("paseo:-/{live_paseo_id}"));
    assert_eq!(verified_identity(&ok_paseo), Principal::Agent { session: ok_paseo.session().clone() });

    let ok_t3 = registry
        .verify_caller(&CallerClaim::t3(Some(live_t3_id), Some(t3_cwd.to_string_lossy().into_owned())))
        .unwrap();
    assert_eq!(ok_t3.session().to_string(), format!("t3:env-nuc/{live_t3_id}"));
    assert_eq!(ok_t3.harness(), Some(&Harness::Gemini));

    // Spoof 1: spoofed / unknown id
    let spoof_id = registry
        .verify_caller(&CallerClaim::paseo(Some("invented-agent-id"), Some(session_cwd.to_string_lossy().into_owned())))
        .unwrap_err();
    assert_eq!(spoof_id.reason, RefusalReason::NotFound);

    // Spoof 2a: cwd outside the session's folder (sibling prefix /work/session-ab)
    let spoof_cwd = registry
        .verify_caller(&CallerClaim::paseo(Some(live_paseo_id), Some(sibling_cwd.to_string_lossy().into_owned())))
        .unwrap_err();
    assert_eq!(spoof_cwd.reason, RefusalReason::OutsideCwd);

    // Spoof 2b: symlink + `..` traversal (`session_cwd/link/..` -> `outside`) MUST be refused!
    let symlink_dotdot_cwd = format!("{}/..", escape_link.to_string_lossy());
    assert!(!within(Path::new(&symlink_dotdot_cwd), &session_cwd));
    let spoof_symlink_dotdot =
        registry.verify_caller(&CallerClaim::paseo(Some(live_paseo_id), Some(symlink_dotdot_cwd))).unwrap_err();
    assert_eq!(spoof_symlink_dotdot.reason, RefusalReason::OutsideCwd);

    // Spoof 2c: symlink + missing child + `..` (`session_cwd/link/missing/..`) MUST fail closed!
    let symlink_missing_dotdot = format!("{}/missing/..", escape_link.to_string_lossy());
    assert!(!within(Path::new(&symlink_missing_dotdot), &session_cwd));

    // Spoof 3: ended session
    let spoof_ended = registry
        .verify_caller(&CallerClaim::paseo(Some(ended_paseo_id), Some(archived_cwd.to_string_lossy().into_owned())))
        .unwrap_err();
    assert_eq!(spoof_ended.reason, RefusalReason::NotLive);

    // Spoof 4: T3 id presented as Paseo (both mcp: prefixed and plain UUID in T3 registry), and Paseo id presented as T3
    let spoof_t3_as_paseo = registry
        .verify_caller(&CallerClaim::paseo(Some(live_t3_id), Some(t3_cwd.to_string_lossy().into_owned())))
        .unwrap_err();
    assert_eq!(spoof_t3_as_paseo.reason, RefusalReason::CrossSurface);

    let spoof_uuid_t3_as_paseo = registry
        .verify_caller(&CallerClaim::paseo(Some(uuid_t3_id), Some(t3_cwd.to_string_lossy().into_owned())))
        .unwrap_err();
    assert_eq!(spoof_uuid_t3_as_paseo.reason, RefusalReason::CrossSurface);

    let spoof_paseo_as_t3 = registry
        .verify_caller(&CallerClaim::t3(Some(live_paseo_id), Some(session_cwd.to_string_lossy().into_owned())))
        .unwrap_err();
    assert_eq!(spoof_paseo_as_t3.reason, RefusalReason::CrossSurface);
}

#[test]
fn t3_thread_lookup_verifies_sqlite_and_json_registries_and_resilience() {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let userdata = root.join("userdata");
    fs::create_dir_all(&userdata).unwrap();
    fs::write(userdata.join("environment-id"), "8c5e81ab-e50f-49b4-a7e9-ba471308fac9\n").unwrap();
    fs::write(
        userdata.join("server-runtime.json"),
        r#"{"version":1,"pid":1171473,"host":"100.112.149.32","port":3773,"origin":"http://100.112.149.32:3773","serverVersion":"0.1.3"}"#,
    )
    .unwrap();

    let wt_dir = root.join("worktrees/bro-3268");
    let proj_root = root.join("projects/maestro");
    let other_dir = root.join("worktrees/bro-3268-sibling");
    for d in [&wt_dir, &proj_root, &other_dir] {
        fs::create_dir_all(d).unwrap();
    }

    let db_path = userdata.join("statev2.sqlite");
    let init_sql = r#"
import json, sqlite3, sys
db, wt_dir, proj_root = sys.argv[1], sys.argv[2], sys.argv[3]
conn = sqlite3.connect(db)
conn.executescript("""
CREATE TABLE projection_projects (
  project_id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  workspace_root TEXT NOT NULL,
  scripts_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  deleted_at TEXT
);
CREATE TABLE orchestration_v2_projection_threads (
  thread_id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  title TEXT NOT NULL,
  default_provider TEXT NOT NULL,
  runtime_mode TEXT NOT NULL,
  interaction_mode TEXT NOT NULL,
  active_provider_thread_id TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  archived_at TEXT,
  deleted_at TEXT,
  payload_json TEXT NOT NULL
);
CREATE TABLE orchestration_v2_projection_runs (
  run_id TEXT PRIMARY KEY,
  thread_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL,
  provider TEXT NOT NULL,
  provider_thread_id TEXT,
  status TEXT NOT NULL,
  requested_at TEXT NOT NULL,
  completed_at TEXT,
  payload_json TEXT NOT NULL
);
CREATE TABLE orchestration_v2_projection_runtime_requests (
  runtime_request_id TEXT PRIMARY KEY,
  thread_id TEXT NOT NULL,
  node_id TEXT NOT NULL,
  provider_turn_id TEXT,
  kind TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  resolved_at TEXT,
  payload_json TEXT NOT NULL
);
""")
conn.execute(
    "INSERT INTO projection_projects VALUES (?, ?, ?, '[]', '2026-10-25T00:00:00Z', '2026-10-25T00:00:00Z', NULL)",
    ("proj-1", "maestro", proj_root),
)
conn.execute(
    "INSERT INTO orchestration_v2_projection_threads VALUES (?, ?, ?, ?, 'full-access', 'default', NULL, '2026-10-25T00:00:00Z', '2026-10-25T00:00:00Z', NULL, NULL, ?)",
    ("mcp:thread-wt", "proj-1", "Worktree thread", "claudeAgent", json.dumps({"worktreePath": wt_dir})),
)
conn.execute(
    "INSERT INTO orchestration_v2_projection_runs VALUES ('run-1', 'mcp:thread-wt', 1, 'claudeAgent', NULL, 'running', '2026-10-25T00:00:00Z', NULL, '{}')",
)
# One malformed runtime request row + one non-dict row + one valid pending approval row
conn.execute(
    "INSERT INTO orchestration_v2_projection_runtime_requests VALUES ('req-bad-json', 'mcp:thread-wt', 'node-0', NULL, 'approval', 'pending', '2026-10-25T00:00:30Z', NULL, '{')",
)
conn.execute(
    "INSERT INTO orchestration_v2_projection_runtime_requests VALUES ('req-bad-type', 'mcp:thread-wt', 'node-0', NULL, 'approval', 'pending', '2026-10-25T00:00:40Z', NULL, '123')",
)
conn.execute(
    "INSERT INTO orchestration_v2_projection_runtime_requests VALUES (?, 'mcp:thread-wt', 'node-1', NULL, 'approval', 'pending', '2026-10-25T00:01:00Z', NULL, ?)",
    ("req-t3-1", json.dumps({"toolName": "Bash", "input": {"command": "git push origin main"}})),
)
conn.execute(
    "INSERT INTO orchestration_v2_projection_threads VALUES (?, ?, ?, ?, 'full-access', 'default', NULL, '2026-10-25T00:00:00Z', '2026-10-25T00:00:00Z', NULL, NULL, ?)",
    ("mcp:thread-root", "proj-1", "Root thread", "codex", json.dumps({"worktreePath": None})),
)
conn.execute(
    "INSERT INTO orchestration_v2_projection_threads VALUES (?, ?, ?, ?, 'full-access', 'default', NULL, '2026-10-25T00:00:00Z', '2026-10-25T00:00:00Z', NULL, NULL, ?)",
    ("mcp:thread-settled", "proj-1", "Settled thread", "codex", json.dumps({"worktreePath": wt_dir, "settledAt": "2026-10-25T01:00:00Z"})),
)
conn.commit()
"#;
    let status =
        Command::new("python3").arg("-c").arg(init_sql).arg(&db_path).arg(&wt_dir).arg(&proj_root).status().unwrap();
    assert!(status.success());

    let t3 = T3Registry::new(&root);
    let discovery = t3.discover();
    assert!(discovery.connected);
    assert_eq!(discovery.version, "0.1.3");
    assert_eq!(discovery.environment_id.as_deref(), Some("8c5e81ab-e50f-49b4-a7e9-ba471308fac9"));
    assert!(matches!(discovery.transport, BridgeTransport::T3McpBridge | BridgeTransport::T3SqliteReadonly));

    // Worktree thread resolves to worktreePath, skips malformed request rows, and keeps valid approval
    let verified_wt =
        t3.verify(&CallerClaim::t3(Some("mcp:thread-wt"), Some(wt_dir.to_string_lossy().into_owned()))).unwrap();
    assert_eq!(verified_wt.session().to_string(), "t3:8c5e81ab-e50f-49b4-a7e9-ba471308fac9/mcp:thread-wt");
    assert_eq!(verified_wt.harness(), Some(&Harness::ClaudeCode));
    assert_eq!(verified_wt.pending_permissions().len(), 1);
    assert_eq!(
        verified_wt.pending_permissions()[0],
        PermissionPrompt {
            request_id: "req-t3-1".to_string(),
            title: "Run a command".to_string(),
            detail: Some("git push origin main".to_string()),
        }
    );

    // Pending-prompt freshness: once req-t3-1 is resolved in SQLite, re-verifying/snapshotting drops the prompt
    let resolve_sql = r#"import sqlite3, sys; c=sqlite3.connect(sys.argv[1]); c.execute("UPDATE orchestration_v2_projection_runtime_requests SET status='resolved' WHERE runtime_request_id='req-t3-1'"); c.commit()"#;
    assert!(Command::new("python3").arg("-c").arg(resolve_sql).arg(&db_path).status().unwrap().success());
    let refreshed_wt =
        t3.verify(&CallerClaim::t3(Some("mcp:thread-wt"), Some(wt_dir.to_string_lossy().into_owned()))).unwrap();
    assert!(refreshed_wt.pending_permissions().is_empty());

    // Root-checkout thread falls back to projection_projects.workspace_root
    let verified_root =
        t3.verify(&CallerClaim::t3(Some("mcp:thread-root"), Some(proj_root.to_string_lossy().into_owned()))).unwrap();
    assert_eq!(verified_root.cwd(), proj_root.to_string_lossy());
    assert_eq!(verified_root.harness(), Some(&Harness::Codex));

    // Settled thread is refused as not live
    let settled_err = t3
        .verify(&CallerClaim::t3(Some("mcp:thread-settled"), Some(wt_dir.to_string_lossy().into_owned())))
        .unwrap_err();
    assert_eq!(settled_err.reason, RefusalReason::NotLive);
    assert!(settled_err.message.contains("settled"));

    // Sibling prefix folder is refused
    let sibling_err =
        t3.verify(&CallerClaim::t3(Some("mcp:thread-wt"), Some(other_dir.to_string_lossy().into_owned()))).unwrap_err();
    assert_eq!(sibling_err.reason, RefusalReason::OutsideCwd);

    // T3 JSON registry: malformed permission entry skipped + ID-less prompt gets content-hashed t3-perm-<hash> stable across reordering
    let t3_json_path = root.join("t3-perms-threads.json");
    fs::write(
        &t3_json_path,
        serde_json::to_string_pretty(&json!([
            {
                "id": "mcp:thread-json-perms",
                "cwd": wt_dir.to_string_lossy(),
                "environment_id": "env-1",
                "provider": "codex",
                "created_at": "2026-10-25T00:00:00Z",
                "status": "running",
                "pending_permissions": [
                    { "bad_entry_missing_name": 42 },
                    { "name": "Bash", "input": { "command": "cargo check" } },
                    { "name": "Bash", "input": { "command": "cargo test" } }
                ]
            }
        ]))
        .unwrap(),
    )
    .unwrap();
    let t3_json_reg = T3Registry::new(&t3_json_path);
    let v_before = t3_json_reg
        .verify(&CallerClaim::t3(Some("mcp:thread-json-perms"), Some(wt_dir.to_string_lossy().into_owned())))
        .unwrap();
    assert_eq!(v_before.pending_permissions().len(), 2);
    let second_id_before = v_before.pending_permissions()[1].request_id.clone();
    assert!(second_id_before.starts_with("t3-perm-"));

    fs::write(
        &t3_json_path,
        serde_json::to_string_pretty(&json!([
            {
                "id": "mcp:thread-json-perms",
                "cwd": wt_dir.to_string_lossy(),
                "environment_id": "env-1",
                "provider": "codex",
                "created_at": "2026-10-25T00:00:00Z",
                "status": "running",
                "pending_permissions": [
                    { "name": "Bash", "input": { "command": "cargo test" } }
                ]
            }
        ]))
        .unwrap(),
    )
    .unwrap();
    let v_after = t3_json_reg
        .verify(&CallerClaim::t3(Some("mcp:thread-json-perms"), Some(wt_dir.to_string_lossy().into_owned())))
        .unwrap();
    assert_eq!(v_after.pending_permissions().len(), 1);
    assert_eq!(v_after.pending_permissions()[0].request_id, second_id_before);

    // Absent thread in valid SQLite database returns NotFound (exit code 3), whereas a broken SQLite
    // schema that fails during query execution returns RegistryDisconnected (exit code 2), never NotFound.
    let absent_err = t3
        .verify(&CallerClaim::t3(Some("mcp:does-not-exist"), Some(wt_dir.to_string_lossy().into_owned())))
        .unwrap_err();
    assert_eq!(absent_err.reason, RefusalReason::NotFound);

    let broken_db = root.join("broken.sqlite");
    let broken_sql = r#"import sqlite3, sys; c=sqlite3.connect(sys.argv[1]); c.execute("CREATE TABLE orchestration_v2_projection_threads (thread_id TEXT)"); c.commit()"#;
    assert!(Command::new("python3").arg("-c").arg(broken_sql).arg(&broken_db).status().unwrap().success());
    let broken_t3 = T3Registry::new(&broken_db);
    let broken_err = broken_t3
        .verify(&CallerClaim::t3(Some("mcp:any-thread"), Some(wt_dir.to_string_lossy().into_owned())))
        .unwrap_err();
    assert_eq!(broken_err.reason, RefusalReason::RegistryDisconnected);

    // Legacy projection_threads SQLite table:
    // (a) When both created_at and session_id are absent, SurfaceRegistry with bridge_secret/generations_file
    //     fails closed (InvalidToken) because no trustworthy incarnation marker is available.
    // (b) When session_id (or created_at) is present, replacing the session under the same thread_id,
    //     worktree_path, and provider_name (even with created_at = NULL) bumps generation across restart
    //     and refuses the old credential with IdReused!
    let legacy_db = root.join("legacy-statev2.sqlite");
    let legacy_init_sql = r#"
import sqlite3, sys
conn = sqlite3.connect(sys.argv[1])
conn.executescript("""
CREATE TABLE projection_threads (
  thread_id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  worktree_path TEXT,
  archived_at TEXT,
  deleted_at TEXT,
  settled_at TEXT
);
CREATE TABLE projection_thread_sessions (
  thread_id TEXT PRIMARY KEY,
  provider_name TEXT,
  status TEXT,
  session_id TEXT
);
""")
conn.execute("INSERT INTO projection_threads VALUES ('mcp:legacy-no-inc', 'proj-1', ?, NULL, NULL, NULL)", (sys.argv[2],))
conn.execute("INSERT INTO projection_thread_sessions VALUES ('mcp:legacy-no-inc', 'codex', 'running', NULL)")
conn.execute("INSERT INTO projection_threads VALUES ('mcp:legacy-1', 'proj-1', ?, NULL, NULL, NULL)", (sys.argv[2],))
conn.execute("INSERT INTO projection_thread_sessions VALUES ('mcp:legacy-1', 'codex', 'running', 'sess-v1')")
conn.commit()
"#;
    assert!(
        Command::new("python3").arg("-c").arg(legacy_init_sql).arg(&legacy_db).arg(&wt_dir).status().unwrap().success()
    );
    let legacy_gen_file = root.join("legacy-generations.json");
    let node_ok: NodeId = "nuc-node-1".parse().unwrap();
    let legacy_reg_1 = SurfaceRegistry::new(None, Some(T3Registry::new(&legacy_db)), None)
        .with_expected_node(node_ok.clone())
        .with_bridge_secret("legacy-secret")
        .with_generations_file(&legacy_gen_file);
    let peer = PeerCredentials { pid: std::process::id(), uid: 1000 };
    let no_inc_legacy_claim = CallerClaim::t3(Some("mcp:legacy-no-inc"), Some(wt_dir.to_string_lossy().into_owned()))
        .with_node(node_ok.clone())
        .with_peer(peer);
    assert_eq!(
        legacy_reg_1.verify_caller_at(&no_inc_legacy_claim, 1_000).unwrap_err().reason,
        RefusalReason::InvalidToken
    );

    let legacy_claim = CallerClaim::t3(Some("mcp:legacy-1"), Some(wt_dir.to_string_lossy().into_owned()))
        .with_node(node_ok.clone())
        .with_peer(peer);
    let legacy_v1 = legacy_reg_1.verify_session(&legacy_claim, Some(1_000)).unwrap();
    let legacy_cred = legacy_reg_1.issue_bridge_credential(&legacy_v1, node_ok.clone(), 1_000, 61_000).unwrap();

    // Replace the session in projection_thread_sessions under the same thread_id, worktree_path, and provider_name (absent created_at)
    let legacy_replace_sql = r#"
import sqlite3, sys
conn = sqlite3.connect(sys.argv[1])
conn.execute("UPDATE projection_thread_sessions SET session_id = 'sess-v2' WHERE thread_id = 'mcp:legacy-1'")
conn.commit()
"#;
    assert!(Command::new("python3").arg("-c").arg(legacy_replace_sql).arg(&legacy_db).status().unwrap().success());
    let legacy_reg_restarted = SurfaceRegistry::new(None, Some(T3Registry::new(&legacy_db)), None)
        .with_expected_node(node_ok.clone())
        .with_bridge_secret("legacy-secret")
        .with_generations_file(&legacy_gen_file);
    let legacy_authed = CallerClaim::t3(Some("mcp:legacy-1"), Some(wt_dir.to_string_lossy().into_owned()))
        .with_node(node_ok)
        .with_credential(legacy_cred);
    assert_eq!(
        legacy_reg_restarted.verify_caller_at(&legacy_authed, 30_000).unwrap_err().reason,
        RefusalReason::IdReused
    );
}

#[test]
fn bridge_credentials_node_binding_id_reuse_and_disconnect_fail_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let t3_json = root.join("threads.json");
    let wt_dir = root.join("work/t3-1");
    fs::create_dir_all(&wt_dir).unwrap();

    let thread_id = "mcp:bridge-thread-1";
    fs::write(
        &t3_json,
        serde_json::to_string_pretty(&json!([
            {
                "id": thread_id,
                "cwd": wt_dir.to_string_lossy(),
                "environment_id": "env-nuc",
                "node": "nuc-node-1",
                "provider": "gemini-vertex",
                "created_at": "2026-10-25T00:00:00Z",
                "status": "running",
                "pending_permissions": [
                    { "id": "perm-1", "name": "Bash", "input": { "command": "git status" } }
                ]
            }
        ]))
        .unwrap(),
    )
    .unwrap();

    let node_ok: NodeId = "nuc-node-1".parse().unwrap();
    let node_wrong: NodeId = "mac-studio-2".parse().unwrap();

    let gen_file = root.join("generations.json");
    let registry = SurfaceRegistry::new(None, Some(T3Registry::new(&t3_json)), None)
        .with_expected_node(node_ok.clone())
        .with_bridge_secret("bridge-secret-key")
        .with_generations_file(&gen_file);

    let unsigned_claim =
        CallerClaim::t3(Some(thread_id), Some(wt_dir.to_string_lossy().into_owned())).with_node(node_ok.clone());

    // When bridge_secret is configured, an unsigned claim with no transport peer is refused -> InvalidToken
    assert_eq!(registry.verify_caller_at(&unsigned_claim, 1_000).unwrap_err().reason, RefusalReason::InvalidToken);

    // Local transport-peer-authenticated claim succeeds and can be issued a signed BridgeCredential
    let peer_claim = unsigned_claim.clone().with_peer(PeerCredentials { pid: std::process::id(), uid: 1000 });
    let verified = registry.verify_session(&peer_claim, Some(1_000)).unwrap();
    let snap1 = registry.snapshot_session(&peer_claim, Some(1_000)).unwrap();
    assert_eq!(snap1.node.as_ref(), Some(&node_ok));
    assert_eq!(snap1.generation, 0);

    // Issue a valid short-lived bridge credential (valid for [1_000, 61_000))
    let cred = registry.issue_bridge_credential(&verified, node_ok.clone(), 1_000, 61_000).unwrap();
    assert!(format!("{cred:?}").contains("<redacted>"));
    let wire = serde_json::to_string(&cred).unwrap();
    assert!(wire.contains(&cred.signature), "wire JSON must include signature");
    let decoded: maestro_hosts::BridgeCredential = serde_json::from_str(&wire).unwrap();
    assert_eq!(decoded.signature, cred.signature);

    // Without a configured bridge_secret, both issuance and credential verification fail closed
    let secretless =
        SurfaceRegistry::new(None, Some(T3Registry::new(&t3_json)), None).with_expected_node(node_ok.clone());
    assert!(secretless.issue_bridge_credential(&verified, node_ok.clone(), 1_000, 61_000).is_err());
    assert_eq!(
        secretless.verify_caller_at(&unsigned_claim.clone().with_credential(cred.clone()), 30_000).unwrap_err().reason,
        RefusalReason::InvalidToken
    );

    // When expected_node differs from thread.node, verification fails closed even if claim.node matches expected_node or is omitted
    let mismatched_expected_t3 = T3Registry::new(&t3_json).with_expected_node(node_wrong.clone());
    assert_eq!(
        mismatched_expected_t3
            .verify(
                &CallerClaim::t3(Some(thread_id), Some(wt_dir.to_string_lossy().into_owned()))
                    .with_node(node_wrong.clone())
            )
            .unwrap_err()
            .reason,
        RefusalReason::WrongNode
    );
    assert_eq!(
        mismatched_expected_t3
            .verify(&CallerClaim::t3(Some(thread_id), Some(wt_dir.to_string_lossy().into_owned())))
            .unwrap_err()
            .reason,
        RefusalReason::WrongNode
    );

    let authed_claim = unsigned_claim.clone().with_credential(cred.clone());
    assert!(registry.verify_caller_at(&authed_claim, 30_000).is_ok());

    // 1. Wrong node on claim or credential -> WrongNode
    let wrong_node_claim =
        CallerClaim::t3(Some(thread_id), Some(wt_dir.to_string_lossy().into_owned())).with_node(node_wrong.clone());
    assert_eq!(registry.verify_caller(&wrong_node_claim).unwrap_err().reason, RefusalReason::WrongNode);
    let mut wrong_node_cred = cred.clone();
    wrong_node_cred.node = node_wrong;
    assert_eq!(
        registry.verify_caller_at(&unsigned_claim.clone().with_credential(wrong_node_cred), 30_000).unwrap_err().reason,
        RefusalReason::WrongNode
    );

    // 2. Stale / expired / future / >1h bridge credential -> StaleCredential
    assert_eq!(registry.verify_caller_at(&authed_claim, 61_000).unwrap_err().reason, RefusalReason::StaleCredential);
    assert_eq!(registry.verify_caller_at(&authed_claim, 500).unwrap_err().reason, RefusalReason::StaleCredential);
    assert!(matches!(
        registry.issue_bridge_credential(&verified, node_ok.clone(), 0, 3_600_001),
        Err(SessionError::StaleCredential { .. })
    ));

    // 3. Forged signature on credential -> InvalidToken
    let mut forged = cred.clone();
    forged.signature = "00".repeat(32);
    assert_eq!(
        registry.verify_caller_at(&unsigned_claim.clone().with_credential(forged), 30_000).unwrap_err().reason,
        RefusalReason::InvalidToken
    );

    // 4. Automatic incarnation-bound ID reuse detection across registry replacement + restart:
    // Replace the session in T3 under the SAME thread_id with a newer created_at incarnation,
    // construct a fresh SurfaceRegistry sharing gen_file (simulating restart without manual bump_generation),
    // and verify that presenting the old generation-0 credential fails closed with IdReused!
    fs::write(
        &t3_json,
        serde_json::to_string_pretty(&json!([
            {
                "id": thread_id,
                "cwd": wt_dir.to_string_lossy(),
                "environment_id": "env-nuc",
                "node": "nuc-node-1",
                "provider": "gemini-vertex",
                "created_at": "2026-10-25T00:05:00Z",
                "status": "running",
                "pending_permissions": []
            }
        ]))
        .unwrap(),
    )
    .unwrap();
    let restarted_registry = SurfaceRegistry::new(None, Some(T3Registry::new(&t3_json)), None)
        .with_expected_node(node_ok.clone())
        .with_bridge_secret("bridge-secret-key")
        .with_generations_file(&gen_file);
    assert_eq!(restarted_registry.verify_caller_at(&authed_claim, 30_000).unwrap_err().reason, RefusalReason::IdReused);
    assert_eq!(restarted_registry.generation_of(&Surface::T3, thread_id), 1);

    // Concurrent generation bumps across threads are serialized and exact
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                restarted_registry.try_bump_generation(&Surface::T3, thread_id).unwrap();
            });
        }
    });
    assert_eq!(restarted_registry.generation_of(&Surface::T3, thread_id), 5);

    // Corrupt generation ledger fails closed -> RegistryDisconnected (never falls back to 0)
    let corrupt_gen = root.join("corrupt-generations.json");
    fs::write(&corrupt_gen, "{not-valid-json").unwrap();
    let corrupt_reg = SurfaceRegistry::new(None, Some(T3Registry::new(&t3_json)), None)
        .with_expected_node(node_ok.clone())
        .with_bridge_secret("bridge-secret-key")
        .with_generations_file(&corrupt_gen);
    assert_eq!(
        corrupt_reg.verify_caller_at(&peer_claim, 30_000).unwrap_err().reason,
        RefusalReason::RegistryDisconnected
    );

    // Unwritable generation storage fails closed -> RegistryDisconnected
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let ro_dir = root.join("ro-gen-dir");
        fs::create_dir_all(&ro_dir).unwrap();
        fs::set_permissions(&ro_dir, fs::Permissions::from_mode(0o500)).unwrap();
        // Root ignores directory mode bits; the check is meaningful only when writes are actually denied.
        let writes_denied = fs::write(ro_dir.join(".probe"), b"").is_err();
        let ro_reg = SurfaceRegistry::new(None, Some(T3Registry::new(&t3_json)), None)
            .with_expected_node(node_ok.clone())
            .with_bridge_secret("bridge-secret-key")
            .with_generations_file(ro_dir.join("generations.json"));
        let ro_res = ro_reg.verify_caller_at(&peer_claim, 30_000);
        fs::set_permissions(&ro_dir, fs::Permissions::from_mode(0o700)).unwrap();
        if writes_denied {
            assert_eq!(ro_res.unwrap_err().reason, RefusalReason::RegistryDisconnected);
        }
    }
    // Deterministic unwritable path (regular file used as parent directory) fails closed even under root
    let file_blocker = root.join("not-a-directory");
    fs::write(&file_blocker, "blocker").unwrap();
    let blocker_reg = SurfaceRegistry::new(None, Some(T3Registry::new(&t3_json)), None)
        .with_expected_node(node_ok.clone())
        .with_bridge_secret("bridge-secret-key")
        .with_generations_file(file_blocker.join("generations.json"));
    assert_eq!(
        blocker_reg.verify_caller_at(&peer_claim, 30_000).unwrap_err().reason,
        RefusalReason::RegistryDisconnected
    );

    // Initialization interleaving: a young lock directory with a missing or empty pid file
    // is treated as still initializing (Some(false)), never dead/reclaimable!
    let lock_dir = root.join("generations.json.lock.d");
    fs::create_dir_all(&lock_dir).unwrap();
    fs::write(lock_dir.join("pid"), "").unwrap();
    assert_eq!(
        maestro_hosts::lock_dir_stale(&lock_dir, &lock_dir),
        Some(false),
        "young lock directory with empty pid file must not be treated as dead during initialization"
    );
    fs::remove_dir_all(&lock_dir).unwrap();

    // Abandoned lock directory from a killed/dead PID is reclaimed immediately without blocking
    fs::create_dir_all(&lock_dir).unwrap();
    fs::write(lock_dir.join("pid"), "4194303 1\n").unwrap();
    fs::write(lock_dir.join("token"), "dead-holder-token\n").unwrap();
    let after_reclaim = restarted_registry.try_bump_generation(&Surface::T3, thread_id).unwrap();
    assert_eq!(after_reclaim, 6);

    // A live child process holding the OS flock + lock.d blocks contenders while alive,
    // and when killed with SIGKILL (`kill()`), the OS kernel releases the flock and the next
    // caller reclaims `.lock.d` immediately!
    let flock_path = root.join("generations.json.flock");
    let hold_py = r#"
import fcntl, os, pathlib, sys, time
flock_path = pathlib.Path(sys.argv[1])
lock_dir = pathlib.Path(sys.argv[2])
f = open(flock_path, "a+b")
fcntl.flock(f.fileno(), fcntl.LOCK_EX)
lock_dir.mkdir(exist_ok=True)
(lock_dir / "pid").write_text(f"{os.getpid()} {int(time.time())}\n")
(lock_dir / "token").write_text("live-holder-token\n")
sys.stdout.write("READY\n")
sys.stdout.flush()
time.sleep(30)
"#;
    let mut live_holder = Command::new("python3")
        .arg("-c")
        .arg(hold_py)
        .arg(&flock_path)
        .arg(&lock_dir)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::BufRead as _;
        let mut reader = std::io::BufReader::new(live_holder.stdout.as_mut().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "READY");
    }
    let busy_res = restarted_registry.try_bump_generation(&Surface::T3, thread_id);
    assert!(matches!(busy_res, Err(SessionError::RegistryDisconnected { .. })));
    assert_eq!(fs::read_to_string(lock_dir.join("token")).unwrap(), "live-holder-token\n");
    // SIGKILL the live lock holder; the kernel releases the flock and the next call reclaims lock_dir
    live_holder.kill().unwrap();
    live_holder.wait().unwrap();
    let after_kill = restarted_registry.try_bump_generation(&Surface::T3, thread_id).unwrap();
    assert_eq!(after_kill, 7);

    // Lost / deleted generations.json ledger refuses previously issued credentials (both before and after re-initialization),
    // even when the credential was issued at generation 0!
    let loss_gen_file = root.join("loss-generations.json");
    let reg_before_loss = SurfaceRegistry::new(None, Some(T3Registry::new(&t3_json)), None)
        .with_expected_node(node_ok.clone())
        .with_bridge_secret("bridge-secret-key")
        .with_generations_file(&loss_gen_file);
    let v_gen0 = reg_before_loss.verify_session(&peer_claim, Some(1_000)).unwrap();
    let cred_before_loss = reg_before_loss.issue_bridge_credential(&v_gen0, node_ok.clone(), 1_000, 61_000).unwrap();
    assert_eq!(cred_before_loss.generation, 0);
    fs::remove_file(&loss_gen_file).unwrap();
    let reg_after_loss = SurfaceRegistry::new(None, Some(T3Registry::new(&t3_json)), None)
        .with_expected_node(node_ok.clone())
        .with_bridge_secret("bridge-secret-key")
        .with_generations_file(&loss_gen_file);
    let claim_before_loss = unsigned_claim.clone().with_credential(cred_before_loss);
    assert_eq!(
        reg_after_loss.verify_caller_at(&claim_before_loss, 30_000).unwrap_err().reason,
        RefusalReason::RegistryDisconnected
    );
    // Even if a fresh peer verification re-initializes loss-generations.json at generation 0 (with a new random ledger_id),
    // the generation-0 credential issued before the ledger was lost is rejected with InvalidToken!
    let _ = reg_after_loss.verify_session(&peer_claim, Some(1_000)).unwrap();
    assert_eq!(
        reg_after_loss.verify_caller_at(&claim_before_loss, 30_000).unwrap_err().reason,
        RefusalReason::InvalidToken
    );

    // Regression for PRRT_kwDOVO-UF86xOZDu / P20 MAJOR 1:
    // Without MAESTRO_GENERATIONS_FILE (in-memory generation ledger), issue a credential at generation 0,
    // replace the session under the same ID with a new incarnation, and rebuild a fresh SurfaceRegistry
    // without generations_file. Because `sign_tuple` binds the session incarnation, the rebuilt registry
    // refuses the old credential with RefusalReason::InvalidToken!
    let stateless_json = root.join("stateless-threads.json");
    fs::write(
        &stateless_json,
        serde_json::to_string_pretty(&json!([
            {
                "id": thread_id,
                "cwd": wt_dir.to_string_lossy(),
                "environment_id": "env-nuc",
                "node": "nuc-node-1",
                "provider": "gemini-vertex",
                "created_at": "2026-10-25T01:00:00Z",
                "status": "running",
                "pending_permissions": []
            }
        ]))
        .unwrap(),
    )
    .unwrap();
    let stateless_reg_1 = SurfaceRegistry::new(None, Some(T3Registry::new(&stateless_json)), None)
        .with_expected_node(node_ok.clone())
        .with_bridge_secret("stateless-bridge-secret");
    let stateless_v1 = stateless_reg_1.verify_session(&peer_claim, Some(1_000)).unwrap();
    let stateless_cred =
        stateless_reg_1.issue_bridge_credential(&stateless_v1, node_ok.clone(), 1_000, 61_000).unwrap();
    assert_eq!(stateless_cred.generation, 0);
    // Reuse the same thread_id with a new created_at incarnation and rebuild SurfaceRegistry without generations_file
    fs::write(
        &stateless_json,
        serde_json::to_string_pretty(&json!([
            {
                "id": thread_id,
                "cwd": wt_dir.to_string_lossy(),
                "environment_id": "env-nuc",
                "node": "nuc-node-1",
                "provider": "gemini-vertex",
                "created_at": "2026-10-25T02:00:00Z",
                "status": "running",
                "pending_permissions": []
            }
        ]))
        .unwrap(),
    )
    .unwrap();
    let stateless_reg_rebuilt = SurfaceRegistry::new(None, Some(T3Registry::new(&stateless_json)), None)
        .with_expected_node(node_ok.clone())
        .with_bridge_secret("stateless-bridge-secret");
    let stateless_claim = unsigned_claim.clone().with_credential(stateless_cred);
    assert_eq!(
        stateless_reg_rebuilt.verify_caller_at(&stateless_claim, 30_000).unwrap_err().reason,
        RefusalReason::InvalidToken
    );

    // T3 record without created_at or run_id incarnation marker fails closed unconditionally
    // (even when neither bridge_secret nor generations_file is configured)
    let no_inc_json = root.join("no-inc-threads.json");
    fs::write(
        &no_inc_json,
        serde_json::to_string_pretty(&json!([
            {
                "id": thread_id,
                "cwd": wt_dir.to_string_lossy(),
                "environment_id": "env-nuc",
                "node": "nuc-node-1",
                "provider": "gemini-vertex",
                "status": "running",
                "pending_permissions": []
            }
        ]))
        .unwrap(),
    )
    .unwrap();
    let no_inc_reg = SurfaceRegistry::new(None, Some(T3Registry::new(&no_inc_json)), None).with_expected_node(node_ok);
    assert_eq!(no_inc_reg.verify_caller_at(&peer_claim, 30_000).unwrap_err().reason, RefusalReason::InvalidToken);

    // 5. Disconnected registry fails closed -> RegistryDisconnected
    let disconnected_reg = SurfaceRegistry::new(None, Some(T3Registry::new(&t3_json).with_disconnected(true)), None);
    assert_eq!(disconnected_reg.verify_caller(&peer_claim).unwrap_err().reason, RefusalReason::RegistryDisconnected);

    let updates = SurfaceRegistry::diff_snapshots(None, Some(&snap1));
    assert_eq!(updates, vec![SessionUpdate::Snapshot { snapshot: Box::new(snap1.clone()) }]);
    let ended_updates = SurfaceRegistry::diff_snapshots(Some(&snap1), None);
    assert_eq!(ended_updates, vec![SessionUpdate::Ended { session: snap1.session }]);
}

#[test]
fn live_permission_prompts_and_mcp_agent_requests_use_verified_host_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let paseo_dir = root.join("paseo-agents");
    let cwd = root.join("work/arc-1");
    let outside_cwd = root.join("work/other");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&outside_cwd).unwrap();

    let id = "agent-perm-11111111";
    write_paseo_record(
        &paseo_dir,
        id,
        &cwd,
        None,
        Some("running"),
        None,
        None,
        json!([
            {
                "id": "req-7c1",
                "name": "Bash",
                "input": { "command": "gh pr merge 933" }
            },
            {
                "id": "req-7c2",
                "name": "Write",
                "input": { "file_path": format!("{}/src/lib.rs", cwd.to_string_lossy()) }
            }
        ]),
    );

    let node: NodeId = "nuc-node-1".parse().unwrap();
    let registry = SurfaceRegistry::new(Some(PaseoRegistry::new(&paseo_dir)), None, None)
        .with_expected_node(node.clone())
        .with_bridge_secret("mcp-bridge-secret");
    let peer = PeerCredentials { pid: std::process::id(), uid: 1000 };
    let verified = registry
        .verify_caller(
            &CallerClaim::paseo(Some(id), Some(cwd.to_string_lossy().into_owned()))
                .with_node(node.clone())
                .with_peer(peer),
        )
        .unwrap();
    let needs = registry.needs_for(&verified, Some("maestro"), Some("2026-10-25T12:00:00Z"));
    assert_eq!(needs.len(), 2);
    assert_eq!(needs[0].kind, NeedKind::Approval);
    assert_eq!(needs[0].source, maestro_core::need::NeedSource::Approvals);
    assert_eq!(needs[0].key, format!("approval:paseo:-/{id}#req-7c1"));
    assert_eq!(needs[0].unblocks, vec![format!("paseo:-/{id}")]);
    assert_eq!(needs[0].title, "Run a command: gh pr merge 933");
    assert_eq!(needs[1].title, "Write a file: src/lib.rs");

    // Malformed prompt entries are skipped without breaking verification, and
    // prompts without an explicit id get a stable content-hashed key independent of queue index.
    let id2 = "agent-perm-22222222";
    write_paseo_record(
        &paseo_dir,
        id2,
        &cwd,
        None,
        Some("running"),
        None,
        None,
        json!([
            { "not_a_permission": 123 },
            { "name": "Bash", "input": { "command": "git status" } },
            { "name": "Bash", "input": { "command": "git diff" } }
        ]),
    );
    let verified2 = registry
        .verify_caller(
            &CallerClaim::paseo(Some(id2), Some(cwd.to_string_lossy().into_owned()))
                .with_node(node.clone())
                .with_peer(peer),
        )
        .unwrap();
    let needs_two = registry.needs_for(&verified2, Some("maestro"), None);
    assert_eq!(needs_two.len(), 2);
    let second_key_before = needs_two[1].key.clone();

    write_paseo_record(
        &paseo_dir,
        id2,
        &cwd,
        None,
        Some("running"),
        None,
        None,
        json!([
            { "name": "Bash", "input": { "command": "git diff" } }
        ]),
    );
    let verified2_after = registry
        .verify_caller(
            &CallerClaim::paseo(Some(id2), Some(cwd.to_string_lossy().into_owned()))
                .with_node(node.clone())
                .with_peer(peer),
        )
        .unwrap();
    let needs_after = registry.needs_for(&verified2_after, Some("maestro"), None);
    assert_eq!(needs_after.len(), 1);
    assert_eq!(needs_after[0].key, second_key_before);

    // End-to-end MCP call_with_hosts and agent_request_with_hosts verification
    let ws = root.join("ws");
    fs::create_dir_all(ws.join("workspace/.control/asks")).unwrap();
    fs::write(
        ws.join("workspace/.control/asks/sample.yaml"),
        "schema_version: 1
arc: test
opened: '2026-10-25T00:00:00Z'
tz: UTC
lanes: []
asks: []
decisions: []
",
    )
    .unwrap();
    let store = Store::new(Paths::under(&ws), TreeSource::WorkingTree);

    // 1. maestro_needs_list & maestro_context include live permission prompts when unscoped,
    // exclude them from both packet and markdown when scoped to project-b, and preserve null when asks are unreadable.
    let mcp_needs = call_with_hosts(&store, &registry, "maestro_needs_list", json!({})).unwrap();
    let need_items = mcp_needs["data"].as_array().unwrap();
    assert!(
        need_items.iter().any(|n| n["key"] == format!("approval:paseo:-/{id}#req-7c1")),
        "maestro_needs_list missing live permission prompt: {mcp_needs}"
    );
    let filtered_b =
        call_with_hosts(&store, &registry, "maestro_needs_list", json!({ "project": "project-b" })).unwrap();
    let filtered_items = filtered_b["data"].as_array().unwrap();
    assert!(
        !filtered_items.iter().any(|n| n["key"] == format!("approval:paseo:-/{id}#req-7c1")),
        "project-b filter must exclude live prompt from session in project-a: {filtered_b}"
    );
    let ctx_unscoped = call_with_hosts(&store, &registry, "maestro_context", json!({})).unwrap();
    assert!(
        ctx_unscoped["packet"]["needs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["key"] == format!("approval:paseo:-/{id}#req-7c1")),
        "unscoped maestro_context should include live permission prompt"
    );
    assert!(ctx_unscoped["markdown"].as_str().unwrap().contains("gh pr merge 933"));

    let ctx_scoped_b =
        call_with_hosts(&store, &registry, "maestro_context", json!({ "project": "project-b" })).unwrap();
    assert!(
        !ctx_scoped_b["packet"]["needs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["key"] == format!("approval:paseo:-/{id}#req-7c1")),
        "project-scoped maestro_context packet must exclude unscoped live prompt"
    );
    assert!(
        !ctx_scoped_b["markdown"].as_str().unwrap().contains("gh pr merge 933"),
        "project-scoped maestro_context markdown must not leak live prompt command detail"
    );

    let unread_ws = root.join("unread-ws");
    fs::create_dir_all(&unread_ws).unwrap();
    let unread_store = Store::new(Paths::under(&unread_ws), TreeSource::WorkingTree);
    let unread_needs = call_with_hosts(&unread_store, &registry, "maestro_needs_list", json!({})).unwrap();
    assert!(unread_needs["data"].is_null(), "unreadable asks section must keep data: null");

    // 2. MCP agent_request_with_hosts requires a valid fresh signed BridgeCredential (or Direct token):
    // - Bare {session, cwd} without credential is refused (InvalidToken)
    let unsigned_mcp_err = agent_request_with_hosts(
        &store,
        &registry,
        RequestKind::Handback,
        json!({
            "session": format!("paseo:-/{id}"),
            "cwd": cwd.to_string_lossy(),
            "summary": "unsigned tuple must be refused"
        }),
    )
    .unwrap_err();
    assert!(unsigned_mcp_err.message.contains("authenticated bridge credential or launch token is required"));

    // - Valid fresh credential spanning current_time_ms() is accepted -> verified: true, Disposition::Recorded
    let now = maestro_hosts::current_time_ms();
    let valid_cred = registry.issue_bridge_credential(&verified, node.clone(), now - 5_000, now + 60_000).unwrap();
    let handback_ok = agent_request_with_hosts(
        &store,
        &registry,
        RequestKind::Handback,
        json!({
            "session": format!("paseo:-/{id}"),
            "cwd": cwd.to_string_lossy(),
            "node": node.as_str(),
            "credential": valid_cred,
            "summary": "completed host-identity verification"
        }),
    )
    .unwrap();
    assert_eq!(handback_ok["request"]["verified"], json!(true));
    assert_eq!(handback_ok["request"]["disposition"], json!(Disposition::Recorded));

    // - Expired credential on the normal MCP path (verify_caller with real clock) is refused -> StaleCredential
    let expired_cred = registry.issue_bridge_credential(&verified, node.clone(), now - 60_000, now - 1).unwrap();
    let expired_mcp_err = agent_request_with_hosts(
        &store,
        &registry,
        RequestKind::Handback,
        json!({
            "session": format!("paseo:-/{id}"),
            "cwd": cwd.to_string_lossy(),
            "node": node.as_str(),
            "credential": expired_cred,
            "summary": "expired credential must be refused"
        }),
    )
    .unwrap_err();
    assert!(expired_mcp_err.message.contains("StaleCredential"));

    // - Future credential on the normal MCP path is refused -> StaleCredential
    let future_cred = registry.issue_bridge_credential(&verified, node.clone(), now + 30_000, now + 90_000).unwrap();
    let future_mcp_err = agent_request_with_hosts(
        &store,
        &registry,
        RequestKind::Handback,
        json!({
            "session": format!("paseo:-/{id}"),
            "cwd": cwd.to_string_lossy(),
            "node": node.as_str(),
            "credential": future_cred,
            "summary": "future credential must be refused"
        }),
    )
    .unwrap_err();
    assert!(future_mcp_err.message.contains("StaleCredential"));

    // - Malformed credential fails invalid_params
    let bad_cred_err = agent_request_with_hosts(
        &store,
        &registry,
        RequestKind::Handback,
        json!({
            "session": format!("paseo:-/{id}"),
            "cwd": cwd.to_string_lossy(),
            "credential": "not-an-object",
            "summary": "malformed credential must fail"
        }),
    )
    .unwrap_err();
    assert!(bad_cred_err.message.contains("`credential`"));

    // - T3 verified session persists verified environment ("env-nuc") and refuses mismatched environment
    // Paseo record containing BOTH startedAt and createdAt deserializes cleanly and prefers startedAt,
    // and replacing a Paseo session under the same ID with absent timestamps (different persistence.sessionId)
    // bumps generation across restart and refuses the old credential with IdReused!
    let dual_id = "agent-dual-ts-11111111";
    let dual_folder = paseo_dir.join(folder_of(&cwd));
    fs::create_dir_all(&dual_folder).unwrap();
    let dual_path = dual_folder.join(format!("{dual_id}.json"));
    fs::write(
        &dual_path,
        serde_json::to_string_pretty(&json!({
            "id": dual_id,
            "provider": "claude",
            "cwd": cwd.to_string_lossy(),
            "createdAt": "2026-10-09T15:00:00.000Z",
            "startedAt": "2026-10-09T15:05:00.000Z",
            "lastStatus": "running",
            "pendingPermissions": [
                { "id": "req-dual-1", "name": "Bash", "input": { "command": "echo dual" } }
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    let paseo_only = PaseoRegistry::new(&paseo_dir);
    let dual_found = paseo_only.find(dual_id).expect("Paseo record with both startedAt and createdAt must parse");
    assert_eq!(dual_found.started_at.as_deref(), Some("2026-10-09T15:05:00.000Z"));

    // Paseo same-metadata ID reuse across restart with absent timestamps (distinguished by persistence.sessionId):
    let reuse_paseo_id = "agent-reuse-paseo-1111";
    let reuse_paseo_path = dual_folder.join(format!("{reuse_paseo_id}.json"));
    let paseo_gen_file = root.join("paseo-generations.json");
    fs::write(
        &reuse_paseo_path,
        serde_json::to_string_pretty(&json!({
            "id": reuse_paseo_id,
            "provider": "claude",
            "cwd": cwd.to_string_lossy(),
            "lastStatus": "running",
            "persistence": { "sessionId": "handle-v1" },
            "pendingPermissions": []
        }))
        .unwrap(),
    )
    .unwrap();
    let paseo_reg_1 = SurfaceRegistry::new(Some(PaseoRegistry::new(&paseo_dir)), None, None)
        .with_expected_node(node.clone())
        .with_bridge_secret("mcp-bridge-secret")
        .with_generations_file(&paseo_gen_file);
    let paseo_v1 = paseo_reg_1
        .verify_caller(
            &CallerClaim::paseo(Some(reuse_paseo_id), Some(cwd.to_string_lossy().into_owned()))
                .with_node(node.clone())
                .with_peer(peer),
        )
        .unwrap();
    let paseo_cred_1 = paseo_reg_1.issue_bridge_credential(&paseo_v1, node.clone(), now - 5_000, now + 60_000).unwrap();
    fs::write(
        &reuse_paseo_path,
        serde_json::to_string_pretty(&json!({
            "id": reuse_paseo_id,
            "provider": "claude",
            "cwd": cwd.to_string_lossy(),
            "lastStatus": "running",
            "persistence": { "sessionId": "handle-v2" },
            "pendingPermissions": []
        }))
        .unwrap(),
    )
    .unwrap();
    let paseo_reg_restarted = SurfaceRegistry::new(Some(PaseoRegistry::new(&paseo_dir)), None, None)
        .with_expected_node(node.clone())
        .with_bridge_secret("mcp-bridge-secret")
        .with_generations_file(&paseo_gen_file);
    let paseo_reuse_claim = CallerClaim::paseo(Some(reuse_paseo_id), Some(cwd.to_string_lossy().into_owned()))
        .with_node(node.clone())
        .with_credential(paseo_cred_1);
    assert_eq!(paseo_reg_restarted.verify_caller(&paseo_reuse_claim).unwrap_err().reason, RefusalReason::IdReused);

    let t3_json = root.join("t3-env-threads.json");
    fs::write(
        &t3_json,
        serde_json::to_string_pretty(&json!([
            {
                "id": "mcp:t3-env-1",
                "cwd": cwd.to_string_lossy(),
                "environment_id": "env-nuc",
                "provider": "codex",
                "created_at": "2026-10-25T00:00:00Z",
                "status": "running",
                "pending_permissions": []
            }
        ]))
        .unwrap(),
    )
    .unwrap();
    let t3_reg = SurfaceRegistry::new(None, Some(T3Registry::new(&t3_json)), None)
        .with_expected_node(node.clone())
        .with_bridge_secret("mcp-bridge-secret");
    let t3_verified = t3_reg
        .verify_caller(
            &CallerClaim::t3(Some("mcp:t3-env-1"), Some(cwd.to_string_lossy().into_owned()))
                .with_node(node.clone())
                .with_peer(peer),
        )
        .unwrap();
    let t3_cred = t3_reg.issue_bridge_credential(&t3_verified, node.clone(), now - 5_000, now + 60_000).unwrap();
    let t3_handback = agent_request_with_hosts(
        &store,
        &t3_reg,
        RequestKind::Handback,
        json!({
            "session": "t3:-/mcp:t3-env-1",
            "cwd": cwd.to_string_lossy(),
            "node": node.as_str(),
            "credential": t3_cred.clone(),
            "summary": "verified t3 environment persisted"
        }),
    )
    .unwrap();
    assert_eq!(t3_handback["request"]["session"]["environment"], json!("env-nuc"));

    let env_mismatch_err = agent_request_with_hosts(
        &store,
        &t3_reg,
        RequestKind::Handback,
        json!({
            "session": "t3:wrong-env/mcp:t3-env-1",
            "cwd": cwd.to_string_lossy(),
            "node": node.as_str(),
            "credential": t3_cred,
            "summary": "mismatched environment must fail"
        }),
    )
    .unwrap_err();
    assert!(env_mismatch_err.message.contains("does not match verified session"));

    // 3. Forged / outside-cwd caller via agent_request_with_hosts is refused and writes nothing
    let journal_path = maestro_store::requests::journal(&store.paths);
    let lines_before = fs::read_to_string(&journal_path).unwrap().lines().count();
    let forged_err = agent_request_with_hosts(
        &store,
        &registry,
        RequestKind::Handback,
        json!({
            "session": format!("paseo:-/{id}"),
            "cwd": outside_cwd.to_string_lossy(),
            "node": node.as_str(),
            "credential": valid_cred,
            "summary": "forged handback"
        }),
    )
    .unwrap_err();
    assert!(forged_err.message.contains("caller identity refused"));
    let lines_after = fs::read_to_string(&journal_path).unwrap().lines().count();
    assert_eq!(lines_before, lines_after, "forged request must write zero journal lines");
}

#[test]
fn direct_sessions_and_peer_credentials_are_verified() {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let cwd = root.join("direct-work");
    let outside = root.join("direct-work-other");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&outside).unwrap();

    let direct = DirectRegistry::new().with_expected_uid(501);
    direct.register(DirectSession {
        id: "direct-1".to_string(),
        cwd: cwd.to_string_lossy().into_owned(),
        token: "tok-secret-123".to_string(),
        liveness: SessionLiveness::Live,
        harness: Some(Harness::ClaudeCode),
        pending_permissions: vec![],
    });
    direct.register(DirectSession {
        id: "direct-closed".to_string(),
        cwd: cwd.to_string_lossy().into_owned(),
        token: "tok-closed".to_string(),
        liveness: SessionLiveness::Closed,
        harness: Some(Harness::Codex),
        pending_permissions: vec![],
    });

    let registry = SurfaceRegistry::new(None, None, Some(direct));

    // Missing peer credentials when expected_uid is configured -> PeerMismatch
    let no_peer = registry
        .verify_caller(&CallerClaim::direct(
            Some("direct-1"),
            Some(cwd.to_string_lossy().into_owned()),
            Some("tok-secret-123"),
        ))
        .unwrap_err();
    assert_eq!(no_peer.reason, RefusalReason::PeerMismatch);

    // Wrong UID -> PeerMismatch
    let wrong_uid = registry
        .verify_caller(
            &CallerClaim::direct(Some("direct-1"), Some(cwd.to_string_lossy().into_owned()), Some("tok-secret-123"))
                .with_peer(PeerCredentials { pid: 42, uid: 999 }),
        )
        .unwrap_err();
    assert_eq!(wrong_uid.reason, RefusalReason::PeerMismatch);

    // Wrong token -> InvalidToken
    let wrong_tok = registry
        .verify_caller(
            &CallerClaim::direct(Some("direct-1"), Some(cwd.to_string_lossy().into_owned()), Some("wrong-token"))
                .with_peer(PeerCredentials { pid: 42, uid: 501 }),
        )
        .unwrap_err();
    assert_eq!(wrong_tok.reason, RefusalReason::InvalidToken);

    // Closed session -> NotLive
    let closed_err = registry
        .verify_caller(
            &CallerClaim::direct(Some("direct-closed"), Some(cwd.to_string_lossy().into_owned()), Some("tok-closed"))
                .with_peer(PeerCredentials { pid: 42, uid: 501 }),
        )
        .unwrap_err();
    assert_eq!(closed_err.reason, RefusalReason::NotLive);

    // Outside cwd -> OutsideCwd
    let out_err = registry
        .verify_caller(
            &CallerClaim::direct(
                Some("direct-1"),
                Some(outside.to_string_lossy().into_owned()),
                Some("tok-secret-123"),
            )
            .with_peer(PeerCredentials { pid: 42, uid: 501 }),
        )
        .unwrap_err();
    assert_eq!(out_err.reason, RefusalReason::OutsideCwd);

    // Valid Direct claim with matching token, cwd, and peer UID succeeds
    let ok = registry
        .verify_caller(
            &CallerClaim::direct(Some("direct-1"), Some(cwd.to_string_lossy().into_owned()), Some("tok-secret-123"))
                .with_peer(PeerCredentials { pid: 42, uid: 501 }),
        )
        .unwrap();
    assert_eq!(ok.session().to_string(), "direct:-/direct-1");
    assert_eq!(ok.harness(), Some(&Harness::ClaudeCode));

    // DirectSession Debug and Serialize never leak the launch token
    let sample = DirectSession {
        id: "direct-1".to_string(),
        cwd: cwd.to_string_lossy().into_owned(),
        token: "tok-secret-123".to_string(),
        liveness: SessionLiveness::Live,
        harness: Some(Harness::ClaudeCode),
        pending_permissions: vec![],
    };
    let dbg_str = format!("{sample:?}");
    assert!(dbg_str.contains("<redacted>"));
    assert!(!dbg_str.contains("tok-secret-123"));
    let ser_str = serde_json::to_string(&sample).unwrap();
    assert!(!ser_str.contains("tok-secret-123"));

    assert!(within(&cwd, &cwd));
    assert!(!within(&outside, &cwd));
}
