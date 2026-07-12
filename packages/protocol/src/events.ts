// Events — the read surface (DATA-MODEL §A.3 + §B.3, API.md §stream + §4).
//
// The event envelope is the WIRE shape of an event (API.md §stream); the stored
// `event` row is `EventRow` (index-schema.ts), which differs in representation
// (numeric `ts`, JSON-string `payload`) and projects to this envelope at the wire
// boundary. `seq` is the index autoincrement — total order, no gaps, the SSE
// resume cursor. Error codes are part of the protocol (API.md §4).

/** Who produced an event (DATA-MODEL §A.3). */
export type Actor = "agent" | "user" | "tool" | "system";

export const ACTORS = ["agent", "user", "tool", "system"] as const satisfies readonly Actor[];

/**
 * The namespaced event families (DATA-MODEL §A.3, §B.3). `agent` was added in BRO-1756 to admit
 * `agent.said` (HARNESS §6 maps coalesced assistant turns to it, and `agent` is already a first-class
 * actor) — a deliberate widening of the original five, logged in docs/canon-amendments.md.
 */
export const EVENT_NAMESPACES = ["run", "tool", "check", "gate", "budget", "agent"] as const;
export type EventNamespace = (typeof EVENT_NAMESPACES)[number];

/** An event type inside one of the six namespaces. */
export type NamespacedEventType =
  | `run.${string}`
  | `tool.${string}`
  | `check.${string}`
  | `gate.${string}`
  | `budget.${string}`
  | `agent.${string}`;

/**
 * Synthetic event types the runtime projects beyond session.jsonl (API.md
 * §stream). This list is **closed** (D-DURABILITY): node creation surfaces as
 * `node.updated` — there is deliberately no `node.created`.
 */
export const SYNTHETIC_EVENT_TYPES = [
  "node.updated",
  "gate.opened",
  "gate.decided",
  "schedule.fired",
] as const;
export type SyntheticEventType = (typeof SYNTHETIC_EVENT_TYPES)[number];

/** Any valid wire event type — a namespaced type or a closed-list synthetic. */
export type EventType = NamespacedEventType | SyntheticEventType;

/**
 * The concrete event types named across the flows/specs. NON-exhaustive: new
 * types may appear within a namespace, but every entry here is a real event a
 * flow emits. Kept as a named catalog so consumers reference constants, not
 * string literals.
 *
 * Resolution (BRO-1794, tracked in docs/canon-amendments.md): VERIFIER §7 also
 * names `verify.started`, `judge.result`, `verify.error`, which fell OUTSIDE the
 * six envelope namespaces. Rather than widen the namespace set (the move BRO-1756
 * made for `agent.*`), the verifier reap FOLDS them into the `check.*` family —
 * `check.started` / `check.judge` / `check.error` — since the verifier is one
 * check family; the six-namespace envelope stays intact.
 */
