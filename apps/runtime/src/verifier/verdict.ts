// Verifier Stage 3 — verdict assembly + the feedback wire (VERIFIER §4, §5; FLOWS §F4). This module is
// the PURE, side-effect-injectable half of BRO-1794: given a finished {@link VerifierResult} (Stage 0
// diff/tamper + Stage 1 checks + Stage 2 judge) it produces the run's receipt and decides what the
// supervisor does next. It owns four things and NONE of the supervisor's reap plumbing (that wiring —
// base derivation, the runVerifier `next` composition, the exit-0 stub rewrite, live verify.* events —
// is the reap-integration slice, BRO-1794 slice 1b):
//
//   1. verdict.md — the `---`-fenced receipt the inspector renders (rung 3) and the gate reads. The
//      frontmatter IS the wire-shaped {@link VerdictReceipt}; the body is the plain-voice summary.
//   2. check.verdict payload — the VerdictReceipt verbatim (D-EVENTNAMES; work.ts pins the shape).
//   3. fix_plan.md feedback — on fail, APPEND a `## Verifier — attempt N failed` section (never rewrite;
//      the append-only attempt history is Loop-4 signal, and tickFixPlan preserves it — VERIFIER §5).
//   4. the attempt-accounting decision — pass → the gate (F5); fail → respawn OR park blocked when a cap
//      is hit; error → park blocked WITHOUT burning an attempt (a broken harness is never the agent's
//      fault). This is the Loop-2 counterpart of the child's three stop conditions.
//
// Everything here is a pure function of its inputs or takes an injectable IO seam, so the whole surface
// is deterministic under `bun test apps/runtime -t verdict` with no supervisor and no real clock.

import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import type { Verdict, VerdictReceipt } from "@maestro/protocol";
import { stringify as stringifyYaml } from "yaml";
import { fixPlanPath } from "../harness/stop-conditions";
import type { VerifierResult } from "./verifier";

/** The canonical run-dir path of the receipt (beside progress.md / fix_plan.md / checks/ / judge.json). */
export function verdictPath(runDir: string): string {
  return join(runDir, "verdict.md");
}

// ── VerifierResult → VerdictReceipt ─────────────────────────────────────────────────────────────────

/**
 * Project a finished {@link VerifierResult} into the wire-shaped {@link VerdictReceipt} (VERIFIER §4).
 * The receipt drops the runtime-only fields the {@link VerifierResult} carries (`CheckResult.required`/
 * `reason`, `message`) — those are internal signal, not part of the frontmatter/event contract. `attempt`
 * is supplied by the caller (the supervisor owns the counter; this module never invents it).
 */
export function toVerdictReceipt(result: VerifierResult, attempt: number): VerdictReceipt {
  const judge = result.judge;
  return {
    verdict: result.verdict,
    attempt,
    base: result.base,
    diffstat: result.diffstat,
    tampering: result.tampering,
    checks: (result.checks ?? []).map((c) => ({
      name: c.name,
      ok: c.ok,
      exit: c.exit,
      duration_s: c.duration_s,
      log: c.log,
    })),
    judge:
      judge === undefined
        ? { score: null }
        : {
            score: judge.score,
            ...(judge.model !== undefined ? { model: judge.model } : {}),
            ...(judge.detail !== undefined ? { detail: judge.detail } : {}),
          },
  };
}

// ── verdict.md rendering ────────────────────────────────────────────────────────────────────────────

/**
 * The plain-voice body of verdict.md (VERIFIER §4) — the summary the inspector shows and the gate reads.
 * Voice rules (CLAUDE.md): lead with the verb, sentence case, NO em dashes, no emoji, no percentages.
 * Deterministic (no ambient clock, no randomness) so the receipt round-trips in a test.
 */
