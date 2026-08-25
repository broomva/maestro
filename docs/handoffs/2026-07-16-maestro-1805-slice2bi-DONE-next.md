# Handoff — BRO-1805 slice 2b-i DONE (revise + escalate), next = slice 2b-ii (approve)

**Date:** 2026-07-16
**Loop:** autonomous self-firing maestro build loop (P3 gate chain)
**Main tip:** `0ede346` (PR #83 squash-merged)
**BRO-1805 status:** **In Progress** (multi-slice — do NOT mark Done until approve + E2E lands)

## What shipped this iteration (slice 2b-i)

The two **non-merge** F5 gate verdicts on `POST /api/intents`, past `block` (slice 2a).

- **revise** `{ gateId, feedback }` → decide the gate `revise` → node `review → triggered` (send back for a fresh dispatch). **Reuses the terminating `decideGateVerdict` spine** (`resolveGateVerdict("review","revise")→triggered`; the spine's `target !== "review"` node-CAS already handles it). The `feedback` (trimmed) rides the `gate.decided` payload (`extra` widened to `{ reason?, feedback? }`).
- **escalate** `{ gateId, to }` → reassign `node.owner` ("point"). **NON-terminating** (gate-queue.md §4, `TERMINATING_VERDICTS = {approve, revise, block}`): escalate → `review` STAYS, re-decidable. It must NOT commit a gate verdict (that would break re-decidability), so it takes a **distinct `escalateGate` path**, not the terminal spine. Conditional owner CAS `UPDATE node SET owner=to WHERE id AND state='review' AND (owner IS NULL OR owner != to)` (NULL-safe change-guard → re-escalate to the same owner is an idempotent no-op) → on change: `journalGateEscalated` (FS-first, new `gate.escalated` event) + `emitNodeUpdated`.
- **Refactor:** extracted the shared `gateId → gate → session → node` lookup + epoch-ownership guard into **`resolveGateChain(db, gateId) → {g, n}`**. Both `decideGateVerdict` and `escalateGate` reuse it, so the hard-won slice-2a tombstone/epoch checks are single-sourced. Behavior-preserving for `block` (verified by Codex + the 29 existing tests).
- Added `GATE_ESCALATED` to the event catalog. `grant` deferred (needs a capability model — no `capability` column). No `_work.md` write; `node.owner` is DB-only (BRO-1914 gap — the scanner inherits `owner: child.owner ?? parent.owner` on rescan).

## P20 saga — the two strata DISAGREED, reconciled via reachability (like slice 2a)

- **Strata-B (14 agents, ran the code): MERGE 8/10, all findings ≤ MINOR.** Verified good: idempotent CAS spine, epoch guard, shared `resolveGateChain`, correct single-winner (decide) vs last-writer-wins (non-terminating escalate) semantics.
- **Strata-A (Codex, read-only sandbox → couldn't run tests → conservative): DO-NOT-MERGE 5/10, 3 MAJORs.** Resolved:
  - **MAJOR 1 — concurrent-decide phantom 202.** A block/revise decides the gate *between* escalate's `resolveGateChain` read and its owner-CAS → the 0-row owner-CAS was treated as an idempotent no-op → 202. **FIXED**: a re-read disambiguates the 0-row CAS → 409 when the node left `review`. Empirically **near-unreachable** (0/120 race-harness rounds — block can't commit inside escalate's µs read→CAS gap; Strata-B didn't surface it). The 409-branch isn't single-thread-forceable (the early `n.state !== "review"` check catches the read-time case), so I **mutation-proved the reachable sibling**: an open gate on a reverted non-review node (BRO-1914-revert degraded state) → escalate 409; neutralize BOTH the early-check AND the re-read → the phantom 202 surfaces (combined non-review protection is load-bearing). Added a concurrent race harness (block always decides; escalate 202 ⟹ escalated) as an invariant guard.
  - **MAJOR 2 → Strata-B MINOR — escalate emit-recovery asymmetry.** `journalGateEscalated` + `emitNodeUpdated` gate behind the owner-CAS `rowsAffected`; a journal fault after the owner commits isn't retry-repairable (the owner-CAS self-negates on retry), unlike `decideGateVerdict` whose emit rides a **separate** node-transition CAS that re-fires. Bounded (owner DB-durable, self-heals on reconnect D5, rare FS-fault). Folded → **BRO-1914** + **BRO-1915 Scope #4** (comments posted). `gate.escalated` has no rebuild consumer → strictly more latent than `gate.decided`.
  - **MAJOR 3 → Strata-B NIT (accepted-by-design)** — concurrent different-owner escalates last-writer-wins + journal-order divergence. Correct for a non-terminating reassignment (no corruption, deterministic final owner, no consumer of the ordering).
- **NITs fixed:** trim revise `feedback` (match escalate's `to.trim()`); corrected a stale "escalate is a no-op via this spine" docstring; narrowed `resolveGateChain` return `{g,s,n}` → `{g,n}` (dead `s`).
- **Deferred:** `withIdempotencyLease` helper (Strata-B: rule-of-three met across block/revise/escalate ~50-line lease boilerplate) → **slice 2b-ii**, where `approve` is the natural 4th-copy extraction point (re-touching the merged block handler here = scope creep).

**Verification:** 826 runtime+protocol tests pass (+8), tsc + biome clean, dogfooded 14/14 over real HTTP + libSQL file + SSE, every reachable fix mutation-proven, 0 unresolved review threads, 5 CI checks green.

## Reusable lessons

- **A non-terminating verb must NOT reuse the terminal-verdict spine** — committing a verdict for escalate would break re-decidability; give it its own path that leaves the gate open.
- **A 0-row conditional CAS is AMBIGUOUS** — it can mean "idempotent no-op (same value)" OR "the precondition changed underneath" (state left review). Disambiguate with a re-read; don't assume no-op.
- **A race-only defensive branch → mutation-prove the reachable sibling + document the branch** — 0/120 rounds proved the race near-unreachable, so the honest artifact is the deterministic degraded-state test + reasoning, not a flaky race-harness mutation-proof.
- **Emit-recovery asymmetry** — gating a live emit behind a value-change CAS makes it non-retry-repairable (the CAS self-negates on retry); gating it behind a separate state-transition CAS (as `decideGateVerdict` does) makes the retry re-fire it. The latter is the durable pattern.
- **Strata can disagree** — reconcile via reachability, not the lower/higher score. Codex in a read-only sandbox that can't run the tests reasons conservatively; Strata-B (ran the code) adjudicated the same findings down.

## NEXT — BRO-1805 slice 2b-ii (approve)

The last verdict + the heavy one:
- **approve** `{ gateId }` → `done` — **wires `approveMerge` live** (BRO-1802). ⚠️ approve runs the clean-tree-gated `approveMerge` (`isClean`, `merge/merge.ts:147`) — the verdict path must NOT dirty the tree (BRO-1913).
- **Fold BRO-1914** (durable node.state = coordinated workspace state-writer, commit-as-txn under the shared lock) — it's the prerequisite for approve to be rebuild-durable, AND it systematically closes the escalate concurrent-decide TOCTOU + the emit-recovery asymmetry (owner write + journal + emit in one transaction).
- **Fold BRO-1881** (unify approve + new_mission + reap under one index-lock).
- **Extract `withIdempotencyLease`** (approve = the 4th lease-boilerplate copy; extract the shared lease-acquire + try/catch/delete-lease into one helper across block/revise/escalate/approve).
- `playwright gate-slice.spec` E2E if feasible (seeded run → Needs you → each verdict lands its state; auto-done attempt throws).

If slice 2b-ii is too large for one clean PR, cut a sub-slice (BRO-1914 durable writer first, then approve on top) and keep BRO-1805 In Progress.

## Follow-ups filed / open

- **BRO-1914** (Med) — durable node.state coordinated writer (+escalate concurrent-decide TOCTOU + emit asymmetry, commented). The approve prerequisite.
- **BRO-1915** (Med) — gate-row rebuild projector + Scope #4 (reason-column-in-CAS + presence-guarded re-journal; +escalate `gate.escalated` window + emit-loss, commented).
- **BRO-1881** (Med) — index-lock unify (fold into approve).
- **BRO-1916** (Low) — lease pending-vs-committed outcome.

## P3 chain remaining

1805 (slice 2b-ii = approve, next) → 1809 (M5 inspector) → 1813 (adversarial suite) → 1818 (autonomy ledger) → 1821 (P3-exit), then P4/P5.
