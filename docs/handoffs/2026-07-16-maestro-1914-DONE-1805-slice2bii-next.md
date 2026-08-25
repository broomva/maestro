# Handoff — BRO-1914 DONE → next: BRO-1805 slice 2b-ii (approve)

**Loop iteration 2026-07-16 (autonomous self-firing build loop).**
**Merged:** PR [#84](https://github.com/broomva/maestro/pull/84) squash `9162308` — all 4 required checks + CodeRabbit green.
**BRO-1914 → Done. BRO-1805 stays In Progress (this was its prerequisite).**

## What shipped — the coordinated durable node-state writer
node.state is FS-authoritative (scanner re-derives `state`/`owner` from `_work.md` on every reconcile). A DB-only advance (supervisor park, gate verdicts) was lost/reverted on the next reconcile — a run parked at `review` ("Needs you") silently reverted after a restart. This persists every durable transition.

- **`persistNodeState(cwd, nodePath, patches, deps?)`** — `apps/runtime/src/merge/state-writer.ts`. Patches `_work.md` frontmatter, commits it path-scoped as a git txn under the **shared** `serializeWorkspaceGit` lock. Outcomes: `written` | `unchanged` (idempotent, no empty commit) | `absent` (ENOENT, benign) | `failed` (rolled back to exact pre-call bytes, tree clean) + `treeDirty?` on double-fault.
- Avoids both reverted-BRO-1913 modes: commits path-scoped (`gitCommitPaths`, no pre-`add` → commit fault leaves index untouched → worktree-only rollback) so the tree stays clean; writes atomically (temp+rename) so a scanner never sees a torn file.
- **`serializeWorkspaceGit`** (`merge/serialize.ts`, NEW) — per-workspace mutex, now shared by `approveMerge` + `persistNodeState` (first step of BRO-1881). **NON-REENTRANT.**
- **`setWorkFileFields`** (`packages/protocol/src/frontmatter.ts`) — comment-preserving YAML-CST frontmatter patch.
- **Wired**: supervisor `park`→`review`; intent `decideGateVerdict`→target state; `escalateGate`→new owner. Best-effort at the call site, LOGGED.
- Full suite 640 pass / 0 fail. Dogfooded against a real git workspace + libSQL scanIntoIndex rebuild.

## P20 (Strata-B; Codex unusable this iteration — sandbox network/echo)
- **MAJOR** (verified real): persisting `review` durably while verdicts stayed DB-only would revert a **decided** gate node back to a stranded, undecidable `review` (gate has a verdict → all F5 verbs 409). Fix = persist every transition. Re-review MERGE 8/10, 0 surviving.
- **CodeRabbit MAJOR** (double-fault): commit throws AND rollback write also throws → path left patched-but-uncommitted = silent dirty-tree wedge of every later approveMerge, indistinct from a clean-rollback `failed`. **Fixed** → surface distinctly `failed`+`treeDirty`; mutation-proven via an injectable `deps.atomicWrite` seam (root-safe).
- **CodeRabbit MINOR** (unbounded shared lock): deferred → BRO-1805 slice 2b-ii (approveMerge not wired to a live intent yet = no long-hold consumer; latency-window/no-live-consumer→MINOR+fold). Scoped requirement filed on BRO-1805.

## NEXT — BRO-1805 slice 2b-ii = approve (the FINAL F5 verdict)
approve → `done` + live `approveMerge` (BRO-1802) under the clean-tree gate. **Load-bearing constraints:**
1. **The shared `serializeWorkspaceGit` lock is NON-REENTRANT** — do NOT call `persistNodeState`/`persistGateAdvance` from inside `approveMergeCritical` (which already holds it) → self-deadlock. Persist `state:done` OUTSIDE the merge critical section, or make the lock reentrant first.
2. **Bound the intent-path durable write** (`persistGateAdvance`) with a timeout/fail-fast so a slow/stuck `approveMerge` can't stall POST /api/intents. Test e2e against a real held lock (see BRO-1805 Linear comment).
3. Reuse the `decideGateVerdict` terminal spine for approve's verdict CAS + node CAS (approve = the terminating verdict, like block). Persist `state:done` via the coordinated writer (now available).
4. Extract `withIdempotencyLease` (approve = the 4th lease-boilerplate copy).
5. FOLD BRO-1881 (index-lock unify) here if scope allows; else keep 1805 In Progress and cut a sub-slice.
6. Mark BRO-1805 DONE only if approve completes the F5 slice AND an E2E lands (all-4-verdicts); else stays In Progress.

## Chain after 1805
1809 (M5 inspector) → 1813 (adversarial suite) → 1818 (autonomy ledger) → 1821 (P3-exit) → P4/P5. Linear project `6c738b50`.

## Follow-ups open
BRO-1915 (gate-row journal-replay projector + reason-col durability), BRO-1881 (index-lock unify), BRO-1895 (Knowledge slice2b + KG read-path seam), BRO-1916 (lease pending-vs-committed), BRO-1892/1889/1887/1885/1878/1877 (Low).

## Loop discipline reminders
- **Branch FIRST** after `git checkout main` confirms clean, before ANY Read/Edit (rule-of-three slip: 1819+1814+1862).
- P20 local `git diff` (GraphQL 5k/hr); PR create/merge via REST.
- CI `quality` race-harness (2b-i escalate+block) is a known timing flake under CI load → P9 rerun self-heals (28/28 local).