export function renderVerdictBody(result: VerifierResult): string {
  if (result.verdict === "error") {
    const why = (result.message ?? "").trim();
    return why.length > 0
      ? `Verification could not run: ${why}. Parked for you; no attempt was spent.`
      : "Verification could not run because of an infra error. Parked for you; no attempt was spent.";
  }

  if (result.reason === "tampering") {
    const paths = result.tampering.join(", ");
    return `Failed the tamper guard. The run changed protected paths: ${paths}. Protected files (tests, the rubric, the contract) may not be edited by the run.`;
  }
  if (result.reason === "diff_too_large") {
    const { files, plus, minus } = result.diffstat;
    return `Failed the diff guard. The change is too large (${files} files, +${plus}/-${minus}). Scope it down rather than retrying.`;
  }

  const checks = result.checks ?? [];
  const passed = checks.filter((c) => c.ok).length;
  const failed = checks.filter((c) => !c.ok);
  const head = `${passed} of ${checks.length} checks passed.`;
  const lines: string[] = [head];

  for (const c of failed) {
    const where = c.reason === "timeout" ? "timed out" : `failed (exit ${c.exit})`;
    const advisory = c.required ? "" : " (advisory)";
    lines.push(`\`${c.name}\` ${where}${advisory}. See ${c.log}.`);
  }

  const judge = result.judge;
  if (judge !== undefined && judge.score !== null) {
    const detail = judge.detail ? ` See ${judge.detail}.` : "";
    lines.push(`Judge scored ${judge.score}.${detail}`);
  }

  if (result.verdict === "pass") {
    lines.push("Ready for you to review.");
  }
  return lines.join(" ");
}

/**
 * Render the full verdict.md — `---`-fenced frontmatter (the {@link VerdictReceipt}, VERIFIER §4 field
 * order) + the plain-voice body. Reuses the workspace's canonical frontmatter idiom (`stringifyYaml`
 * with `lineWidth: 0`, trailing newlines trimmed, wrapped in `---` fences — matches serializeWorkContract).
 */
export function renderVerdictMd(receipt: VerdictReceipt, body: string): string {
  // Emit in VERIFIER §4 order (stable receipts) rather than object-key order.
  const fm: Record<string, unknown> = {
    verdict: receipt.verdict,
    attempt: receipt.attempt,
    base: receipt.base,
    diffstat: receipt.diffstat,
    tampering: receipt.tampering,
    checks: receipt.checks,
    judge: receipt.judge,
  };
  const yaml = stringifyYaml(fm, { lineWidth: 0 }).replace(/\n+$/, "");
  const trimmed = body.trim();
  return trimmed.length > 0 ? `---\n${yaml}\n---\n\n${trimmed}\n` : `---\n${yaml}\n---\n`;
}

// ── fix_plan.md feedback (VERIFIER §5) ──────────────────────────────────────────────────────────────

/**
 * Render the feedback block appended to fix_plan.md on a fail (VERIFIER §5): a `## Verifier — attempt N
 * failed (ISO)` heading + one `- [ ]` checkbox per failing check / gating judge, each naming its evidence
 * file. `timestamp` is passed in (no ambient clock, like progress.md's `updated`). Targeted, never
 * prescriptive — what failed and where, not how to fix it (that is the respawned agent's job). Returns
 * only the block (the caller appends it); an `error` verdict yields no block (errors are not feedback).
 */
