// OrchState — the 8-state work lifecycle and its transition machine.
//
// Canon: DATA-MODEL §B.2 (the enum), data-contract.md §"The state machine"
// (the amended transition rules), PATTERNS §7 ("transitions enumerated in one
// module; illegal transitions throw"), FLOWS F1–F8 (the transition sources).
//
// The system enum is a developer surface; the UI shows plain voice (see
// ./plain-voice.ts). `queued` was removed from the enum (D-ENUM) — three system
// states collapse to the plain "Queued".

/** The 8-state orchestration lifecycle (DATA-MODEL §B.2, D-ENUM). */
export type OrchState =
  | "proposed" // backlog: specs not yet dispatched
  | "reviewing" // backlog: plan under pre-dispatch review
  | "triggered" // backlog: actionable on the next tick, dispatch pending
  | "running" // active: dispatched, live in a worktree
  | "blocked" // ATTENTION: a worker is stuck, unblock it
  | "review" // ATTENTION: clean run waiting at the human gate ("Needs you")
  | "done" // terminal: the branch is the receipt
  | "canceled"; // terminal: renders under the Done group

/** All eight states, in lifecycle order. */
export const ORCH_STATES = [
  "proposed",
  "reviewing",
  "triggered",
  "running",
  "blocked",
  "review",
  "done",
  "canceled",
] as const satisfies readonly OrchState[];

/** The attention set — states that need a human (DATA-MODEL §B.2). */
export const ATTENTION_STATES = ["blocked", "review"] as const;
export type AttentionState = (typeof ATTENTION_STATES)[number];

/** Terminal states — no outgoing transitions. */
export const TERMINAL_STATES = ["done", "canceled"] as const;
export type TerminalState = (typeof TERMINAL_STATES)[number];

export const isAttentionState = (s: OrchState): s is AttentionState =>
  (ATTENTION_STATES as readonly OrchState[]).includes(s);

export const isTerminalState = (s: OrchState): s is TerminalState =>
  (TERMINAL_STATES as readonly OrchState[]).includes(s);

// ── The gate primitives (shared by the machine and the work contract) ────────

/** Does *done* require the human gate? (DATA-MODEL §A.2, D-GATE) */
export type GateMode = "human" | "auto";

/** Both gate modes — the closed set (type derived FROM the const, the single-source idiom). */
export const GATE_MODES = ["human", "auto"] as const satisfies readonly GateMode[];

export const isGateMode = (s: string): s is GateMode =>
  (GATE_MODES as readonly string[]).includes(s);

/**
 * The four Org-Control-Layer gate verdicts — Maestro's control verbs (D-GATE,
 * FLOWS F5): approve → done/merge; revise → send back (triggered); block →
 * canceled (terminal); escalate → stays at review, reassigned.
 */
export type GateVerdict = "approve" | "revise" | "block" | "escalate";

export const GATE_VERDICTS = [
  "approve",
  "revise",
  "block",
  "escalate",
] as const satisfies readonly GateVerdict[];

// ── The transition machine (PATTERNS §7) ─────────────────────────────────────

/**
 * The enumerated flow-transition graph. Each entry lists the states legally
 * reachable from the key by a *flow* transition (FLOWS F1–F8). Every edge is
 * grounded in a specific flow:
 *
 *  - proposed → reviewing|triggered|running  orchestrator grooming + dispatch (F2, F6)
 *  - reviewing → triggered                    pre-dispatch review resolves (F6)
 *  - triggered → running                      dispatch (F2)
 *  - running → review                         verify pass + gate:human → gate (F4→F5)
 *  - running → done                           verify pass + gate:auto merges (F4) [guarded]
 *  - running → blocked                         spawn fail (F2) / stop condition (F3) / kill (F8)
 *  - blocked → triggered                       human unblocks, redispatch (F9)
 *  - review → done                             approve verdict (F5) [guarded]
 *  - review → triggered                        revise / send back (F5)
 *  - review → canceled                         block verdict, terminal (F5, D-GATE)
 *
 * The human override intent `set_state` (API.md §1, "human override, audited")
 * deliberately BYPASSES this machine — it is audited at the API layer, not
 * routed through `transition`. Cancelling queued/blocked work (there is no flow
 * intent for it) goes through that override.
 *
 * Two edges are additionally guarded (see `transition`):
 *  - review → done   requires an `approve` verdict (PATTERNS §7 invariant;
 *                    FLOWS F5: approve is the ONLY path review→done when gate:human).
 *  - running → done  is legal ONLY under gate:auto (D-AUTODONE; FLOWS F4). Under
 *                    gate:human a clean run parks at `review`, never auto-done.
 */
