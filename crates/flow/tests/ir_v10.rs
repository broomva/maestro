//! Exhaustive Phase B acceptance tests for Maestro Flow Kernel IR v10.
//!
//! Covers every Phase B acceptance criterion in
//! `docs/specs/2026-11-26-flow-kernel-evolvable-structure.md`:
//! - Compile-time rejections (`max_items`, unpinned/cyclic/too-deep `uses:`, >256 expanded nodes,
//!   `over:` outside `needs:`, `different_from(M)` without `reserve_providers` / `provider_pool`).
//! - Crash Points A, B, and C idempotency (zero duplicate spawns, zero orphans).
//! - Concurrent `Adopted` adoption fencing (`former_owners_claim`).
//! - `Superseded` with open `EffectStarted` (`subflow_superseded`, `reconcile_only` ->
//!   `EffectReconciled`, quiescence refused on lease expiry alone until termination receipt + zero
//!   open effects).
//! - Case-folded `disjoint_writes` conflict + unquiesced `Superseded` reservation hold.
//! - Runtime `reserve_providers` / `provider_pool` liveness (`provider_pool_exhausted`).
//! - `different_from(M)` union attestation across completed/nested child performers + unattested
//!   child fail-closed.
//! - Upstream staleness incremental reuse + `stop` refusal until quiesced + spine determinism +
//!   empty 0-item map completion.

use maestro_core::session::{Audit, Harness, IdentityFacts, ProcessFacts, SessionRef, VerifiedSession};
use maestro_flow::kernel::events::{
    Adopted, Approval, Claimed, Completed, EffectStarted, KernelEvent, MapItemSpec, Owner, Performer, Started,
    Superseded, digest, marker_digest, receipt_digest,
};
use maestro_flow::kernel::fencing::{FencedWriteOutcome, FencingError, FencingStore};
use maestro_flow::kernel::ir::{IR_VERSION, Iteration};
use maestro_flow::kernel::spine::{compare, spine};
use maestro_flow::kernel::subflow::{
    SubflowError, check_child_write_fence, child_instance_id, claim_map_item, expand_map, finalize_map_item,
    fold_map_state, has_unsettled_subflows, parent_owner_epoch, quiesce_subflow, reconcile_superseded_effect,
    supersede_stale_items,
};
use maestro_flow::kernel::writeset::{path_matches_pattern, patterns_intersect};
use maestro_flow::kernel::{Completion, Limits, claim, compile, compile_with_path, complete, start};
use maestro_flow::{HostId, InstanceId, Ledger, Options, Reader};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

const LANE_YAML: &str = r#"
flow: lane
version: 1
params: [repo, item_key]
workdir: "{{params.repo}}"
nodes:
  work:
    class: work
    verify: [true]
  verify_lane:
    class: pure
    needs: [work]
    run: [true]
"#;

