// Intents — the write surface (API.md §1).
//
// One endpoint (POST /api/intents), a discriminated union that mirrors the gate
// verbs and the human's vocabulary, nothing else. Results arrive on the event
// stream, never in the response body (PATTERNS §3: intents in, events out).

import type { OrchState } from "./state";

/**
 * A folder is work at any scale (DATA-MODEL §A.1). `routine` is the only kind
 * with runtime-special behavior — it carries a `trigger`.
 */
export type Kind = "question" | "task" | "project" | "initiative" | "routine";

export const KINDS = [
  "question",
  "task",
  "project",
  "initiative",
  "routine",
] as const satisfies readonly Kind[];

/** Trigger kinds for routines / event-driven work (DATA-MODEL §A.2, §B.3 schedule). */
export type TriggerKind = "heartbeat" | "cron" | "hook" | "goal";

export const TRIGGER_KINDS = [
  "heartbeat",
  "cron",
  "hook",
  "goal",
] as const satisfies readonly TriggerKind[];

/** The trigger block on a `kind: routine` contract (DATA-MODEL §A.2). */
export interface Trigger {
  on: TriggerKind;
  /** cron expression | interval | hook selector | goal condition (schedule.spec). */
  at?: string;
  /** idempotency key template — the storm killer (FLOWS F7). */
  idempotency?: string;
}

/**
 * Tick cause (F6) — why the orchestrator woke. API.md §1 pins
 * `interval | hook | manual`; the amendment adds `worker_return` (a worker
 * returning — F6, data-contract MCC_TICK_WAKES).
 */
export type TickCause = "interval" | "hook" | "manual" | "worker_return";

export const TICK_CAUSES = [
  "interval",
  "hook",
  "manual",
  "worker_return",
] as const satisfies readonly TickCause[];

/**
 * The intent discriminated union (API.md §1), verbatim from the wire owner.
 *
 * Plain-voice verb map: `escalate` is surfaced in the UI as "point" (reassign
 * the owner) and pairs with `grant` (attach a capability) — FLOWS F5. These are
 * two distinct intents, not one.
 *
 * Deliberately NOT intents (kept out to preserve the single write path,
 * PATTERNS §3):
 *  - `chat` has its own endpoint (API.md §Chat, POST /api/sessions/:id/chat) —
 *    it streams the UI Message Stream Protocol, it is not an intent.
 *  - `nudge` (F6 "nudge stuck runs") is an orchestrator action expressed through
 *    the existing intents (dispatch / chat), not a wire verb of its own.
 */
export type Intent =
  | { type: "new_mission"; parentPath: string; title: string; brief: string; kind: Kind }
  | { type: "dispatch"; nodeId: string }
  | { type: "approve"; gateId: string }
  | { type: "revise"; gateId: string; feedback: string } // send back
  | { type: "block"; gateId: string; reason?: string }
  | { type: "escalate"; gateId: string; to: string } // point
  | { type: "grant"; gateId: string; capability: string }
  | { type: "kill"; sessionId: string }
  | { type: "set_routine"; nodeId: string; trigger: Trigger } // a sentence, parsed upstream
  | { type: "set_state"; nodeId: string; state: OrchState } // human override, audited
  | { type: "tick"; cause: TickCause };

export type IntentType = Intent["type"];

export const INTENT_TYPES = [
  "new_mission",
  "dispatch",
  "approve",
  "revise",
  "block",
  "escalate",
  "grant",
  "kill",
  "set_routine",
  "set_state",
  "tick",
] as const satisfies readonly IntentType[];

/** The single write endpoint (API.md §1) — POST an `Intent`, get a 202 `IntentAccepted`, results
 *  arrive on the event stream (PATTERNS §3: intents in, events out). Pinned here so a client never
 *  hand-writes the path literal (mirrors chat.ts's `CHAT_ENDPOINT` — the single-source idiom). */
export const INTENTS_ENDPOINT = "/api/intents" as const;

/** Header required on every intent (API.md §Intents) — a retried POST is a no-op. */
export const IDEMPOTENCY_KEY_HEADER = "Idempotency-Key" as const;

/** The 202 ack for an accepted intent — results follow on the stream (API.md §Intents). */
export interface IntentAccepted {
  accepted: true;
}
