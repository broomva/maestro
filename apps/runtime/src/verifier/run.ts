// Verifier orchestration — one verification attempt, end to end (VERIFIER §2; FLOWS §F4). This is the
// composition that the supervisor reap runs after a run claims complete (the exit-0 path). It threads the
// four already-built, already-tested verifier stages into a single call and returns the ONE thing the
// supervisor acts on — a {@link VerdictOutcome} — plus the durable receipt and its no-progress signature.
//
// It owns the ORDER and the durability contract; it owns NONE of the supervisor's reap plumbing (base
// derivation, the verifier-bearer mint, the exit-0 stub rewrite, routing the outcome through
// terminal/respawn — that is the concurrency-critical integration slice, BRO-1794 slice 1b-ii). Every
// side effect is an injected seam, so the whole surface is deterministic under `bun test apps/runtime`
// with no supervisor, no real clock, and a mock model:
//
//   Stage 0 (runVerifier) ─pass→ Stage 1 (runChecks → liftStage1) → Stage 2 (attachJudge)
//        │ fail/error (TERMINAL — no later stage runs; a tamper verdict can never be flipped to pass)
//        ▼
//   assemble the receipt → PERSIST verdict.md (+ append fix_plan feedback on a fail) → decide the outcome
//        → project the attempt as check.* events (started → result(s) → judge → verdict, or check.error).
//
// DURABILITY (D-DURABILITY): verdict.md is the source of truth and is written BEFORE any result-bearing
// event; the {@link VerdictOutcome} is computed from that durable state. The check.* events are its
// PROJECTION for the live stream. An emit that rejects PROPAGATES (this function rejects) rather than
// being swallowed — the receipt is already durable on disk, so the supervisor can safely treat a rejection
// as an infra failure (park blocked, no attempt burned) without losing the verdict. Not silent, not lossy.

import { dirname, join } from "node:path";
import { EVENT_TYPES, type EventType, type VerdictReceipt } from "@maestro/protocol";
import { liftStage1, type RunChecksDeps, runChecks } from "./checks";
import { type AttachJudgeDeps, attachJudge, type JudgeCheckSummary } from "./judge";
import type { Stage0Input, Stage0Verdict } from "./stage0";
import {
  appendVerdictFeedback,
  decideVerdictOutcome,
  nodeVerdictIo,
  renderVerdictBody,
  renderVerdictFeedback,
  toVerdictReceipt,
  type VerdictIo,
  type VerdictOutcome,
  verdictSignature,
  writeVerdict,
} from "./verdict";
import { runVerifier, type VerifierResult } from "./verifier";

/**
 * Emit one verification event onto the run's stream. The supervisor injects a wrapper over its
 * `runEmitter`/`sys` (SessionTee) path so the event lands byte-identically to a child-emitted one; tests
 * inject a recorder. The payload is the wire-shaped body for the given `check.*` type (see EVENT_TYPES).
 */
export type VerifyEmit = (type: EventType, payload: Record<string, unknown>) => Promise<void>;

/** Everything one verification attempt needs. Stage inputs are the already-resolved pieces the supervisor
 *  derives from the run context (base/branch, the contract's checks, the rubric); every effect is a seam. */
export interface RunVerificationDeps {
  /** Stage 0 input — cwd, base, branch, protect globs, size limits (+ injectable git/match in tests). */
  stage0: Stage0Input;
  /** Stage 1 check specs + how to enter the sandbox (+ injectable runner/clock/shell in tests). `runDir`
   *  and `writeLog` are supplied by this function — every run-dir write goes through {@link RunVerificationDeps.io}. */
  checks: Omit<RunChecksDeps, "runDir" | "writeLog">;
  /** Stage 2 judge — the rubric (or null to skip), diff, brief, model caller (+ env/maxTokens in tests).
   *  `checks` (the summaries) is DERIVED from the Stage-1 result; `writeJudge` goes through `io`. */
  judge: Omit<AttachJudgeDeps, "checks" | "writeJudge">;
  /** The run receipts dir — verdict.md, fix_plan.md, checks/, judge.json all live under it. */
  runDir: string;
  /** This verification attempt's 1-based number on the run (1st verify = 1). */
  attempt: number;
  /** `verifier.max_attempts` — the consecutive-fail cap before `verifier_exhausted` (>= 1). */
  maxAttempts: number;
  /** Agent iterations (model-call beats) the run has consumed (`run_budget.iterations`). */
  iterations: number;
  /** The effective iteration cap (contract `budget.max_iterations` ?? runtime default). */
  maxIterations: number;
  /** The immediately-preceding attempt's verdict signature, for the no-progress check (undefined first). */
  priorSignature?: string;
  /** Emit a verification event (a `check.*` type). Rejections propagate — see the module header. */
  emit: VerifyEmit;
  /** ISO-8601 timestamp for the fix_plan feedback block (injected; no ambient clock). */
  now: () => string;
  /** The single run-dir fs seam (default {@link nodeVerdictIo}); a fake in tests records every write. */
  io?: VerdictIo;
}

/** What one verification attempt produced: the supervisor's next move, the durable receipt, and the
 *  no-progress signature (the supervisor threads it into the NEXT attempt's `priorSignature`). */