fn lead_v3_yaml() -> String {
    format!(
        r#"
flow: lead
version: 3
params: [repo, max_parallel]
workdir: "{{{{params.repo}}}}"
receipt_stdin: true
subflows:
  "lane@1": |
{}
nodes:
  plan_waves:
    class: pure
    run: [true]
  execute_lanes:
    class: map
    needs: [plan_waves]
    over: ".nodes.plan_waves.outputs.lanes"
    item_key: ".lane_id"
    max_items: 8
    max_concurrency_param: max_parallel
    ready_when: disjoint_writes
    reserve_providers: 1
    on_item_failure: collect_all
    item_retry: {{max: 1}}
    uses: lane@1
  cross_review:
    class: work
    needs: [execute_lanes]
    routing: {{provider: "different_from(execute_lanes)"}}
    verify: [true]
"#,
        LANE_YAML
            .lines()
            .map(|l| format!("    {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn owner(id: &str) -> Owner {
    let s: SessionRef = format!("t3:local/{id}").parse().unwrap();
    Owner { session: s, marker_sha256: marker_digest(id) }
}

fn verified_owner(id: &str) -> VerifiedSession {
    let s: SessionRef = format!("t3:local/{id}").parse().unwrap();
    VerifiedSession::new(s, "/tmp", Some(Harness::Codex), None, vec![]).with_owner_marker(id)
}

fn complete_lane_child(
    g: &mut maestro_flow::WriteGuard<'_>,
    cid: &InstanceId,
    perf: Option<Performer>,
    outputs: serde_json::Value,
) {
    let cseq1 = g.log(cid).unwrap().seq();
    let work_out = json!({"worked": true});
    let work_digest = receipt_digest(&work_out, None);
    let work_done = Completed {
        node: "work".into(),
        iteration: Iteration::new(),
        attempt: 1,
        inputs: BTreeMap::new(),
        outputs: work_out,
        verify: None,
        performer: perf.clone(),
        performers: Vec::new(),
        tool_use_id: None,
        audit: None,
    };
    g.append(cid, cseq1, work_done.to_event().unwrap()).unwrap();
    let cseq2 = g.log(cid).unwrap().seq();
    let verify_done = Completed {
        node: "verify_lane".into(),
        iteration: Iteration::new(),
        attempt: 1,
        inputs: BTreeMap::from([("work".to_string(), work_digest)]),
        outputs,
        verify: None,
        performer: perf,
        performers: Vec::new(),
        tool_use_id: None,
        audit: None,
    };
    g.append(cid, cseq2, verify_done.to_event().unwrap()).unwrap();
}

fn remove_child_worktree_receipt(g: &mut maestro_flow::WriteGuard<'_>, child: &InstanceId) -> String {
    let clog = g.log(child).unwrap();
    let cstart: Started = serde_json::from_value(clog.events[0].body.clone()).unwrap();
    let wt = cstart
        .params
        .get("child_worktree")
        .and_then(|v| v.as_str())
        .expect("child must have unique allocated child_worktree")
        .to_string();
    let p = std::path::Path::new(&wt);
    if p.exists() {
        std::fs::remove_dir_all(p).unwrap();
    }
    format!("worktree_removed:{wt}")
}

/// Returns a shell snippet that atomically executes BOTH a target state mutation AND consumes
/// `MAESTRO_FENCING_EPOCH` inside ONE SQLite `BEGIN IMMEDIATE` transaction in `MAESTRO_FENCING_DB`.
fn atomic_target_commit_py(extra_py: &str) -> String {
    format!(
        r#"python3 -c '
import os, sqlite3, sys
{extra_py}
db = os.environ["MAESTRO_FENCING_DB"]
res = os.environ["MAESTRO_FENCING_RESOURCE"]
ep = int(os.environ["MAESTRO_FENCING_EPOCH"])
op = os.environ["MAESTRO_FENCING_OP_ID"]
dig = os.environ["MAESTRO_FENCING_DIGEST"]
conn = sqlite3.connect(db, timeout=15.0)
conn.isolation_level = None
conn.execute("BEGIN IMMEDIATE")
conn.execute("CREATE TABLE IF NOT EXISTS target_writes (id INTEGER PRIMARY KEY AUTOINCREMENT, resource TEXT, epoch INTEGER, op_id TEXT)")
row = conn.execute("SELECT granted_epoch, written_epoch FROM resource_epochs WHERE resource_id = ?", (res,)).fetchone()
if not row or row[0] != ep or row[1] >= ep:
    conn.execute("ROLLBACK")
    sys.exit(2)
conn.execute("UPDATE resource_epochs SET written_epoch = ?, last_op_id = ?, last_payload_digest = ?, last_result_json = \"{{}}\" WHERE resource_id = ?", (ep, op, dig, res))
conn.execute("INSERT INTO target_writes (resource, epoch, op_id) VALUES (?, ?, ?)", (res, ep, op))
conn.execute("COMMIT")
'"#
    )
}

fn init_git_repo(wt: &std::path::Path) {
    std::fs::create_dir_all(wt.join("src/a")).unwrap();
    let git = |args: &[&str]| {
        let st = Command::new("git").args(args).current_dir(wt).status().unwrap();
        assert!(st.success(), "git {args:?} failed");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(wt.join("src/a/ok.rs"), "pub fn a() {}\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
}

fn setup_parent(yaml: &str, mut params: serde_json::Value) -> (tempfile::TempDir, Ledger, InstanceId, Owner) {
    let dir = tempfile::tempdir().unwrap();
    if params.get("repo").and_then(|v| v.as_str()) == Some("/tmp") {
        let wt = dir.path().join("repo");
        init_git_repo(&wt);
        params["repo"] = json!(wt.to_str().unwrap());
    }
    let root = dir.path().join("flows");
    let host = HostId::new("test-host").unwrap();
    let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
    let compiled = compile(yaml).expect("flow compiles");
    let me = owner("owner-1");
    let fi = InstanceId::new("fi_parent_001").unwrap();
    let started = Started {
        flow: compiled.name(),
        ir_hash: compiled.hash().to_string(),
        ir: compiled.ir().clone(),
        params,
        owner: Some(me.clone()),
        audit: None,
        parent_ref: None,
    };
    let mut g = ledger.lock().unwrap();
    g.append(&fi, 0, started.to_event().unwrap()).unwrap();
    drop(g);
    (dir, ledger, fi, me)
}

#[test]
fn t01_compile_rejects_missing_or_oversized_max_items() {
    let missing = lead_v3_yaml().replace("    max_items: 8\n", "");
    assert!(compile(&missing).is_err(), "missing max_items must fail");

    let zero = lead_v3_yaml().replace("max_items: 8", "max_items: 0");
    assert!(compile(&zero).is_err(), "max_items=0 must fail");

    let too_big = lead_v3_yaml().replace("max_items: 8", "max_items: 33");
    assert!(compile(&too_big).is_err(), "max_items>32 must fail");
}

#[test]
fn t02_compile_rejects_unpinned_cyclic_or_too_deep_uses_and_over_256_nodes() {
    let unpinned = lead_v3_yaml().replace("uses: lane@1", "uses: lane");
    assert!(compile(&unpinned).is_err(), "unpinned uses must fail");

    // Cyclic subflow (a@1 -> b@1 -> a@1)
    let dir = tempfile::tempdir().unwrap();
    let a = r#"
flow: a
version: 1
nodes:
  call_b:
    class: subflow
    uses: b@1
"#;
    let b = r#"
flow: b
version: 1
nodes:
  call_a:
    class: subflow
    uses: a@1
"#;
    std::fs::write(dir.path().join("a.yaml"), a).unwrap();
    std::fs::write(dir.path().join("b.yaml"), b).unwrap();
    let err = compile_with_path(a, Some(&dir.path().join("a.yaml"))).unwrap_err();
    assert_eq!(err.code(), "unbounded_loop", "cyclic uses must be refused: {err}");

    // Depth > 2 (d0 -> d1 -> d2 -> d3)
    let d3 = "flow: d3\nversion: 1\nnodes:\n  x: {class: pure, run: [true]}\n";
    let d2 = "flow: d2\nversion: 1\nnodes:\n  x: {class: subflow, uses: d3@1}\n";
    let d1 = "flow: d1\nversion: 1\nnodes:\n  x: {class: subflow, uses: d2@1}\n";
    let d0 = "flow: d0\nversion: 1\nnodes:\n  x: {class: subflow, uses: d1@1}\n";
    for (name, body) in [("d3.yaml", d3), ("d2.yaml", d2), ("d1.yaml", d1), ("d0.yaml", d0)] {
        std::fs::write(dir.path().join(name), body).unwrap();
    }
    assert!(
        compile_with_path(d0, Some(&dir.path().join("d0.yaml"))).is_err(),
        "depth 3 > MAX_SUBFLOW_DEPTH(2) must fail"
    );

    // >256 statically bounded expanded nodes (32 items * 9 child nodes = 288 > 256)
    let mut big_child = String::from("flow: big\nversion: 1\nnodes:\n");
    for i in 0..9 {
        big_child.push_str(&format!("  n{i}: {{class: pure, run: [true]}}\n"));
    }
    let big_parent = format!(
        r#"
flow: p
version: 1
subflows:
  "big@1": |
{}
nodes:
  prep: {{class: pure, run: [true]}}
  fan:
    class: map
    needs: [prep]
    over: ".nodes.prep.outputs.xs"
    item_key: ".id"
    max_items: 32
    uses: big@1
"#,
        big_child.lines().map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n")
    );
    assert!(compile(&big_parent).is_err(), ">256 expanded nodes must fail");
}

#[test]
fn t03_compile_rejects_over_outside_needs_and_different_from_map_without_provider_reservation() {
    let bad_over = lead_v3_yaml().replace("    needs: [plan_waves]\n", "");
    assert!(compile(&bad_over).is_err(), "over referencing node outside needs must fail");

    let no_reserve = lead_v3_yaml().replace("    reserve_providers: 1\n", "");
    assert!(
        compile(&no_reserve).is_err(),
        "different_from(map) without reserve_providers or strict-subset provider_pool must fail"
    );

    // Full-set provider_pool (all 4 providers) MUST also fail!
    let full_pool = lead_v3_yaml().replace(
        "    reserve_providers: 1",
        "    provider_pool: [claude, codex, gemini, grok]",
    );
    assert!(compile(&full_pool).is_err(), "non-strict provider_pool must fail");

    // Strict-subset provider_pool passes!
    let strict_pool = lead_v3_yaml().replace(
        "    reserve_providers: 1",
        "    provider_pool: [claude, gemini]",
    );
    assert!(compile(&strict_pool).is_ok(), "strict-subset provider_pool should compile");
}

#[test]
fn t04_compile_accepts_lead_v3_and_pins_subflow_ir_hash() {
    let compiled = compile(&lead_v3_yaml()).expect("lead@3 compiles cleanly");
    assert_eq!(compiled.ir().ir_version, IR_VERSION);
    assert_eq!(compiled.ir().ir_version, 10);
    assert!(compiled.ir().subflows.contains_key("lane@1"));
    // Older IR v1-v9 definitions keep their exact lower ir_version!
    let v1 = compile("flow: tiny\nversion: 1\nnodes:\n  a: {class: pure, run: [true]}\n").unwrap();
    assert_eq!(v1.ir().ir_version, 1);
}

#[test]
fn t05_crash_points_a_b_c_idempotency_zero_duplicates_zero_orphans() {
    let (_dir, mut ledger, parent, me) = setup_parent(&lead_v3_yaml(), json!({"repo": "/tmp", "max_parallel": 2}));
    let items = vec![MapItemSpec {
        key: "lane-a".into(),
        input_digest: "sha256:la".into(),
        input: json!({"lane_id": "lane-a"}),
        write_set: vec!["src/a/**".into()],
    }];

    // Expand map under write lock.
    let mut g = ledger.lock().unwrap();
    let epoch = expand_map(&mut g, &parent, "execute_lanes", Iteration::new(), items, BTreeMap::new(), &me).unwrap();
    assert_eq!(epoch, 0);

    // Crash Point B simulation: deterministic child log already created on disk BEFORE parent
    // appended SubflowStarted!
    let det_id = child_instance_id(&parent, "execute_lanes", &Iteration::new(), 0, "lane-a", 0).unwrap();
    let parent_log = g.log(&parent).unwrap();
    let p_started: Started = serde_json::from_value(parent_log.events[0].body.clone()).unwrap();
    let child_ir = p_started.ir.subflows.get("lane@1").unwrap().clone();
    let child_repo = p_started.params["repo"].as_str().unwrap().to_string();
    let base_sha = String::from_utf8(
        Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&child_repo).output().unwrap().stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let child_wt = g.root_path().join("worktrees").join(det_id.as_str());
    std::fs::create_dir_all(child_wt.parent().unwrap()).unwrap();
    let add_st = Command::new("git")
        .args(["worktree", "add", "--detach", "-q"])
        .arg(&child_wt)
        .arg(&base_sha)
        .current_dir(&child_repo)
        .status()
        .unwrap();
    assert!(add_st.success());
    let child_wt_str = child_wt.to_string_lossy().to_string();
    let child_started = Started {
        flow: "lane@1".into(),
        ir_hash: maestro_flow::kernel::ir::content_hash(&child_ir).unwrap(),
        ir: child_ir,
        params: json!({
            "parent_repo": child_repo,
            "repo": child_wt_str,
            "child_worktree": child_wt_str,
            "base_sha": base_sha,
            "item_key": "lane-a",
        }),
        owner: Some(me.clone()),
        audit: None,
        parent_ref: None,
    };
    g.append(&det_id, 0, child_started.to_event().unwrap()).unwrap();

    // Recovery claim MUST adopt existing deterministic child (zero duplicates!) and write parent SubflowStarted.
    let perf = Performer { harness: "claude-code".into(), model: None, session: Some("t3:local/c1".into()) };
    let (cid1, o_ep) = claim_map_item(
        &mut g,
        &parent,
        "execute_lanes",
        Iteration::new(),
        "lane-a",
        Some(perf.clone()),
        &me,
    )
    .unwrap();
    assert_eq!(cid1, det_id);
    assert_eq!(o_ep, 0);

    // Second claim on same item is idempotent!
    let (cid2, _) = claim_map_item(
        &mut g,
        &parent,
        "execute_lanes",
        Iteration::new(),
        "lane-a",
        Some(perf.clone()),
        &me,
    )
    .unwrap();
    assert_eq!(cid2, det_id);
    assert_eq!(g.instances().unwrap().len(), 2, "exactly parent + 1 child (zero duplicates/orphans)");

    // Complete child work + verify_lane nodes, then finalize twice (Crash Point C idempotency).
    complete_lane_child(&mut g, &det_id, Some(perf), json!({"sha": "abc123"}));

    let settled1 = finalize_map_item(
        &mut g,
        &parent,
        "execute_lanes",
        Iteration::new(),
        "lane-a",
        &det_id,
        None,
        BTreeMap::new(),
        &me,
    )
    .unwrap();
    assert!(settled1, "single-item map settles on first finalize");
    let settled2 = finalize_map_item(
        &mut g,
        &parent,
        "execute_lanes",
        Iteration::new(),
        "lane-a",
        &det_id,
        None,
        BTreeMap::new(),
        &me,
    )
    .unwrap();
    assert!(!settled2, "second finalize is idempotent no-op");
}

#[test]
fn t06_owner_epoch_adoption_fencing_rejects_former_owner_child_writes() {
    let (_dir, mut ledger, parent, old_owner) = setup_parent(&lead_v3_yaml(), json!({"repo": "/tmp", "max_parallel": 2}));
    let new_owner = owner("owner-2");
    let items = vec![MapItemSpec {
        key: "lane-a".into(),
        input_digest: "sha256:la".into(),
        input: json!({"lane_id": "lane-a"}),
        write_set: vec!["src/a/**".into()],
    }];

    let mut g = ledger.lock().unwrap();
    expand_map(&mut g, &parent, "execute_lanes", Iteration::new(), items, BTreeMap::new(), &old_owner).unwrap();
    let (child, old_epoch) =
        claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", None, &old_owner).unwrap();
    assert_eq!(old_epoch, 0);

    // Owner adopts parent -> owner_epoch increments 0 -> 1!
    let adopted = Adopted {
        from: old_owner.clone(),
        to: new_owner.clone(),
        approval: Approval {
            ask_id: "ask-1".into(),
            ledger: "asks.yaml".into(),
            answer_option: "adopt".into(),
            source: "main@abc".into(),
            row_hash: "sha256:ans".into(),
        },
        audit: None,
    };
    let pseq = g.log(&parent).unwrap().seq();
    g.append(&parent, pseq, adopted.to_event().unwrap()).unwrap();

    let plog = g.log(&parent).unwrap();
    let clog = g.log(&child).unwrap();
    assert_eq!(parent_owner_epoch(&plog), 1);

    // Former owner MUST be refused (NotOwner)!
    assert_eq!(
        check_child_write_fence(&plog, &clog, &old_owner, old_epoch, false).unwrap_err(),
        SubflowError::NotOwner
    );
    // Even new owner carrying stale old_epoch=0 MUST be refused with former_owners_claim!
    assert_eq!(
        check_child_write_fence(&plog, &clog, &new_owner, old_epoch, false).unwrap_err(),
        SubflowError::FormerOwnersClaim { claim_epoch: 0, current_epoch: 1 }
    );
    // New owner with fresh epoch=1 succeeds!
    assert!(check_child_write_fence(&plog, &clog, &new_owner, 1, false).is_ok());
}

#[test]
fn t07_superseded_child_with_open_effect_reconcile_only_and_quiescence_guards() {
    let eff_lane = r#"
flow: lane
version: 1
params: [repo, item_key]
workdir: "{{params.repo}}"
nodes:
  work:
    class: effect
    run: [sh, -c, "true"]
    reconcile: [sh, -c, "printf reconciled > reconcile_ran.txt"]
  verify_lane:
    class: pure
    needs: [work]
    run: [true]
"#;
    let yaml = lead_v3_yaml().replace(
        &LANE_YAML.lines().map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n"),
        &eff_lane.lines().map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n"),
    );
    let (_dir, mut ledger, parent, me) = setup_parent(&yaml, json!({"repo": "/tmp", "max_parallel": 2}));
    let items_v1 = vec![MapItemSpec {
        key: "lane-a".into(),
        input_digest: "sha256:v1".into(),
        input: json!({"lane_id": "lane-a", "rev": 1}),
        write_set: vec!["src/a/**".into()],
    }];
    let items_v2 = vec![MapItemSpec {
        key: "lane-a".into(),
        input_digest: "sha256:v2".into(),
        input: json!({"lane_id": "lane-a", "rev": 2}),
        write_set: vec!["src/a/**".into()],
    }];

    let mut g = ledger.lock().unwrap();
    expand_map(&mut g, &parent, "execute_lanes", Iteration::new(), items_v1, BTreeMap::new(), &me).unwrap();
    let (child, ep) = claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", None, &me).unwrap();

    // Record a real live worker Claimed event + open EffectStarted on the child!
    let mut live_worker = Command::new("sleep").arg("60").spawn().unwrap();
    let live_pid = live_worker.id();
    let claimed_ev = Claimed {
        node: "work".into(),
        iteration: Iteration::new(),
        attempt: 1,
        inputs: BTreeMap::new(),
        token_sha256: "sha256:worker-token".into(),
        lease_until: "2099-01-01T00:00:00Z".into(),
        cancel_at: None,
        performer: None,
        owner_epoch: Some(ep),
        tool_use_id: None,
        audit: Some(Audit {
            identity: IdentityFacts { mode: "test".into(), variable: None, thread: None },
            process: ProcessFacts { pid: live_pid, ..Default::default() },
        }),
    };
    let cseq0 = g.log(&child).unwrap().seq();
    g.append(&child, cseq0, claimed_ev.to_event().unwrap()).unwrap();
    let eff = EffectStarted {
        node: "work".into(),
        iteration: Iteration::new(),
        attempt: 1,
        inputs: BTreeMap::new(),
        guard_digest: None,
    };
    let cseq = g.log(&child).unwrap().seq();
    g.append(&child, cseq, eff.to_event().unwrap()).unwrap();

    // Upstream becomes stale -> supersedes running child!
    let (_next_ep, sups) =
        supersede_stale_items(&mut g, &parent, "execute_lanes", Iteration::new(), items_v2, BTreeMap::new(), &me)
            .unwrap();
    assert_eq!(sups, vec![child.to_string()]);
    assert!(has_unsettled_subflows(&g.log(&parent).unwrap()), "unquiesced superseded child must block stop");

    // Normal writes on Superseded child MUST fail with subflow_superseded!
    let plog = g.log(&parent).unwrap();
    let clog = g.log(&child).unwrap();
    assert_eq!(
        check_child_write_fence(&plog, &clog, &me, ep, false).unwrap_err().code(),
        "subflow_superseded"
    );

    // Quiescence MUST fail while open EffectStarted exists AND while worker is alive!
    assert_eq!(
        quiesce_subflow(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", &child, "", &me)
            .unwrap_err()
            .code(),
        "quiescence_refused"
    );
    assert_eq!(
        quiesce_subflow(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", &child, "pid:999:exited", &me)
            .unwrap_err()
            .code(),
        "quiescence_refused"
    );

    // Reconcile for a WRONG attempt (attempt=99) MUST fail closed!
    let pid_rcpt = format!("pid:{live_pid}:exited");
    assert_eq!(
        reconcile_superseded_effect(
            &mut g,
            &parent,
            &child,
            "work",
            Iteration::new(),
            99,
            BTreeMap::new(),
            json!({"ok": true}),
            &pid_rcpt,
            &me,
            ep,
        )
        .unwrap_err()
        .code(),
        "subflow_superseded"
    );
    // Reconcile while the original worker PID is STILL ALIVE MUST fail closed (Round 5 Finding #3)!
    assert_eq!(
        reconcile_superseded_effect(
            &mut g,
            &parent,
            &child,
            "work",
            Iteration::new(),
            1,
            BTreeMap::new(),
            json!({"ok": true}),
            &pid_rcpt,
            &me,
            ep,
        )
        .unwrap_err()
        .code(),
        "quiescence_refused"
    );
    let child_wt = {
        let clog = g.log(&child).unwrap();
        let cstart: Started = serde_json::from_value(clog.events[0].body.clone()).unwrap();
        PathBuf::from(cstart.params.get("child_worktree").and_then(|v| v.as_str()).unwrap())
    };
    assert!(!child_wt.join("reconcile_ran.txt").exists(), "reconcile command must not run while worker is alive");

    // Kill and reap the original worker -> post-termination reconcile now executes!
    let _ = live_worker.kill();
    let _ = live_worker.wait();
    reconcile_superseded_effect(
        &mut g,
        &parent,
        &child,
        "work",
        Iteration::new(),
        1,
        BTreeMap::new(),
        json!({"ok": true}),
        &pid_rcpt,
        &me,
        ep,
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(child_wt.join("reconcile_ran.txt")).unwrap(),
        "reconciled",
        "declared reconcile command must actually execute inside child's isolated worktree after worker exit"
    );

    // Now confirmed child-bound worktree removal + zero open effects -> SubflowQuiesced succeeds!
    let wt_rcpt = remove_child_worktree_receipt(&mut g, &child);
    quiesce_subflow(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", &child, &wt_rcpt, &me)
        .unwrap();
    let st = fold_map_state(&g.log(&parent).unwrap(), "execute_lanes", &Iteration::new());
    assert!(st.superseded_unquiesced.is_empty());
    assert!(st.quiesced_children.contains(child.as_str()));
}

#[test]
fn t08_disjoint_writes_case_folded_conflict_and_unquiesced_superseded_hold() {
    let (_dir, mut ledger, parent, me) = setup_parent(&lead_v3_yaml(), json!({"repo": "/tmp", "max_parallel": 4}));
    let items = vec![
        MapItemSpec {
            key: "lane-upper".into(),
            input_digest: "sha256:u".into(),
            input: json!({"lane_id": "lane-upper"}),
            write_set: vec!["src/Foo.rs".into()],
        },
        MapItemSpec {
            key: "lane-lower".into(),
            input_digest: "sha256:l".into(),
            input: json!({"lane_id": "lane-lower"}),
            write_set: vec!["src/foo.rs".into()],
        },
        MapItemSpec {
            key: "lane-disjoint".into(),
            input_digest: "sha256:d".into(),
            input: json!({"lane_id": "lane-disjoint"}),
            write_set: vec!["docs/**".into()],
        },
    ];

    let mut g = ledger.lock().unwrap();
    expand_map(&mut g, &parent, "execute_lanes", Iteration::new(), items.clone(), BTreeMap::new(), &me).unwrap();
    let (child_upper, _) =
        claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-upper", None, &me).unwrap();

    // Case-folded overlapping sibling MUST be refused with write_set_conflict!
    let err = claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-lower", None, &me).unwrap_err();
    assert_eq!(err.code(), "write_set_conflict");

    // Disjoint sibling succeeds concurrently!
    assert!(claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-disjoint", None, &me).is_ok());

    // Superseding lane-upper STILL holds src/foo.rs reservation until SubflowQuiesced!
    let items_v2 = vec![
        MapItemSpec {
            key: "lane-upper".into(),
            input_digest: "sha256:u2".into(),
            input: json!({"lane_id": "lane-upper", "v": 2}),
            write_set: vec!["src/Foo.rs".into()],
        },
        MapItemSpec {
            key: "lane-lower".into(),
            input_digest: "sha256:l".into(),
            input: json!({"lane_id": "lane-lower"}),
            write_set: vec!["src/foo.rs".into()],
        },
    ];
    supersede_stale_items(&mut g, &parent, "execute_lanes", Iteration::new(), items_v2, BTreeMap::new(), &me).unwrap();
    let still_blocked =
        claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-lower", None, &me).unwrap_err();
    assert_eq!(still_blocked.code(), "write_set_conflict");

    // Once lane-upper is quiesced via its own removed worktree, lane-lower unlocks immediately!
    let wt_rcpt = remove_child_worktree_receipt(&mut g, &child_upper);
    quiesce_subflow(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-upper", &child_upper, &wt_rcpt, &me)
        .unwrap();
    assert!(claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-lower", None, &me).is_ok());
}

#[test]
fn t09_reserve_providers_and_provider_pool_exhausted_enforced_at_claim() {
    let (_dir, mut ledger, parent, me) = setup_parent(&lead_v3_yaml(), json!({"repo": "/tmp", "max_parallel": 4}));
    let items: Vec<MapItemSpec> = ["i1", "i2", "i3", "i4"]
        .into_iter()
        .map(|k| MapItemSpec {
            key: k.into(),
            input_digest: format!("sha256:{k}"),
            input: json!({"lane_id": k}),
            write_set: vec![format!("src/{k}/**")],
        })
        .collect();

    let mut g = ledger.lock().unwrap();
    expand_map(&mut g, &parent, "execute_lanes", Iteration::new(), items, BTreeMap::new(), &me).unwrap();

    // PROVIDERS has 4 providers; reserve_providers=1 allows at most 3 distinct providers in map!
    for (k, harness) in [("i1", "claude-code"), ("i2", "codex"), ("i3", "gemini")] {
        let p = Performer { harness: harness.into(), model: None, session: Some(format!("t3:local/{k}")) };
        claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), k, Some(p), &me).unwrap();
    }
    // 4th distinct provider ("grok") would leave 0 providers for downstream different_from -> MUST fail!
    let fourth = Performer { harness: "grok".into(), model: None, session: Some("t3:local/i4".into()) };
    let err = claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), "i4", Some(fourth), &me).unwrap_err();
    assert_eq!(err.code(), "provider_pool_exhausted");

    // Reusing one of the already-used 3 providers succeeds (still preserves 1 unused provider)!
    let reuse = Performer { harness: "claude-code".into(), model: None, session: Some("t3:local/i4b".into()) };
    assert!(claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), "i4", Some(reuse), &me).is_ok());
}

#[test]
fn t10_spine_determinism_across_different_child_completion_orders_and_empty_map() {
    // Two runs completing lane-b then lane-a vs lane-a then lane-b MUST yield equal spines!
    let run_order = |order: [&str; 2]| {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("flows");
        let wt = dir.path().join("repo");
        init_git_repo(&wt);
        let host = HostId::new("test-host").unwrap();
        let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
        let compiled = compile(&lead_v3_yaml()).unwrap();
        let me = owner("owner-spine");
        let fi = InstanceId::new("fi_spine").unwrap();
        let started = Started {
            flow: compiled.name(),
            ir_hash: compiled.hash().to_string(),
            ir: compiled.ir().clone(),
            params: json!({"repo": wt.to_str().unwrap(), "max_parallel": 2}),
            owner: Some(me.clone()),
            audit: None,
            parent_ref: None,
        };
        let items = vec![
            MapItemSpec {
                key: "lane-a".into(),
                input_digest: "sha256:a".into(),
                input: json!({"lane_id": "lane-a"}),
                write_set: vec!["src/a/**".into()],
            },
            MapItemSpec {
                key: "lane-b".into(),
                input_digest: "sha256:b".into(),
                input: json!({"lane_id": "lane-b"}),
                write_set: vec!["src/b/**".into()],
            },
        ];
        {
            let mut g = ledger.lock().unwrap();
            g.append(&fi, 0, started.to_event().unwrap()).unwrap();
            let pw_outputs = json!({"lanes": []});
            let pw_digest = maestro_flow::kernel::events::receipt_digest(&pw_outputs, None);
            let pw = Completed {
                node: "plan_waves".into(),
                iteration: Iteration::new(),
                attempt: 1,
                inputs: BTreeMap::new(),
                outputs: pw_outputs,
                verify: None,
                performer: None,
                performers: Vec::new(),
                tool_use_id: None,
                audit: None,
            };
            let pseq = g.log(&fi).unwrap().seq();
            g.append(&fi, pseq, pw.to_event().unwrap()).unwrap();
            let map_inputs = BTreeMap::from([("plan_waves".to_string(), pw_digest)]);
            expand_map(&mut g, &fi, "execute_lanes", Iteration::new(), items, map_inputs.clone(), &me).unwrap();
            let mut children = BTreeMap::new();
            for k in ["lane-a", "lane-b"] {
                let p = Performer { harness: "claude-code".into(), model: None, session: Some(format!("t3:local/{k}")) };
                let (cid, _) = claim_map_item(&mut g, &fi, "execute_lanes", Iteration::new(), k, Some(p), &me).unwrap();
                children.insert(k, cid);
            }
            for k in order {
                let cid = &children[k];
                complete_lane_child(
                    &mut g,
                    cid,
                    Some(Performer { harness: "claude-code".into(), model: None, session: None }),
                    json!({"k": k}),
                );
                finalize_map_item(&mut g, &fi, "execute_lanes", Iteration::new(), k, cid, None, map_inputs.clone(), &me)
                    .unwrap();
            }
        }
        let reader = Reader::new(&root, Options::default()).unwrap();
        let rg = reader.lock().unwrap();
        spine(&rg, &fi).unwrap()
    };

    let s1 = run_order(["lane-a", "lane-b"]);
    let s2 = run_order(["lane-b", "lane-a"]);
    assert_eq!(compare(&s1, &s2), None, "different child completion orders must produce equal spines");
    let map_rec = s1.records.iter().find(|r| r.node == "execute_lanes").expect("execute_lanes on spine");
    assert_eq!(map_rec.map_items.len(), 2);
    assert_eq!(map_rec.map_items[0].item_key, "lane-a");
    assert_eq!(map_rec.map_items[1].item_key, "lane-b");

    // Empty 0-item map completes immediately and cleanly!
    let (_d, mut ledger, parent, me) = setup_parent(&lead_v3_yaml(), json!({"repo": "/tmp", "max_parallel": 2}));
    let mut g = ledger.lock().unwrap();
    expand_map(&mut g, &parent, "execute_lanes", Iteration::new(), vec![], BTreeMap::new(), &me).unwrap();
    let plog = g.log(&parent).unwrap();
    assert!(plog.events.iter().any(|e| e.kind == Completed::KIND));
    assert!(!has_unsettled_subflows(&plog));
}

#[test]
fn t11_different_from_map_union_attestation_in_live_document() {
    let (dir, mut ledger, parent, me) = setup_parent(&lead_v3_yaml(), json!({"repo": "/tmp", "max_parallel": 2}));
    let items = vec![
        MapItemSpec {
            key: "lane-a".into(),
            input_digest: digest(&json!(1)),
            input: json!({"lane_id": "lane-a"}),
            write_set: vec!["src/a/**".into()],
        },
        MapItemSpec {
            key: "lane-b".into(),
            input_digest: digest(&json!(2)),
            input: json!({"lane_id": "lane-b"}),
            write_set: vec!["src/b/**".into()],
        },
    ];
    {
        let mut g = ledger.lock().unwrap();
        // Complete plan_waves first so execute_lanes inputs are valid.
        let pseq = g.log(&parent).unwrap().seq();
        let pw = Completed {
            node: "plan_waves".into(),
            iteration: Iteration::new(),
            attempt: 1,
            inputs: BTreeMap::new(),
            outputs: json!({"lanes": []}),
            verify: None,
            performer: None,
            performers: Vec::new(),
            tool_use_id: None,
            audit: None,
        };
        g.append(&parent, pseq, pw.to_event().unwrap()).unwrap();
        expand_map(&mut g, &parent, "execute_lanes", Iteration::new(), items, BTreeMap::new(), &me).unwrap();
        for (k, h) in [("lane-a", "claude-code"), ("lane-b", "gemini")] {
            let perf = Performer { harness: h.into(), model: None, session: Some(format!("t3:local/{k}")) };
            let (cid, _) =
                claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), k, Some(perf.clone()), &me).unwrap();
            complete_lane_child(&mut g, &cid, Some(perf), json!({"ok": true}));
            finalize_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), k, &cid, None, BTreeMap::new(), &me)
                .unwrap();
        }
    }
    let reader = Reader::new(&dir.path().join("flows"), Options::default()).unwrap();
    let rg = reader.lock().unwrap();
    let plog = rg.log(&parent).unwrap();
    let map_completed = plog
        .events
        .iter()
        .rev()
        .find(|e| e.kind == Completed::KIND && e.body["node"] == "execute_lanes")
        .expect("execute_lanes completed");
    let performers = map_completed.body["performers"].as_array().unwrap();
    let harnesses: Vec<&str> = performers.iter().filter_map(|p| p["harness"].as_str()).collect();
    assert!(harnesses.contains(&"claude-code"));
    assert!(harnesses.contains(&"gemini"));
    assert_eq!(harnesses.len(), 2);
}

#[test]
fn t12_effect_fencing_wired_in_write_rs_and_atomic_tx() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("flows");
    let workdir = dir.path().join("work");
    std::fs::create_dir_all(&workdir).unwrap();
    let host = HostId::new("test-host").unwrap();
    let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
    let sess = verified_owner("owner-fencer");

    // Effect flow that verifies MAESTRO_FENCING_EPOCH, MAESTRO_FENCING_RESOURCE, and MAESTRO_FENCING_DB
    // 1. Real target-side atomic epoch consumption: the effect command atomically mutates
    //    `target_writes` AND consumes `written_epoch = MAESTRO_FENCING_EPOCH` in ONE SQLite tx!
    let marker = workdir.join("seen_epoch.txt");
    let target_cmd = atomic_target_commit_py(&format!(
        r#"open({:?}, "w").write(os.environ["MAESTRO_FENCING_EPOCH"])"#,
        marker.display().to_string()
    ));
    let flow_yaml = format!(
        r#"
flow: fenced_eff
version: 1
params: [repo, fencing_resource]
workdir: "{{{{params.repo}}}}"
nodes:
  ship:
    class: effect
    run: [sh, -c, {}]
    reconcile: [sh, -c, "true"]
"#,
        serde_json::to_string(&target_cmd).unwrap()
    );
    let compiled = compile(&flow_yaml).unwrap();
    let s1 = start(
        &mut ledger,
        &sess,
        &compiled,
        json!({"repo": workdir.to_str().unwrap(), "fencing_resource": "branch:main"}),
    )
    .unwrap();
    let c1 = claim(&mut ledger, &sess, &s1.instance, "ship").unwrap();
    let comp = complete(&mut ledger, &sess, &c1.token, json!({"ok": true}), &Limits::default()).unwrap();
    assert!(matches!(comp, Completion::Completed { .. }), "target-atomic fenced effect must complete: {comp:?}");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "1", "first effect received monotonic epoch 1");

    // 2. A fenced effect whose target does NOT atomically consume MAESTRO_FENCING_EPOCH MUST be
    //    refused fail-closed with `target_did_not_consume_epoch`!
    let flow_unconsumed_yaml = r#"
