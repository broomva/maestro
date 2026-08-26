# Maestro handoff — BRO-1794 slice 1a DONE → slice 1b next

**Date:** 2026-07-12
**Branch/tip:** `main` @ `b5df2a5` (BRO-1794 slice 1a squash-merged as PR #63)
**Phase:** P3 — Loop 2 + the gate. BRO-1794 is **In Progress** (slice 1b remains).

## Just shipped — BRO-1794 slice 1a (the verdict module, #63 `b5df2a5`)

`apps/runtime/src/verifier/verdict.ts` + `verdict.test.ts` — additive, no edits to existing code. The pure/injectable half of BRO-1794. Exports (all reused verbatim by slice 1b):

- `toVerdictReceipt(result: VerifierResult, attempt: number): VerdictReceipt` — the `verdict.md` frontmatter AND the `check.verdict` payload. **`EVENT_TYPES.CHECK_VERDICT` and `VerdictReceipt` already exist in protocol** (`events.ts:79`, `work.ts:191`) — do not re-add them.
- `renderVerdictBody(result)` + `renderVerdictMd(receipt, body)` — plain-voice body + `---`-fenced frontmatter (VERIFIER §4 order).
- `renderVerdictFeedback(result, attempt, timestamp)` + `appendVerdictFeedback(runDir, block, io?)` — VERIFIER §5 append-only `## Verifier — attempt N failed (ISO)` block; `tickFixPlan`/`parseFixPlan`-compatible; uses `fixPlanPath` from `harness/stop-conditions`.
- `decideVerdictOutcome(input): VerdictOutcome` — pass → `park_review`; fail → `respawn` OR `park_blocked` (`no_progress` > `verifier_exhausted` > `iteration_cap`); error → `park_blocked` reason `verify_error`, `burnsAttempt: false`. Every fail burns an attempt (tamper included). Pure over `{verdict, attempt, maxAttempts, iterations, maxIterations, signature, priorSignature}`.
- `verdictSignature(result)` — order-independent no-progress key (folds diffstat for `diff_too_large`).
- `writeVerdict(runDir, receipt, body, io?)`, `verdictPath(runDir)`, `VerdictIo` (injectable fs; `nodeVerdictIo` default — `read` returns `null` for not-found, rejects on a real fault).

**Validation:** 61 verdict + 517 runtime pass; tsc + biome clean. P11-dogfooded. P20 R1 9/10 clean; CodeRabbit then caught 2 real majors (history-clobber on a transient read fault; no_progress misfire from a diffstat-blind signature) — both fixed mutation-proven, threads resolved, P20 R2 re-fire 9/10 clean.

## Next — BRO-1794 slice 1b (the reap wiring)

Wire the module into the supervisor's reap. This is concurrency-critical — review under a concurrency lens.

**The insertion point:** `apps/runtime/src/supervisor/supervisor.ts:614-624`, the exit-0 STUB (currently parks `review` directly). Replace it with: run the verifier → assemble + persist the receipt → emit `check.verdict` → route on `decideVerdictOutcome`.

**Work items:**
1. **Derive `base`** (the real gap). The supervisor does NOT capture the commit the run branched from today (`session` row has only `branch`). Options: record base at dispatch (the worktree branches off `cfg.workspace` HEAD in `gitWorktreeAdd`, `sandbox/worktree.ts:190`) into a session column, OR compute a merge-base at reap. Prefer recording at dispatch (deterministic, no reap-time git).
2. **Compose `runVerifier`'s `next`** = `runChecks` + `liftStage1` (`verifier/checks.ts`) then `attachJudge` (`verifier/judge.ts`). Thread `Stage0Input` (cwd = workspace root, base, branch = `sandbox.branch`, `effectiveProtect(ctx.contract.done)`, diff limits from `ctx.contract.done.diff`). The judge stage needs the rubric (`ctx.contract.done.judge`), a `proxyJudgeCaller` bound to the run's proxy + a verifier-role bearer, and a `writeJudge` seam pointed at the run dir.
3. **Assemble + persist:** `toVerdictReceipt(result, attempt)` → `writeVerdict(runDir, receipt, body)` → emit `check.verdict` with the receipt as payload via the `runEmitter`/`sys` path.
4. **Emit the live verify events** `verify.started` / `check.result` / `judge.result` / `verify.error` — and **reconcile the namespaces**: `verify.*` and `judge.*` are NOT among the 6 `EVENT_NAMESPACES` (`events.ts:19,53-58` flags this as 1794-owned). Either fold them into `check.*` (`check.started`/`check.judge`/`check.error`) or widen `EVENT_NAMESPACES` + `NamespacedEventType`. Pick one, update `EVENT_TYPES`.
5. **Route on the decision:** `park_review` → the existing `terminal(...)` review path; `respawn` → `appendVerdictFeedback` then the existing `respawn()` machinery (`supervisor.ts:743`); `park_blocked` → `terminal(...)` blocked carrying the reason.
6. **Attempt accounting:** add a verification-`attempt` counter (NEW — no db column; `run_budget.iterations` counts model-calls, not verify-attempts) + `verifier.max_attempts` config (`config.ts`, default 5). Thread the prior verdict signature for `no_progress`. Stage-0 tamper burns an attempt; `verify_error` does not.

**done.check:** `cd apps/maestro && bun test apps/runtime -t verdict` (extend for the reap path; add a supervisor/reap integration test — fail→fix_plan→redispatch bounded by both caps, error parks without burn). `--filter` is NOT a bun flag.

**Docs:** VERIFIER.md §2 Stage 3 + §4/§5 + §7 (events); FLOWS.md §F4; AUTONOMY.md §Loop 2.

## Loop mechanics
Branch FIRST after `git checkout main` (rule-of-THREE slip). P3 chain: **1794(1b)** → 1802 → 1805 → 1809 → 1813 → 1818 → 1821 (P3 exit), then P4/P5. Stop via `ScheduleWakeup({stop:true})` only when ALL P0–P5 non-OUTLOOK Done. Linear project `6c738b50`.
