//! IR v10 bounded dynamic `map` & version-pinned `subflow` orchestration kernel.
//!
//! Enforces every cross-instance invariant of the two-timescale evolvable flow-kernel spec:
//! - Deterministic child instance IDs: `fi_sub_<sha256(parent|node|iter|epoch|item_key|retry)[..24]>`
//! - Strict canonical lock order (`lock(parent) -> lock(child)` via the ledger's single `WriteGuard`)
//! - Crash Points A, B, and C idempotent recovery (zero duplicate child spawns, zero orphans)
//! - Monotonic `owner_epoch` adoption fencing (`former_owners_claim`)
//! - `Superseded` write fencing (`subflow_superseded`) with the narrow `reconcile_only` exception
//!   for open `EffectStarted` entries (`EffectReconciled`)
//! - True quiescence (`SubflowQuiesced`): refuses mere lease expiry; requires confirmed worker
//!   termination receipt AND zero unreconciled `EffectStarted` entries
//! - Atomic claim-time `disjoint_writes` + `max_concurrency` check retaining reservations on BOTH
//!   `SubflowStarted` AND unquiesced `Superseded` siblings until `SubflowQuiesced`
//! - Runtime `reserve_providers` / `provider_pool` liveness enforcer (`provider_pool_exhausted`)
//! - Recursive child performer union attestation for downstream `different_from(map_node)`

use super::events::{
    Adopted, Answered, Claimed, Completed, EffectReconciled, EffectStarted, KernelEvent, MapExpanded, MapItemSpec,
    Owner, ParentRef, Performer, Retired, Started, SubflowCompleted, SubflowFailed, SubflowQuiesced, SubflowStarted,
    Superseded, digest, performers_receipt_digest,
};
use super::ir::{Class, Iteration, OnItemFailure, ReadyWhen};
use super::replay::{Reason, State, next_of, pinned};
use super::write::{PROVIDERS, provider_of};
use super::writeset::{validate_write_set, verify_worktree_writes_auto, write_sets_intersect};
use crate::clock;
use crate::ids::InstanceId;
use crate::ledger::WriteGuard;
use crate::log::Log;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Stable refusal codes for IR v10 map & subflow operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubflowError {
    #[error("not a map or subflow node: {0}")]
    WrongClass(String),
    #[error("caller is not the current parent owner")]
    NotOwner,
    #[error("former_owners_claim: stale owner_epoch {claim_epoch} != current {current_epoch}")]
    FormerOwnersClaim { claim_epoch: u64, current_epoch: u64 },
    #[error("subflow_superseded: child {child} is superseded (or parent retired)")]
    SubflowSuperseded { child: String },
    #[error("write_set_conflict: item {item_key} overlaps active or unquiesced sibling {sibling}")]
    WriteSetConflict { item_key: String, sibling: String },
    #[error("write_set_violation: item {item_key} wrote outside declared write_set: {violations:?}")]
    WriteSetViolation { item_key: String, violations: Vec<String> },
    #[error("invalid_child: {detail}")]
    InvalidChild { detail: String },
    #[error("child_not_terminal: child {child} is not terminal ({detail})")]
    ChildNotTerminal { child: String, detail: String },
    #[error("concurrency_limit: {in_flight} in-flight >= max {limit}")]
    ConcurrencyLimit { in_flight: usize, limit: usize },
    #[error("provider_pool_exhausted: {detail}")]
    ProviderPoolExhausted { detail: String },
    #[error("quiescence_refused: {detail}")]
    QuiescenceRefused { detail: String },
    #[error("invalid_map_expansion: {detail}")]
    InvalidExpansion { detail: String },
    #[error("kernel storage error: {0}")]
    Storage(String),
}

impl SubflowError {
    pub fn code(&self) -> &'static str {
        match self {
            SubflowError::WrongClass(_) => "wrong_class",
            SubflowError::NotOwner => "not_owner",
            SubflowError::FormerOwnersClaim { .. } => "former_owners_claim",
            SubflowError::SubflowSuperseded { .. } => "subflow_superseded",
            SubflowError::WriteSetConflict { .. } => "write_set_conflict",
            SubflowError::WriteSetViolation { .. } => "write_set_violation",
            SubflowError::InvalidChild { .. } => "invalid_child",
            SubflowError::ChildNotTerminal { .. } => "child_not_terminal",
            SubflowError::ConcurrencyLimit { .. } => "concurrency_limit",
            SubflowError::ProviderPoolExhausted { .. } => "provider_pool_exhausted",
            SubflowError::QuiescenceRefused { .. } => "quiescence_refused",
            SubflowError::InvalidExpansion { .. } => "invalid_map_expansion",
            SubflowError::Storage(_) => "storage_error",
        }
    }
}

/// Returns the parent-declared `write_set` for a child instance spawned by a `map` node, if any.
pub fn child_declared_write_set(parent_log: &Log, parent_ref: &ParentRef) -> Option<Vec<String>> {
    let st = fold_map_state(parent_log, &parent_ref.node, &parent_ref.iteration);
    let item = st.items.get(&parent_ref.item_key)?;
    if item.write_set.is_empty() { None } else { Some(item.write_set.clone()) }
}

/// Returns the `owner_epoch` recorded when `parent_log` spawned `child_instance`.
pub fn child_spawn_owner_epoch(parent_log: &Log, child_instance: &str) -> Option<u64> {
    parent_log
        .events
        .iter()
        .rev()
        .filter(|e| e.kind == SubflowStarted::KIND)
        .filter_map(|e| serde_json::from_value::<SubflowStarted>(e.body.clone()).ok())
        .find(|s| s.child_instance == child_instance)
        .map(|s| s.owner_epoch)
}

