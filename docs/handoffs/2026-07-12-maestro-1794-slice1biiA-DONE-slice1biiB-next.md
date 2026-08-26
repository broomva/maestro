# Handoff — BRO-1794 slice 1b-ii-A DONE, slice 1b-ii-B next

**Date:** 2026-07-12
**Ticket:** BRO-1794 (verdict assembly + feedback wire) — **still In Progress**; slices 1a + 1b-i + 1b-ii-A merged, **1b-ii-B remains**.
**Main tip:** `c17f10e` (PR #65 squash-merged green via CI-gated automerge).
**Linear project:** `6c738b50-fc1f-4b4c-8704-139c4747c62a`.

## What shipped this slice (1b-ii-A — the DETERMINISTIC reap wiring)

Wired the shipped `runVerification` (slice 1b-i, `apps/runtime/src/verifier/run.ts`) into the
supervisor reap for the **deterministic path** — Stage 0 (tamper/diff guard) + Stage 1
(deterministic checks). The **Stage-2 LLM judge is deferred to slice 1b-ii-B**.

### Files
- **`apps/runtime/src/supervisor/supervisor.ts`** (932 → ~1090): the exit-0 stub at the old
  `:614` is replaced by `verifyAndRoute(ctx, entry, mismatch, respawns, verify)`.
  - `!done` → park `review` (unchanged fallback when there is no done contract to verify).
  - else build `runVerification` deps:
    - `stage0` from `ctx.base` / `sandbox.branch` / `effectiveProtect(done)` / `done.diff` limits.
    - `checks` from `normalizeChecks(done.check)` + `sandbox.spawnContext()`.
    - `judge` = **stub** (`rubricText: null`, `diff: ""`, `brief: ""`, throwing `call`). `attachJudge`
      with `rubricText:null` yields `judge:{score:null}` and never invokes `call` — the judge is a no-op.
  - one `teeEmit` SessionTee over `runEmitter(ctx.runId, sandbox.runDir)`.
  - `attempt = verify.attempt + 1`; `maxIterations`/`maxAttempts` resolved from config.
  - route `result.outcome`:
    - `respawn` → `respawn(ctx, respawns, { attempt, priorSignature: result.signature })`
    - `park_blocked` → `terminal(blocked, reason, "run.finished")`
    - `park_review` → `terminal(review, "run.finished")`
  - **kill-during-verify** re-check: after the awaited `runVerification`, `cancelled.has(ctx.runId)`
    → `terminalKilled` (a kill that arrives during verify still wins on the return path; the
    throw path is caught by the reap outer catch → `containOrKilled`).
  - `RunContext` gains **`base: string`** + **`nodePath: string`** (NO db column — captured at
    dispatch, flow dispatch→launch→reap→respawn live, persisted only inside `verdict.md`).
  - `VerifyState { attempt: number; priorSignature?: string }` threaded through
    `reap`/`reapInner`/`respawn`, **distinct** from the fresh-context `respawns` bound.
  - `readIterations(runId)` reads `run_budget.iterations` (guarded → 0 on any failure).
  - dispatch captures `base = await gitHead(sandbox.workdir)` after `factory.create`;
    initial reap `reap(ctx, entry, watermark, 0, { attempt: 0 })`.
- **`apps/runtime/src/git/git.ts`**: added `gitHead(cwd): Promise<string>` (`rev-parse HEAD`,
  throws `GitError` on failure so the caller contains it as a crash, never diffs against `""`).
- **`apps/runtime/src/config.ts`**: `verifierMaxAttempts?: number` +
  `DEFAULT_VERIFIER_MAX_ATTEMPTS` (= protocol `VERIFIER_MAX_ATTEMPTS` = 5) +
  `MAESTRO_VERIFIER_MAX_ATTEMPTS` env parse.
- **`apps/runtime/src/supervisor/supervisor.test.ts`**: 4 new tests —
  1. exit-0 + passing check → park review + `verdict.md` + `check.verdict` event +
     `payload.base` === real workspace HEAD + `judge:{score:null}`.
  2. failing check + `loadConfig({MAESTRO_VERIFIER_MAX_ATTEMPTS:"1"})` → park_blocked
     `verifier_exhausted` + `fix_plan` (spawner.calls() === 1).
  3. identically-failing check → respawn (spawner.calls() === 2) → `no_progress`.
  4. **real-process dogfood** — a child that writes + commits `feature.ts` → Stage 0
     (diffstat.files=1) + Stage 1 (`test -f feature.ts`) pass → `verdict.md`.

### Verification
- Full runtime suite green; `tsc` + `biome ci .` clean.
- P11 dogfooded through the real supervisor reap with a real `Bun.spawn` child.
- P20 **8/10, 0 blocking / 0 confirmed** (all 4 findings refuted): kill-during-verify safe on
  both return + throw paths; base captured once at dispatch + preserved through respawn;
  verify-attempt and respawns are distinct bounded loops; tests non-vacuous.

### Judgment calls (reusable)
- **Deferring the judge is NOT a fail-open.** Under `gate:human` every clean run parks `review`
  regardless of a judge score; the judge only becomes decision-load-bearing under `gate:auto`
  (BRO-1802, unbuilt). No reachable verdict today depends on it. Verify the *decision graph*,
  not the stage list, before calling a partial wire fail-open.
- **Two bounded retry loops need distinct state.** `VerifyState.attempt` (verify-attempt cap,
  → `verifier_exhausted`) vs `respawns` (fresh-context cap). Collapsing them mis-bounds both.
- **An awaited verify extends the reap async window** → re-check every `cancelled` guard AFTER
  the await on the return path AND rely on the outer catch on the throw path.

### Follow-ups filed
- **BRO-1875** (Low) — deterministic kill-during-verify regression test; needs a `runVerify?`
  injection seam on `SupervisorDeps` (test-only gap; fail-closed already correct → ticket, not re-fire).
- **BRO-1874** (Low, from 1b-i) — cosmetic `check.judge{score:null}` emit condition on the
  judge-error path.

## NEXT — slice 1b-ii-B (wire the Stage-2 LLM judge)

The judge is currently stubbed. 1b-ii-B replaces the stub with the real proxy-backed judge.
In `verifyAndRoute`, when `done.judge` is set:

1. **Mint the verifier bearer:** `tokens.mint({ session: ctx.runId, runDir: sandbox.runDir,
   role: "verifier", budget: ctx.budget })` (mirrors the agent mint; idempotent per session →
   revokes the dead agent token, safe). Build `proxyJudgeCaller({ proxyUrl: proxy.url, bearer })`
   → `deps.judge.call`. **Revoke in a `finally`** after verify (or on the kill/throw path).
2. **Resolve judge inputs:**
   - `rubricText` = read `join(sandbox.workdir, done.judge)`; **ENOENT → `""`** so `parseRubric`
     errors → verdict `error` → park blocked (**fail-CLOSED**, attempt not burned).
   - `diff` = `git(sandbox.workdir, ["diff", ctx.base, sandbox.branch])`.stdout (full text).
   - `brief` = body of `_work.md` at `join(sandbox.workdir, ctx.nodePath === "" ? "_work.md"
     : join(ctx.nodePath, "_work.md"))` (`ctx.nodePath` already threaded this slice).
3. Pass into `runVerification`'s `judge` deps (currently `rubricText:null`/`diff:""`/`brief:""`/throwing `call`).
4. **done.check** = a dispatch→reap integration test with the **MOCK PROXY** (mount via the
   `dispatch.test.ts` harness — NOT `makeSupervisor`, which has no proxy): `MAESTRO_MOCK_MODEL=1`,
   scripted mock judge JSON reply, assert `verdict.md` + a `check.judge` event + `mock.calls`
   shows a forward with `role:"verifier"`.

Judge becomes decision-load-bearing under **gate:auto** → **pairs with BRO-1802**. Concurrency
lens again (a kill during the model call → the bearer's forward 401s). **Only after 1b-ii-B does
BRO-1794 become Done.**

Reference seams (read-only): `verifier/run.ts` (`RunVerificationDeps.judge`), `verifier/judge.ts`
(`attachJudge`/`proxyJudgeCaller`/`ProxyCallerConfig`/`RunJudgeDeps`), `verifier/checks.ts`,
`work.ts` (`Done.judge` = path string, `effectiveProtect`, `normalizeChecks`,
`VERIFIER_MAX_ATTEMPTS`), `tokens.ts` (`mint` idempotent-per-session), `proxy/*` (mock upstream).

## After BRO-1794 (all slices Done)
P3 chain: **1802** (approve = squash-merge, gate:auto) → **1805** (F5 gate slice) → **1809**
(M5 inspector) → **1813** (adversarial suite) → **1818** (autonomy ledger) → **1821** (P3-exit).
Then P4/P5. Stop the autonomous loop only when ALL P0–P5 non-OUTLOOK tickets are Done.