export function renderVerdictFeedback(
  result: VerifierResult,
  attempt: number,
  timestamp: string,
): string {
  if (result.verdict !== "fail") return "";

  const items: string[] = [];
  if (result.reason === "tampering") {
    for (const p of result.tampering) {
      items.push(`- [ ] tamper: ${p} is protected and may not be changed by the run`);
    }
  } else if (result.reason === "diff_too_large") {
    const { files, plus, minus } = result.diffstat;
    items.push(
      `- [ ] scope: the diff is too large (${files} files, +${plus}/-${minus}) — split the work, do not retry as-is`,
    );
  } else {
    const checks = result.checks ?? [];
    for (const c of checks) {
      if (c.ok) continue;
      const where = c.reason === "timeout" ? "timed out" : `failed (exit ${c.exit})`;
      items.push(`- [ ] ${c.name}: ${where} (see ${c.log})`);
    }
    // The judge gates only when no REQUIRED check already failed (a required-check fail is the verdict).
    const requiredFailed = checks.some((c) => c.required && !c.ok);
    const judge = result.judge;
    if (!requiredFailed && judge !== undefined && judge.score !== null) {
      const evidence = judge.detail ? ` (see ${judge.detail})` : "";
      items.push(`- [ ] judge: scored ${judge.score} below the rubric threshold${evidence}`);
    }
  }

  // A fail with no enumerable item still records the attempt (so the append-only history stays complete).
  if (items.length === 0) {
    items.push("- [ ] verification failed (see verdict.md)");
  }

  return `## Verifier — attempt ${attempt} failed (${timestamp})\n${items.join("\n")}\n`;
}

// ── IO seam (injectable; node fs by default) ────────────────────────────────────────────────────────

/** The filesystem operations the persist helpers need — injected as fakes in tests, node fs in the runtime. */
export interface VerdictIo {
  read(path: string): Promise<string>;
  write(path: string, data: string): Promise<void>;
  mkdirp(path: string): Promise<void>;
}

/** The default IO seam — node:fs/promises. */
export const nodeVerdictIo: VerdictIo = {
  read: (path) => readFile(path, "utf8"),
  write: (path, data) => writeFile(path, data, "utf8"),
  mkdirp: async (path) => {
    await mkdir(path, { recursive: true });
  },
};

/**
 * Write verdict.md into the run dir (creating it if needed). Returns the receipt so the caller can emit
 * it verbatim as the `check.verdict` payload without re-deriving it.
 */
export async function writeVerdict(
  runDir: string,
  receipt: VerdictReceipt,
  body: string,
  io: VerdictIo = nodeVerdictIo,
): Promise<VerdictReceipt> {
  const path = verdictPath(runDir);
  await io.mkdirp(dirname(path));
  await io.write(path, renderVerdictMd(receipt, body));
  return receipt;
}

/**
 * APPEND the verifier feedback block to fix_plan.md (VERIFIER §5) — never a rewrite. A missing/empty file
 * is seeded with a `# Fix plan` header first so the appended section reads as history under it; an
 * existing file is preserved verbatim and the block is appended after exactly one blank-line separator.
 * A no-op block (a non-fail verdict) leaves the file untouched. Returns true iff a block was appended.
 */
export async function appendVerdictFeedback(
  runDir: string,
  block: string,
  io: VerdictIo = nodeVerdictIo,
): Promise<boolean> {
  if (block.trim().length === 0) return false;
  const path = fixPlanPath(runDir);
  await io.mkdirp(dirname(path));

  let existing = "";
  try {
    existing = await io.read(path);
  } catch {
    existing = ""; // no fix_plan yet — seed one below
  }

  // Trailing newlines are stripped from BOTH the seed header and an existing file so the join adds
  // exactly one blank-line separator (a seed `"# Fix plan\n"` would otherwise yield two blank lines).
  const base = existing.trim().length === 0 ? "# Fix plan" : existing.replace(/\n+$/, "");
  await io.write(path, `${base}\n\n${block.replace(/\n+$/, "")}\n`);
  return true;
}

// ── the attempt-accounting decision (VERIFIER §5, AUTONOMY §4) ───────────────────────────────────────

/** Why a run parked `blocked` off the verifier loop (a supervisor/verifier-derived reason, distinct from
 *  the child's `ExitReason`). `verify_error` = infra (no attempt burned); the other three are cap halts. */
export type VerdictParkReason =
  | "verify_error"
  | "verifier_exhausted"
  | "iteration_cap"
  | "no_progress";

/** What the supervisor does after a verification attempt (VERIFIER §2 Stage 3 / §5, FLOWS §F4). */
export type VerdictOutcome =
  | { action: "park_review"; burnsAttempt: true }
  | { action: "respawn"; burnsAttempt: true }
  | { action: "park_blocked"; reason: VerdictParkReason; burnsAttempt: boolean };