/// Derives the deterministic child `InstanceId` for Crash Points A/B/C idempotency:
/// `fi_sub_<sha256(parent|node|iteration|map_epoch|item_key|retry_attempt)[0..24]>`.
pub fn child_instance_id(
    parent: &InstanceId,
    node: &str,
    iteration: &Iteration,
    map_epoch: u64,
    item_key: &str,
    retry_attempt: u32,
) -> Result<InstanceId, SubflowError> {
    let payload = serde_json::json!({
        "parent": parent.as_str(),
        "node": node,
        "iteration": iteration,
        "map_epoch": map_epoch,
        "item_key": item_key,
        "retry_attempt": retry_attempt,
    });
    let hash = digest(&payload);
    let hex = hash.strip_prefix("sha256:").unwrap_or(&hash);
    let short = &hex[..hex.len().min(24)];
    InstanceId::new(format!("fi_sub_{short}")).map_err(|e| SubflowError::Storage(e.to_string()))
}

/// Current monotonic `owner_epoch` of a parent instance: `0` at `Started`, incremented by 1 per
/// `Adopted` event.
pub fn parent_owner_epoch(log: &Log) -> u64 {
    log.events.iter().filter(|e| e.kind == Adopted::KIND).count() as u64
}

/// Current effective owner of a parent instance (latest `Adopted.to`, else `Started.owner`).
pub fn parent_current_owner(log: &Log) -> Option<Owner> {
    let mut owner = log
        .events
        .first()
        .and_then(|e| serde_json::from_value::<Started>(e.body.clone()).ok())
        .and_then(|s| s.owner);
    for e in &log.events {
        if e.kind == Adopted::KIND
            && let Ok(a) = serde_json::from_value::<Adopted>(e.body.clone())
        {
            owner = Some(a.to);
        }
    }
    owner
}

fn parent_is_retired(log: &Log) -> bool {
    log.events.iter().any(|e| e.kind == Retired::KIND)
}

/// Summary of a map node's runtime item state replayed purely from parent log events.
#[derive(Debug, Clone, Default)]
pub struct MapRuntimeState {
    pub latest_epoch: Option<u64>,
    pub items: BTreeMap<String, MapItemSpec>,
    /// Active `SubflowStarted` children not yet completed, failed, or superseded.
    pub in_flight: BTreeMap<String, SubflowStarted>,
    /// Latest completed receipt per `item_key`.
    pub completed: BTreeMap<String, SubflowCompleted>,
    /// Failure attempts per `item_key`.
    pub failed_attempts: BTreeMap<String, Vec<SubflowFailed>>,
    /// Superseded children awaiting `SubflowQuiesced` (still holding write_set reservations!).
    pub superseded_unquiesced: BTreeMap<String, Superseded>,
    /// All quiesced child instance IDs.
    pub quiesced_children: BTreeSet<String>,
    /// Every child `SubflowStarted` record by `child_instance` ID.
    pub started_by_child: BTreeMap<String, SubflowStarted>,
    /// Providers already used by started or completed items in the current epoch.
    pub used_providers: BTreeSet<String>,
}

/// Replays the parent log (read-only) for one `(node, iteration)` map node.
pub fn fold_map_state(log: &Log, node: &str, iteration: &Iteration) -> MapRuntimeState {
    let mut st = MapRuntimeState::default();
    for ev in &log.events {
        match ev.kind.as_str() {
            MapExpanded::KIND => {
                if let Ok(m) = serde_json::from_value::<MapExpanded>(ev.body.clone())
                    && m.node == node
                    && &m.iteration == iteration
                {
                    st.latest_epoch = Some(m.map_epoch);
                    st.items.clear();
                    for item in m.items {
                        st.items.insert(item.key.clone(), item);
                    }
                }
            }
            SubflowStarted::KIND => {
                if let Ok(s) = serde_json::from_value::<SubflowStarted>(ev.body.clone())
                    && s.node == node
                    && &s.iteration == iteration
                {
                    if let Some(p) = s.performer.as_ref().and_then(|p| provider_of(&p.harness)) {
                        st.used_providers.insert(p.to_string());
                    }
                    st.started_by_child.insert(s.child_instance.clone(), s.clone());
                    st.in_flight.insert(s.item_key.clone(), s);
                }
            }
            SubflowCompleted::KIND => {
                if let Ok(c) = serde_json::from_value::<SubflowCompleted>(ev.body.clone())
                    && c.node == node
                    && &c.iteration == iteration
                {
                    for perf in &c.performers {
                        if let Some(p) = provider_of(&perf.harness) {
                            st.used_providers.insert(p.to_string());
                        }
                    }
                    if st.in_flight.get(&c.item_key).is_some_and(|s| s.child_instance == c.child_instance) {
                        st.in_flight.remove(&c.item_key);
                    }
                    st.completed.insert(c.item_key.clone(), c);
                }
            }
            SubflowFailed::KIND => {
                if let Ok(f) = serde_json::from_value::<SubflowFailed>(ev.body.clone())
                    && f.node == node
                    && &f.iteration == iteration
                {
                    for perf in &f.performers {
                        if let Some(p) = provider_of(&perf.harness) {
                            st.used_providers.insert(p.to_string());
                        }
                    }
                    if st.in_flight.get(&f.item_key).is_some_and(|s| s.child_instance == f.child_instance) {
                        st.in_flight.remove(&f.item_key);
                    }
                    st.failed_attempts.entry(f.item_key.clone()).or_default().push(f);
                }
            }
            Superseded::KIND => {
                if let Ok(sup) = serde_json::from_value::<Superseded>(ev.body.clone())
                    && sup.node == node
                    && &sup.iteration == iteration
                {
                    if st.in_flight.get(&sup.item_key).is_some_and(|s| s.child_instance == sup.child_instance) {
                        st.in_flight.remove(&sup.item_key);
                    }
                    if !st.quiesced_children.contains(&sup.child_instance) {
                        st.superseded_unquiesced.insert(sup.child_instance.clone(), sup);
                    }
                }
            }
            SubflowQuiesced::KIND => {
                if let Ok(q) = serde_json::from_value::<SubflowQuiesced>(ev.body.clone())
                    && q.node == node
                    && &q.iteration == iteration
                {
                    st.superseded_unquiesced.remove(&q.child_instance);
                    st.quiesced_children.insert(q.child_instance);
                }
            }
            _ => {}
        }
    }
    st
}