flow: fenced_unconsumed
version: 1
params: [repo, fencing_resource]
workdir: "{{params.repo}}"
nodes:
  ship:
    class: effect
    run: [sh, -c, "true"]
    reconcile: [sh, -c, "true"]
"#;
    let compiled_u = compile(flow_unconsumed_yaml).unwrap();
    let su = start(
        &mut ledger,
        &sess,
        &compiled_u,
        json!({"repo": workdir.to_str().unwrap(), "fencing_resource": "branch:unconsumed"}),
    )
    .unwrap();
    let cu = claim(&mut ledger, &sess, &su.instance, "ship").unwrap();
    match complete(&mut ledger, &sess, &cu.token, json!({"ok": true}), &Limits::default()).unwrap() {
        Completion::Refused { code, .. } => assert_eq!(code, "target_did_not_consume_epoch"),
        other => panic!("expected target_did_not_consume_epoch refusal, got {other:?}"),
    }

    // Mid-flight stale epoch rejection: another worker advances granted_epoch in fencing_epochs.sqlite
    // before the effect finishes -> complete() MUST refuse with `stale_fencing_epoch`!
    let db_path = root.join("fencing_epochs.sqlite");
    let flow_stale_yaml = format!(
        r#"
flow: fenced_stale
version: 1
params: [repo, fencing_resource]
workdir: "{{{{params.repo}}}}"
nodes:
  ship:
    class: effect
    run: [python3, -c, "import sqlite3; c=sqlite3.connect(r'{}'); c.execute(\"UPDATE resource_epochs SET granted_epoch = granted_epoch + 1 WHERE resource_id = 'branch:stale'\"); c.commit()"]
    reconcile: [sh, -c, "true"]
"#,
        db_path.display()
    );
    let compiled_stale = compile(&flow_stale_yaml).unwrap();
    let s2 = start(
        &mut ledger,
        &sess,
        &compiled_stale,
        json!({"repo": workdir.to_str().unwrap(), "fencing_resource": "branch:stale"}),
    )
    .unwrap();
    let c2 = claim(&mut ledger, &sess, &s2.instance, "ship").unwrap();
    let comp2 = complete(&mut ledger, &sess, &c2.token, json!({"ok": true}), &Limits::default()).unwrap();
    match comp2 {
        Completion::Refused { code, .. } => assert_eq!(code, "stale_fencing_epoch"),
        other => panic!("expected stale_fencing_epoch refusal, got {other:?}"),
    }

    // Atomic transactional mutation via commit_fenced_write_with_tx (rolls back on stale!).
    let fstore = FencingStore::open(&db_path).unwrap();
    let e_a = fstore.grant_epoch("res:tx").unwrap();
    let out = fstore
        .commit_fenced_write_with_tx("res:tx", e_a, "op1", "d1", "{}", |conn| {
            conn.execute_batch("CREATE TABLE IF NOT EXISTS side_effect (val TEXT); INSERT INTO side_effect VALUES ('a');")
                .map_err(|e| FencingError::Storage(e.to_string()))
        })
        .unwrap();
    assert_eq!(out, FencedWriteOutcome::Committed { epoch: e_a });
    let e_b = fstore.grant_epoch("res:tx").unwrap();
    assert!(e_b > e_a);
    let stale_err = fstore.commit_fenced_write_with_tx("res:tx", e_a, "op2", "d2", "{}", |_| Ok(()));
    assert!(stale_err.is_err(), "stale epoch rejected before mutate_tx");
}

