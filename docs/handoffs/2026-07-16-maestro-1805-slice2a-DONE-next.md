# Handoff — BRO-1805 slice 2a DONE (block verdict), next = slice 2b (approve/revise/escalate)

**Date:** 2026-07-16
**Loop:** autonomous self-firing maestro build loop (P3 gate chain)
**Main tip:** `1c51e595` (PR #82 squash-merged)
**BRO-1805 status:** **In Progress** (multi-slice — do NOT mark Done until slice 2b + E2E lands)

## What shipped this iteration (slice 2a)

The first of the four F5 gate verdicts on the write surface (`POST /api/intents`). A human decides a `review` node's open `completion` gate with `{ type: "block", gateId, reason? }` → node **canceled** (D-GATE). Slice 1 (PR #81) opened the gate; this slice decides one verb.

**Change (surgical — `apps/runtime/src/api/intents.ts` + `intents.test.ts` ONLY):**

- New shared spine **`decideGateVerdict(db, gateId, verdict, extra, now)`**:
  1. resolve `gateId → gate (isNull deletedAt, else 404) → session (isNull deletedAt, else 404 "no live session") → node (deletedAt check, else 404)`
  2. **epoch-ownership guard** — refuse `409` if any newer live session exists on the node (`gt(session.startedAt, s.startedAt)`); an old session's stale gate must never cancel a review a newer run owns
  3. `target = resolveGateVerdict("review", verdict)` (resolves against the literal `review`, the only decidable state)
  4. **atomic verdict CAS** `UPDATE gate SET verdict WHERE id AND verdict IS NULL AND deletedAt IS NULL` — the single commit point; elects one decider under concurrent different-key decides. On win → `journalGateDecided` (FS-first via `SessionTee`: `bindIndexWriter(db)` + `fsJournal(runDir)`; payload widened to `{ gateId, kind, verdict, decidedBy: "human", decidedAt, reason? }`).
  5. **idempotent node completion** `UPDATE node SET state=target WHERE id AND state='review'` → on move, `emitNodeUpdated` (global, `sessionId: null`, `projectLiveNode(row)`).
- **block route handler**: validates `gateId` (non-empty string) + `reason` (string|undefined); lease acquire INSIDE the typed try; on error → best-effort lease delete + typed refusal / `intent_failed` 500.
- Only `block` is wired. approve / revise / escalate are slice 2b.

**Deliberately NOT here** (block-only, kept surgical):
- No `_work.md` write — BRO-1913 proved an uncommitted `_work.md` write dirties the tree (wedges the clean-tree-gated `approveMerge`) and a bare `writeFile` of a scanned file is a tombstone hazard.
- The node.state write is DB-only — the accepted BRO-1914 gap (scanner rescans `_work.md` state into the node row; a DB-only write reverts on rebuild). The decided gate **row** commits durably; `node.updated(canceled)` drops the card correctly.

## P20 saga (3 rounds — edge-dense file — Codex + Strata-B + CodeRabbit → MERGE, 0 surviving)

- **Codex round 1 (3/10):** fixed FS-journal (slice-2a's `gate.decided` was DB-only `db.insert(event)` — the durability asymmetry vs slice-1's FS-journaled `gate.opened`), tombstone lookups, partial-write repair, refusal-typing.
- **Codex round 2 (4/10 DO-NOT-MERGE):** REACHABLE findings fixed + mutation-proven:
  - **finding-4** — lease acquired OUTSIDE the typed try → a SQLITE_BUSY on the lease insert escaped as untyped 500 and never released. Moved lease acquire inside the try + best-effort lease delete on throw.
  - **finding-3 (defensive)** — an old session's still-pending gate could cancel a review a newer session owns. UNREACHABLE without the accepted BRO-1914 rescan-revert gap (a node leaves `review` only via a verdict, a rescan-revert, or unwired `set_state`; the node CAS `WHERE state='review'` already declines a reverted node) — closed anyway with the epoch guard. **Mutation-proof:** neutralize the guard (`if(newer)` → `if(false)`) → the superseded-session test regresses `409 → 202`.
- **Strata-B round 2 (fan-out, 10 agents): MERGE 8/10**, all 5 round-1 fixes cleared. 2 surviving findings = 1 MINOR root: the CAS→journal pair is non-transactional, so after a rare FS-append (ENOSPC/EROFS/EACCES) or SQLITE_BUSY fault, `gate.decided` (+ the human's `reason`, which has no gate-row column) can be permanently un-journaled (the matching-verdict retry never re-journals). **Latent** — the decided gate row commits + survives normal restart, `node.updated(canceled)` still drops the card, and the sole consumer of the missing event is BRO-1915's unbuilt rebuild projector. Recommended fold (not blocker) → BRO-1915 Scope #4.
- **CodeRabbit's 6 threads → the same 3 accepted-scope classes**, each resolved reject-with-reason + ticket + resolved thread:
  - epoch check→CAS TOCTOU (`intents.ts:335` + test) → **BRO-1914** (residual named in-code; full atomicity needs the coordinated single-writer libsql can't give in-process).
  - non-transactional CAS→journal window (`intents.ts:369` + test) → **BRO-1915 Scope #4** (nullable `reason` column set inside the CAS + presence-guarded re-journal).
  - phantom-202 pending-lease (`intents.ts:550` + test) → **BRO-1916** (NEW, Low — pre-existing lease-protocol class shared with `new_mission`/`kill`; lease is presence not outcome).

Merge decision = same **D7 severity-calibration precedent** as slice 1 (confirmed defect with no live consumer + accepted-scope + ticket → merge-gate PASS).

**Verification:** 817 runtime+protocol tests pass, tsc + biome clean, dogfooded over real HTTP + libSQL file + SSE (block → gate decided + node canceled + `gate.decided`/`node.updated` cross the wire; same-key retry no-op), 0 unresolved review threads, 5 CI checks green.

## Reusable lessons

- **CodeRabbit's own severity label ("Major") is NOT the P20 gate.** When 2 independent strata + reachability analysis already adjudicated a finding as accepted-scope, CodeRabbit re-surfacing the same class is a reject-with-reason + ticket, not a re-opened blocker — but STILL trace each for a genuinely NEW reachable defect (none here; all 6 collapsed to the 3 known classes).
- **A durability fix (DB-only event → FS-journal) can INTRODUCE a new non-transactional partial-write window.** The FS-first journal fixed slice-1's asymmetry but opened the CAS→journal gap. Latency-classify it: does the missing datum have a LIVE consumer today? Here no (the consumer is an unbuilt projector) → MINOR/latent → ticket-fold.
- **The epoch guard is defense-in-depth, not a close.** A check→CAS TOCTOU on a non-transactional store is only fully closed by a coordinated single-writer; guard cheaply + ticket the residual.
- **Mutation-prove a defensive guard by neutralizing its predicate**, not by deleting the test.
- **When a review flags a fix as scope-creep, split it to a ticket, don't inline it** (reason-column durability → BRO-1915 #4, not this PR).
- **No branch-to-main slip** (branched FIRST — the rule-of-three fix holds across 1912/1913/1805/1805-2a).

## NEXT — BRO-1805 slice 2b (approve / revise / escalate)

Wire the remaining three verdicts through the same `decideGateVerdict` spine (`docs/contracts/gate-queue.md`):
- **approve** → `done` — **wires `approveMerge` live** (BRO-1802). ⚠️ approve runs the clean-tree-gated `approveMerge` (`isClean`, `merge/merge.ts:147`) — the verdict path must NOT dirty the tree (BRO-1913 lesson). **Fold BRO-1881** (unify approve + new_mission + reap under one index-lock) and **BRO-1914** (durable node.state = coordinated workspace state-writer, commit-as-txn under the shared lock) in/before this slice.
- **revise** → `triggered` + redispatch with `fix_plan` feedback.
- **escalate/grant** → row stays `review`, re-decidable.
- `gate.decided` payload widening is already done (slice 2a) — it also unblocks BRO-1915's projector.
- data-gate stream parts reconciled by id; 5s grace/undo window.
- State-machine enforcement: no auto-done under `gate: human`.
- `playwright gate-slice.spec` E2E (seeded run → Needs you → each verdict lands its state; auto-done attempt throws).

If slice 2b is too large for one clean PR, cut a sub-slice (e.g. approve-verb-only first, since it carries the approveMerge + BRO-1881/1914 weight) and keep BRO-1805 In Progress.

## Follow-ups filed / open

- **BRO-1916** (NEW, Low) — idempotency lease: distinguish pending vs committed outcome (kills phantom-202 on concurrent same-key; fix belongs at the shared lease helper, all intents).
- **BRO-1915** (Med) — gate-row journal-replay projector **+ Scope #4** (folded: nullable `reason` column in the verdict CAS + presence-guarded re-journal). Sequence with/after the `gate.decided` slice.
- **BRO-1914** (Med) — durable node.state coordinated writer (couples to the approve slice; the DB-only-revert and epoch-TOCTOU both close here).
- **BRO-1881** (Med) — index-lock unify (fold into the approve/merge slice).

## P3 chain remaining

1805 (slice 2b = approve/revise/escalate, next) → 1809 (M5 inspector) → 1813 (adversarial suite) → 1818 (autonomy ledger) → 1821 (P3-exit), then P4/P5.
