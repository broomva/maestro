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
        LANE_YAML.lines().map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n")
    )
}

fn owner(id: &str) -> Owner {
    let s: SessionRef = format!("t3:local/{id}").parse().unwrap();
    Owner { session: s, marker_sha256: marker_digest(id) }
}

fn make_test_cgroup(root: &std::path::Path, name: &str, pids: &[u32], populated: bool) -> PathBuf {
    let cg = root.join(name);
    std::fs::create_dir_all(&cg).unwrap();
    let procs = pids.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    std::fs::write(cg.join("cgroup.procs"), if procs.is_empty() { String::new() } else { format!("{procs}\n") })
        .unwrap();
    let pop = if populated { 1 } else { 0 };
    std::fs::write(cg.join("cgroup.events"), format!("populated {pop}\nfrozen 0\n")).unwrap();
    cg
}

fn empty_test_cgroup(cg: &std::path::Path) {
    std::fs::write(cg.join("cgroup.procs"), "").unwrap();
    std::fs::write(cg.join("cgroup.events"), "populated 0\nfrozen 0\n").unwrap();
}

fn verified_owner(id: &str) -> VerifiedSession {
    let s: SessionRef = format!("t3:local/{id}").parse().unwrap();
    let live_pgid = rustix::process::getpgrp().as_raw_nonzero().get() as u32;
    let cg_root = std::env::temp_dir().join(format!("maestro-ir-v10-cg-{}", std::process::id()));
    let cg = make_test_cgroup(&cg_root, &format!("maestro-worker-{id}"), &[std::process::id()], true);
    VerifiedSession::new(s, "/tmp", Some(Harness::Codex), None, vec![])
        .with_owner_marker(id)
        .with_worker_pgid(live_pgid)
        .with_worker_cgroup(cg.to_string_lossy())
}

fn verified_owner_with_pgid(id: &str, pgid: u32) -> VerifiedSession {
    let s: SessionRef = format!("t3:local/{id}").parse().unwrap();
    VerifiedSession::new(s, "/tmp", Some(Harness::Codex), None, vec![]).with_owner_marker(id).with_worker_pgid(pgid)
}

fn verified_owner_no_pgid(id: &str) -> VerifiedSession {
    let s: SessionRef = format!("t3:local/{id}").parse().unwrap();
    VerifiedSession::new(s, "/tmp", Some(Harness::Codex), None, vec![]).with_owner_marker(id)
}