/// Returns true if the parent log has ANY `Superseded` child that has not yet logged
/// `SubflowQuiesced` or ANY expanded `map` node with unfinished items (used by `stop::assess`).
pub fn has_unsettled_subflows(log: &Log) -> bool {
    let mut superseded: BTreeSet<String> = BTreeSet::new();
    let mut expanded_nodes: BTreeMap<(String, Iteration), usize> = BTreeMap::new();
    let mut completed_nodes: BTreeSet<(String, Iteration)> = BTreeSet::new();
    for ev in &log.events {
        match ev.kind.as_str() {
            Superseded::KIND => {
                if let Ok(s) = serde_json::from_value::<Superseded>(ev.body.clone()) {
                    superseded.insert(s.child_instance);
                }
            }
            SubflowQuiesced::KIND => {
                if let Ok(q) = serde_json::from_value::<SubflowQuiesced>(ev.body.clone()) {
                    superseded.remove(&q.child_instance);
                }
            }
            MapExpanded::KIND => {
                if let Ok(m) = serde_json::from_value::<MapExpanded>(ev.body.clone()) {
                    expanded_nodes.insert((m.node, m.iteration), m.items.len());
                }
            }
            Completed::KIND => {
                if let Ok(c) = serde_json::from_value::<Completed>(ev.body.clone()) {
                    completed_nodes.insert((c.node, c.iteration));
                }
            }
            _ => {}
        }
    }
    if !superseded.is_empty() {
        return true;
    }
    expanded_nodes.keys().any(|k| !completed_nodes.contains(k))
}

/// Checks whether a child instance currently has an open `EffectStarted` without a matching
/// `Completed` or `EffectReconciled`.
pub fn child_has_open_effect(child_log: &Log) -> bool {
    let mut open: BTreeSet<(String, Iteration, u32)> = BTreeSet::new();
    for ev in &child_log.events {
        match ev.kind.as_str() {
            EffectStarted::KIND => {
                if let Ok(s) = serde_json::from_value::<EffectStarted>(ev.body.clone()) {
                    open.insert((s.node, s.iteration, s.attempt));
                }
            }
            Completed::KIND => {
                if let Ok(c) = serde_json::from_value::<Completed>(ev.body.clone()) {
                    open.remove(&(c.node, c.iteration, c.attempt));
                }
            }
            EffectReconciled::KIND => {
                if let Ok(r) = serde_json::from_value::<EffectReconciled>(ev.body.clone()) {
                    open.remove(&(r.node, r.iteration, r.attempt));
                }
            }
            _ => {}
        }
    }
    !open.is_empty()
}

/// Validates parent-delegated ownership, `owner_epoch` adoption fencing, and `Superseded` write
/// fencing on a child instance under `lock(parent) -> lock(child)`.
///
/// When `reconcile_only` is true, permits ONLY reconciliation of an already-open `EffectStarted`
/// on a `Superseded` child (still strictly fenced by `owner_epoch` and current parent owner).
pub fn check_child_write_fence(
    parent_log: &Log,
    child_log: &Log,
    caller_owner: &Owner,
    claim_owner_epoch: u64,
    reconcile_only: bool,
) -> Result<(), SubflowError> {
    let Some(current_owner) = parent_current_owner(parent_log) else {
        return Err(SubflowError::NotOwner);
    };
    if &current_owner != caller_owner {
        return Err(SubflowError::NotOwner);
    }
    let current_epoch = parent_owner_epoch(parent_log);
    if claim_owner_epoch != current_epoch {
        return Err(SubflowError::FormerOwnersClaim {
            claim_epoch: claim_owner_epoch,
            current_epoch,
        });
    }
    if parent_is_retired(parent_log) {
        return Err(SubflowError::SubflowSuperseded { child: child_log.instance.to_string() });
    }
    let child_id = child_log.instance.as_str();
    let is_superseded = parent_log.events.iter().any(|ev| {
        ev.kind == Superseded::KIND
            && serde_json::from_value::<Superseded>(ev.body.clone())
                .is_ok_and(|s| s.child_instance == child_id)
    });
    let is_quiesced = parent_log.events.iter().any(|ev| {
        ev.kind == SubflowQuiesced::KIND
            && serde_json::from_value::<SubflowQuiesced>(ev.body.clone())
                .is_ok_and(|q| q.child_instance == child_id)
    });
    if is_quiesced {
        return Err(SubflowError::SubflowSuperseded { child: child_id.to_string() });
    }
    if is_superseded {
        if reconcile_only && child_has_open_effect(child_log) {
            return Ok(());
        }
        return Err(SubflowError::SubflowSuperseded { child: child_id.to_string() });
    }
    Ok(())
}

