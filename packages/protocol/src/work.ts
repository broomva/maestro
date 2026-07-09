// Work contract types — the orchestration contract (DATA-MODEL §A.2) and the
// full `done:` schema (VERIFIER §1).
//
// This is the single most important data structure in Maestro: YAML frontmatter
// on every work folder's `_work.md`, readable by agent and human alike.

import type { Kind, Trigger } from "./intents";
import type { GateMode, OrchState } from "./state";

/** The budget guardrail contract (DATA-MODEL §A.2) — enforced in the request path. */
export interface Budget {
  per_run_usd?: number;
  per_day_usd?: number;
  max_iterations?: number;
}

/** The three independent stop conditions (DATA-MODEL §A.2, AUTONOMY). */
export type StopCondition = "cap" | "no_progress" | "budget";

export const STOP_CONDITIONS = [
  "cap",
  "no_progress",
  "budget",
] as const satisfies readonly StopCondition[];

/**
 * A named deterministic check (VERIFIER §1). A bare string on `Done.check` is
 * sugar for one check named "check".
 */
export interface DoneCheck {
  name: string;
  run: string;
  /** Default DEFAULT_CHECK_TIMEOUT_S; hard cap MAX_CHECK_TIMEOUT_S. */
  timeout_s?: number;
  /** Advisory checks (required: false) report in the verdict, never gate. */
  required?: boolean;
}

/** The verifiable success function — the full VERIFIER §1 schema. */
export interface Done {
  /** Deterministic oracle — ordered, all required must pass. String = one check named "check". */
  check: string | DoneCheck[];
  /** Optional LLM judge rubric path — a supplement, never the sole gate for gate:auto. */
  judge?: string;
  /** Paths the RUN may not modify — the anti-reward-hacking guard. Authors extend, never shrink. */
  protect?: string[];
  /** Diff-size limits — exceed → verdict fail (reason "diff too large"). */
  diff?: { max_files?: number; max_lines?: number };
  stop_on?: StopCondition[];
}

/** The orchestration contract — `_work.md` frontmatter (DATA-MODEL §A.2). */
export interface WorkContract {
  /** Stable UUID — survives renames/moves. */
  id: string;
  kind: Kind;
  state: OrchState;
  /** human (@handle) or agent (agent:name) responsible. */
  owner?: string;
  gate: GateMode;
  budget?: Budget;
  done?: Done;
  /** Only for kind: routine / event-driven work (Loop 3). */
  trigger?: Trigger;
  /** ISO date. */
  created: string;
  /** ISO date. */
  updated: string;
}

// ── VERIFIER §1 defaults ─────────────────────────────────────────────────────

export const DEFAULT_CHECK_TIMEOUT_S = 600;
export const MAX_CHECK_TIMEOUT_S = 1800;
export const DEFAULT_DIFF_MAX_FILES = 30;
export const DEFAULT_DIFF_MAX_LINES = 2000;
export const DEFAULT_STOP_ON: readonly StopCondition[] = ["cap", "no_progress", "budget"];

/**
 * The protect floor — the agent never edits its own success function (VERIFIER
 * §1). Contract authors extend it, never shrink below it (the runtime enforces).
 */
export const DEFAULT_PROTECT_GLOBS: readonly string[] = [
  "**/*.test.*",
  "**/rubric.md",
  "**/_work.md",
];

/** Consecutive-fail cap before a run parks at `blocked` (VERIFIER §5). */
export const VERIFIER_MAX_ATTEMPTS = 5;

// ── Helpers ──────────────────────────────────────────────────────────────────

/** Normalize `check` to the named-check array form (bare string → one check named "check"). */
export function normalizeChecks(check: Done["check"]): DoneCheck[] {
  return typeof check === "string" ? [{ name: "check", run: check }] : check;
}

/** True when the contract has at least one runnable deterministic check. */
export function hasCheck(done: Done | undefined): boolean {
  if (!done) return false;
  return normalizeChecks(done.check).length > 0;
}

/** Thrown when a work contract violates a VERIFIER §1 invariant. */
export class InvalidContractError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "InvalidContractError";
  }
}

/**
 * Enforce the gate-pairing rule (VERIFIER §1): `gate: auto` is legal only when
 * the done-check list is non-empty. A judge-only contract must be `gate: human`.
 * Throws `InvalidContractError` otherwise.
 */
export function assertContractGate(contract: Pick<WorkContract, "gate" | "done">): void {
  if (contract.gate === "auto" && !hasCheck(contract.done)) {
    throw new InvalidContractError(
      "gate:auto requires a non-empty done.check; judge-only contracts must use gate:human (VERIFIER §1)",
    );
  }
}

/** The effective protect set: the floor ∪ author additions (never below the floor). */
export function effectiveProtect(done: Done | undefined): string[] {
  const extra = done?.protect ?? [];
  return Array.from(new Set([...DEFAULT_PROTECT_GLOBS, ...extra]));
}

// ── Session + verdict receipt (wire-shaped downstream) ───────────────────────

/** session.status enum (DATA-MODEL §B.3). Distinct from a node's OrchState. */
export type SessionStatus = "running" | "blocked" | "review" | "done" | "canceled";

export const SESSION_STATUSES = [
  "running",
  "blocked",
  "review",
  "done",
  "canceled",
] as const satisfies readonly SessionStatus[];

/** Gate row kind (DATA-MODEL §B.3 gate table). */
export type GateKind = "completion" | "irreversible-action";

/**
 * Child exit reason for exit code 10 (D-EVENTNAMES; `run.exiting {code, reason}`).
 * The supervisor derives `run.finished` after reap.
 */
export type ExitReason = "budget" | "iteration_cap" | "no_progress" | "user_stop" | "fresh_context";

export const EXIT_REASONS = [
  "budget",
  "iteration_cap",
  "no_progress",
  "user_stop",
  "fresh_context",
] as const satisfies readonly ExitReason[];

/** The verifier's overall verdict (VERIFIER §4). */
export type Verdict = "pass" | "fail" | "error";

/**
 * `verdict.md` frontmatter — the receipt (VERIFIER §4). Carried verbatim as the
 * `check.verdict` event payload, so it is wire-shaped and lives here.
 */
export interface VerdictReceipt {
  verdict: Verdict;
  /** Which verification attempt on this run. */
  attempt: number;
  /** Commit the diff was judged against. */
  base: string;
  diffstat: { files: number; plus: number; minus: number };
  /** Protected paths touched, if any. */
  tampering: string[];
  checks: { name: string; ok: boolean; exit: number; duration_s: number; log: string }[];
  judge: { score: number | null; model?: string; detail?: string };
}
