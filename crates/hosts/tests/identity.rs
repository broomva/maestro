//! Host identity integration, golden-parity, spoof-refusal and permission-need tests (BRO-3268).

use maestro_core::authority::Principal;
use maestro_core::need::NeedKind;
use maestro_hosts::{
    CallerClaim, DirectRegistry, DirectSession, Harness, PARENT_AGENT_LABEL, PaseoRegistry, PeerCredentials,
    PermissionLike, PermissionPrompt, RefusalReason, SessionLiveness, SurfaceRegistry, T3Registry, describe_permission,
    within,
};
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
fn four_of_four_spoof_cases_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let paseo_dir = root.join("paseo-agents");
    let t3_json = root.join("threads.json");
    let session_cwd = root.join("work/session-a");
    let sibling_cwd = root.join("work/session-ab");
    let archived_cwd = root.join("work/archived");
    let t3_cwd = root.join("work/t3-thread");
    for d in [&paseo_dir, &session_cwd, &sibling_cwd, &archived_cwd, &t3_cwd] {
        fs::create_dir_all(d).unwrap();
    }

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
                "status": "running"
            },
            {
                "id": uuid_t3_id,
                "cwd": t3_cwd.to_string_lossy(),
                "environment_id": "env-nuc",
                "provider": "codex",
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
    assert_eq!(ok_paseo.principal(), Principal::Agent { session: ok_paseo.session().clone() });

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

    // Spoof 2: cwd outside the session's folder (including sibling prefix /work/session-ab)
    let spoof_cwd = registry
        .verify_caller(&CallerClaim::paseo(Some(live_paseo_id), Some(sibling_cwd.to_string_lossy().into_owned())))
        .unwrap_err();
    assert_eq!(spoof_cwd.reason, RefusalReason::OutsideCwd);

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
fn t3_thread_lookup_verifies_sqlite_and_json_registries() {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let userdata = root.join("userdata");
    fs::create_dir_all(&userdata).unwrap();
    fs::write(userdata.join("environment-id"), "8c5e81ab-e50f-49b4-a7e9-ba471308fac9\n").unwrap();

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

    // Worktree thread resolves to worktreePath and carries environment-id + pending permission prompt
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
}

#[test]
fn live_permission_prompts_become_needs_on_paseo_and_t3() {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let paseo_dir = root.join("paseo-agents");
    let cwd = root.join("work/arc-1");
    fs::create_dir_all(&cwd).unwrap();

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

    let registry = SurfaceRegistry::new(Some(PaseoRegistry::new(&paseo_dir)), None, None);
    let verified =
        registry.verify_caller(&CallerClaim::paseo(Some(id), Some(cwd.to_string_lossy().into_owned()))).unwrap();
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
    let verified2 =
        registry.verify_caller(&CallerClaim::paseo(Some(id2), Some(cwd.to_string_lossy().into_owned()))).unwrap();
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
    let verified2_after =
        registry.verify_caller(&CallerClaim::paseo(Some(id2), Some(cwd.to_string_lossy().into_owned()))).unwrap();
    let needs_after = registry.needs_for(&verified2_after, Some("maestro"), None);
    assert_eq!(needs_after.len(), 1);
    assert_eq!(needs_after[0].key, second_key_before);
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