const LEGAL_TRANSITIONS: Record<OrchState, readonly OrchState[]> = {
  proposed: ["reviewing", "triggered", "running"],
  reviewing: ["triggered"],
  triggered: ["running"],
  running: ["review", "done", "blocked"],
  blocked: ["triggered"],
  review: ["done", "triggered", "canceled"],
  done: [],
  canceled: [],
};

/** The raw flow-transition graph (read-only) — for board/graph rendering + audits. */
export const TRANSITIONS: Readonly<Record<OrchState, readonly OrchState[]>> = LEGAL_TRANSITIONS;

/** Thrown when a transition is not in the enumerated flow graph. */
export class IllegalTransitionError extends Error {
  readonly from: OrchState;
  readonly to: OrchState;
  constructor(from: OrchState, to: OrchState) {
    super(`Illegal OrchState transition: ${from} → ${to}`);
    this.name = "IllegalTransitionError";
    this.from = from;
    this.to = to;
  }
}

/** Thrown when a legal edge is missing its required gate verdict / gate mode. */
export class GateRequiredError extends Error {
  readonly from: OrchState;
  readonly to: OrchState;
  constructor(from: OrchState, to: OrchState, detail: string) {
    super(`Gate required for ${from} → ${to}: ${detail}`);
    this.name = "GateRequiredError";
    this.from = from;
    this.to = to;
  }
}

/** True if `from → to` is a legal flow edge (ignores the verdict/gate guards). */
export const isLegalTransition = (from: OrchState, to: OrchState): boolean =>
  LEGAL_TRANSITIONS[from].includes(to);

/** Context for the two guarded edges. */
export interface TransitionContext {
  /** The gate verdict — required for the review → done edge (FLOWS F5 approve). */
  verdict?: GateVerdict;
  /** The contract's gate mode — required for the running → done edge (D-AUTODONE). */
  gate?: GateMode;
}

/**
 * Apply an OrchState transition (PATTERNS §7). Returns the target state on
 * success; throws `IllegalTransitionError` for a non-flow edge, or
 * `GateRequiredError` when a guarded edge is missing its verdict/gate mode.
 */
export function transition(from: OrchState, to: OrchState, ctx: TransitionContext = {}): OrchState {
  if (!LEGAL_TRANSITIONS[from].includes(to)) {
    throw new IllegalTransitionError(from, to);
  }
  // Guard 1 — review → done requires an approve verdict (PATTERNS §7; FLOWS F5:
  // "approve is the only path from review to done when gate:human").
  if (from === "review" && to === "done" && ctx.verdict !== "approve") {
    throw new GateRequiredError(from, to, "requires an `approve` gate verdict");
  }
  // Guard 2 — running → done only under gate:auto (D-AUTODONE; FLOWS F4). Under
  // gate:human a clean run parks at review, never auto-done.
  if (from === "running" && to === "done" && ctx.gate !== "auto") {
    throw new GateRequiredError(
      from,
      to,
      "requires gate:auto; under gate:human a clean run parks at `review`",
    );
  }
  return to;
}

/**
 * Resolve a gate verdict to the next OrchState (D-GATE, FLOWS F5). `escalate`
 * (point / grant) leaves the row at `review` — reassigned, not decided. Throws
 * if the current state is not `review` (verdicts only apply at the gate).
 */
export function resolveGateVerdict(current: OrchState, verdict: GateVerdict): OrchState {
  if (current !== "review") {
    throw new IllegalTransitionError(current, current);
  }
  switch (verdict) {
    case "approve":
      return "done";
    case "revise":
      return "triggered";
    case "block":
      return "canceled";
    case "escalate":
      return "review";
  }
}