export const EVENT_TYPES = {
  // run.* — lifecycle (FLOWS F2/F4/F8/F9, HARNESS, D-EVENTNAMES)
  RUN_STARTED: "run.started",
  RUN_BEAT: "run.beat", // loop-beat summary {iteration, diffstat} — HARNESS §6
  RUN_EXITING: "run.exiting", // child seam {code, reason} — HARNESS owns it
  RUN_FINISHED: "run.finished", // supervisor-derived after reap (D-EVENTNAMES)
  RUN_FAILED: "run.failed",
  RUN_KILLED: "run.killed",
  RUN_ORPHANED: "run.orphaned",
  RUN_HUNG: "run.hung", // supervisor liveness escalation — child silent > hungMs (HARNESS §2, BRO-1767)
  RUN_EXIT_MISMATCH: "run.exit_mismatch", // run.exiting code ≠ real exit code — Loop-4 harness-bug signal (HARNESS §4, BRO-1779)
  RUN_RESTART_REQUESTED: "run.restart_requested", // child hit the context ceiling → wrote progress.md, asks for a fresh-context respawn before exit-10 fresh_context (HARNESS §5, BRO-1795)
  // agent.* — the child's own utterances (HARNESS §6; BRO-1756 widening)
  AGENT_SAID: "agent.said", // coalesced assistant turn (one per completed text block, not per token)
  // tool.*
  TOOL_CALL: "tool.call",
  TOOL_RESULT: "tool.result", // HARNESS §6 — { tool, ok, summary }
  // check.* — verification (VERIFIER §7). The reap emits these in order: started → per-check result(s) →
  // judge → verdict; an infra failure emits `check.error` instead. verify.started/judge.result/verify.error
  // are FOLDED here (BRO-1794) rather than widening the namespace set — the verifier is one check family.
  CHECK_STARTED: "check.started", // a verification attempt began { attempt } (was VERIFIER §7 `verify.started`)
  CHECK_RESULT: "check.result", // one deterministic check's outcome { name, ok, exit, duration_s, log }
  CHECK_JUDGE: "check.judge", // the Stage-2 judge receipt { score, model, detail? } (was `judge.result`)
  CHECK_VERDICT: "check.verdict", // the VerdictReceipt verbatim — D-EVENTNAMES: renamed from bare `verdict`
  CHECK_ERROR: "check.error", // verification could not run (infra) { message } (was VERIFIER §7 `verify.error`)
  // gate.*
  GATE_OPENED: "gate.opened",
  GATE_DECIDED: "gate.decided",
  GATE_APPROVED: "gate.approved", // FLOWS F5
  // budget.* — the budget-in-path guard (HARNESS §3, F3.1). BRO-1788 named `refused`
  // (pre-forward refusal) + `metered` (post-response accounting); `exhausted` is the
  // child's loop-halt event (F3). All three are durable (D-DURABILITY).
  BUDGET_EXHAUSTED: "budget.exhausted",
  BUDGET_REFUSED: "budget.refused", // HARNESS §3 step 1 — over-limit, request never forwarded
  BUDGET_METERED: "budget.metered", // HARNESS §3 step 3 — actual usage { usd, tokens }
  // synthetics (projected by the runtime)
  NODE_UPDATED: "node.updated",
  SCHEDULE_FIRED: "schedule.fired",
} as const satisfies Record<string, EventType>;

/**
 * The event envelope — the WIRE shape of an event (API.md §stream). The stored row
 * is `EventRow` (index-schema.ts): the same fields, but `ts` epoch-ms and `payload`
 * raw JSON text, projected to this envelope at the wire boundary. `sessionId` is
 * nullable for synthetics (D-DURABILITY). `ts` here is ISO-8601 on the wire
 * (session.jsonl uses ISO strings, DATA-MODEL §A.3).
 */
export interface EventEnvelope<P = unknown> {
  seq: number;
  sessionId?: string | null;
  ts: string;
  actor: Actor;
  type: EventType;
  payload?: P;
}

/** The namespace of an event type, or null if the prefix is not a known family. */
export const eventNamespace = (type: string): EventNamespace | null => {
  const prefix = type.split(".")[0] ?? "";
  return (EVENT_NAMESPACES as readonly string[]).includes(prefix)
    ? (prefix as EventNamespace)
    : null;
};

export const isSyntheticEventType = (type: string): type is SyntheticEventType =>
  (SYNTHETIC_EVENT_TYPES as readonly string[]).includes(type);

/** True if `type` is a valid wire event type — a known namespace or a closed-list synthetic. */
export const isWireEventType = (type: string): boolean =>
  eventNamespace(type) !== null || isSyntheticEventType(type);

// ── Errors (API.md §4) ───────────────────────────────────────────────────────

/** Error codes — part of the protocol (API.md §4). The UI renders them in plain voice. */
export type ErrorCode =
  | "budget_exhausted"
  | "lease_held"
  | "gate_required"
  | "not_found"
  | "unauthorized"
  // Intent write-surface refusals (API.md §1 Intents, BRO-1820):
  | "invalid_intent" // malformed body, unknown type, missing/invalid field, or missing Idempotency-Key
  | "unsupported_intent" // a valid Intent type whose handler is not wired yet (P1 ships new_mission only)
  | "intent_failed"; // well-formed intent, its side effect (FS/git) failed; nothing half-created, retryable

export const ERROR_CODES = [
  "budget_exhausted",
  "lease_held",
  "gate_required",
  "not_found",
  "unauthorized",
  "invalid_intent",
  "unsupported_intent",
  "intent_failed",
] as const satisfies readonly ErrorCode[];

/** The typed refusal shape (API.md §4). */
export interface ErrorResponse {
  error: { code: ErrorCode; message: string; retryable: boolean };
}