fn spawn_worker_pgrp() -> (std::process::Child, u32) {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let mut child = Command::new("python3")
        .args([
            "-c",
            "import os, sys, time; os.setpgrp(); sys.stdout.write(str(os.getpgrp()) + '\\n'); sys.stdout.flush(); time.sleep(60)",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    let pgid: u32 = line.trim().parse().unwrap();
    (child, pgid)
}

fn spawn_dead_pgrp() -> u32 {
    let out = Command::new("python3").args(["-c", "import os; os.setpgrp(); print(os.getpgrp())"]).output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().parse().unwrap()
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
    let full_pool =
        lead_v3_yaml().replace("    reserve_providers: 1", "    provider_pool: [claude, codex, gemini, grok]");
    assert!(compile(&full_pool).is_err(), "non-strict provider_pool must fail");

    // Strict-subset provider_pool passes!
    let strict_pool = lead_v3_yaml().replace("    reserve_providers: 1", "    provider_pool: [claude, gemini]");
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
    let (cid1, o_ep) =
        claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", Some(perf.clone()), &me).unwrap();
    assert_eq!(cid1, det_id);
    assert_eq!(o_ep, 0);

    // Second claim on same item is idempotent!
    let (cid2, _) =
        claim_map_item(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", Some(perf.clone()), &me).unwrap();
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
    let (_dir, mut ledger, parent, old_owner) =
        setup_parent(&lead_v3_yaml(), json!({"repo": "/tmp", "max_parallel": 2}));
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

    // Record a real live worker process group + cgroup + open EffectStarted on the child!
    let (mut live_worker, live_pgid) = spawn_worker_pgrp();
    let cg_t7 = make_test_cgroup(_dir.path(), "cg-t7", &[live_pgid], true);
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
        worker_pgid: Some(live_pgid),
        worker_cgroup: Some(cg_t7.to_string_lossy().to_string()),
        tool_use_id: None,
        audit: None,
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
    assert_eq!(check_child_write_fence(&plog, &clog, &me, ep, false).unwrap_err().code(), "subflow_superseded");

    // Quiescence MUST fail while open EffectStarted exists AND while worker is alive!
    assert_eq!(
        quiesce_subflow(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", &child, "", &me)
            .unwrap_err()
            .code(),
        "quiescence_refused"
    );
    assert_eq!(
        quiesce_subflow(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", &child, "pgid:999:exited", &me)
            .unwrap_err()
            .code(),
        "quiescence_refused"
    );

    // Reconcile for a WRONG attempt (attempt=99) MUST fail closed!
    let pgid_rcpt = format!("pgid:{live_pgid}:exited");
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
            &pgid_rcpt,
            &me,
            ep,
        )
        .unwrap_err()
        .code(),
        "subflow_superseded"
    );
    // Reconcile while the original worker process group is STILL ALIVE MUST fail closed!
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
            &pgid_rcpt,
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

    // Kill and reap the original worker process group + empty cgroup -> post-termination reconcile now executes!
    let _ = live_worker.kill();
    let _ = live_worker.wait();
    empty_test_cgroup(&cg_t7);
    reconcile_superseded_effect(
        &mut g,
        &parent,
        &child,
        "work",
        Iteration::new(),
        1,
        BTreeMap::new(),
        json!({"ok": true}),
        &pgid_rcpt,
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
    quiesce_subflow(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", &child, &wt_rcpt, &me).unwrap();
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
                let p =
                    Performer { harness: "claude-code".into(), model: None, session: Some(format!("t3:local/{k}")) };
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
                finalize_map_item(
                    &mut g,
                    &fi,
                    "execute_lanes",
                    Iteration::new(),
                    k,
                    cid,
                    None,
                    map_inputs.clone(),
                    &me,
                )
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

    // 1. Built-in kernel atomic SQLite KV actuator: executes directly in Rust inside ONE SQLite
    //    BEGIN IMMEDIATE transaction (zero external shell, zero wrapper script!).
    let flow_kv_yaml = r#"
flow: fenced_kv
version: 1
params: [repo, fencing_resource]
workdir: "{{params.repo}}"
nodes:
  ship:
    class: effect
    run: ["maestro:fenced-sqlite-kv", "release_tag", "v1.0.0"]
    reconcile: [sh, -c, "true"]
"#;
    let compiled_kv = compile(flow_kv_yaml).unwrap();
    let s1 = start(
        &mut ledger,
        &sess,
        &compiled_kv,
        json!({"repo": workdir.to_str().unwrap(), "fencing_resource": "branch:main"}),
    )
    .unwrap();
    let c1 = claim(&mut ledger, &sess, &s1.instance, "ship").unwrap();
    let comp = complete(&mut ledger, &sess, &c1.token, json!({"ok": true}), &Limits::default()).unwrap();
    assert!(matches!(comp, Completion::Completed { .. }), "builtin atomic SQLite KV actuator must complete: {comp:?}");
    let fstore_check = FencingStore::open(root.join("fencing_epochs.sqlite")).unwrap();
    let kv = fstore_check.read_fenced_kv("branch:main", "release_tag").unwrap().expect("kv row must exist");
    assert_eq!(kv.0, "v1.0.0");
    assert_eq!(kv.1, 1, "first atomic KV write must consume epoch 1");

    // 2. Built-in kernel atomic git-ref CAS actuator: creates refs/heads/fenced-ship from 0{40} -> HEAD,
    //    refuses CAS mismatch, and refuses external shell commands on fenced resources!
    let repo_dir = dir.path().join("git-repo");
    init_git_repo(&repo_dir);
    let head_oid = String::from_utf8(
        Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&repo_dir).output().unwrap().stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let zero_oid = "0".repeat(head_oid.len());
    let flow_git_yaml = format!(
        r#"
flow: fenced_git
version: 1
params: [repo, fencing_resource]
workdir: "{{{{params.repo}}}}"
nodes:
  ship:
    class: effect
    run: ["maestro:fenced-git-ref-cas", "refs/heads/fenced-ship", "{zero_oid}", "{head_oid}"]
    reconcile: [sh, -c, "true"]
"#
    );
    let compiled_git = compile(&flow_git_yaml).unwrap();
    let sg = start(
        &mut ledger,
        &sess,
        &compiled_git,
        json!({"repo": repo_dir.to_str().unwrap(), "fencing_resource": "git:fenced-ship"}),
    )
    .unwrap();
    let cg = claim(&mut ledger, &sess, &sg.instance, "ship").unwrap();
    let comp_g = complete(&mut ledger, &sess, &cg.token, json!({"ok": true}), &Limits::default()).unwrap();
    assert!(matches!(comp_g, Completion::Completed { .. }), "builtin git-ref CAS actuator must complete: {comp_g:?}");
    let created_oid = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "refs/heads/fenced-ship"])
            .current_dir(&repo_dir)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    assert_eq!(created_oid, head_oid, "refs/heads/fenced-ship must point to HEAD");

    // Second CAS with stale expected_old_oid (still 0{40} when ref now exists!) MUST fail closed!
    let sg2 = start(
        &mut ledger,
        &sess,
        &compiled_git,
        json!({"repo": repo_dir.to_str().unwrap(), "fencing_resource": "git:fenced-ship"}),
    )
    .unwrap();
    let cg2 = claim(&mut ledger, &sess, &sg2.instance, "ship").unwrap();
    match complete(&mut ledger, &sess, &cg2.token, json!({"ok": true}), &Limits::default()).unwrap() {
        Completion::Refused { code, .. } => assert_eq!(code, "fenced_actuator_failed"),
        other => panic!("expected fenced_actuator_failed on CAS mismatch, got {other:?}"),
    }

    // 3. External shell command on a fenced resource MUST be refused BEFORE execution!
    let ext_yaml = r#"
flow: fenced_ext
version: 1
params: [repo, fencing_resource]
workdir: "{{params.repo}}"
nodes:
  ship:
    class: effect
    run: [sh, -c, "echo bad > leaked.txt"]
    reconcile: [sh, -c, "true"]
"#;
    let compiled_ext = compile(ext_yaml).unwrap();
    let se = start(
        &mut ledger,
        &sess,
        &compiled_ext,
        json!({"repo": workdir.to_str().unwrap(), "fencing_resource": "branch:ext"}),
    )
    .unwrap();
    let ce = claim(&mut ledger, &sess, &se.instance, "ship").unwrap();
    match complete(&mut ledger, &sess, &ce.token, json!({"ok": true}), &Limits::default()).unwrap() {
        Completion::Refused { code, .. } => assert_eq!(code, "unfenced_external_effect_requires_reconcile"),
        other => panic!("expected unfenced_external_effect_requires_reconcile, got {other:?}"),
    }
    assert!(!workdir.join("leaked.txt").exists(), "external shell command must NEVER execute on fenced path");

    // Atomic transactional mutation via commit_fenced_write_with_tx (rolls back on stale!).
    let db_path = root.join("fencing_epochs.sqlite");
    let fstore = FencingStore::open(&db_path).unwrap();
    let e_a = fstore.grant_epoch("res:tx").unwrap();
    let out = fstore
        .commit_fenced_write_with_tx("res:tx", e_a, "op1", "d1", "{}", |conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS side_effect (val TEXT); INSERT INTO side_effect VALUES ('a');",
            )
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
        let cg_c1 = make_test_cgroup(dir.path(), "cg-t14-c1", &[], false);
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
            worker_pgid: Some(spawn_dead_pgrp()),
            worker_cgroup: Some(cg_c1.to_string_lossy().to_string()),
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
        quiesce_subflow(&mut g, &parent, "execute_lanes", Iteration::new(), "lane-a", &cid1, &wt_rcpt, &me).unwrap();

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
    let harnesses: Vec<&str> =
        map_completed.body["performers"].as_array().unwrap().iter().filter_map(|p| p["harness"].as_str()).collect();
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

    // Stale worker bound to fencing_epoch=1 tries to run a built-in atomic SQLite KV actuator.
    let yaml = r#"
flow: stale_target_guard
version: 1
params: [repo, fencing_resource, fencing_epoch]
workdir: "{{params.repo}}"
nodes:
  ship:
    class: effect
    run: ["maestro:fenced-sqlite-kv", "status", "stale-write"]
    reconcile: [sh, -c, "true"]
"#;
    let compiled = compile(yaml).unwrap();
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
        fstore.read_fenced_kv("branch:protected", "status").unwrap().is_none(),
        "stale worker MUST NOT mutate fenced_kv when epoch is already superseded"
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
    let started =
        start(&mut ledger, &sess, &compiled, json!({"repo": wt.to_str().unwrap(), "max_parallel": 2})).unwrap();

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
    assert!(matches!(
        complete(&mut ledger, &sess, &c_w.token, json!({"ok": true}), &Limits::default()).unwrap(),
        Completion::Completed { .. }
    ));
    let c_v = claim(&mut ledger, &sess, &child_id, "verify_lane").unwrap();
    assert!(matches!(
        complete(&mut ledger, &sess, &c_v.token, json!({"sha": "ok"}), &Limits::default()).unwrap(),
        Completion::Completed { .. }
    ));

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
    let started =
        start(&mut ledger, &old_sess, &compiled, json!({"repo": wt.to_str().unwrap(), "max_parallel": 2})).unwrap();
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
        let s_bad =
            start(&mut ledger, &new_sess, &compiled, json!({"repo": bad_dir.to_str().unwrap(), "max_parallel": 2}))
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
        let s2 =
            start(&mut ledger, &old_sess, &compiled, json!({"repo": wt.to_str().unwrap(), "max_parallel": 2})).unwrap();
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
    assert!(
        matches!(verify_claim, Completion::Completed { .. }),
        "new owner after adoption must NOT be stranded on existing child"
    );
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

    // 1. Per-resource ResourceLockGuard serializes grant_epoch across threads until release.
    let fstore = FencingStore::open(&db_path).unwrap();
    let bar = Arc::new(Barrier::new(2));
    let db_bg = db_path.clone();
    let bar_bg = bar.clone();
    let holder = std::thread::spawn(move || {
        let store = FencingStore::open(&db_bg).unwrap();
        let rlock = store.acquire_resource_lock("branch:serialized").unwrap();
        let e1 = store.grant_epoch_locked(&rlock, "branch:serialized").unwrap();
        assert_eq!(e1, 1);
        bar_bg.wait();
        std::thread::sleep(Duration::from_millis(200));
        store.execute_atomic_sqlite_kv("branch:serialized", e1, "op-1", "sha256:d1", "{}", "k", "v1").unwrap();
        drop(rlock);
    });
    bar.wait();
    let t0 = Instant::now();
    let e2 = fstore.grant_epoch("branch:serialized").unwrap();
    let waited = t0.elapsed();
    holder.join().unwrap();
    assert_eq!(e2, 2, "second worker receives epoch 2 ONLY after first worker finishes & commits epoch 1");
    assert!(
        waited >= Duration::from_millis(80),
        "grant_epoch must block while active effect holds resource lock (waited {waited:?})"
    );
    assert_eq!(fstore.read_fenced_kv("branch:serialized", "k").unwrap().unwrap().0, "v1");

    // 2. Built-in atomic actuator runs cleanly through complete() and consumes epoch 3.
    let yaml = r#"
flow: serialized_effect
version: 1
params: [repo, fencing_resource]
workdir: "{{params.repo}}"
nodes:
  ship:
    class: effect
    run: ["maestro:fenced-sqlite-kv", "k", "v3"]
    reconcile: [sh, -c, "true"]
"#;
    let compiled = compile(yaml).unwrap();
    let s = start(
        &mut ledger,
        &sess,
        &compiled,
        json!({"repo": workdir.to_str().unwrap(), "fencing_resource": "branch:serialized"}),
    )
    .unwrap();
    let c = claim(&mut ledger, &sess, &s.instance, "ship").unwrap();
    let comp = complete(&mut ledger, &sess, &c.token, json!({"ok": true}), &Limits::default()).unwrap();
    assert!(matches!(comp, Completion::Completed { .. }), "effect must complete cleanly: {comp:?}");
    let row = fstore.read_fenced_kv("branch:serialized", "k").unwrap().unwrap();
    assert_eq!(row.0, "v3");
    assert_eq!(row.1, 3);
}

#[test]
fn t21_quiescence_verifies_real_os_and_fs_and_base_sha_cannot_be_spoofed() {
    let (dir, mut ledger, parent, me) = setup_parent(&lead_v3_yaml(), json!({"repo": "/tmp", "max_parallel": 2}));
    let wt = dir.path().join("repo");

    // 1. Even if caller passes a bogus/future `base_sha` in parent params, claim_map_item MUST strip
    //    it and pin the REAL initial git rev-parse HEAD!
    let real_head =
        String::from_utf8(Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&wt).output().unwrap().stdout)
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
        let (cid, _) =
            claim_map_item(&mut g2, &parent2, "execute_lanes", Iteration::new(), "lane-a", None, &me2).unwrap();
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
    finalize_map_item(
        &mut g,
        &parent,
        "execute_lanes",
        Iteration::new(),
        "lane-a",
        &cid,
        Some("boom"),
        BTreeMap::new(),
        &me,
    )
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
    assert!(
        patterns_intersect("İ.txt", "i\u{0307}.txt").unwrap(),
        "U+0130 must follow official full CaseFolding to i + U+0307"
    );
    assert!(
        !patterns_intersect("ı.txt", "i.txt").unwrap(),
        "Turkish dotless U+0131 stays distinct under default Unicode CaseFolding"
    );

    // 2. Concurrent normal kernel claim() on a 2-item disjoint map dispatches TWO DISTINCT children!
    let dir = tempfile::tempdir().unwrap();
    let wt = dir.path().join("repo");
    init_git_repo(&wt);
    let root = dir.path().join("flows");
    let host = HostId::new("test-host").unwrap();
    let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
    let sess = verified_owner("owner-conc");
    let compiled = compile(&lead_v3_yaml()).unwrap();
    let started =
        start(&mut ledger, &sess, &compiled, json!({"repo": wt.to_str().unwrap(), "max_parallel": 2})).unwrap();

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
        assert!(matches!(
            complete(&mut ledger, &sess, &cw.token, json!({"ok": true}), &Limits::default()).unwrap(),
            Completion::Completed { .. }
        ));
        let cv = claim(&mut ledger, &sess, cid, "verify_lane").unwrap();
        assert!(matches!(
            complete(&mut ledger, &sess, &cv.token, json!({"ok": true}), &Limits::default()).unwrap(),
            Completion::Completed { .. }
        ));
    }
    assert!(matches!(
        complete(&mut ledger, &sess, &cm1.token, json!({}), &Limits::default()).unwrap(),
        Completion::Completed { .. }
    ));
    assert!(matches!(
        complete(&mut ledger, &sess, &cm2.token, json!({}), &Limits::default()).unwrap(),
        Completion::Completed { .. }
    ));
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
        let started =
            start(&mut ledger, &sess, &compiled, json!({"repo": wt.to_str().unwrap(), "max_parallel": 2})).unwrap();
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
        let expected_cid =
            child_instance_id(&started.instance, "execute_lanes", &Iteration::new(), 0, "lane-a", 0).unwrap();
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
        let started =
            start(&mut ledger, &sess, &compiled, json!({"repo": wt.to_str().unwrap(), "max_parallel": 2})).unwrap();
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
        let (cid, ep) =
            claim_map_item(&mut g, &started.instance, "execute_lanes", Iteration::new(), "lane-a", None, &me).unwrap();
        let child_wt_str = {
            let clog = g.log(&cid).unwrap();
            let cstart: Started = serde_json::from_value(clog.events[0].body.clone()).unwrap();
            cstart.params.get("child_worktree").and_then(|v| v.as_str()).unwrap().to_string()
        };

        // Attempt 1: dead historical process group; Attempt 2: STILL-ALIVE worker process group!
        let dead_pgid = spawn_dead_pgrp();
        let (mut live_proc, live_pgid) = spawn_worker_pgrp();
        let cg_dead = make_test_cgroup(dir.path(), "cg-t23-dead", &[], false);
        let cg_live = make_test_cgroup(dir.path(), "cg-t23-live", &[live_pgid], true);
        for (attempt, pgid, cg_path) in [(1u32, dead_pgid, &cg_dead), (2u32, live_pgid, &cg_live)] {
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
                worker_pgid: Some(pgid),
                worker_cgroup: Some(cg_path.to_string_lossy().to_string()),
                tool_use_id: None,
                audit: None,
            };
            let seq = g.log(&cid).unwrap().seq();
            g.append(&cid, seq, cev.to_event().unwrap()).unwrap();
        }
        finalize_map_item(
            &mut g,
            &started.instance,
            "execute_lanes",
            Iteration::new(),
            "lane-a",
            &cid,
            Some("boom"),
            BTreeMap::new(),
            &me,
        )
        .unwrap();

        // Historical dead PGID while attempt 2 worker process group is still alive -> REFUSED!
        assert_eq!(
            quiesce_subflow(
                &mut g,
                &started.instance,
                "execute_lanes",
                Iteration::new(),
                "lane-a",
                &cid,
                &format!("pgid:{dead_pgid}:exited"),
                &me
            )
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
                quiesce_subflow(
                    &mut g,
                    &started.instance,
                    "execute_lanes",
                    Iteration::new(),
                    "lane-a",
                    &cid,
                    &format!("worktree_readonly:{child_wt_str}"),
                    &me
                )
                .unwrap_err()
                .code(),
                "quiescence_refused"
            );
            let _ = std::fs::set_permissions(&child_wt_str, std::fs::Permissions::from_mode(0o755));
        }
        // Even after removing child_wt, worktree_removed MUST fail while live_pgid is alive!
        let removed_rcpt = remove_child_worktree_receipt(&mut g, &cid);
        assert_eq!(
            quiesce_subflow(
                &mut g,
                &started.instance,
                "execute_lanes",
                Iteration::new(),
                "lane-a",
                &cid,
                &removed_rcpt,
                &me
            )
            .unwrap_err()
            .code(),
            "quiescence_refused"
        );
        // Kill & reap live_proc and drain its cgroup -> NOW quiescence succeeds!
        let _ = live_proc.kill();
        let _ = live_proc.wait();
        empty_test_cgroup(&cg_live);
        quiesce_subflow(
            &mut g,
            &started.instance,
            "execute_lanes",
            Iteration::new(),
            "lane-a",
            &cid,
            &removed_rcpt,
            &me,
        )
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
            child_eff_yaml.lines().map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n")
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
        let child_wt = {
            let g = ledger.lock().unwrap();
            let s: Started = serde_json::from_value(g.log(&cid).unwrap().events[0].body.clone()).unwrap();
            PathBuf::from(s.params.get("child_worktree").and_then(|v| v.as_str()).unwrap())
        };
        let ce = claim(&mut ledger, &sess, &cid, "do_effect").unwrap();
        let res = complete(&mut ledger, &sess, &ce.token, json!({"ok": true}), &Limits::default()).unwrap();
        match res {
            Completion::Refused { code, .. } => {
                assert_eq!(
                    code, "unfenced_child_effect",
                    "child effect without fencing_resource MUST be refused BEFORE executing command"
                );
            }
            other => panic!("expected unfenced_child_effect refusal on unfenced child effect, got {other:?}"),
        }
        assert!(
            !child_wt.join("unfenced.txt").exists(),
            "unfenced child effect command MUST NEVER execute before refusal!"
        );
    }
}

