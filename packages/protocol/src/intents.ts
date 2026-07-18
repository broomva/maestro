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

/** The wire header carrying the CALLER's actor on a POST /api/intents (default "user" when absent — the
 *  human SPA sends no actor header, so absence MUST read as human or the UI's own gate decisions break).
 *  The runtime reads it to enforce the ORCHESTRATOR §4 human-verb gate server-side.
 *
 *  TRUST MODEL (read before relying on this): the header is only as trustworthy as the code that SETS it.
 *  Today nothing sets it to an agent value — no agent-side intent path exists yet (the orchestrator session
 *  is not wired), so the gate is CORRECT BUT INERT. For the gate to actually fence an autonomous
 *  orchestrator, its intent path must (a) be issued by host tool code that pins this header to `agent:*`
 *  (the model never composes the raw request), and (b) run in a harness with no shell / no arbitrary
 *  network egress, so the model cannot bypass the tool and POST `X-Maestro-Actor: user` itself. Neither
 *  (a) nor (b) is implemented here — they are BLOCKING preconditions on the orchestrator-wiring slice, not
 *  facts this comment may assume. This is a coarse in-process guard, not un-spoofable auth; token-scoped
 *  caller identity is P6 / relay. */
export const MAESTRO_ACTOR_HEADER = "X-Maestro-Actor" as const;

/** ORCHESTRATOR §4 — the four gate verbs + the kill switch + the audited state override are HUMAN-ONLY:
 *  the runtime rejects them from an `agent:*` actor regardless of prompt content ("defense sits in the
 *  API, not the prompt"). The intents an agent MAY issue are the rest — new_mission, dispatch, set_routine,
 *  tick (`nudge` is a chat message, not a wire verb; see the Intent doc comment above). */
export const HUMAN_ONLY_INTENT_TYPES = [
  "approve",
  "revise",
  "block",
  "escalate",
  "grant",
  "kill",
  "set_state",
] as const satisfies readonly IntentType[];

/** True when `actor` is an agent — bare `agent` or a namespaced `agent:*` such as `agent:maestro` — the
 *  class the §4 human-verb gate rejects. Any non-agent actor (the default `user`, plus `system` / `tool`)
 *  passes: matching `agent:` as a PREFIX (not `agent` anywhere) keeps a name like `user:agentsmith` human.
 *
 *  The value is trimmed + lower-cased before matching so a trusted caller that sets ` Agent:Maestro ` (a
 *  casing / whitespace slip) still gates rather than silently falling open. Normalization is for the
 *  predicate only — callers keep the original string for identity / audit. This does NOT make the gate
 *  fail-closed on unknown values (an unrecognized actor stays human): the header is host-set, and the
 *  human SPA's no-header default must remain human. Fail-closed identity is P6's job, not this guard's. */
export function isAgentActor(actor: string): boolean {
  const a = actor.trim().toLowerCase();
  return a === "agent" || a.startsWith("agent:");
}

/** The 202 ack for an accepted intent — results follow on the stream (API.md §Intents). */
export interface IntentAccepted {
  accepted: true;
}