/// Atomically appends `MapExpanded` under exclusive `WriteGuard` (never inside read-only `next`).
/// If `items` is empty (`0` items), also records a clean empty `Completed` receipt on the `map`
/// node.
#[allow(clippy::too_many_arguments)]
pub fn expand_map(
    guard: &mut WriteGuard,
    parent: &InstanceId,
    node: &str,
    iteration: Iteration,
    items: Vec<MapItemSpec>,
    inputs: BTreeMap<String, String>,
    caller_owner: &Owner,
) -> Result<u64, SubflowError> {
    let log = guard.log(parent).map_err(|e| SubflowError::Storage(e.to_string()))?;
    if parent_current_owner(&log).as_ref() != Some(caller_owner) {
        return Err(SubflowError::NotOwner);
    }
    let started = pinned(&log).map_err(|e| SubflowError::Storage(e.to_string()))?;
    let Some(n) = started.ir.nodes.get(node) else {
        return Err(SubflowError::WrongClass(node.to_string()));
    };
    let Class::Map { max_items, .. } = &n.class else {
        return Err(SubflowError::WrongClass(node.to_string()));
    };
    if items.len() > *max_items as usize {
        return Err(SubflowError::InvalidExpansion {
            detail: format!("{} items exceeds max_items {max_items}", items.len()),
        });
    }
    let mut keys = BTreeSet::new();
    let mut normalized_items = Vec::with_capacity(items.len());
    for mut item in items {
        if item.key.trim().is_empty() {
            return Err(SubflowError::InvalidExpansion { detail: "empty item_key".into() });
        }
        if !keys.insert(item.key.clone()) {
            return Err(SubflowError::InvalidExpansion {
                detail: format!("duplicate item_key {:?}", item.key),
            });
        }
        item.write_set =
            validate_write_set(&item.write_set).map_err(|detail| SubflowError::InvalidExpansion { detail })?;
        normalized_items.push(item);
    }

    let state = fold_map_state(&log, node, &iteration);
    let map_epoch = state.latest_epoch.map_or(0, |e| e + 1);
    let ev = MapExpanded {
        node: node.to_string(),
        iteration: iteration.clone(),
        map_epoch,
        inputs: inputs.clone(),
        items: normalized_items.clone(),
    };
    let new_ev = ev.to_event().map_err(|e| SubflowError::Storage(e.to_string()))?;
    { let _seq = guard.log(parent).map_err(|e| SubflowError::Storage(e.to_string()))?.seq(); guard.append(parent, _seq, new_ev) }
        .map_err(|e| SubflowError::Storage(e.to_string()))?;

    // Empty map (0 items) completes immediately with an empty items map and empty performers union.
    if normalized_items.is_empty() {
        let done = Completed {
            node: node.to_string(),
            iteration,
            attempt: 1,
            inputs,
            outputs: serde_json::json!({ "items": {} }),
            verify: None,
            performer: None,
            performers: Vec::new(),
            tool_use_id: None,
            audit: None,
        };
        let done_ev = done.to_event().map_err(|e| SubflowError::Storage(e.to_string()))?;
        { let _seq = guard.log(parent).map_err(|e| SubflowError::Storage(e.to_string()))?.seq(); guard.append(parent, _seq, done_ev) }
            .map_err(|e| SubflowError::Storage(e.to_string()))?;
    }
    Ok(map_epoch)
}