#[test]
fn t24_round6_audit_prefencing_and_crash_recovery() {
    // 1. Round 6 P1 #1a: child claim with audit: None MUST be refused at claim time!
    //    And Round 6 P1 #1b: a child log containing ANY Claimed event with audit: None (even
    //    after an earlier dead audited claimant) MUST be refused by quiesce_subflow & reconcile!
    {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("repo");
        init_git_repo(&wt);
        let root = dir.path().join("flows");
        let host = HostId::new("test-host").unwrap();
        let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
        let sess = verified_owner("owner-r6-audit");
        let no_pgid = verified_owner_no_pgid("owner-r6-audit");
        let compiled = compile(&lead_v3_yaml()).unwrap();
        let started =
            start(&mut ledger, &sess, &compiled, json!({"repo": wt.to_str().unwrap(), "max_parallel": 2})).unwrap();
        let cp = claim(&mut ledger, &sess, &started.instance, "plan_waves").unwrap();
        complete(
            &mut ledger,
            &sess,
            &cp.token,
            json!({"lanes": [{"lane_id": "lane-a", "write_set": ["src/a/**"]}]}),
            &Limits::default(),
        )
        .unwrap();
        let cm = claim(&mut ledger, &sess, &started.instance, "execute_lanes").unwrap();
        let cid = InstanceId::new(cm.inputs.get("__map_child_instance").unwrap()).unwrap();

        // Session without dedicated worker_pgid MUST be refused when claiming inside a child subflow!
        let err = claim(&mut ledger, &no_pgid, &cid, "work").unwrap_err();
        assert_eq!(err.code(), "missing_worker_pgid", "child claim without worker_pgid must fail closed");

        // Hand-crafted child log with dead attempt 1 + missing-worker_pgid attempt 2 -> quiescence MUST refuse!
        let dead_pgid = spawn_dead_pgrp();
        let me = owner("owner-r6-audit");
        let mut g = ledger.lock().unwrap();
        let ep = parent_owner_epoch(&g.log(&started.instance).unwrap());
        let c1 = Claimed {
            node: "work".into(),
            iteration: Iteration::new(),
            attempt: 1,
            inputs: BTreeMap::new(),
            token_sha256: "sha256:a1".into(),
            lease_until: "2099-01-01T00:00:00Z".into(),
            cancel_at: None,
            performer: None,
            owner_epoch: Some(ep),
            worker_pgid: Some(dead_pgid),
            worker_cgroup: None,
            tool_use_id: None,
            audit: None,
        };
        let c2 = Claimed {
            node: "work".into(),
            iteration: Iteration::new(),
            attempt: 2,
            inputs: BTreeMap::new(),
            token_sha256: "sha256:a2".into(),
            lease_until: "2099-01-01T00:00:00Z".into(),
            cancel_at: None,
            performer: None,
            owner_epoch: Some(ep),
            worker_pgid: None,
            worker_cgroup: None,
            tool_use_id: None,
            audit: None,
        };
        for c in [c1, c2] {
            let seq = g.log(&cid).unwrap().seq();
            g.append(&cid, seq, c.to_event().unwrap()).unwrap();
        }
        finalize_map_item(
            &mut g,
            &started.instance,
            "execute_lanes",
            Iteration::new(),
            "lane-a",
            &cid,
            Some("fail"),
            BTreeMap::new(),
            &me,
        )
        .unwrap();
        let removed_rcpt = remove_child_worktree_receipt(&mut g, &cid);
        assert_eq!(
            quiesce_subflow(
                &mut g,
                &started.instance,
                "execute_lanes",
                Iteration::new(),
                "lane-a",
                &cid,
                &removed_rcpt,
                &me
            )
            .unwrap_err()
            .code(),
            "quiescence_refused",
            "unaudited Claimed event must fail closed on quiescence even after worktree removal"
        );
    }

    // 2. Round 6 P1 #2: child effect WITH fencing_resource pointing to an UNREGISTERED arbitrary
    //    shell command MUST be refused BEFORE executing the command (zero file written!).
    {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("repo");
        init_git_repo(&wt);
        let root = dir.path().join("flows");
        let host = HostId::new("test-host").unwrap();
        let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
        let sess = verified_owner("owner-r6-unreg");
        let child_eff_yaml = r#"
flow: child_unreg
version: 1
params: [repo, item_key, fencing_resource]
workdir: "{{params.repo}}"
nodes:
  do_effect:
    class: effect
    run: ["sh", "-c", "echo leaked > unregistered.txt"]
    reconcile: ["sh", "-c", "true"]
"#;
        let parent_yaml = format!(
            r#"
flow: parent_unreg
version: 1
params: [repo, max_parallel, fencing_resource]
workdir: "{{{{params.repo}}}}"
nodes:
  plan:
    class: work
    verify: ["sh", "-c", "true"]
  run_map:
    class: map
    needs: [plan]
    over: ".nodes.plan.outputs.lanes"
    uses: child_unreg@1
    item_key: ".lane_id"
    max_items: 4
    max_concurrency_param: max_parallel
    ready_when: disjoint_writes
subflows:
  child_unreg@1: |
{}
"#,
            child_eff_yaml.lines().map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n")
        );
        let compiled = compile(&parent_yaml).unwrap();
        let started = start(
            &mut ledger,
            &sess,
            &compiled,
            json!({"repo": wt.to_str().unwrap(), "max_parallel": 2, "fencing_resource": "branch:unreg"}),
        )
        .unwrap();
        let cp = claim(&mut ledger, &sess, &started.instance, "plan").unwrap();
        complete(
            &mut ledger,
            &sess,
            &cp.token,
            json!({"lanes": [{"lane_id": "lane-a", "write_set": ["unregistered.txt"]}]}),
            &Limits::default(),
        )
        .unwrap();
        let cm = claim(&mut ledger, &sess, &started.instance, "run_map").unwrap();
        let cid = InstanceId::new(cm.inputs.get("__map_child_instance").unwrap()).unwrap();
        let child_wt = {
            let g = ledger.lock().unwrap();
            let s: Started = serde_json::from_value(g.log(&cid).unwrap().events[0].body.clone()).unwrap();
            PathBuf::from(s.params.get("child_worktree").and_then(|v| v.as_str()).unwrap())
        };
        let ce = claim(&mut ledger, &sess, &cid, "do_effect").unwrap();
        let res = complete(&mut ledger, &sess, &ce.token, json!({"ok": true}), &Limits::default()).unwrap();
        match res {
            Completion::Refused { code, .. } => {
                assert_eq!(code, "unfenced_external_effect_requires_reconcile");
            }
            other => panic!("expected unfenced_external_effect_requires_reconcile refusal, got {other:?}"),
        }
        assert!(
            !child_wt.join("unregistered.txt").exists(),
            "external shell effect command MUST NOT execute on fenced path!"
        );
    }

    // 3. Round 6 P2 #1 & #2: Crash Point B retry recovers cleanly if worktree wasn't created yet
    //    AND refuses if an existing worktree was dirtied or moved off pinned base_sha!
    {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("repo");
        init_git_repo(&wt);
        let root = dir.path().join("flows");
        let host = HostId::new("test-host").unwrap();
        let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
        let sess = verified_owner("owner-r6-crash");
        let compiled = compile(&lead_v3_yaml()).unwrap();
        let started =
            start(&mut ledger, &sess, &compiled, json!({"repo": wt.to_str().unwrap(), "max_parallel": 2})).unwrap();
        let me = owner("owner-r6-crash");
        let mut g = ledger.lock().unwrap();
        expand_map(
            &mut g,
            &started.instance,
            "execute_lanes",
            Iteration::new(),
            vec![
                MapItemSpec {
                    key: "lane-recover".into(),
                    input_digest: digest(&json!(1)),
                    input: json!({"lane_id": "lane-recover"}),
                    write_set: vec!["src/r/**".into()],
                },
                MapItemSpec {
                    key: "lane-dirty".into(),
                    input_digest: digest(&json!(2)),
                    input: json!({"lane_id": "lane-dirty"}),
                    write_set: vec!["src/d/**".into()],
                },
            ],
            BTreeMap::new(),
            &me,
        )
        .unwrap();

        // Simulate crash after child Started append, before worktree creation & parent SubflowStarted:
        let (cid_r, _) =
            claim_map_item(&mut g, &started.instance, "execute_lanes", Iteration::new(), "lane-recover", None, &me)
                .unwrap();
        let wt_r = {
            let s: Started = serde_json::from_value(g.log(&cid_r).unwrap().events[0].body.clone()).unwrap();
            PathBuf::from(s.params.get("child_worktree").and_then(|v| v.as_str()).unwrap())
        };
        // Remove the worktree and truncate parent's SubflowStarted by testing on lane-dirty via direct Started:
        assert!(wt_r.is_dir());

        // Now directly test Crash Point B with dirty worktree on lane-dirty:
        let base_sha = String::from_utf8(
            Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&wt).output().unwrap().stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        let cid_d =
            child_instance_id(&started.instance, "execute_lanes", &Iteration::new(), 0, "lane-dirty", 0).unwrap();
        let wt_d = root.join("worktrees").join(cid_d.as_str());
        let p_started: Started =
            serde_json::from_value(g.log(&started.instance).unwrap().events[0].body.clone()).unwrap();
        let child_ir = p_started.ir.subflows.get("lane@1").unwrap().clone();
        let subflow_hash = {
            let sr: Started = serde_json::from_value(g.log(&cid_r).unwrap().events[0].body.clone()).unwrap();
            sr.ir_hash
        };
        let pre_started = Started {
            flow: "lane@1".into(),
            ir_hash: subflow_hash,
            ir: child_ir,
            params: json!({
                "item": {"lane_id": "lane-dirty"},
                "item_key": "lane-dirty",
                "parent_repo": wt.to_str().unwrap(),
                "repo": wt_d.to_str().unwrap(),
                "child_worktree": wt_d.to_str().unwrap(),
                "base_sha": base_sha,
            }),
            owner: Some(me.clone()),
            audit: None,
            parent_ref: Some(maestro_flow::kernel::events::ParentRef {
                parent_instance: started.instance.to_string(),
                node: "execute_lanes".into(),
                iteration: Iteration::new(),
                map_epoch: 0,
                item_key: "lane-dirty".into(),
                retry_attempt: 0,
            }),
        };
        g.append(&cid_d, 0, pre_started.to_event().unwrap()).unwrap();
        // First: Crash-recovery with missing worktree creates it cleanly!
        // Create and dirty wt_d BEFORE retry -> Crash Point B retry MUST refuse dirty worktree!
        assert!(
            Command::new("git")
                .args(["worktree", "add", "--detach", "-q"])
                .arg(&wt_d)
                .arg(&base_sha)
                .current_dir(&wt)
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(wt_d.join("tampered.txt"), "dirty").unwrap();
        let dirty_err =
            claim_map_item(&mut g, &started.instance, "execute_lanes", Iteration::new(), "lane-dirty", None, &me)
                .unwrap_err();
        assert!(
            dirty_err.to_string().contains("not clean"),
            "Crash Point B retry MUST reject dirty child worktree, got: {dirty_err}"
        );
        // Remove dirty worktree completely (simulating crash between child Started and worktree add) -> retry recovers!
        assert!(
            Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(&wt_d)
                .current_dir(&wt)
                .status()
                .unwrap()
                .success()
        );
        let (recovered_cid, _) =
            claim_map_item(&mut g, &started.instance, "execute_lanes", Iteration::new(), "lane-dirty", None, &me)
                .unwrap();
        assert_eq!(recovered_cid, cid_d);
        assert!(wt_d.is_dir(), "crash between child Started and git worktree add must recover cleanly");
    }
}

#[test]
fn t25_round7_pgrp_termination_content_addressed_targets_and_staging_recovery() {
    // 1. Round 7 P1 #1: short-lived CLI process exits (audit.process.pid is dead!) while the
    //    worker agent remains alive in its dedicated process group (`worker_pgid`) -> quiescence
    //    MUST refuse ALL receipts (`pid:<dead_cli>`, `pgid:<agent_pgid>`, `worktree_removed:`)
    //    until the entire worker process group exits!
    {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("repo");
        init_git_repo(&wt);
        let root = dir.path().join("flows");
        let host = HostId::new("test-host").unwrap();
        let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
        let sess = verified_owner("owner-r7-pgrp");
        let compiled = compile(&lead_v3_yaml()).unwrap();
        let started =
            start(&mut ledger, &sess, &compiled, json!({"repo": wt.to_str().unwrap(), "max_parallel": 2})).unwrap();
        let me = owner("owner-r7-pgrp");
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
        let (cid, ep) =
            claim_map_item(&mut g, &started.instance, "execute_lanes", Iteration::new(), "lane-a", None, &me).unwrap();

        // Live agent in its own process group; short-lived CLI process already exited!
        let (mut agent_proc, agent_pgid) = spawn_worker_pgrp();
        let mut cli_proc = Command::new("true").spawn().unwrap();
        let dead_cli_pid = cli_proc.id();
        let _ = cli_proc.wait();
        let cg_agent = make_test_cgroup(dir.path(), "cg-t25-agent", &[agent_pgid], true);
        let cev = Claimed {
            node: "work".into(),
            iteration: Iteration::new(),
            attempt: 1,
            inputs: BTreeMap::new(),
            token_sha256: "sha256:cli-claim".into(),
            lease_until: "2099-01-01T00:00:00Z".into(),
            cancel_at: None,
            performer: None,
            owner_epoch: Some(ep),
            worker_pgid: Some(agent_pgid),
            worker_cgroup: Some(cg_agent.to_string_lossy().to_string()),
            tool_use_id: None,
            audit: Some(Audit {
                identity: IdentityFacts { mode: "cli".into(), variable: None, thread: None },
                process: ProcessFacts { pid: dead_cli_pid, ..Default::default() },
            }),
        };
        let seq = g.log(&cid).unwrap().seq();
        g.append(&cid, seq, cev.to_event().unwrap()).unwrap();
        finalize_map_item(
            &mut g,
            &started.instance,
            "execute_lanes",
            Iteration::new(),
            "lane-a",
            &cid,
            Some("fail"),
            BTreeMap::new(),
            &me,
        )
        .unwrap();
        let removed_rcpt = remove_child_worktree_receipt(&mut g, &cid);

        // Dead CLI PID receipt -> REFUSED (single-PID CLI death proves nothing!).
        assert_eq!(
            quiesce_subflow(
                &mut g,
                &started.instance,
                "execute_lanes",
                Iteration::new(),
                "lane-a",
                &cid,
                &format!("pid:{dead_cli_pid}:exited"),
                &me
            )
            .unwrap_err()
            .code(),
            "quiescence_refused"
        );
        // Live agent process group receipt -> REFUSED!
        assert_eq!(
            quiesce_subflow(
                &mut g,
                &started.instance,
                "execute_lanes",
                Iteration::new(),
                "lane-a",
                &cid,
                &format!("pgid:{agent_pgid}:exited"),
                &me
            )
            .unwrap_err()
            .code(),
            "quiescence_refused"
        );
        // Even worktree_removed -> REFUSED while agent_pgid is alive!
        assert_eq!(
            quiesce_subflow(
                &mut g,
                &started.instance,
                "execute_lanes",
                Iteration::new(),
                "lane-a",
                &cid,
                &removed_rcpt,
                &me
            )
            .unwrap_err()
            .code(),
            "quiescence_refused"
        );
        // Kill & reap agent_proc + empty its cgroup -> NOW succeeds!
        let _ = agent_proc.kill();
        let _ = agent_proc.wait();
        empty_test_cgroup(&cg_agent);
        quiesce_subflow(
            &mut g,
            &started.instance,
            "execute_lanes",
            Iteration::new(),
            "lane-a",
            &cid,
            &format!("pgid:{agent_pgid}:exited"),
            &me,
        )
        .unwrap();
    }

    // 2. Round 8 A/A invariants:
    //    (a) Claiming a child subflow with an ALREADY-DEAD worker_pgid MUST be refused at claim time
    //        (`dead_worker_pgid`), and caller-supplied `Started.params.worker_pgid` MUST be ignored!
    //    (b) External wrapper scripts (e.g. `#!/bin/sh` calling `./ship`) on a child effect MUST
    //        be refused BEFORE exec (`unfenced_external_effect_requires_reconcile`, zero mutation),
    //        while built-in Rust atomic actuators (`maestro:fenced-sqlite-kv`, `maestro:fenced-git-ref-cas`)
    //        execute atomically inside child subflows!
    {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("repo");
        init_git_repo(&wt);
        let root = dir.path().join("flows");
        let host = HostId::new("test-host").unwrap();
        let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
        let sess = verified_owner("owner-r8-aa");
        let dead_pgid = spawn_dead_pgrp();
        let dead_sess = verified_owner_with_pgid("owner-r8-aa", dead_pgid);
        let no_pgid_sess = verified_owner_no_pgid("owner-r8-aa");

        let child_builtin_yaml = r#"
flow: child_builtin
version: 1
params: [repo, item_key, fencing_resource, worker_pgid]
workdir: "{{params.repo}}"
nodes:
  kv_put:
    class: effect
    run: ["maestro:fenced-sqlite-kv", "lane_result", "committed"]
    reconcile: [sh, -c, "true"]
"#;
        let parent_yaml = format!(
            r#"
flow: parent_builtin
version: 1
params: [repo, max_parallel, fencing_resource, worker_pgid]
workdir: "{{{{params.repo}}}}"
nodes:
  plan:
    class: work
    verify: [sh, -c, "true"]
  run_map:
    class: map
    needs: [plan]
    over: ".nodes.plan.outputs.lanes"
    uses: child_builtin@1
    item_key: ".lane_id"
    max_items: 4
    max_concurrency_param: max_parallel
    ready_when: disjoint_writes
subflows:
  child_builtin@1: |
{}
"#,
            child_builtin_yaml.lines().map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n")
        );
        let compiled = compile(&parent_yaml).unwrap();
        let live_pgid = rustix::process::getpgrp().as_raw_nonzero().get() as u32;
        let started = start(
            &mut ledger,
            &sess,
            &compiled,
            json!({
                "repo": wt.to_str().unwrap(),
                "max_parallel": 2,
                "fencing_resource": "child:kv",
                "worker_pgid": live_pgid
            }),
        )
        .unwrap();
        let cp = claim(&mut ledger, &sess, &started.instance, "plan").unwrap();
        complete(
            &mut ledger,
            &sess,
            &cp.token,
            json!({"lanes": [{"lane_id": "lane-a", "write_set": ["src/a/**"]}]}),
            &Limits::default(),
        )
        .unwrap();
        let cm = claim(&mut ledger, &sess, &started.instance, "run_map").unwrap();
        let cid = InstanceId::new(cm.inputs.get("__map_child_instance").unwrap()).unwrap();

        // Caller-supplied Started.params.worker_pgid MUST NOT satisfy claim when session.worker_pgid() is None!
        assert_eq!(
            claim(&mut ledger, &no_pgid_sess, &cid, "kv_put").unwrap_err().code(),
            "missing_worker_pgid",
            "Started.params.worker_pgid fallback must be completely removed"
        );
        // Already-dead worker_pgid MUST be refused at claim time!
        assert_eq!(
            claim(&mut ledger, &dead_sess, &cid, "kv_put").unwrap_err().code(),
            "dead_worker_pgid",
            "claiming with an already-dead PGID must fail closed at claim time"
        );
        // Live host-attested PGID + built-in atomic SQLite KV actuator succeeds cleanly!
        let ck = claim(&mut ledger, &sess, &cid, "kv_put").unwrap();
        let comp = complete(&mut ledger, &sess, &ck.token, json!({"ok": true}), &Limits::default()).unwrap();
        assert!(
            matches!(comp, Completion::Completed { .. }),
            "child built-in atomic KV actuator must succeed: {comp:?}"
        );
        let fstore = FencingStore::open(root.join("fencing_epochs.sqlite")).unwrap();
        assert_eq!(fstore.read_fenced_kv("child:kv", "lane_result").unwrap().unwrap().0, "committed");

        // External wrapper script (`#!/bin/sh` calling `./ship`) on a fenced effect MUST be refused before exec!
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let wrapper = bin_dir.join("outside-wrapper");
        let leaked = wt.join("wrapper_leaked.txt");
        std::fs::write(&wrapper, format!("#!/bin/sh\necho leaked > {}\n", leaked.display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o555)).unwrap();
        }
        let wrap_yaml = format!(
            r#"
flow: wrapper_guard
version: 1
params: [repo, fencing_resource]
workdir: "{{{{params.repo}}}}"
nodes:
  ship:
    class: effect
    run: [{}]
    reconcile: [sh, -c, "true"]
"#,
            serde_json::to_string(&wrapper.to_string_lossy()).unwrap()
        );
        let cw = compile(&wrap_yaml).unwrap();
        let sw =
            start(&mut ledger, &sess, &cw, json!({"repo": wt.to_str().unwrap(), "fencing_resource": "branch:wrap"}))
                .unwrap();
        let clw = claim(&mut ledger, &sess, &sw.instance, "ship").unwrap();
        match complete(&mut ledger, &sess, &clw.token, json!({"ok": true}), &Limits::default()).unwrap() {
            Completion::Refused { code, .. } => assert_eq!(code, "unfenced_external_effect_requires_reconcile"),
            other => panic!("expected unfenced_external_effect_requires_reconcile on external wrapper, got {other:?}"),
        }
        assert!(!leaked.exists(), "external wrapper script MUST NEVER execute on fenced path");
    }

    // 3. Round 7 P2: interruption DURING `git worktree add` leaves a partial `.staging-<cid>-*`
    //    directory -> retry cleans it up, prunes, allocates atomically via `git worktree move`,
    //    and succeeds cleanly without stranding!
    {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("repo");
        init_git_repo(&wt);
        let root = dir.path().join("flows");
        let host = HostId::new("test-host").unwrap();
        let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
        let sess = verified_owner("owner-r7-stage");
        let compiled = compile(&lead_v3_yaml()).unwrap();
        let started =
            start(&mut ledger, &sess, &compiled, json!({"repo": wt.to_str().unwrap(), "max_parallel": 2})).unwrap();
        let me = owner("owner-r7-stage");
        let mut g = ledger.lock().unwrap();
        expand_map(
            &mut g,
            &started.instance,
            "execute_lanes",
            Iteration::new(),
            vec![MapItemSpec {
                key: "lane-stage".into(),
                input_digest: digest(&json!(1)),
                input: json!({"lane_id": "lane-stage"}),
                write_set: vec!["src/s/**".into()],
            }],
            BTreeMap::new(),
            &me,
        )
        .unwrap();
        let base_sha = String::from_utf8(
            Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&wt).output().unwrap().stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        let cid = child_instance_id(&started.instance, "execute_lanes", &Iteration::new(), 0, "lane-stage", 0).unwrap();
        let wt_root = root.join("worktrees");
        let wt_final = wt_root.join(cid.as_str());
        // Simulate crash DURING git worktree add: child Started exists, partial .staging-<cid>-99999 exists!
        let partial_staging = wt_root.join(format!(".staging-{}-99999", cid.as_str()));
        std::fs::create_dir_all(&partial_staging).unwrap();
        std::fs::write(partial_staging.join("partial-junk"), "interrupted").unwrap();
        let p_started: Started =
            serde_json::from_value(g.log(&started.instance).unwrap().events[0].body.clone()).unwrap();
        let child_ir = p_started.ir.subflows.get("lane@1").unwrap().clone();
        let subflow_hash = match &p_started.ir.nodes["execute_lanes"].class {
            maestro_flow::kernel::ir::Class::Map { subflow_ir_hash, .. } => subflow_ir_hash.clone(),
            _ => unreachable!(),
        };
        let pre_started = Started {
            flow: "lane@1".into(),
            ir_hash: subflow_hash,
            ir: child_ir,
            params: json!({
                "item": {"lane_id": "lane-stage"},
                "item_key": "lane-stage",
                "parent_repo": wt.to_str().unwrap(),
                "repo": wt_final.to_str().unwrap(),
                "child_worktree": wt_final.to_str().unwrap(),
                "base_sha": base_sha,
            }),
            owner: Some(me.clone()),
            audit: None,
            parent_ref: Some(maestro_flow::kernel::events::ParentRef {
                parent_instance: started.instance.to_string(),
                node: "execute_lanes".into(),
                iteration: Iteration::new(),
                map_epoch: 0,
                item_key: "lane-stage".into(),
                retry_attempt: 0,
            }),
        };
        g.append(&cid, 0, pre_started.to_event().unwrap()).unwrap();
        let (recovered_cid, _) =
            claim_map_item(&mut g, &started.instance, "execute_lanes", Iteration::new(), "lane-stage", None, &me)
                .unwrap();
        assert_eq!(recovered_cid, cid);
        assert!(!partial_staging.exists(), "stale partial staging dir must be cleaned up on recovery");
        assert!(wt_final.is_dir(), "atomic staged allocation must promote final child_worktree");
    }
}