#[test]
fn t13_normal_write_verbs_enforce_child_fence_and_6_surface_write_set() {
    let dir = tempfile::tempdir().unwrap();
    // Create a real git worktree repo for the child lane BEFORE setting up parent so child workdir == wt.
    let wt = dir.path().join("wt_lane");
    std::fs::create_dir_all(wt.join("src/a")).unwrap();
    let git = |args: &[&str]| {
        let st = Command::new("git").args(args).current_dir(&wt).status().unwrap();
        assert!(st.success(), "git {args:?} failed");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(wt.join("src/a/ok.rs"), "pub fn a() {}\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);

    let (_pdir, mut ledger, parent, me) =
        setup_parent(&lead_v3_yaml(), json!({"repo": wt.to_str().unwrap(), "max_parallel": 2}));
    let sess = verified_owner("owner-1");

    let items = vec![MapItemSpec {
        key: "lane-a".into(),
        input_digest: digest(&json!(1)),
        input: json!({"lane_id": "lane-a"}),
        write_set: vec!["src/a/**".into()],
    }];
    let child_id = {
        let mut g = ledger.lock().unwrap();
        expand_map(&mut g, &parent, "execute_lanes", Iteration::new(), items, BTreeMap::new(), &me).unwrap();
        let (cid, _) = claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", None, &me).unwrap();
        cid
    };

    // 1. Write an untracked file OUTSIDE declared write_set (`src/b/sneaky.rs`) inside the child's
    //    OWN unique allocated worktree (never equal to parent `wt`!) -> normal complete() MUST refuse!
    let child_wt = {
        let g = ledger.lock().unwrap();
        let clog = g.log(&child_id).unwrap();
        let cstart: Started = serde_json::from_value(clog.events[0].body.clone()).unwrap();
        let p = PathBuf::from(cstart.params.get("child_worktree").and_then(|v| v.as_str()).unwrap());
        assert_ne!(p, wt, "child must receive its own unique git worktree, never share parent repo");
        p
    };
    std::fs::create_dir_all(child_wt.join("src/b")).unwrap();
    std::fs::write(child_wt.join("src/b/sneaky.rs"), "oops\n").unwrap();
    let c = claim(&mut ledger, &sess, &child_id, "work").unwrap();
    let comp = complete(&mut ledger, &sess, &c.token, json!({"ok": true}), &Limits::default()).unwrap();
    match comp {
        Completion::Refused { code, .. } => assert_eq!(code, "write_set_violation"),
        other => panic!("expected write_set_violation refusal on child worktree escape, got {other:?}"),
    }

    // 2. Supersede the child on the parent -> normal claim() AND complete() on child MUST fail
    //    with `subflow_superseded`!
    std::fs::remove_file(child_wt.join("src/b/sneaky.rs")).unwrap();
    {
        let mut g = ledger.lock().unwrap();
        let sup = Superseded {
            node: "execute_lanes".into(),
            iteration: Iteration::new(),
            map_epoch: 1,
            item_key: "lane-a".into(),
            child_instance: child_id.to_string(),
            superseded_by_epoch: 2,
        };
        let pseq = g.log(&parent).unwrap().seq();
        g.append(&parent, pseq, sup.to_event().unwrap()).unwrap();
    }
    let err = complete(&mut ledger, &sess, &c.token, json!({"ok": true}), &Limits::default()).unwrap_err();
    assert_eq!(err.code(), "subflow_superseded");
    let claim_err = claim(&mut ledger, &sess, &child_id, "work").unwrap_err();
    assert_eq!(claim_err.code(), "subflow_superseded");
}

#[test]
fn t14_finalize_map_item_rejects_unrelated_and_unfinished_children() {
    let (_dir, mut ledger, parent, me) = setup_parent(&lead_v3_yaml(), json!({"repo": "/tmp", "max_parallel": 2}));
    let items = vec![
        MapItemSpec {
            key: "lane-a".into(),
            input_digest: digest(&json!(1)),
            input: json!({"lane_id": "lane-a"}),
            write_set: vec!["src/a/**".into()],
        },
        MapItemSpec {
            key: "lane-b".into(),
            input_digest: digest(&json!(2)),
            input: json!({"lane_id": "lane-b"}),
            write_set: vec!["src/b/**".into()],
        },
    ];
    let mut g = ledger.lock().unwrap();
    expand_map(&mut g, &parent, "execute_lanes", Iteration::new(), items, BTreeMap::new(), &me).unwrap();
    let (cid_a, _) = claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", None, &me).unwrap();
    let (cid_b, _) = claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-b", None, &me).unwrap();

    // 1. Supplying lane-b's child ID to finalize lane-a MUST fail closed (`invalid_child`)!
    let wrong_child = finalize_map_item(
        &mut g,
        &parent,
        "execute_lanes",
        Iteration::new(),
        "lane-a",
        &cid_b,
        None,
        BTreeMap::new(),
        &me,
    )
    .unwrap_err();
    assert_eq!(wrong_child.code(), "invalid_child");

    // 2. Supplying lane-a's unfinished child (no Completed event yet) MUST fail closed (`child_not_terminal`)!
    let unfinished = finalize_map_item(
        &mut g,
        &parent,
        "execute_lanes",
        Iteration::new(),
        "lane-a",
        &cid_a,
        None,
        BTreeMap::new(),
        &me,
    )
    .unwrap_err();
    assert_eq!(unfinished.code(), "child_not_terminal");
}

#[test]
fn t15_performer_union_includes_failed_attempt_when_retry_succeeds() {
    let (dir, mut ledger, parent, me) = setup_parent(&lead_v3_yaml(), json!({"repo": "/tmp", "max_parallel": 2}));
    let items = vec![MapItemSpec {
        key: "lane-a".into(),
        input_digest: digest(&json!(1)),
        input: json!({"lane_id": "lane-a"}),
        write_set: vec!["src/a/**".into()],
    }];
    {
        let mut g = ledger.lock().unwrap();
        let pseq = g.log(&parent).unwrap().seq();
        let pw = Completed {
            node: "plan_waves".into(),
            iteration: Iteration::new(),
            attempt: 1,
            inputs: BTreeMap::new(),
            outputs: json!({"lanes": []}),
            verify: None,
            performer: None,
            performers: Vec::new(),
            tool_use_id: None,
            audit: None,
        };
        g.append(&parent, pseq, pw.to_event().unwrap()).unwrap();
        expand_map(&mut g, &parent, "execute_lanes", Iteration::new(), items, BTreeMap::new(), &me).unwrap();

        // Attempt 1: Claude attempts and fails!
        let p_claude = Performer { harness: "claude-code".into(), model: None, session: Some("t3:local/c1".into()) };
        let (cid1, _) =
            claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", Some(p_claude.clone()), &me)
                .unwrap();
        let c1_seq = g.log(&cid1).unwrap().seq();
        let c1_claim = Claimed {
            node: "work".into(),
            iteration: Iteration::new(),
            attempt: 1,
            lease_until: "2099-01-01T00:00:00Z".into(),
            token_sha256: "sha256:00".into(),
            inputs: BTreeMap::new(),
            performer: Some(p_claude),
            tool_use_id: None,
            cancel_at: None,
            audit: None,
            owner_epoch: Some(0),
        };
        g.append(&cid1, c1_seq, c1_claim.to_event().unwrap()).unwrap();
        finalize_map_item(
            &mut g,
            &parent,
            "execute_lanes",
            Iteration::new(),
            "lane-a",
            &cid1,
            Some("test_failed"),
            BTreeMap::new(),
            &me,
        )
        .unwrap();

        // Retry BEFORE quiescing failed child MUST fail closed (`quiescence_refused`)!
        let p_codex = Performer { harness: "codex".into(), model: None, session: Some("t3:local/x1".into()) };
        assert_eq!(
            claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", Some(p_codex.clone()), &me)
                .unwrap_err()
                .code(),
            "quiescence_refused"
        );
        // Once failed child's own worktree is removed & quiesced, retry succeeds in a NEW unique worktree!
        let wt_rcpt = remove_child_worktree_receipt(&mut g, &cid1);
        quiesce_subflow(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", &cid1, &wt_rcpt, &me)
            .unwrap();

        // Attempt 2 (retry): Codex runs and succeeds!
        let (cid2, _) =
            claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", Some(p_codex.clone()), &me)
                .unwrap();
        complete_lane_child(&mut g, &cid2, Some(p_codex), json!({"ok": true}));
        let settled = finalize_map_item(
            &mut g,
            &parent,
            "execute_lanes",
            Iteration::new(),
            "lane-a",
            &cid2,
            None,
            BTreeMap::new(),
            &me,
        )
        .unwrap();
        assert!(settled);
    }

    let reader = Reader::new(&dir.path().join("flows"), Options::default()).unwrap();
    let rg = reader.lock().unwrap();
    let plog = rg.log(&parent).unwrap();
    let map_completed = plog
        .events
        .iter()
        .rev()
        .find(|e| e.kind == Completed::KIND && e.body["node"] == "execute_lanes")
        .expect("execute_lanes completed");
    let harnesses: Vec<&str> = map_completed.body["performers"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["harness"].as_str())
        .collect();
    assert!(harnesses.contains(&"claude-code"), "failed attempt performer MUST remain in union: {harnesses:?}");
    assert!(harnesses.contains(&"codex"), "successful retry performer MUST be in union: {harnesses:?}");
}

#[test]
fn t16_unicode_case_folding_in_write_set_overlap() {
    assert!(patterns_intersect("Ä.txt", "ä.txt").unwrap());
    assert!(patterns_intersect("SRC/Café/*.rs", "src/café/main.rs").unwrap());
    assert!(path_matches_pattern("src/CAFÉ/lib.rs", "src/café/**").unwrap());
    // Full Unicode CaseFolding.txt expansions: ß/ẞ <-> ss, ligatures, Greek sigma!
    assert!(patterns_intersect("ß.txt", "ss.txt").unwrap());
    assert!(patterns_intersect("ẞ.txt", "SS.txt").unwrap());
    assert!(patterns_intersect("ﬁle.rs", "FILE.RS").unwrap());
    assert!(patterns_intersect("Σ.txt", "ς.txt").unwrap());
}

#[test]
fn t17_stale_fencing_epoch_blocks_subprocess_before_target_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("flows");
    let workdir = dir.path().join("work");
    std::fs::create_dir_all(&workdir).unwrap();
    let host = HostId::new("test-host").unwrap();
    let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
    let sess = verified_owner("owner-precheck");

    // Pre-grant epoch 1, then advance resource to epoch 2 (simulating a newer worker winning the grant!).
    let fstore = FencingStore::open(root.join("fencing_epochs.sqlite")).unwrap();
    let e1 = fstore.grant_epoch("branch:protected").unwrap();
    let e2 = fstore.grant_epoch("branch:protected").unwrap();
    assert_eq!((e1, e2), (1, 2));

    // Stale worker bound to fencing_epoch=1 tries to run a Class::Effect that would write `mutated.txt`.
    let target = workdir.join("mutated.txt");
    let yaml = format!(
        r#"
flow: stale_target_guard
version: 1
params: [repo, fencing_resource, fencing_epoch]
workdir: "{{{{params.repo}}}}"
nodes:
  ship:
    class: effect
    run: [sh, -c, 'printf mutated > "{}"']
    reconcile: [sh, -c, "true"]
"#,
        target.display()
    );
    let compiled = compile(&yaml).unwrap();
    let s = start(
        &mut ledger,
        &sess,
        &compiled,
        json!({"repo": workdir.to_str().unwrap(), "fencing_resource": "branch:protected", "fencing_epoch": 1}),
    )
    .unwrap();
    let c = claim(&mut ledger, &sess, &s.instance, "ship").unwrap();
    let comp = complete(&mut ledger, &sess, &c.token, json!({"ok": true}), &Limits::default()).unwrap();
    match comp {
        Completion::Refused { code, .. } => assert_eq!(code, "stale_fencing_epoch"),
        other => panic!("expected stale_fencing_epoch refusal, got {other:?}"),
    }
    assert!(
        !target.exists(),
        "stale worker MUST be rejected BEFORE spawning the effect subprocess — target must NOT be mutated!"
    );
}

#[test]
fn t18_normal_kernel_claim_and_complete_drive_class_map_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let wt = dir.path().join("repo");
    init_git_repo(&wt);
    let root = dir.path().join("flows");
    let host = HostId::new("test-host").unwrap();
    let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
    let sess = verified_owner("owner-e2e");

    let compiled = compile(&lead_v3_yaml()).unwrap();
    let started = start(&mut ledger, &sess, &compiled, json!({"repo": wt.to_str().unwrap(), "max_parallel": 2}))
        .unwrap();

    // 1. Run plan_waves through normal claim -> complete, emitting one lane item.
    let c_pw = claim(&mut ledger, &sess, &started.instance, "plan_waves").unwrap();
    let r_pw = complete(
        &mut ledger,
        &sess,
        &c_pw.token,
        json!({"lanes": [{"lane_id": "lane-a", "write_set": ["src/a/**"]}]}),
        &Limits::default(),
    )
    .unwrap();
    assert!(matches!(r_pw, Completion::Completed { .. }));

    // 2. Claim Class::Map `execute_lanes` through NORMAL kernel `claim()` (auto-expands + spawns child!).
    let c_map = claim(&mut ledger, &sess, &started.instance, "execute_lanes").unwrap();
    let child_id = InstanceId::new(c_map.inputs.get("__map_child_instance").expect("spawned child id")).unwrap();

    // 3. Drive the spawned child strictly through NORMAL kernel `claim()` + `complete()`!
    let c_w = claim(&mut ledger, &sess, &child_id, "work").unwrap();
    assert!(matches!(complete(&mut ledger, &sess, &c_w.token, json!({"ok": true}), &Limits::default()).unwrap(), Completion::Completed { .. }));
    let c_v = claim(&mut ledger, &sess, &child_id, "verify_lane").unwrap();
    assert!(matches!(complete(&mut ledger, &sess, &c_v.token, json!({"sha": "ok"}), &Limits::default()).unwrap(), Completion::Completed { .. }));

    // 4. Complete Class::Map `execute_lanes` through NORMAL kernel `complete()`!
    let r_map = complete(&mut ledger, &sess, &c_map.token, json!({}), &Limits::default()).unwrap();
    assert!(matches!(r_map, Completion::Completed { .. }), "normal map complete must settle parent map: {r_map:?}");
}

#[test]
fn t19_adoption_does_not_strand_child_and_worktree_verification_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let wt = dir.path().join("repo");
    init_git_repo(&wt);
    let root = dir.path().join("flows");
    let host = HostId::new("test-host").unwrap();
    let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
    let old_sess = verified_owner("owner-old");
    let new_sess = verified_owner("owner-new");
    let old_owner = owner("owner-old");
    let new_owner = owner("owner-new");

    let compiled = compile(&lead_v3_yaml()).unwrap();
    let started = start(&mut ledger, &old_sess, &compiled, json!({"repo": wt.to_str().unwrap(), "max_parallel": 2}))
        .unwrap();
    let c_pw = claim(&mut ledger, &old_sess, &started.instance, "plan_waves").unwrap();
    complete(
        &mut ledger,
        &old_sess,
        &c_pw.token,
        json!({"lanes": [{"lane_id": "lane-a", "write_set": ["src/a/**"]}]}),
        &Limits::default(),
    )
    .unwrap();
    let c_map = claim(&mut ledger, &old_sess, &started.instance, "execute_lanes").unwrap();
    let child_id = InstanceId::new(c_map.inputs.get("__map_child_instance").unwrap()).unwrap();

    // Old owner takes a claim on child at epoch 0, then parent is Adopted by new_owner (epoch 1)!
    let old_child_claim = claim(&mut ledger, &old_sess, &child_id, "work").unwrap();
    {
        let mut g = ledger.lock().unwrap();
        let adopt = Adopted {
            from: old_owner,
            to: new_owner,
            approval: Approval {
                ledger: "asks.yaml".into(),
                ask_id: "ask-adopt".into(),
                answer_option: "adopt".into(),
                source: "owner".into(),
                row_hash: "sha256:aa".into(),
            },
            audit: None,
        };
        let pseq = g.log(&started.instance).unwrap().seq();
        g.append(&started.instance, pseq, adopt.to_event().unwrap()).unwrap();
    }

    // 1. Former owner's pre-adoption claim MUST fail (`not_owner`), AND even new owner using old claim token MUST fail (`former_owners_claim`)!
    assert_eq!(
        complete(&mut ledger, &old_sess, &old_child_claim.token, json!({"ok": true}), &Limits::default())
            .unwrap_err()
            .code(),
        "not_owner"
    );
    assert_eq!(
        complete(&mut ledger, &new_sess, &old_child_claim.token, json!({"ok": true}), &Limits::default())
            .unwrap_err()
            .code(),
        "former_owners_claim"
    );

    // 2. New owner takes a fresh claim on the EXISTING child (with expired/replaced lease via new attempt) -> NOT stranded!
    // Force old claim's lease to be superseded by creating a fresh child for a non-git worktree check too,
    // and verify new_owner can claim & complete the next node on the adopted child!
    let verify_claim = {
        // First let's verify new_sess CAN claim a fresh child on a non-git dir and gets fail-closed write_set_verification_failed!
        let bad_dir = dir.path().join("not_a_git_repo");
        std::fs::create_dir_all(&bad_dir).unwrap();
        let s_bad = start(&mut ledger, &new_sess, &compiled, json!({"repo": bad_dir.to_str().unwrap(), "max_parallel": 2}))
            .unwrap();
        let cpw2 = claim(&mut ledger, &new_sess, &s_bad.instance, "plan_waves").unwrap();
        complete(
            &mut ledger,
            &new_sess,
            &cpw2.token,
            json!({"lanes": [{"lane_id": "lane-ng", "write_set": ["src/a/**"]}]}),
            &Limits::default(),
        )
        .unwrap();
        let cm2 = claim(&mut ledger, &new_sess, &s_bad.instance, "execute_lanes").unwrap();
        let bad_child = InstanceId::new(cm2.inputs.get("__map_child_instance").unwrap()).unwrap();
        let cw2 = claim(&mut ledger, &new_sess, &bad_child, "work").unwrap();
        match complete(&mut ledger, &new_sess, &cw2.token, json!({"ok": true}), &Limits::default()).unwrap() {
            Completion::Refused { code, .. } => assert_eq!(code, "write_set_verification_failed"),
            other => panic!("missing .git worktree MUST fail closed, got {other:?}"),
        }
        // Also confirm new_sess can claim an un-claimed node on an adopted child (not stranded!).
        let s2 = start(&mut ledger, &old_sess, &compiled, json!({"repo": wt.to_str().unwrap(), "max_parallel": 2}))
            .unwrap();
        let cpw3 = claim(&mut ledger, &old_sess, &s2.instance, "plan_waves").unwrap();
        complete(
            &mut ledger,
            &old_sess,
            &cpw3.token,
            json!({"lanes": [{"lane_id": "lane-adopted", "write_set": ["src/a/**"]}]}),
            &Limits::default(),
        )
        .unwrap();
        let cm3 = claim(&mut ledger, &old_sess, &s2.instance, "execute_lanes").unwrap();
        let adopted_child = InstanceId::new(cm3.inputs.get("__map_child_instance").unwrap()).unwrap();
        {
            let mut g = ledger.lock().unwrap();
            let adopt2 = Adopted {
                from: owner("owner-old"),
                to: owner("owner-new"),
                approval: Approval {
                    ledger: "asks.yaml".into(),
                    ask_id: "ask-adopt-2".into(),
                    answer_option: "adopt".into(),
                    source: "owner".into(),
                    row_hash: "sha256:bb".into(),
                },
                audit: None,
            };
            let pseq = g.log(&s2.instance).unwrap().seq();
            g.append(&s2.instance, pseq, adopt2.to_event().unwrap()).unwrap();
        }
        let c_new = claim(&mut ledger, &new_sess, &adopted_child, "work").unwrap();
        complete(&mut ledger, &new_sess, &c_new.token, json!({"ok": true}), &Limits::default()).unwrap()
    };
    assert!(matches!(verify_claim, Completion::Completed { .. }), "new owner after adoption must NOT be stranded on existing child");
}

