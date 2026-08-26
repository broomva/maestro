# Handoff — BRO-1805 slice 1 DONE (completion gate opens), next = slice 2 (verdicts)

**Date:** 2026-07-16
**Loop:** autonomous self-firing maestro build loop (P3 gate chain)
**Main tip:** `dd78149` (PR #81 squash-merged)
**BRO-1805 status:** **In Progress** (multi-slice — do NOT mark Done until the verdicts + E2E slice lands)

## What shipped this iteration (slice 1)

The first vertical cut of the **F5 gate slice** (BRO-1805, the P3 centerpiece) — and the direct start on the provider-arc candidate "review-not-shown-as-Needs-you": a `review` node with no gate row can't be rendered as Needs-you with verbs.

**Change (surgical — `apps/runtime/src/supervisor/supervisor.ts` + `supervisor.test.ts` ONLY):**
- Generalized `openQuestionGate` → **`openGate(ctx, kind, proposal, emit)`** (`GateKind` param).
- A clean run that parks at `review` now inserts a pending **`completion` gate row** (`kind`, `proposalJson`, `verdict: null`) + journals `gate.opened { gateId, kind }` + surfaces `gateId` on the reap.
- Both completion review paths route through it: the `!done` no-check clean pass AND the verify-PASS `park_review`. The exit-20 `question` gate is unchanged (same code, `kind: "question"`).
- New DRY helper **`terminalReviewOrBlocked(ctx, entry, gateId, mismatch, respawns)`**: `gateId === undefined` (openGate insert faulted) → park **`blocked`** with typed `gate_open_failed` (redispatchable); else park `review`. The "review ⟹ has a gate" invariant now lives in exactly one place.

**Verdict wiring is a LATER slice — slice 1 is open-only.** No `gate.decided`, no approve/revise/block/escalate, no live `approveMerge`.

## P20 saga (Codex Strata-A 4/10 DO-NOT-MERGE → MERGE, 0 surviving blocker/major)

Two MAJORs past green CI + 34 passing supervisor tests, both resolved + mutation-proven:

1. **Gateless `review` on insert fault (Codex MAJOR).** `openGate` swallows an index-insert fault → `undefined`, but the caller still parked `review` with no gateId → an undecidable "Needs you" (gate-queue.md keys the four verdicts off gateId) that is also not redispatchable. **Fix:** `terminalReviewOrBlocked` parks `blocked` + `gate_open_failed`. Mutation-proven (flip the guard's park target `blocked→review` → the insert-fault test fails on `nodeState`). Fault forced in-test by **dropping the `gate` table** (only the gate insert faults; node/session/event/lease writes still succeed).
2. **False durability claim (Codex + Strata-B MAJOR).** The `openGate` docstring claimed the row "survives an index loss." It does not: the row is an index write that survives a NORMAL restart (persisted `index.db`; crash-recovery only reconciles the event tail) but is NOT reconstructed on `--rebuild`/index-loss — `TABLE_REBUILD.gate = ["journal-replay"]` is declared with no projector, and `gate.opened`'s payload lacks `sessionId/openedAt/proposalJson`. `index-schema.test.ts:67-71` only asserts the static map = a vacuous test. **Fix:** corrected docstring to accurate durability scope + filed **BRO-1915** (gate-row journal-replay projector).

Also added a `fresh_context respawn → then passes → exactly ONE completion gate` regression guard (respawn proven via `calls()==2`). Codex cleared: no duplicate gates, no exit-20 semantic drift, `gate:auto` fallback acceptable (auto-merge = BRO-1802, unwired into this path), ordering fine.

**Verification:** 808 runtime+protocol tests pass, tsc + biome clean, 4 required CI jobs + CodeRabbit green, 0 unresolved review threads. Merged via REST squash.

## Reusable lessons

- **A `review` node is only decidable if it carries a gate** — a best-effort gate insert that can fail must not leave the node at `review`; route review-vs-blocked through ONE helper so the invariant is single-sourced.
- **A swallowed insert that gates a downstream decision needs a caller-side fallback, not just `return undefined`** — the swallow keeps cleanup running; the caller picks the safe state (blocked + typed reason = redispatchable).
- **Force a single-table fault in-test by dropping that table** — isolates the fault to the one insert while sibling-table writes still succeed.
- **Don't claim durability the rebuild path doesn't deliver** — a `TABLE_REBUILD` entry is INTENT; grep for the projector before a docstring says "survives an index loss."
- **No branch-to-main slip** (branched FIRST — the rule-of-three fix holds across 1912/1913/1805).

## NEXT — BRO-1805 slice 2 (the four verdict intents)

Wire, end-to-end (see the ticket Scope + `docs/contracts/gate-queue.md`):
- **approve** → merge ticket — **wires `approveMerge` live** (the BRO-1802 primitive). ⚠️ approve runs the clean-tree-gated `approveMerge` (`isClean`, `merge/merge.ts:147`) — the verdict-writer must NOT dirty the tree (BRO-1913 lesson). **Fold BRO-1881** (unify approve+new_mission+reap under one index-lock) and **BRO-1914** (durable node.state = coordinated workspace state-writer, commit-as-txn under `serializeApprove`) in/before this slice.
- **revise** → `triggered` + redispatch with `fix_plan` feedback.
- **block** → `canceled` (D-GATE).
- **escalate/grant** → row stays `review`, re-decidable.
- `gate.decided` journaled to FS (D-DURABILITY) — the payload widening this slice does also unblocks **BRO-1915**'s projector.
- data-gate stream parts reconciled by id; 5s grace/undo window.
- State-machine enforcement: no auto-done under `gate: human` (transition throws).
- `playwright gate-slice.spec` E2E (seeded run → Needs you → each verdict lands its state; auto-done attempt throws).

## Follow-ups filed / open

- **BRO-1915** (Med) — gate-row journal-replay projector (F9 durability closure; sequence with/after the `gate.decided` slice).
- **BRO-1881** (Med) — index-lock unify (fold into the approve/merge slice).
- **BRO-1914** (Med) — durable node.state (couples to the approve slice; finding-1 manifests only when approve goes live).

## P3 chain remaining

1805 (slice 2 = verdicts, next) → 1809 (M5 inspector) → 1813 (adversarial suite) → 1818 (autonomy ledger) → 1821 (P3-exit), then P4/P5.
