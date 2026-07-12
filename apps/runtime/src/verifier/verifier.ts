// The verifier pipeline (VERIFIER §2) — Loop 2's composition seam. Spawned by the supervisor after a
// run's terminal `run.finished` (the reap exit-0 path; the events + state transitions that wire this in
// are BRO-1794's scope). This module owns the STAGE ORDER and the one invariant that makes the guard
// trustworthy:
//
//   A Stage 0 non-pass verdict is TERMINAL — no later stage runs, so nothing can override a `tampering`
//   or `diff_too_large` fail back to pass. Gaming the checks *is* the failure (VERIFIER §6).
//
// Today only Stage 0 is wired. Stage 1 (deterministic checks, BRO-1778) and Stage 2 (LLM judge,
// BRO-1786) attach through the `next` continuation: it runs ONLY when Stage 0 passed. Stage 2's model
// call is what needs the separate `--role verifier` child process (writer≠judge as a process boundary);
// Stage 0 is pure git + glob and runs runtime-side after the writer/child is already dead.

import type { Verdict } from "@maestro/protocol";
import { type DiffStat, runStage0, type Stage0Input, type Stage0Verdict } from "./stage0";

/** The verifier's result so far. Stage 0 always populates `diffstat` + `base`; `reason` is set on a
 *  Stage 0 fail; `message` on an infra error. Later stages (checks/judge) extend this shape. */
export interface VerifierResult {
  verdict: Verdict;
  /** The Stage 0 fail reason, if the run failed the guard. */
  reason?: "tampering" | "diff_too_large";
  /** Protected paths the run touched (empty unless `reason === "tampering"`). */
  tampering: string[];
  /** The net diff size (VERIFIER §4 `diffstat`). */
  diffstat: DiffStat;
  /** The commit the diff was judged against (VERIFIER §4 `base`). */
  base: string;
  /** Set only on `verdict === "error"` — the infra failure that stopped the guard. */
  message?: string;
}

export interface VerifierDeps {
  /** Stage 0's input (repo, base, branch, protect globs, limits). */
  stage0: Stage0Input;
  /**
   * The post-Stage-0 continuation (Stage 1 checks → Stage 2 judge). Called ONLY when Stage 0 PASSED —
   * a fail or error short-circuits before it, which is exactly why a tamper verdict can never be
   * overridden. BRO-1778 supplies the real continuation; today it is absent, so a clean run passes.
   */
  next?: (stage0: Extract<Stage0Verdict, { verdict: "pass" }>) => Promise<VerifierResult>;
}

/** Lift a Stage 0 verdict into a {@link VerifierResult}. A non-pass Stage 0 IS the verifier's verdict. */
function fromStage0(s0: Stage0Verdict, base: string): VerifierResult {
  if (s0.verdict === "error") {
    return {
      verdict: "error",
      tampering: [],
      diffstat: { files: 0, plus: 0, minus: 0 },
      base,
      message: s0.message,
    };
  }
  if (s0.verdict === "fail") {
    return {
      verdict: "fail",
      reason: s0.reason,
      tampering: s0.reason === "tampering" ? s0.tampering : [],
      diffstat: s0.diffstat,
      base,
    };
  }
  return { verdict: "pass", tampering: [], diffstat: s0.diffstat, base };
}

/**
 * Run one verification attempt. Stage 0 first; if it does not PASS, that verdict is returned verbatim and
 * `next` is NEVER called (the short-circuit that makes a tamper verdict un-overridable). On a Stage 0
 * pass, control hands to `next` when present, else the run passes Stage 0 alone.
 */
export async function runVerifier(deps: VerifierDeps): Promise<VerifierResult> {
  const s0 = await runStage0(deps.stage0);
  if (s0.verdict !== "pass") {
    return fromStage0(s0, deps.stage0.base);
  }
  if (deps.next) {
    return deps.next(s0);
  }
  return fromStage0(s0, deps.stage0.base);
}