export interface RunVerificationResult {
  /** What the supervisor does next (park review / respawn / park blocked). */
  outcome: VerdictOutcome;
  /** The receipt written to verdict.md AND emitted verbatim as the `check.verdict` payload. */
  receipt: VerdictReceipt;
  /** This verdict's stable signature — the supervisor carries it as the next attempt's `priorSignature`. */
  signature: string;
}

/** All run-dir writes (check evidence logs, judge.json) route through the one {@link VerdictIo} seam, so a
 *  single injected fake captures the whole filesystem side of a verification in tests. */
function runDirWriter(
  runDir: string,
  io: VerdictIo,
): (relPath: string, content: string) => Promise<void> {
  return async (relPath, content) => {
    const abs = join(runDir, relPath);
    await io.mkdirp(dirname(abs));
    await io.write(abs, content);
  };
}

/**
 * Run one verification attempt end to end (VERIFIER §2, FLOWS §F4). See the module header for the stage
 * order and the durability contract. Returns the supervisor's next move + the durable receipt + the
 * no-progress signature. Rejects only if the pipeline or an `emit` rejects — the supervisor maps a
 * rejection to an infra park (blocked, no attempt burned); the durable verdict.md is written first.
 */
export async function runVerification(deps: RunVerificationDeps): Promise<RunVerificationResult> {
  const io = deps.io ?? nodeVerdictIo;
  const { attempt, emit } = deps;
  const writeToRunDir = runDirWriter(deps.runDir, io);

  // 1. Signal the attempt has begun, before any potentially-slow check or model work.
  await emit(EVENT_TYPES.CHECK_STARTED, { attempt });

  // 2. Run the pipeline. Stage 0 first; its `next` (Stage 1 checks → Stage 2 judge) runs ONLY on a Stage 0
  //    pass — the short-circuit that makes a tamper/oversize verdict un-overridable (verifier.ts).
  const next = async (
    s0pass: Extract<Stage0Verdict, { verdict: "pass" }>,
  ): Promise<VerifierResult> => {
    const stage1 = await runChecks({
      ...deps.checks,
      runDir: deps.runDir,
      writeLog: writeToRunDir,
    });
    const lifted = liftStage1(stage1, s0pass.diffstat, deps.stage0.base);
    // The judge sees the check OUTCOMES (names + pass/fail), never the transcript (VERIFIER §2 Stage 2).
    const summaries: JudgeCheckSummary[] = (lifted.checks ?? []).map((c) => ({
      name: c.name,
      ok: c.ok,
      ...(c.reason !== undefined ? { reason: c.reason } : {}),
    }));
    return attachJudge(lifted, { ...deps.judge, checks: summaries, writeJudge: writeToRunDir });
  };
  const result = await runVerifier({ stage0: deps.stage0, next });

  // 3. PERSIST the durable receipt FIRST (D-DURABILITY: verdict.md is the source of truth). On a fail,
  //    append the append-only fix_plan feedback so a respawned attempt sees exactly what failed (VERIFIER
  //    §5); a non-fail verdict renders an empty block and appendVerdictFeedback no-ops.
  const receipt = toVerdictReceipt(result, attempt);
  await writeVerdict(deps.runDir, receipt, renderVerdictBody(result), io);
  await appendVerdictFeedback(deps.runDir, renderVerdictFeedback(result, attempt, deps.now()), io);

  // 4. Decide the supervisor's next move from the DURABLE result (independent of the projection below).
  const signature = verdictSignature(result);
  const outcome = decideVerdictOutcome({
    verdict: result.verdict,
    attempt,
    maxAttempts: deps.maxAttempts,
    iterations: deps.iterations,
    maxIterations: deps.maxIterations,
    signature,
    priorSignature: deps.priorSignature,
  });

  // 5. Project the attempt as the ordered check.* stream. `checks` is absent on a Stage-0 short-circuit
  //    fail (tampering/diff_too_large) — no deterministic check ran, so no check.result is emitted.
  for (const c of result.checks ?? []) {
    await emit(EVENT_TYPES.CHECK_RESULT, {
      name: c.name,
      ok: c.ok,
      exit: c.exit,
      duration_s: c.duration_s,
      log: c.log,
    });
  }
  // The judge event fires only when the judge actually ran (a model is pinned) — never on a no-rubric
  // pass (judge: { score: null } with no model) where there is nothing to report.
  const judge = result.judge;
  if (judge !== undefined && judge.model !== undefined) {
    await emit(EVENT_TYPES.CHECK_JUDGE, {
      score: judge.score,
      model: judge.model,
      ...(judge.detail !== undefined ? { detail: judge.detail } : {}),
    });
  }
  // An infra error's `message` is runtime-only (the receipt projects it away) — surface it live so the
  // stream carries the reason the run parked blocked.
  if (result.verdict === "error") {
    await emit(EVENT_TYPES.CHECK_ERROR, {
      message: result.message ?? "verification could not run",
    });
  }
  await emit(EVENT_TYPES.CHECK_VERDICT, { ...receipt });

  return { outcome, receipt, signature };
}
