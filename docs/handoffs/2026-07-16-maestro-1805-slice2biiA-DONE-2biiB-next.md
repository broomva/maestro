# Handoff — BRO-1805 slice 2b-ii-A DONE → next: slice 2b-ii-B

**Loop iteration 2026-07-16 (autonomous self-firing build loop).**
**Merged:** PR [#85](https://github.com/broomva/maestro/pull/85) squash `43763a4` — all 4 required checks + CodeRabbit green.
**BRO-1805 stays In Progress** (2b-ii-B remains — see below).

## What shipped — the `approve` verb (the final F5 verdict)
`approve { gateId }` squash-merges the run (`approveMerge`, D1) then moves the node `review → done`. It is the ONE verdict with an irreversible side effect **before** it can commit, so it does NOT reuse the terminal `decideGateVerdict` spine:
- resolve chain → load `verdict.md` receipt (`readVerdict`; no passing receipt → refuse; value-validated base/attempt);
- **CLAIM** the gate (verdict CAS `WHERE verdict IS NULL`) BEFORE the merge (elects one approver, locks out concurrent block/revise);
- `approveMerge`. On **merged**: node→done + persist `_work.md=done` (BRO-1914 writer, OUTSIDE the non-reentrant lock) FIRST, THEN journal `gate.decided(approve)` + emit `gate.approved(sha)`. On **stale/refused/throw**: `releaseApproveClaim` (verdict→null) → gate reopens + typed refusal.
- `completeApproveIfLanded` — completes the node ONLY when it is already `done` (the merge winner's durable landed-signal), never off the bare claim.

Files: `apps/runtime/src/api/intents.ts` (approveGate + helpers + route), `apps/runtime/src/verifier/verdict.ts` (readVerdict). Full runtime suite 653 pass. Runtime all-4-verdicts coverage now complete (block 2a · revise+escalate 2b-i · approve 2b-ii-A).

## P20 (Strata-B; Codex unusable again — 5th consecutive)
Round 1 caught a **real data-integrity blocker** (all 3 lenses converged, 4/10): both idempotent-completion branches completed the node off the **bare claim**, so a concurrent DIFFERENT-key approve flips node→done while the owner is mid-merge → owner's then-stale merge releases the claim → **node=done over unmerged work on a reopened undecidable gate** (no crash). Fixed via the `node.state` landed-signal guard (mutation-proven). Round 2: MERGE 9/9/8, 0 surviving. CodeRabbit MINOR (readVerdict value-validation) also fixed.

## NEXT — BRO-1805 slice 2b-ii-B (finishes BRO-1805)
1. **Browser E2E** `playwright gate-slice.spec` — the done.check's all-4-verdicts E2E (seeded run → Needs you → each verdict lands its state → auto-done attempt throws). **This is required to mark BRO-1805 Done.**
2. **Auto-rebase-redispatch on stale** (D1 rung 3) — today a stale approve returns a typed 409; wire the automatic "rebase & re-verify" redispatch (needs a supervisor `dispatch` seam in the intent deps + a rebase fix_plan append).
3. **Extract `withIdempotencyLease`** — approve is the 4th copy of the lease boilerplate (block/revise/escalate/approve); extract the shared wrapper.
4. **BRO-1881 index-lock unify** + the **deferred intent-lock bound** (bound `persistGateAdvance` against a stuck `approveMerge`; the CodeRabbit MINOR from #84 — see the BRO-1805 Linear note).
5. **Git-evidence self-heal** for the 2 documented crash/transient-fault residuals (pre-merge-crash orphaned claim; merged-but-not-done). Non-corrupting today.
6. Fold to **BRO-1915**: approve's `gate.decided` journal (written after the node transition to protect it) is not retry-repairable on a post-complete journal fault.
7. Deferred nits: misleading 409 message on a lost-then-reopened CAS race; observability gap on a released claim (self-heals on reconnect).

Mark **BRO-1805 Done** only when the approve verb + the browser E2E (all-4-verdicts) both land.

## Chain after 1805
1809 (M5 inspector) → 1813 (adversarial suite) → 1818 (autonomy ledger) → 1821 (P3-exit) → P4/P5. Linear project `6c738b50`.

## Loop discipline reminders
- **Branch FIRST** after `git checkout main` confirms clean, before ANY Read/Edit (rule-of-three: 1819+1814+1862). Held again this iteration.
- P20 **round 2 is mandatory** after a blocker fix (it confirmed the fix + surfaced the cheap nits this iteration).
- Codex (Strata-A) is unusable — Strata-B fresh-subagent workflow is the P20 gate.
- Pre-commit hook (biome check on staged) is STRICTER than ad-hoc `biome ci <files>` → format --write + re-add.
- P20 local `git diff` (GraphQL 5k/hr); PR create/merge via REST.