/** Everything {@link decideVerdictOutcome} reasons over. All numbers are supplied by the supervisor; this
 *  function invents none of them, so it stays pure and fully testable. */
export interface VerdictDecisionInput {
  /** The verification verdict (Stage 0 short-circuit, or the full Stage 1+2 result). */
  verdict: Verdict;
  /** This verification attempt's 1-based number on the run (1st verify = 1). */
  attempt: number;
  /** `verifier.max_attempts` — consecutive-fail cap before `verifier_exhausted` (config default 5, >= 1). */
  maxAttempts: number;
  /** Iterations (agent model-call beats) consumed by the run so far (`run_budget.iterations`). */
  iterations: number;
  /** The effective iteration cap (contract `budget.max_iterations` ?? runtime default). */
  maxIterations: number;
  /** A stable signature of THIS verdict (see {@link verdictSignature}). */
  signature: string;
  /** The immediately-preceding verification's signature, if any (undefined on the first attempt). */
  priorSignature?: string;
}

/**
 * Decide what happens after a verification attempt (VERIFIER §5, FLOWS §F4). The three fail-side halts
 * are evaluated in a fixed, documented order so the reported reason is deterministic when several
 * coincide (they all park `blocked`, so the order only chooses which reason the human sees):
 *
 *   1. `no_progress` — this verdict is IDENTICAL to the previous one ("the agent agreeing with itself",
 *      AUTONOMY §4). Another respawn cannot help, so this is the sharpest, most actionable reason and
 *      wins first.
 *   2. `verifier_exhausted` — `attempt >= maxAttempts` consecutive fails.
 *   3. `iteration_cap` — `iterations >= maxIterations` (verification attempts count against the run's
 *      iteration budget, VERIFIER §5). A global-budget backstop, reported last.
 *
 * A `pass` goes to the gate (F5 park review; `gate: auto` auto-merge is BRO-1802). An `error` parks
 * `blocked` and does NOT burn an attempt — a broken harness is never charged to the agent (VERIFIER §2).
 */
export function decideVerdictOutcome(input: VerdictDecisionInput): VerdictOutcome {
  if (input.verdict === "error") {
    return { action: "park_blocked", reason: "verify_error", burnsAttempt: false };
  }
  if (input.verdict === "pass") {
    return { action: "park_review", burnsAttempt: true };
  }

  // verdict === "fail" — a fail always burns an attempt (Stage-0 tamper included, VERIFIER §5).
  if (input.priorSignature !== undefined && input.priorSignature === input.signature) {
    return { action: "park_blocked", reason: "no_progress", burnsAttempt: true };
  }
  if (input.attempt >= input.maxAttempts) {
    return { action: "park_blocked", reason: "verifier_exhausted", burnsAttempt: true };
  }
  if (input.iterations >= input.maxIterations) {
    return { action: "park_blocked", reason: "iteration_cap", burnsAttempt: true };
  }
  return { action: "respawn", burnsAttempt: true };
}

/**
 * A stable signature of a verdict for the no-progress check ("identical verdict twice in a row",
 * VERIFIER §5). Two verifications with the SAME failing checks/criteria, the same tamper set, and the
 * same judge score produce the same signature — that is what "identical verdict" means here. Order-
 * independent (sets are sorted) so a reordered-but-equivalent result still matches. A `pass`/`error`
 * signature is never compared (the loop only continues on fail), but is still well-defined.
 */
export function verdictSignature(result: VerifierResult): string {
  const failingChecks = (result.checks ?? [])
    .filter((c) => !c.ok)
    .map((c) => `${c.name}:${c.reason ?? "fail"}`)
    .sort();
  return JSON.stringify({
    v: result.verdict,
    r: result.reason ?? null,
    t: [...result.tampering].sort(),
    c: failingChecks,
    j: result.judge?.score ?? null,
  });
}
