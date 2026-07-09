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

/** The namespaced event families (DATA-MODEL §A.3, §B.3; pinned by the ticket). */
export const EVENT_NAMESPACES = ["run", "tool", "check", "gate", "budget"] as const;
export type EventNamespace = (typeof EVENT_NAMESPACES)[number];

/** An event type inside one of the five namespaces. */
export type NamespacedEventType =
  | `run.${string}`
  | `tool.${string}`
  | `check.${string}`
  | `gate.${string}`
  | `budget.${string}`;

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
 * Note (canon discrepancy, tracked in docs/canon-amendments.md): VERIFIER §7
 * also names `verify.started`, `judge.result`, `verify.error`, which fall
 * OUTSIDE the five namespaces the envelope pins. They are not admitted by
 * EventType here; the verifier-implementation ticket owns reconciling them
 * (fold into `check.*` or widen the namespace set — a deliberate protocol edit).
 */
export const EVENT_TYPES = {
  // run.* — lifecycle (FLOWS F2/F4/F8/F9, HARNESS, D-EVENTNAMES)
  RUN_STARTED: "run.started",
  RUN_EXITING: "run.exiting", // child seam {code, reason} — HARNESS owns it
  RUN_FINISHED: "run.finished", // supervisor-derived after reap (D-EVENTNAMES)
  RUN_FAILED: "run.failed",
  RUN_KILLED: "run.killed",
  RUN_ORPHANED: "run.orphaned",
  // tool.*
  TOOL_CALL: "tool.call",
  // check.* — verification (VERIFIER §7, the namespaced subset)
  CHECK_RESULT: "check.result",
  CHECK_VERDICT: "check.verdict", // D-EVENTNAMES: renamed from bare `verdict`
  // gate.*
  GATE_OPENED: "gate.opened",
  GATE_DECIDED: "gate.decided",
  GATE_APPROVED: "gate.approved", // FLOWS F5
  // budget.*
  BUDGET_EXHAUSTED: "budget.exhausted",
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
  | "unauthorized";

export const ERROR_CODES = [
  "budget_exhausted",
  "lease_held",
  "gate_required",
  "not_found",
  "unauthorized",
] as const satisfies readonly ErrorCode[];

/** The typed refusal shape (API.md §4). */
export interface ErrorResponse {
  error: { code: ErrorCode; message: string; retryable: boolean };
}