#[test]
fn t26_cgroup_containment_git_hooks_disabled_and_wal_crash_atomic_cas() {
    // 1. OS cgroup containment prevents setsid()/setpgid() escape:
    //    - Claiming a child subflow without worker_cgroup -> refused (`missing_worker_cgroup`).
    //    - Claiming with an empty/dead cgroup -> refused (`dead_worker_cgroup`).
    //    - Even after worker_pgid exits (`kill(-pgid, 0) == ESRCH`), if `cgroup.procs` still lists
    //      a detached descendant PID OR `cgroup.events` still reports `populated 1`, quiescence
    //      MUST fail closed (`quiescence_refused`)! Only when BOTH pgid is dead AND cgroup is empty
    //      (`populated 0`, 0 PIDs) does quiescence succeed.
    {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("repo");
        init_git_repo(&wt);
        let root = dir.path().join("flows");
        let host = HostId::new("test-host").unwrap();
        let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
        let live_pgid = rustix::process::getpgrp().as_raw_nonzero().get() as u32;
        let sess_no_cg = verified_owner_with_pgid("owner-cg", live_pgid);
        let empty_cg = make_test_cgroup(dir.path(), "cg-empty", &[], false);
        let sess_empty_cg =
            verified_owner_with_pgid("owner-cg", live_pgid).with_worker_cgroup(empty_cg.to_string_lossy());
        let sess = verified_owner("owner-cg");

        let compiled = compile(&lead_v3_yaml()).unwrap();
        let started =
            start(&mut ledger, &sess, &compiled, json!({"repo": wt.to_str().unwrap(), "max_parallel": 2})).unwrap();
        let cp = claim(&mut ledger, &sess, &started.instance, "plan_waves").unwrap();
        complete(
            &mut ledger,
            &sess,
            &cp.token,
            json!({"lanes": [{"lane_id": "lane-a", "write_set": ["src/a/**"]}]}),
            &Limits::default(),
        )
        .unwrap();
        let cm = claim(&mut ledger, &sess, &started.instance, "execute_lanes").unwrap();
        let cid = InstanceId::new(cm.inputs.get("__map_child_instance").unwrap()).unwrap();

        assert_eq!(claim(&mut ledger, &sess_no_cg, &cid, "work").unwrap_err().code(), "missing_worker_cgroup");
        assert_eq!(claim(&mut ledger, &sess_empty_cg, &cid, "work").unwrap_err().code(), "dead_worker_cgroup");

        let (mut worker_proc, worker_pgid) = spawn_worker_pgrp();
        let worker_cg = make_test_cgroup(dir.path(), "cg-live-worker", &[worker_pgid, 88888], true);
        let sess_worker =
            verified_owner_with_pgid("owner-cg", worker_pgid).with_worker_cgroup(worker_cg.to_string_lossy());
        let _cw = claim(&mut ledger, &sess_worker, &cid, "work").unwrap();

        let me = owner("owner-cg");
        let mut g = ledger.lock().unwrap();
        supersede_stale_items(
            &mut g,
            &started.instance,
            "execute_lanes",
            Iteration::new(),
            vec![MapItemSpec {
                key: "lane-a".into(),
                input_digest: digest(&json!({"v": 2})),
                input: json!({"lane_id": "lane-a"}),
                write_set: vec!["src/a/**".into()],
            }],
            BTreeMap::new(),
            &me,
        )
        .unwrap();

        // Kill the primary worker process group so kill(-worker_pgid, 0) == ESRCH, AND remove the
        // child worktree — simulating an escaped setsid() descendant (PID 88888) still inside worker_cg!
        let _ = worker_proc.kill();
        let _ = worker_proc.wait();
        let _ = remove_child_worktree_receipt(&mut g, &cid);
        std::fs::write(worker_cg.join("cgroup.procs"), "88888\n").unwrap();
        let rcpt = format!("pgid:{worker_pgid}:exited");
        assert_eq!(
            quiesce_subflow(&mut g, &started.instance, "execute_lanes", Iteration::new(), "lane-a", &cid, &rcpt, &me)
                .unwrap_err()
                .code(),
            "quiescence_refused",
            "escaped setsid() descendant still in cgroup.procs MUST block quiescence"
        );
        // Empty cgroup.procs but still populated 1 (child sub-cgroup active) -> STILL refused!
        std::fs::write(worker_cg.join("cgroup.procs"), "").unwrap();
        assert_eq!(
            quiesce_subflow(&mut g, &started.instance, "execute_lanes", Iteration::new(), "lane-a", &cid, &rcpt, &me)
                .unwrap_err()
                .code(),
            "quiescence_refused",
            "cgroup.events populated 1 MUST block quiescence"
        );
        // Both cgroup.procs empty AND populated 0 -> quiescence succeeds!
        empty_test_cgroup(&worker_cg);
        quiesce_subflow(&mut g, &started.instance, "execute_lanes", Iteration::new(), "lane-a", &cid, &rcpt, &me)
            .unwrap();
    }

    // 2. Repository-controlled git hooks (`reference-transaction` in .git/hooks AND core.hooksPath)
    //    MUST NEVER execute during `maestro:fenced-git-ref-cas`!
    {
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path().join("repo");
        init_git_repo(&repo_dir);
        let root = dir.path().join("flows");
        let host = HostId::new("test-host").unwrap();
        let mut ledger = Ledger::open(&root, &host, Options::default()).unwrap();
        let sess = verified_owner("owner-hooks");

        let hook1_flag = dir.path().join("hook1_ran.txt");
        let hook2_flag = dir.path().join("hook2_ran.txt");
        let default_hook = repo_dir.join(".git/hooks/reference-transaction");
        std::fs::write(&default_hook, format!("#!/bin/sh\necho pwned > {}\n", hook1_flag.display())).unwrap();
        let evil_hooks_dir = dir.path().join("evil-hooks");
        std::fs::create_dir_all(&evil_hooks_dir).unwrap();
        let custom_hook = evil_hooks_dir.join("reference-transaction");
        std::fs::write(&custom_hook, format!("#!/bin/sh\necho pwned > {}\n", hook2_flag.display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&default_hook, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::set_permissions(&custom_hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(
            Command::new("git")
                .args(["config", "core.hooksPath", evil_hooks_dir.to_str().unwrap()])
                .current_dir(&repo_dir)
                .status()
                .unwrap()
                .success()
        );

        let head_oid = String::from_utf8(
            Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&repo_dir).output().unwrap().stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        let zero_oid = "0".repeat(head_oid.len());
        let flow_yaml = format!(
            r#"
flow: fenced_git_no_hooks
version: 1
params: [repo, fencing_resource]
workdir: "{{{{params.repo}}}}"
nodes:
  ship:
    class: effect
    run: ["maestro:fenced-git-ref-cas", "refs/heads/no-hooks-ship", "{zero_oid}", "{head_oid}"]
    reconcile: [sh, -c, "true"]
"#
        );
        let compiled = compile(&flow_yaml).unwrap();
        let s = start(
            &mut ledger,
            &sess,
            &compiled,
            json!({"repo": repo_dir.to_str().unwrap(), "fencing_resource": "git:no-hooks"}),
        )
        .unwrap();
        let c = claim(&mut ledger, &sess, &s.instance, "ship").unwrap();
        let comp = complete(&mut ledger, &sess, &c.token, json!({"ok": true}), &Limits::default()).unwrap();
        assert!(matches!(comp, Completion::Completed { .. }), "hook-disabled git-ref CAS must complete: {comp:?}");
        assert!(!hook1_flag.exists(), ".git/hooks/reference-transaction MUST NEVER execute on fenced path");
        assert!(!hook2_flag.exists(), "core.hooksPath reference-transaction MUST NEVER execute on fenced path");
    }

    // 3. WAL-first crash-atomic git-ref CAS recovery:
    //    Simulate a crash AFTER written_epoch=1 was consumed & git update-ref applied the ref,
    //    leaving `fenced_git_refs.state = 'pending'`. A DIFFERENT writer MUST be blocked fail-closed
    //    (`pending_git_ref_cas_recovery_required`), while retrying the exact same `(epoch=1, op_id,
    //    payload_digest)` reconciles cleanly to `IdempotentReplay` and marks `state = 'committed'`.
    {
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path().join("repo");
        init_git_repo(&repo_dir);
        let head_oid = String::from_utf8(
            Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&repo_dir).output().unwrap().stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        let zero_oid = "0".repeat(head_oid.len());
        let db_path = dir.path().join("fencing_epochs.sqlite");
        let fstore = FencingStore::open(&db_path).unwrap();
        let rlock = fstore.acquire_resource_lock("git:crash-wal").unwrap();
        let ep1 = fstore.grant_epoch_locked(&rlock, "git:crash-wal").unwrap();
        assert_eq!(ep1, 1);

        // Execute normally, then rewind `fenced_git_refs.state` back to `'pending'` to simulate a
        // crash right after `git update-ref` succeeded and before the final `state = 'committed'`!
        let out = fstore
            .execute_atomic_git_ref_cas(
                &rlock,
                &repo_dir,
                "git:crash-wal",
                ep1,
                "op-1",
                "sha256:dig1",
                "{}",
                "refs/heads/wal-ship",
                &zero_oid,
                &head_oid,
            )
            .unwrap();
        assert_eq!(out, FencedWriteOutcome::Committed { epoch: 1 });
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute("UPDATE fenced_git_refs SET state = 'pending' WHERE resource_id = 'git:crash-wal'", []).unwrap();
        drop(conn);

        // A different writer (epoch 2, op-2) MUST be refused fail-closed while 'pending' is unresolved!
        let ep2 = fstore.grant_epoch_locked(&rlock, "git:crash-wal").unwrap();
        assert_eq!(ep2, 2);
        let blocked = fstore
            .execute_atomic_git_ref_cas(
                &rlock,
                &repo_dir,
                "git:crash-wal",
                ep2,
                "op-2",
                "sha256:dig2",
                "{}",
                "refs/heads/wal-ship",
                &head_oid,
                &head_oid,
            )
            .unwrap_err();
        assert!(
            blocked.to_string().contains("pending_git_ref_cas_recovery_required"),
            "different writer must fail closed on pending crash-recovery row, got: {blocked}"
        );

        // Exact retry of `(ep1, op-1, sha256:dig1)` reconciles against git rev-parse -> committed!
        let recovered = fstore
            .execute_atomic_git_ref_cas(
                &rlock,
                &repo_dir,
                "git:crash-wal",
                ep1,
                "op-1",
                "sha256:dig1",
                "{}",
                "refs/heads/wal-ship",
                &zero_oid,
                &head_oid,
            )
            .unwrap();
        assert!(matches!(recovered, FencedWriteOutcome::IdempotentReplay { epoch: 1, .. }));

        // Once recovered to 'committed', ep2 can now proceed cleanly!
        let out2 = fstore
            .execute_atomic_git_ref_cas(
                &rlock,
                &repo_dir,
                "git:crash-wal",
                ep2,
                "op-2",
                "sha256:dig2",
                "{}",
                "refs/heads/wal-ship",
                &head_oid,
                &head_oid,
            )
            .unwrap();
        assert_eq!(out2, FencedWriteOutcome::Committed { epoch: 2 });
    }
}