#[test]
fn t20_grant_epoch_blocks_while_effect_holds_resource_lock() {
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("flows");
    let workdir = dir.path().join("work");
    std::fs::create_dir_all(&workdir).unwrap();
    let host = HostId::new("test-host").unwrap();
    let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
    let sess = verified_owner("owner-serial");
    let db_path = root.join("fencing_epochs.sqlite");
    let started_flag = workdir.join("effect_running.txt");

    // Effect command signals it started, sleeps 200ms, then performs a REAL target-side atomic
    // SQLite mutation + epoch consumption in ONE BEGIN IMMEDIATE transaction!
    let target_cmd = atomic_target_commit_py(&format!(
        r#"import time
open({:?}, "w").write("started")
time.sleep(0.2)"#,
        started_flag.display().to_string()
    ));
    let yaml = format!(
        r#"
flow: serialized_effect
version: 1
params: [repo, fencing_resource]
workdir: "{{{{params.repo}}}}"
nodes:
  ship:
    class: effect
    run: [sh, -c, {}]
    reconcile: [sh, -c, "true"]
"#,
        serde_json::to_string(&target_cmd).unwrap()
    );
    let compiled = compile(&yaml).unwrap();
    let s = start(
        &mut ledger,
        &sess,
        &compiled,
        json!({"repo": workdir.to_str().unwrap(), "fencing_resource": "branch:serialized"}),
    )
    .unwrap();
    let c = claim(&mut ledger, &sess, &s.instance, "ship").unwrap();

    // Concurrent worker thread waits until effect is running, then attempts grant_epoch("branch:serialized").
    // Because grant_epoch acquires the same ResourceLockGuard, it MUST block until the effect finishes!
    let db_clone = db_path.clone();
    let flag_clone = started_flag.clone();
    let bar = Arc::new(Barrier::new(2));
    let bar_t = bar.clone();
    let handle = std::thread::spawn(move || {
        bar_t.wait();
        while !flag_clone.exists() {
            std::thread::sleep(Duration::from_millis(5));
        }
        let t0 = Instant::now();
        let fstore = FencingStore::open(&db_clone).unwrap();
        let e2 = fstore.grant_epoch("branch:serialized").unwrap();
        (e2, t0.elapsed())
    });

    bar.wait();
    let comp = complete(&mut ledger, &sess, &c.token, json!({"ok": true}), &Limits::default()).unwrap();
    assert!(matches!(comp, Completion::Completed { .. }), "effect must complete cleanly without mid-flight grant override: {comp:?}");
    let (e2, waited) = handle.join().unwrap();
    assert_eq!(e2, 2, "second worker receives epoch 2 ONLY after first worker finishes & commits epoch 1");
    assert!(waited >= Duration::from_millis(80), "grant_epoch must block while active effect holds resource lock (waited {waited:?})");
}

