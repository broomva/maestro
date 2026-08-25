# Maestro handoff — BRO-1794 slice 1b-i DONE → slice 1b-ii next

**Date:** 2026-07-12
**Branch/tip:** `main` @ `d9f02c8` (BRO-1794 slice 1b-i squash-merged as PR #64)
**Phase:** P3 — Loop 2 + the gate. BRO-1794 is **In Progress** (slice 1b-ii remains).

## Just shipped — BRO-1794 slice 1b-i (the verify runner + event fold, #64 `d9f02c8`)

`apps/runtime/src/verifier/run.ts` + `run.test.ts` (new) — the **pure verify-orchestration** half of the reap wiring, split from the concurrency-critical supervisor integration. Plus the protocol event fold.

- **`runVerification(deps): { outcome, receipt, signature }`** — composes the four shipped stages: `runVerifier({ stage0, next })` where `next` = `runChecks` → `liftStage1` → `attachJudge`. Then `toVerdictReceipt` → `writeVerdict` (persist **before** any result event) → `appendVerdictFeedback` on a fail → `decideVerdictOutcome` (from durable state) → project the ordered `check.*` stream.
- **Durability**: verdict.md is the source of truth, written before the event projection; an `emit` rejection **propagates** (fail-closed — the receipt is already on disk, so the supervisor maps a rejection to an infra park without data loss). Mutation-proven (reorder-persist-after-emit reds the durability tests).
- **Injected seams**: a single `VerdictIo` for every run-dir write (check logs, judge.json, verdict.md, fix_plan.md), and a `VerifyEmit` the supervisor wraps over `runEmitter`/`sys`. `deps.checks` = `Omit<RunChecksDeps,"runDir"|"writeLog">`; `deps.judge` = `Omit<AttachJudgeDeps,"checks"|"writeJudge">` (summaries derived from the Stage-1 result; writeJudge routed through io).
- **Protocol fold** (`packages/protocol/src/events.ts`): `verify.started`/`judge.result`/`verify.error` → `check.started`/`check.judge`/`check.error` (added to `EVENT_TYPES`). Namespace count stays **six**. Canon spec §7 (`handoff/.../VERIFIER.md`), README, and canon-amendments ledger all repaired in place.

**Validation**: 19 verify-runner + 12 events tests green; protocol suite green; tsc + biome clean. P11-dogfooded against real git + real subprocess checks + real fs. P20 **8/10, 0 blocking** (7 findings all refuted). CodeRabbit caught canon-spec traceability drift → fixed. Noted minor → **BRO-1874** (Low).

## Next — BRO-1794 slice 1b-ii (the supervisor reap wiring)

Wire the shipped `runVerification` into the supervisor. **Concurrency-critical — review under a concurrency lens.** All seams below confirmed by the Explore recon (agent `a75944450926cb6e3`).

**The insertion point:** `apps/runtime/src/supervisor/supervisor.ts:614-624`, the exit-0 STUB (currently parks `review` directly). Replace it with: build the verifier deps → `await runVerification(...)` → route its `VerdictOutcome`.

**Work items:**
1. **Base capture** (the one real gap). `session` has NO base column and `gitWorktreeAdd` doesn't return the sha it branched from. Add an exported `gitHead(cwd)` / `revParse` to `apps/runtime/src/git/git.ts` (a private copy already lives in `child/broomva-child.ts:176`), capture `git rev-parse HEAD` of `cfg.workspace` at **dispatch** (`supervisor.ts:849-854`, before `factory.create`), thread it onto `RunContext` (`supervisor.ts:210-217`). It flows dispatch→launch→reap live and is persisted only in `verdict.md` — **NO db column, NO migration** (capture at create time, not reap — workspace HEAD may move).
2. **Compose the deps.** `stage0`: `{ cwd: workspace root, base, branch: sandbox.branch, protect: effectiveProtect(ctx.contract.done), maxFiles/maxLines from ctx.contract.done.diff }`. `checks`: `{ checks: normalizeChecks(ctx.contract.done.check), spawnContext: sandbox.spawnContext() }`. `judge`: `{ rubricText: <read rubric.md or null>, diff: <git diff base..branch full text>, brief: <_work.md body>, call: proxyJudgeCaller(...) }`.
3. **Verifier bearer.** Mint `role:"verifier"` via `tokens.mint({ session: ctx.runId, runDir: sandbox.runDir, role: "verifier", budget: ctx.budget })` (mirrors the AGENT mint at `supervisor.ts:699`). Idempotent per session — re-minting under `ctx.runId` **revokes the agent token** (safe: the agent is dead by reap). Build `proxyJudgeCaller({ proxyUrl: proxy.url, bearer })`. **Revoke after** the verify. The proxy resolves the pinned judge model from the token's role — so the bearer's role MUST be `"verifier"`.
4. **Emit wrapper + persist.** `emit = (type, payload) => runEmitter(ctx.runId, ctx.sandbox.runDir)(sys(type, payload))`. `runVerification` handles writeVerdict/appendVerdictFeedback + the `check.*` stream (incl. `check.verdict` carrying the `VerdictReceipt`) internally.
5. **Route the `VerdictOutcome`:**
   - `park_review` → `terminal(ctx, entry, { sessionStatus:"review", nodeState:"review", event:"run.finished", crash:false, ... })` (the stub's current behavior; whether to open a gate row is a design call — exit-20's `openQuestionGate` shows the mechanics).
   - `respawn` → `appendVerdictFeedback` already ran inside `runVerification`, so just `return respawn(ctx, respawns + 1)` (`supervisor.ts:743`; the fresh child re-reads fix_plan.md at boot — confirmed).
   - `park_blocked` → `terminal(ctx, entry, { sessionStatus:"blocked", nodeState:"blocked", event:"run.finished" (NOT run.failed — that's crash-only), reason: <VerdictParkReason>, ... })`.
6. **Attempt accounting.** NEW verify-attempt counter (no db col — `run_budget.iterations` counts model-calls). Thread `attempt`, `priorSignature` (the prior `runVerification().signature`), `iterations`, `maxIterations` (contract `budget.max_iterations`), `maxAttempts` = new **flat** config `verifierMaxAttempts` (`config.ts`, `DEFAULT_VERIFIER_MAX_ATTEMPTS=5`, `MAESTRO_VERIFIER_MAX_ATTEMPTS`). Judge model is already env-driven (`MAESTRO_MODEL_VERIFIER`) — NOT new config.
7. **Concurrency**: the new `await`ed verify extends the reap's async window. Every `cancelled`-race guard (`supervisor.ts:237,587,747,775`) must be honored — a kill landing **during** verify must still route to `terminalKilled` (`supervisor.ts:490`). This is the single riskiest edit.

**done.check:** `cd apps/maestro && bun test apps/runtime -t verify` (extend for the reap path) + a **dispatch→reap integration test** (`MAESTRO_MOCK_MODEL=1`, seed a node with a `done` contract carrying checks+rubric, scripted mock judge reply, exit-0 child; assert `ReapResult.sessionStatus`, `verdict.md` at `runs/run-r1/`, a `check.verdict` event row, and `mock.calls` shows a `role:"verifier"` forward). Fail→fix_plan→redispatch bounded by both caps; error parks without burning an attempt. `--filter` is NOT a bun flag.

**Docs:** VERIFIER.md §2 Stage 3 wiring; FLOWS.md §F4; AUTONOMY.md §Loop 2.

## Loop mechanics
Branch FIRST after `git checkout main` (rule-of-THREE slip). P3 chain: **1794(1b-ii)** → 1802 → 1805 → 1809 → 1813 → 1818 → 1821 (P3 exit), then P4/P5. Stop via `ScheduleWakeup({stop:true})` only when ALL P0–P5 non-OUTLOOK Done. Linear project `6c738b50`.