/// Claims one `map` item under exclusive `WriteGuard`, enforcing:
/// - Crash Points A & B idempotency via deterministic `fi_sub_*` child IDs;
/// - Atomic claim-time `disjoint_writes` retaining reservations on `SubflowStarted` AND unquiesced
///   `Superseded` siblings until `SubflowQuiesced`;
/// - `max_concurrency` limit;
/// - Runtime `reserve_providers` / `provider_pool` liveness enforcer (`provider_pool_exhausted`);
/// - Canonical `parent -> child` creation order (child `Started` with `parent_ref`, then parent
///   `SubflowStarted` with `owner_epoch`).
#[allow(clippy::too_many_arguments)]
pub fn claim_map_item(
    guard: &mut WriteGuard,
    parent: &InstanceId,
    node: &str,
    iteration: Iteration,
    item_key: &str,
    performer: Option<Performer>,
    caller_owner: &Owner,
) -> Result<(InstanceId, u64), SubflowError> {
    let parent_log = guard.log(parent).map_err(|e| SubflowError::Storage(e.to_string()))?;
    if parent_current_owner(&parent_log).as_ref() != Some(caller_owner) {
        return Err(SubflowError::NotOwner);
    }
    if parent_is_retired(&parent_log) {
        return Err(SubflowError::SubflowSuperseded { child: parent.to_string() });
    }
    let started = pinned(&parent_log).map_err(|e| SubflowError::Storage(e.to_string()))?;
    let Some(n) = started.ir.nodes.get(node) else {
        return Err(SubflowError::WrongClass(node.to_string()));
    };
    let Class::Map {
        max_items,
        max_concurrency_param,
        ready_when,
        reserve_providers,
        provider_pool,
        item_retry_max,
        uses,
        subflow_ir_hash,
        ..
    } = &n.class
    else {
        return Err(SubflowError::WrongClass(node.to_string()));
    };

    let state = fold_map_state(&parent_log, node, &iteration);
    let Some(map_epoch) = state.latest_epoch else {
        return Err(SubflowError::InvalidExpansion { detail: format!("map node {node} has not been expanded") });
    };
    let Some(item) = state.items.get(item_key) else {
        return Err(SubflowError::InvalidExpansion { detail: format!("unknown item_key {item_key:?}") });
    };

    // If already in-flight in this epoch, return existing child idempotently!
    if let Some(active) = state.in_flight.get(item_key) {
        let cid = InstanceId::new(&active.child_instance).map_err(|e| SubflowError::Storage(e.to_string()))?;
        return Ok((cid, active.owner_epoch));
    }

    let retry_attempt = state.failed_attempts.get(item_key).map_or(0, Vec::len) as u32;
    if retry_attempt > *item_retry_max {
        return Err(SubflowError::InvalidExpansion {
            detail: format!("item {item_key} exhausted item_retry.max ({item_retry_max})"),
        });
    }

    // 1. Concurrency limit recheck under exclusive write lock.
    let mut limit = *max_items as usize;
    if let Some(param_name) = max_concurrency_param
        && let Some(v) = started.params.get(param_name).and_then(Value::as_u64)
        && v > 0
    {
        limit = limit.min(v as usize);
    }
    if state.in_flight.len() >= limit {
        return Err(SubflowError::ConcurrencyLimit { in_flight: state.in_flight.len(), limit });
    }

    // 2. Atomic claim-time `disjoint_writes` check retaining reservations on BOTH `SubflowStarted`
    //    AND unquiesced `Superseded` siblings until `SubflowQuiesced`!
    if matches!(ready_when, Some(ReadyWhen::DisjointWrites)) && !item.write_set.is_empty() {
        for (sib_key, active_sib) in &state.in_flight {
            if write_sets_intersect(&item.write_set, &active_sib.write_set)
                .map_err(|detail| SubflowError::InvalidExpansion { detail })?
            {
                return Err(SubflowError::WriteSetConflict {
                    item_key: item_key.to_string(),
                    sibling: sib_key.clone(),
                });
            }
        }
        for (child_id, sup) in &state.superseded_unquiesced {
            if let Some(orig) = state.started_by_child.get(child_id)
                && write_sets_intersect(&item.write_set, &orig.write_set)
                    .map_err(|detail| SubflowError::InvalidExpansion { detail })?
            {
                return Err(SubflowError::WriteSetConflict {
                    item_key: item_key.to_string(),
                    sibling: format!("superseded:{}:{child_id}", sup.item_key),
                });
            }
        }
    }

    // 3. Runtime provider-pool & `reserve_providers` liveness enforcer.
    if let Some(perf) = &performer {
        let Some(prov) = provider_of(&perf.harness) else {
            return Err(SubflowError::ProviderPoolExhausted {
                detail: format!("performer harness {:?} has no attested provider", perf.harness),
            });
        };
        if !provider_pool.is_empty() && !provider_pool.iter().any(|p| p == prov) {
            return Err(SubflowError::ProviderPoolExhausted {
                detail: format!("provider {prov:?} is not in allowed provider_pool {provider_pool:?}"),
            });
        }
        if let Some(reserve) = reserve_providers {
            let mut prospective = state.used_providers.clone();
            prospective.insert(prov.to_string());
            let remaining = PROVIDERS.len().saturating_sub(prospective.len());
            if remaining < *reserve as usize {
                return Err(SubflowError::ProviderPoolExhausted {
                    detail: format!(
                        "accepting provider {prov:?} would leave {remaining} unused providers (< reserve_providers={reserve})"
                    ),
                });
            }
        }
    }

    // 4. Deterministic child ID (Crash Points A & B idempotency).
    let child_id = child_instance_id(parent, node, &iteration, map_epoch, item_key, retry_attempt)?;
    let owner_epoch = parent_owner_epoch(&parent_log);
    let child_ir = started
        .ir
        .subflows
        .get(uses)
        .ok_or_else(|| SubflowError::Storage(format!("missing pinned subflow IR {uses}")))?
        .clone();

    // Crash Point B check: if deterministic child log ALREADY exists with matching parent_ref and
    // child_ir_hash, do NOT spawn a duplicate — simply append the missing parent SubflowStarted!
    let child_exists = guard.log(&child_id).is_ok();
    if !child_exists {
        let mut child_params = Map::new();
        child_params.insert("item".to_string(), item.input.clone());
        child_params.insert("item_key".to_string(), Value::String(item_key.to_string()));
        if let Some(obj) = started.params.as_object() {
            for (k, v) in obj {
                child_params.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        let child_started = Started {
            flow: uses.clone(),
            ir_hash: subflow_ir_hash.clone(),
            ir: child_ir,
            params: Value::Object(child_params),
            owner: Some(caller_owner.clone()),
            audit: None,
            parent_ref: Some(ParentRef {
                parent_instance: parent.to_string(),
                node: node.to_string(),
                iteration: iteration.clone(),
                map_epoch,
                item_key: item_key.to_string(),
                retry_attempt,
            }),
        };
        let ev = child_started.to_event().map_err(|e| SubflowError::Storage(e.to_string()))?;
        guard.append(&child_id, 0, ev)
            .map_err(|e| SubflowError::Storage(e.to_string()))?;
    }

    let parent_ev = SubflowStarted {
        node: node.to_string(),
        iteration,
        map_epoch,
        item_key: item_key.to_string(),
        retry_attempt,
        child_instance: child_id.to_string(),
        child_ir_hash: subflow_ir_hash.clone(),
        write_set: item.write_set.clone(),
        owner_epoch,
        performer,
    };
    let ev = parent_ev.to_event().map_err(|e| SubflowError::Storage(e.to_string()))?;
    { let _seq = guard.log(parent).map_err(|e| SubflowError::Storage(e.to_string()))?.seq(); guard.append(parent, _seq, ev) }
        .map_err(|e| SubflowError::Storage(e.to_string()))?;

    Ok((child_id, owner_epoch))
}

/// Handles upstream staleness mid-map: preserves unchanged `(item_key, input_digest)` completed
/// items incrementally and marks changed or removed active items `Superseded` (retaining their
/// `write_set` reservations until `SubflowQuiesced`).
#[allow(clippy::too_many_arguments)]
pub fn supersede_stale_items(
    guard: &mut WriteGuard,
    parent: &InstanceId,
    node: &str,
    iteration: Iteration,
    new_items: Vec<MapItemSpec>,
    inputs: BTreeMap<String, String>,
    caller_owner: &Owner,
) -> Result<(u64, Vec<String>), SubflowError> {
    let parent_log = guard.log(parent).map_err(|e| SubflowError::Storage(e.to_string()))?;
    if parent_current_owner(&parent_log).as_ref() != Some(caller_owner) {
        return Err(SubflowError::NotOwner);
    }
    let state = fold_map_state(&parent_log, node, &iteration);
    let next_epoch = state.latest_epoch.map_or(0, |e| e + 1);
    let new_map: BTreeMap<String, &MapItemSpec> = new_items.iter().map(|i| (i.key.clone(), i)).collect();

    let mut superseded_children = Vec::new();
    for (old_key, active) in &state.in_flight {
        let unchanged = new_map
            .get(old_key)
            .zip(state.items.get(old_key))
            .is_some_and(|(new_spec, old_spec)| new_spec.input_digest == old_spec.input_digest);
        if !unchanged {
            let sup = Superseded {
                node: node.to_string(),
                iteration: iteration.clone(),
                map_epoch: active.map_epoch,
                item_key: old_key.clone(),
                child_instance: active.child_instance.clone(),
                superseded_by_epoch: next_epoch,
            };
            let ev = sup.to_event().map_err(|e| SubflowError::Storage(e.to_string()))?;
            { let _seq = guard.log(parent).map_err(|e| SubflowError::Storage(e.to_string()))?.seq(); guard.append(parent, _seq, ev) }
                .map_err(|e| SubflowError::Storage(e.to_string()))?;
            superseded_children.push(active.child_instance.clone());
        }
    }

    let _epoch = expand_map(guard, parent, node, iteration, new_items, inputs, caller_owner)?;
    Ok((next_epoch, superseded_children))
}

/// Narrowly scoped `--reconcile-only` completion permitted on a `Superseded` child that has an
/// open `EffectStarted` (fenced by `parent.owner` + `owner_epoch`). Never executes `run:`.
#[allow(clippy::too_many_arguments)]
pub fn reconcile_superseded_effect(
    guard: &mut WriteGuard,
    parent: &InstanceId,
    child: &InstanceId,
    child_node: &str,
    child_iteration: Iteration,
    attempt: u32,
    inputs: BTreeMap<String, String>,
    reconcile_outputs: Value,
    caller_owner: &Owner,
    claim_owner_epoch: u64,
) -> Result<(), SubflowError> {
    let parent_log = guard.log(parent).map_err(|e| SubflowError::Storage(e.to_string()))?;
    let child_log = guard.log(child).map_err(|e| SubflowError::Storage(e.to_string()))?;
    check_child_write_fence(&parent_log, &child_log, caller_owner, claim_owner_epoch, true)?;
    if !child_has_open_effect(&child_log) {
        return Err(SubflowError::SubflowSuperseded { child: child.to_string() });
    }
    let rec = EffectReconciled {
        node: child_node.to_string(),
        iteration: child_iteration,
        attempt,
        inputs,
        outputs: reconcile_outputs,
    };
    let ev = rec.to_event().map_err(|e| SubflowError::Storage(e.to_string()))?;
    { let _seq = guard.log(child).map_err(|e| SubflowError::Storage(e.to_string()))?.seq(); guard.append(child, _seq, ev) }
        .map_err(|e| SubflowError::Storage(e.to_string()))?;
    Ok(())
}

/// Records `SubflowQuiesced` on a `Superseded` child, releasing its `write_set` reservation.
/// NEVER infers quiescence from wall-clock lease expiry alone: requires BOTH a non-empty confirmed
/// worker `termination_receipt` AND zero open `EffectStarted` entries in the child log!
#[allow(clippy::too_many_arguments)]
pub fn quiesce_subflow(
    guard: &mut WriteGuard,
    parent: &InstanceId,
    node: &str,
    iteration: Iteration,
    item_key: &str,
    child: &InstanceId,
    termination_receipt: &str,
    caller_owner: &Owner,
) -> Result<(), SubflowError> {
    if termination_receipt.trim().is_empty() {
        return Err(SubflowError::QuiescenceRefused {
            detail: "confirmed worker termination_receipt is required (lease expiry alone never establishes quiescence)".into(),
        });
    }
    let parent_log = guard.log(parent).map_err(|e| SubflowError::Storage(e.to_string()))?;
    if parent_current_owner(&parent_log).as_ref() != Some(caller_owner) {
        return Err(SubflowError::NotOwner);
    }
    let child_log = guard.log(child).map_err(|e| SubflowError::Storage(e.to_string()))?;
    if child_has_open_effect(&child_log) {
        return Err(SubflowError::QuiescenceRefused {
            detail: format!("child {child} still has an unreconciled EffectStarted"),
        });
    }
    let state = fold_map_state(&parent_log, node, &iteration);
    let Some(sup) = state.superseded_unquiesced.get(child.as_str()) else {
        if state.quiesced_children.contains(child.as_str()) {
            return Ok(());
        }
        return Err(SubflowError::QuiescenceRefused {
            detail: format!("child {child} is not in superseded_unquiesced state"),
        });
    };
    let ev = SubflowQuiesced {
        node: node.to_string(),
        iteration,
        map_epoch: sup.map_epoch,
        item_key: item_key.to_string(),
        child_instance: child.to_string(),
        termination_receipt: termination_receipt.to_string(),
    };
    let new_ev = ev.to_event().map_err(|e| SubflowError::Storage(e.to_string()))?;
    { let _seq = guard.log(parent).map_err(|e| SubflowError::Storage(e.to_string()))?.seq(); guard.append(parent, _seq, new_ev) }
        .map_err(|e| SubflowError::Storage(e.to_string()))?;
    Ok(())
}

/// Recursively collects the deduplicated union of performers recorded across a child log (and any
/// nested `SubflowCompleted` / `SubflowFailed` receipts inside it).
pub fn collect_child_performers(child_log: &Log) -> Vec<Performer> {
    let mut out: Vec<Performer> = Vec::new();
    let mut seen: BTreeSet<(String, Option<String>, Option<String>)> = BTreeSet::new();
    let mut push = |p: Performer| {
        let key = (p.harness.clone(), p.model.clone(), p.session.clone());
        if seen.insert(key) {
            out.push(p);
        }
    };
    for ev in &child_log.events {
        match ev.kind.as_str() {
            Claimed::KIND => {
                if let Ok(cl) = serde_json::from_value::<Claimed>(ev.body.clone())
                    && let Some(p) = cl.performer
                {
                    push(p);
                }
            }
            Completed::KIND => {
                if let Ok(c) = serde_json::from_value::<Completed>(ev.body.clone()) {
                    if let Some(p) = c.performer {
                        push(p);
                    }
                    for p in c.performers {
                        push(p);
                    }
                }
            }
            SubflowCompleted::KIND => {
                if let Ok(sc) = serde_json::from_value::<SubflowCompleted>(ev.body.clone()) {
                    for p in sc.performers {
                        push(p);
                    }
                }
            }
            SubflowFailed::KIND => {
                if let Ok(sf) = serde_json::from_value::<SubflowFailed>(ev.body.clone()) {
                    for p in sf.performers {
                        push(p);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Crash Point C idempotent finalization of one `map` item:
/// Reads the child's latched log under lock (never caller say-so), records `SubflowCompleted` or
/// `SubflowFailed`, and once all items settle appends the parent `Completed` receipt with
/// `item_key`-sorted outputs and deduplicated child `performers` union!
#[allow(clippy::too_many_arguments)]
pub fn finalize_map_item(
    guard: &mut WriteGuard,
    parent: &InstanceId,
    node: &str,
    iteration: Iteration,
    item_key: &str,
    child: &InstanceId,
    failed_error: Option<&str>,
    inputs: BTreeMap<String, String>,
    caller_owner: &Owner,
) -> Result<bool, SubflowError> {
    let parent_log = guard.log(parent).map_err(|e| SubflowError::Storage(e.to_string()))?;
    if parent_current_owner(&parent_log).as_ref() != Some(caller_owner) {
        return Err(SubflowError::NotOwner);
    }
    let started = pinned(&parent_log).map_err(|e| SubflowError::Storage(e.to_string()))?;
    let Some(n) = started.ir.nodes.get(node) else {
        return Err(SubflowError::WrongClass(node.to_string()));
    };
    let Class::Map { on_item_failure, item_retry_max, subflow_ir_hash, .. } = &n.class else {
        return Err(SubflowError::WrongClass(node.to_string()));
    };
    let child_log = guard.log(child).map_err(|e| SubflowError::Storage(e.to_string()))?;
    let state = fold_map_state(&parent_log, node, &iteration);
    let map_epoch = state.latest_epoch.unwrap_or(0);

    // Idempotent Crash Point C check: already finalized for this child?
    if state.completed.get(item_key).is_some_and(|c| c.child_instance == child.as_str()) {
        return Ok(false);
    }

    // 1. Verify `child` is actually the active (`in_flight`) child spawned for THIS item_key (not
    //    unrelated, not belonging to another item, and not superseded/quiesced).
    let Some(active_started) = state.in_flight.get(item_key) else {
        return Err(SubflowError::InvalidChild {
            detail: format!("item {item_key:?} has no active in-flight child (supplied {child})"),
        });
    };
    if active_started.child_instance != child.as_str() {
        return Err(SubflowError::InvalidChild {
            detail: format!(
                "supplied child {child} is not the active child {} for item {item_key:?}",
                active_started.child_instance
            ),
        });
    }
    if state.superseded_unquiesced.contains_key(child.as_str()) || state.quiesced_children.contains(child.as_str()) {
        return Err(SubflowError::SubflowSuperseded { child: child.to_string() });
    }

    let mut performers = collect_child_performers(&child_log);
    if let Some(sp) = active_started.performer.clone() {
        let key = (sp.harness.clone(), sp.model.clone(), sp.session.clone());
        if !performers.iter().any(|p| (p.harness.clone(), p.model.clone(), p.session.clone()) == key) {
            performers.push(sp);
        }
    }

    if let Some(err) = failed_error {
        let retry_attempt = state.failed_attempts.get(item_key).map_or(0, Vec::len) as u32;
        let fail_ev = SubflowFailed {
            node: node.to_string(),
            iteration: iteration.clone(),
            map_epoch,
            item_key: item_key.to_string(),
            retry_attempt,
            child_instance: child.to_string(),
            error: err.to_string(),
            performers,
        };
        let ev = fail_ev.to_event().map_err(|e| SubflowError::Storage(e.to_string()))?;
        { let _seq = guard.log(parent).map_err(|e| SubflowError::Storage(e.to_string()))?.seq(); guard.append(parent, _seq, ev) }
            .map_err(|e| SubflowError::Storage(e.to_string()))?;
    } else {
        // 2. Verify the child instance is genuinely terminal (never convert an empty or unfinished
        //    child log into SubflowCompleted with null outputs!).
        let child_started = pinned(&child_log).map_err(|e| SubflowError::Storage(e.to_string()))?;
        let child_next =
            next_of(&child_log, &child_started, clock::now()).map_err(|e| SubflowError::Storage(e.to_string()))?;
        if !child_next.enabled.is_empty() {
            let names: Vec<&str> = child_next.enabled.iter().map(|e| e.node.as_str()).collect();
            return Err(SubflowError::ChildNotTerminal {
                child: child.to_string(),
                detail: format!("enabled nodes still remain: {names:?}"),
            });
        }
        let non_done: Vec<(&String, State)> = child_next
            .pending
            .iter()
            .filter(|(_, s)| {
                !matches!(s.state, State::Done)
                    && !s.unmet.iter().any(|r| matches!(r, Reason::Unreachable { .. } | Reason::Switch { .. }))
            })
            .map(|(k, s)| (k, s.state))
            .collect();
        if !non_done.is_empty() {
            return Err(SubflowError::ChildNotTerminal {
                child: child.to_string(),
                detail: format!("pending non-terminal nodes remain: {non_done:?}"),
            });
        }
        let has_terminal_receipt = child_log
            .events
            .iter()
            .any(|e| e.kind == Completed::KIND || e.kind == Answered::KIND);
        if !has_terminal_receipt {
            return Err(SubflowError::ChildNotTerminal {
                child: child.to_string(),
                detail: "child log contains no Completed or Answered receipt".to_string(),
            });
        }

        // 3. Enforce 6-surface post-write isolation if the item declared a write_set and the
        //    child's workdir is a git worktree!
        if let Some(item_spec) = state.items.get(item_key)
            && !item_spec.write_set.is_empty()
            && let Some(raw_wd) = child_started.ir.workdir.as_deref()
        {
            let wd_str = resolve_simple_workdir(raw_wd, &child_started.params);
            let wd = PathBuf::from(&wd_str);
            if wd.is_dir() && wd.join(".git").exists() {
                let base_sha = child_started.params.get("base_sha").and_then(Value::as_str);
                let ver = verify_worktree_writes_auto(&wd, base_sha, &item_spec.write_set)
                    .map_err(|e| SubflowError::Storage(format!("write_set verification failed: {e}")))?;
                if !ver.is_ok() {
                    return Err(SubflowError::WriteSetViolation {
                        item_key: item_key.to_string(),
                        violations: ver.violations,
                    });
                }
            }
        }

        // Extract child's terminal Completed outputs directly from the child log (never say-so).
        let child_outputs = child_log
            .events
            .iter()
            .rev()
            .find(|e| e.kind == Completed::KIND)
            .and_then(|e| serde_json::from_value::<Completed>(e.body.clone()).ok())
            .map(|c| c.outputs)
            .unwrap_or(Value::Null);
        let outputs_digest = digest(&child_outputs);
        let done_ev = SubflowCompleted {
            node: node.to_string(),
            iteration: iteration.clone(),
            map_epoch,
            item_key: item_key.to_string(),
            child_instance: child.to_string(),
            child_ir_hash: subflow_ir_hash.clone(),
            terminal_status: "done".to_string(),
            outputs_digest,
            outputs: child_outputs,
            performers,
        };
        let ev = done_ev.to_event().map_err(|e| SubflowError::Storage(e.to_string()))?;
        { let _seq = guard.log(parent).map_err(|e| SubflowError::Storage(e.to_string()))?.seq(); guard.append(parent, _seq, ev) }
            .map_err(|e| SubflowError::Storage(e.to_string()))?;
    }

    // Re-read parent log to see if the entire map node is now settled.
    let updated_log = guard.log(parent).map_err(|e| SubflowError::Storage(e.to_string()))?;
    let updated = fold_map_state(&updated_log, node, &iteration);
    if !updated.superseded_unquiesced.is_empty() {
        return Ok(false);
    }

    let all_settled = updated.items.keys().all(|k| {
        if updated.completed.contains_key(k) {
            return true;
        }
        let fails = updated.failed_attempts.get(k).map_or(0, Vec::len) as u32;
        matches!(on_item_failure, OnItemFailure::CollectAll) && fails > *item_retry_max
    });
    if !all_settled {
        return Ok(false);
    }

    // Build deterministic `item_key`-sorted outputs + deduplicated union of attested performers
    // across BOTH completed AND failed attempts (so a provider that participated in a failed
    // attempt before a retry succeeded can NEVER review its own work downstream!).
    let mut items_out = Map::new();
    let mut spine_entries = Vec::new();
    let mut all_performers = Vec::new();
    let mut seen_perf = BTreeSet::new();
    let mut push_perf = |p: &Performer| {
        let pk = (p.harness.clone(), p.model.clone(), p.session.clone());
        if seen_perf.insert(pk) {
            all_performers.push(p.clone());
        }
    };
    for k in updated.items.keys() {
        // Always include every failed-attempt performer even when a retry later succeeded!
        if let Some(fails) = updated.failed_attempts.get(k) {
            for f in fails {
                for p in &f.performers {
                    push_perf(p);
                }
            }
        }
        if let Some(c) = updated.completed.get(k) {
            items_out.insert(
                k.clone(),
                serde_json::json!({
                    "status": c.terminal_status,
                    "child_instance": c.child_instance,
                    "child_ir_hash": c.child_ir_hash,
                    "outputs": c.outputs,
                }),
            );
            spine_entries.push(serde_json::json!({
                "item_key": k,
                "child_ir_hash": c.child_ir_hash,
                "terminal_status": c.terminal_status,
            }));
            for p in &c.performers {
                push_perf(p);
            }
        } else if let Some(fails) = updated.failed_attempts.get(k) {
            let last_err = fails.last().map(|f| f.error.as_str()).unwrap_or("failed");
            items_out.insert(
                k.clone(),
                serde_json::json!({ "status": "failed", "error": last_err }),
            );
            spine_entries.push(serde_json::json!({
                "item_key": k,
                "child_ir_hash": subflow_ir_hash,
                "terminal_status": "failed",
            }));
        }
    }

    let map_outputs = serde_json::json!({
        "items": items_out,
        "spine": spine_entries,
    });
    let _digest = performers_receipt_digest(&map_outputs, None, &serde_json::json!(all_performers));
    let completed = Completed {
        node: node.to_string(),
        iteration,
        attempt: 1,
        inputs,
        outputs: map_outputs,
        verify: None,
        performer: None,
        performers: all_performers,
        tool_use_id: None,
        audit: None,
    };
    let ev = completed.to_event().map_err(|e| SubflowError::Storage(e.to_string()))?;
    { let _seq = guard.log(parent).map_err(|e| SubflowError::Storage(e.to_string()))?.seq(); guard.append(parent, _seq, ev) }
        .map_err(|e| SubflowError::Storage(e.to_string()))?;
    Ok(true)
}

fn resolve_simple_workdir(template: &str, params: &Value) -> String {
    let mut out = template.to_string();
    if let Some(obj) = params.as_object() {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                out = out.replace(&format!("{{{{params.{k}}}}}"), s);
            }
        }
    }
    out
}