#[test]
fn t21_quiescence_verifies_real_os_and_fs_and_base_sha_cannot_be_spoofed() {
    let (dir, mut ledger, parent, me) = setup_parent(&lead_v3_yaml(), json!({"repo": "/tmp", "max_parallel": 2}));
    let wt = dir.path().join("repo");

    // 1. Even if caller passes a bogus/future `base_sha` in parent params, claim_map_item MUST strip
    //    it and pin the REAL initial git rev-parse HEAD!
    let real_head = String::from_utf8(
        Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&wt).output().unwrap().stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let (_d2, mut ledger2, parent2, me2) = setup_parent(
        &lead_v3_yaml(),
        json!({"repo": wt.to_str().unwrap(), "base_sha": "deadbeef00000000000000000000000000000000", "max_parallel": 2}),
    );
    {
        let mut g2 = ledger2.lock().unwrap();
        expand_map(
            &mut g2,
            &parent2,
            "execute_lanes",
            Iteration::new(),
            vec![MapItemSpec {
                key: "lane-a".into(),
                input_digest: digest(&json!(1)),
                input: json!({"lane_id": "lane-a"}),
                write_set: vec!["src/a/**".into()],
            }],
            BTreeMap::new(),
            &me2,
        )
        .unwrap();
        let (cid, _) = claim_map_item(&mut g2, &parent2, "execute_lanes", Iteration::new(), "lane-a", None, &me2).unwrap();
        let clog = g2.log(&cid).unwrap();
        let cstart: Started = serde_json::from_value(clog.events[0].body.clone()).unwrap();
        assert_eq!(
            cstart.params.get("base_sha").and_then(|v| v.as_str()),
            Some(real_head.as_str()),
            "caller-supplied base_sha must be overwritten with real initial git HEAD"
        );
    }

    // 2. Quiescence MUST reject arbitrary strings, unrelated dead PIDs, unrelated absent paths,
    //    unrelated read-only dirs, and the child's own still-writable worktree — and accept ONLY
    //    verified removal or read-only freeze of THIS child's own allocated child_worktree!
    let mut g = ledger.lock().unwrap();
    expand_map(
        &mut g,
        &parent,
        "execute_lanes",
        Iteration::new(),
        vec![MapItemSpec {
            key: "lane-a".into(),
            input_digest: digest(&json!(1)),
            input: json!({"lane_id": "lane-a"}),
            write_set: vec!["src/a/**".into()],
        }],
        BTreeMap::new(),
        &me,
    )
    .unwrap();
    let (cid, _) = claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", None, &me).unwrap();
    let child_wt_str = {
        let clog = g.log(&cid).unwrap();
        let cstart: Started = serde_json::from_value(clog.events[0].body.clone()).unwrap();
        let s = cstart.params.get("child_worktree").and_then(|v| v.as_str()).unwrap().to_string();
        assert_ne!(PathBuf::from(&s), wt, "child_worktree must be unique and distinct from parent repo");
        s
    };
    finalize_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", &cid, Some("boom"), BTreeMap::new(), &me)
        .unwrap();

    // Arbitrary string -> REFUSED!
    assert_eq!(
        quiesce_subflow(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", &cid, "anything", &me)
            .unwrap_err()
            .code(),
        "quiescence_refused"
    );
    // Unrelated dead PID (not recorded in child_log Claimed.audit.pid!) -> REFUSED!
    let mut dummy = Command::new("true").spawn().unwrap();
    let unrelated_dead = dummy.id();
    let _ = dummy.wait();
    let unrelated_pid_rcpt = format!("pid:{unrelated_dead}:exited");
    assert_eq!(
        quiesce_subflow(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", &cid, &unrelated_pid_rcpt, &me)
            .unwrap_err()
            .code(),
        "quiescence_refused"
    );
    // Unrelated nonexistent path -> REFUSED!
    let unrelated_path_rcpt = format!("worktree_removed:{}", dir.path().join("unrelated_absent").display());
    assert_eq!(
        quiesce_subflow(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", &cid, &unrelated_path_rcpt, &me)
            .unwrap_err()
            .code(),
        "quiescence_refused"
    );
    // Child's OWN worktree while it STILL exists on disk -> REFUSED!
    let own_still_exists = format!("worktree_removed:{child_wt_str}");
    assert_eq!(
        quiesce_subflow(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", &cid, &own_still_exists, &me)
            .unwrap_err()
            .code(),
        "quiescence_refused"
    );
    // Removing THIS child's OWN allocated child_worktree -> ACCEPTED!
    let own_removed = remove_child_worktree_receipt(&mut g, &cid);
    quiesce_subflow(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", &cid, &own_removed, &me).unwrap();
}

#[test]
fn t22_concurrent_normal_map_claims_dispatch_distinct_items_and_complete_unicode_fold() {
    // 1. Complete Unicode CaseFolding.txt C+F checks (including ϐ -> β and official İ -> i\u{0307}!).
    assert!(patterns_intersect("ϐ.txt", "β.txt").unwrap(), "Greek curled beta U+03D0 must fold to beta U+03B2");
    assert!(patterns_intersect("İ.txt", "i\u{0307}.txt").unwrap(), "U+0130 must follow official full CaseFolding to i + U+0307");
    assert!(!patterns_intersect("ı.txt", "i.txt").unwrap(), "Turkish dotless U+0131 stays distinct under default Unicode CaseFolding");

    // 2. Concurrent normal kernel claim() on a 2-item disjoint map dispatches TWO DISTINCT children!
    let dir = tempfile::tempdir().unwrap();
    let wt = dir.path().join("repo");
    init_git_repo(&wt);
    let root = dir.path().join("flows");
    let host = HostId::new("test-host").unwrap();
    let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
    let sess = verified_owner("owner-conc");
    let compiled = compile(&lead_v3_yaml()).unwrap();
    let started = start(&mut ledger, &sess, &compiled, json!({"repo": wt.to_str().unwrap(), "max_parallel": 2})).unwrap();

    let c_pw = claim(&mut ledger, &sess, &started.instance, "plan_waves").unwrap();
    complete(
        &mut ledger,
        &sess,
        &c_pw.token,
        json!({"lanes": [
            {"lane_id": "lane-a", "write_set": ["src/a/**"]},
            {"lane_id": "lane-b", "write_set": ["src/b/**"]}
        ]}),
        &Limits::default(),
    )
    .unwrap();

    // First normal claim -> lane-a; second concurrent normal claim (before lane-a finishes!) -> lane-b!
    let cm1 = claim(&mut ledger, &sess, &started.instance, "execute_lanes").unwrap();
    let cm2 = claim(&mut ledger, &sess, &started.instance, "execute_lanes").unwrap();
    let ik1 = cm1.inputs.get("__map_item_key").unwrap();
    let ik2 = cm2.inputs.get("__map_item_key").unwrap();
    let cid1 = InstanceId::new(cm1.inputs.get("__map_child_instance").unwrap()).unwrap();
    let cid2 = InstanceId::new(cm2.inputs.get("__map_child_instance").unwrap()).unwrap();
    assert_eq!(ik1, "lane-a");
    assert_eq!(ik2, "lane-b");
    assert_ne!(cid1, cid2, "concurrent normal map claims must dispatch distinct children");
    // Verify BOTH concurrent children have unique, distinct git worktrees (neither sharing parent repo!).
    {
        let g = ledger.lock().unwrap();
        let s1: Started = serde_json::from_value(g.log(&cid1).unwrap().events[0].body.clone()).unwrap();
        let s2: Started = serde_json::from_value(g.log(&cid2).unwrap().events[0].body.clone()).unwrap();
        let w1 = s1.params.get("child_worktree").and_then(|v| v.as_str()).unwrap();
        let w2 = s2.params.get("child_worktree").and_then(|v| v.as_str()).unwrap();
        assert_ne!(w1, w2, "concurrent siblings must have distinct per-child worktrees");
        assert_ne!(PathBuf::from(w1), wt, "child 1 must not share parent repo");
        assert_ne!(PathBuf::from(w2), wt, "child 2 must not share parent repo");
    }

    // Both children complete independently and both parent map tokens complete cleanly!
    for cid in [&cid1, &cid2] {
        let cw = claim(&mut ledger, &sess, cid, "work").unwrap();
        assert!(matches!(complete(&mut ledger, &sess, &cw.token, json!({"ok": true}), &Limits::default()).unwrap(), Completion::Completed { .. }));
        let cv = claim(&mut ledger, &sess, cid, "verify_lane").unwrap();
        assert!(matches!(complete(&mut ledger, &sess, &cv.token, json!({"ok": true}), &Limits::default()).unwrap(), Completion::Completed { .. }));
    }
    assert!(matches!(complete(&mut ledger, &sess, &cm1.token, json!({}), &Limits::default()).unwrap(), Completion::Completed { .. }));
    assert!(matches!(complete(&mut ledger, &sess, &cm2.token, json!({}), &Limits::default()).unwrap(), Completion::Completed { .. }));
}

#[test]
fn t23_round5_quiescence_reconcile_fencing_and_worktree_hardening() {
    // 1. Round 5 Finding #4: pre-existing symlink or directory at `<flows_root>/worktrees/<child_id>`
    //    MUST be refused by claim_map_item (never trusted or reused!).
    {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("repo");
        init_git_repo(&wt);
        let root = dir.path().join("flows");
        let host = HostId::new("test-host").unwrap();
        let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
        let sess = verified_owner("owner-r5-wt");
        let compiled = compile(&lead_v3_yaml()).unwrap();
        let started = start(&mut ledger, &sess, &compiled, json!({"repo": wt.to_str().unwrap(), "max_parallel": 2})).unwrap();
        let me = owner("owner-r5-wt");
        let mut g = ledger.lock().unwrap();
        expand_map(
            &mut g,
            &started.instance,
            "execute_lanes",
            Iteration::new(),
            vec![MapItemSpec {
                key: "lane-a".into(),
                input_digest: digest(&json!(1)),
                input: json!({"lane_id": "lane-a"}),
                write_set: vec!["src/a/**".into()],
            }],
            BTreeMap::new(),
            &me,
        )
        .unwrap();
        let expected_cid = child_instance_id(&started.instance, "execute_lanes", &Iteration::new(), 0, "lane-a", 0).unwrap();
        let wt_root = root.join("worktrees");
        std::fs::create_dir_all(&wt_root).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&wt, wt_root.join(expected_cid.as_str())).unwrap();
        let err = claim_map_item(&mut g, &started.instance, "execute_lanes", Iteration::new(), "lane-a", None, &me)
            .unwrap_err();
        assert!(
            err.to_string().contains("pre-existing child worktree path"),
            "symlinked pre-existing child worktree path must be refused, got: {err}"
        );
    }

    // 2. Round 5 Finding #1: worktree_removed, root-only worktree_readonly, and historical dead PID
    //    MUST ALL fail while any recorded child worker PID is alive or any file inside the worktree
    //    remains writable.
    {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("repo");
        init_git_repo(&wt);
        let root = dir.path().join("flows");
        let host = HostId::new("test-host").unwrap();
        let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
        let sess = verified_owner("owner-r5-q");
        let compiled = compile(&lead_v3_yaml()).unwrap();
        let started = start(&mut ledger, &sess, &compiled, json!({"repo": wt.to_str().unwrap(), "max_parallel": 2})).unwrap();
        let me = owner("owner-r5-q");
        let mut g = ledger.lock().unwrap();
        expand_map(
            &mut g,
            &started.instance,
            "execute_lanes",
            Iteration::new(),
            vec![MapItemSpec {
                key: "lane-a".into(),
                input_digest: digest(&json!(1)),
                input: json!({"lane_id": "lane-a"}),
                write_set: vec!["src/a/**".into()],
            }],
            BTreeMap::new(),
            &me,
        )
        .unwrap();
        let (cid, ep) = claim_map_item(&mut g, &started.instance, "execute_lanes", Iteration::new(), "lane-a", None, &me).unwrap();
        let child_wt_str = {
            let clog = g.log(&cid).unwrap();
            let cstart: Started = serde_json::from_value(clog.events[0].body.clone()).unwrap();
            cstart.params.get("child_worktree").and_then(|v| v.as_str()).unwrap().to_string()
        };

        // Attempt 1: dead historical PID; Attempt 2: STILL-ALIVE worker PID!
        let mut dead_proc = Command::new("true").spawn().unwrap();
        let dead_pid = dead_proc.id();
        let _ = dead_proc.wait();
        let mut live_proc = Command::new("sleep").arg("60").spawn().unwrap();
        let live_pid = live_proc.id();
        for (attempt, pid) in [(1u32, dead_pid), (2u32, live_pid)] {
            let cev = Claimed {
                node: "work".into(),
                iteration: Iteration::new(),
                attempt,
                inputs: BTreeMap::new(),
                token_sha256: format!("sha256:tok-{attempt}"),
                lease_until: "2099-01-01T00:00:00Z".into(),
                cancel_at: None,
                performer: None,
                owner_epoch: Some(ep),
                tool_use_id: None,
                audit: Some(Audit {
                    identity: IdentityFacts { mode: "test".into(), variable: None, thread: None },
                    process: ProcessFacts { pid, ..Default::default() },
                }),
            };
            let seq = g.log(&cid).unwrap().seq();
            g.append(&cid, seq, cev.to_event().unwrap()).unwrap();
        }
        finalize_map_item(&mut g, &started.instance, "execute_lanes", Iteration::new(), "lane-a", &cid, Some("boom"), BTreeMap::new(), &me)
            .unwrap();

        // Historical dead PID while attempt 2 worker is still alive -> REFUSED!
        assert_eq!(
            quiesce_subflow(&mut g, &started.instance, "execute_lanes", Iteration::new(), "lane-a", &cid, &format!("pid:{dead_pid}:exited"), &me)
                .unwrap_err()
                .code(),
            "quiescence_refused"
        );
        // Root-only chmod 0o555 while files inside remain writable -> REFUSED!
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&child_wt_str, std::fs::Permissions::from_mode(0o555));
            assert_eq!(
                quiesce_subflow(&mut g, &started.instance, "execute_lanes", Iteration::new(), "lane-a", &cid, &format!("worktree_readonly:{child_wt_str}"), &me)
                    .unwrap_err()
                    .code(),
                "quiescence_refused"
            );
            let _ = std::fs::set_permissions(&child_wt_str, std::fs::Permissions::from_mode(0o755));
        }
        // Even after removing child_wt, worktree_removed MUST fail while live_pid is alive!
        let removed_rcpt = remove_child_worktree_receipt(&mut g, &cid);
        assert_eq!(
            quiesce_subflow(&mut g, &started.instance, "execute_lanes", Iteration::new(), "lane-a", &cid, &removed_rcpt, &me)
                .unwrap_err()
                .code(),
            "quiescence_refused"
        );
        // Kill & reap live_pid -> NOW quiescence succeeds!
        let _ = live_proc.kill();
        let _ = live_proc.wait();
        quiesce_subflow(&mut g, &started.instance, "execute_lanes", Iteration::new(), "lane-a", &cid, &removed_rcpt, &me)
            .unwrap();
    }

    // 3. Round 5 Finding #2: child subflow Class::Effect MUST ALWAYS enforce target-side atomic
    //    epoch consumption, even when `fencing_resource` is omitted or `require_target_fencing: false`.
    {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("repo");
        init_git_repo(&wt);
        let root = dir.path().join("flows");
        let host = HostId::new("test-host").unwrap();
        let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
        let sess = verified_owner("owner-r5-fence");

        let child_eff_yaml = r#"
flow: child_eff
version: 1
params: [repo, item_key, require_target_fencing]
workdir: "{{params.repo}}"
nodes:
  do_effect:
    class: effect
    run: ["sh", "-c", "echo unfenced-write > unfenced.txt"]
    reconcile: ["sh", "-c", "true"]
"#;
        let parent_yaml = format!(
            r#"
flow: parent_eff
version: 1
params: [repo, max_parallel, require_target_fencing]
workdir: "{{{{params.repo}}}}"
nodes:
  plan:
    class: work
    verify: ["sh", "-c", "true"]
  run_map:
    class: map
    needs: [plan]
    over: ".nodes.plan.outputs.lanes"
    uses: child_eff@1
    item_key: ".lane_id"
    max_items: 4
    max_concurrency_param: max_parallel
    ready_when: disjoint_writes
subflows:
  child_eff@1: |
{}
"#,
            child_eff_yaml
                .lines()
                .map(|l| format!("    {l}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let compiled = compile(&parent_yaml).unwrap();
        let started = start(
            &mut ledger,
            &sess,
            &compiled,
            json!({"repo": wt.to_str().unwrap(), "max_parallel": 2, "require_target_fencing": false}),
        )
        .unwrap();
        let cp = claim(&mut ledger, &sess, &started.instance, "plan").unwrap();
        complete(
            &mut ledger,
            &sess,
            &cp.token,
            json!({"lanes": [{"lane_id": "lane-a", "write_set": ["unfenced.txt"]}]}),
            &Limits::default(),
        )
        .unwrap();
        let cm = claim(&mut ledger, &sess, &started.instance, "run_map").unwrap();
        let cid = InstanceId::new(cm.inputs.get("__map_child_instance").unwrap()).unwrap();
        let ce = claim(&mut ledger, &sess, &cid, "do_effect").unwrap();
        let res = complete(&mut ledger, &sess, &ce.token, json!({"ok": true}), &Limits::default()).unwrap();
        match res {
            Completion::Refused { code, .. } => {
                assert_eq!(
                    code, "target_did_not_consume_epoch",
                    "child effect without fencing_resource or with require_target_fencing=false MUST fail closed on missing target atomic consumption"
                );
            }
            other => panic!("expected target_did_not_consume_epoch refusal on unfenced child effect, got {other:?}"),
        }
    }
}

