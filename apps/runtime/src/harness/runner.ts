// Runner port + the Claude Agent SDK adapter (HARNESS §1, §6). A "runner" wraps an agent loop and
// translates its message stream into the `session.jsonl` event vocabulary. This ticket (BRO-1756)
// lands the PORT + the §6 translation the adapter owns; the live `start()` loop is deferred to
// dispatch (F2) because it needs the metered model proxy (HARNESS §3) that BRO-1788 builds — this
// ticket precedes it. Runner-per-role: agent (Loop 1), verifier (Loop 2), orchestrator (F6).

import type { Actor, EventType } from "@maestro/protocol";
import { EVENT_TYPES } from "@maestro/protocol";
import type { ChildInvocation, ChildRole } from "./spawn-contract";

/**
 * A translated `session.jsonl` line — the DATA-MODEL §A.3 event minus the fields the supervisor
 * stamps on tee: `seq` (the index autoincrement) and `ts` (emit time). The runner produces these;
 * the supervisor's tee (BRO-1767) assigns seq + appends. Kept payload-nested (the EventEnvelope
 * wire shape) — flattening to the A.3 on-disk shape is the tee's serialization concern.
 */
export interface ChildEmittedEvent {
  actor: Actor;
  type: EventType;
  payload?: Record<string, unknown>;
}

/**
 * The Claude Agent SDK emits a rich message stream; these are the OCCURRENCES HARNESS §6 maps to
 * session events, normalized. The live `ClaudeSdkRunner.start()` (deferred) normalizes the real SDK
 * stream into these; `translateSdkOccurrence` owns the §6 mapping over them. Keeping the mapping
 * over a normalized shape (rather than the SDK's concrete message types) keeps the §6 table the one
 * place the vocabulary is decided, and keeps this module free of the SDK dependency until start() lands.
 */
export type SdkOccurrence =
  | { kind: "assistant_turn"; text: string } // one COMPLETED text/reasoning block (not per token)
  | { kind: "tool_use"; tool: string; input?: unknown; path?: string }
  | { kind: "tool_result"; tool: string; ok: boolean; summary?: string }
  | { kind: "model_call_completed" } // §6: none — the proxy emits budget.metered
  | { kind: "run_beat"; iteration: number; diffstat?: string }
  | { kind: "run_started"; run?: string; branch?: string }
  | { kind: "run_exiting"; code: number; reason: string };

function pruneUndefined(o: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(o)) if (v !== undefined) out[k] = v;
  return out;
}

/** A short, log-safe summary of a tool input (the audit trail stores decisions, not payloads). */
export function summarizeInput(input: unknown): string | undefined {
  if (input === undefined || input === null) return undefined;
  let s: string;
  if (typeof input === "string") {
    s = input;
  } else {
    // HARNESS §5: the §6 audit-trail translation must NOT crash the child. A circular, bigint, or
    // otherwise non-serializable tool input degrades to String(...) — it never throws. (JSON.stringify
    // returns undefined for a top-level function; the ?? catches that too.)
    try {
      s = JSON.stringify(input) ?? String(input);
    } catch {
      s = String(input);
    }
  }
  return s.length > 200 ? `${s.slice(0, 197)}...` : s;
}

/**
 * The HARNESS §6 mapping: one SDK occurrence → one `session.jsonl` event (or `null` when §6 says the
 * occurrence is not logged). Actors follow the DATA-MODEL §A.3 examples: the agent authors its own
 * turns and tool calls; the tool authors its results; lifecycle beats are system.
 */
export function translateSdkOccurrence(occ: SdkOccurrence): ChildEmittedEvent | null {
  switch (occ.kind) {
    case "assistant_turn":
      return { actor: "agent", type: EVENT_TYPES.AGENT_SAID, payload: { text: occ.text } };
    case "tool_use":
      return {
        actor: "agent",
        type: EVENT_TYPES.TOOL_CALL,
        payload: pruneUndefined({
          tool: occ.tool,
          input: summarizeInput(occ.input),
          path: occ.path,
        }),
      };
    case "tool_result":
      return {
        actor: "tool",
        type: EVENT_TYPES.TOOL_RESULT,
        payload: pruneUndefined({ tool: occ.tool, ok: occ.ok, summary: occ.summary }),
      };
    case "model_call_completed":
      return null; // §6: the model call itself is not logged — the proxy emits budget.metered (§3)
    case "run_beat":
      return {
        actor: "system",
        type: EVENT_TYPES.RUN_BEAT,
        payload: pruneUndefined({ iteration: occ.iteration, diffstat: occ.diffstat }),
      };
    case "run_started":
      return {
        actor: "system",
        type: EVENT_TYPES.RUN_STARTED,
        payload: pruneUndefined({ run: occ.run, branch: occ.branch }),
      };
    case "run_exiting":
      return {
        actor: "system",
        type: EVENT_TYPES.RUN_EXITING,
        payload: { code: occ.code, reason: occ.reason },
      };
  }
}

/** A live child run: its translated event stream + a graceful stop (HARNESS §2 SIGTERM semantics). */
export interface RunnerHandle {
  events: AsyncIterable<ChildEmittedEvent>;
  stop(reason: string): Promise<number>;
}

/** The runner port — one adapter per agent loop. The adapter owns spawning the loop and the §6
 *  translation; the supervisor owns the seam (env/argv, tee, budget, reap). */
export interface Runner {
  readonly role: ChildRole;
  start(invocation: ChildInvocation, env: Record<string, string>): Promise<RunnerHandle>;
}

/**
 * The Claude Agent SDK runner — the first (agent-role) adapter. It owns the §6 translation
 * (`translate`); the live `start()` loop lands with dispatch (F2), because it needs the metered
 * model proxy (`baseURL` + per-session bearer, HARNESS §3) that BRO-1788 builds. This ticket precedes
 * the proxy, so `start()` refuses loudly rather than pretending to run.
 */
export class ClaudeSdkRunner implements Runner {
  readonly role: ChildRole = "agent";

  // `async` so the deferral REJECTS rather than throwing synchronously — honoring the declared
  // Promise<RunnerHandle> contract before the F2 dispatcher codes against the seam (a sync throw
  // would silently change the caller's error handling at swap-in). The live loop (which drives the
  // §6 `translateSdkOccurrence` mapping over the normalized SDK stream) lands with dispatch: it needs
  // the metered model proxy (baseURL + per-session bearer, HARNESS §3) that BRO-1788 builds.
  async start(_invocation: ChildInvocation, _env: Record<string, string>): Promise<RunnerHandle> {
    throw new Error(
      "ClaudeSdkRunner.start() lands with dispatch (F2): the live SDK loop needs the metered model " +
        "proxy (baseURL + per-session bearer, HARNESS §3) that BRO-1788 builds. This ticket ships the " +
        "port + the §6 translation; the loop is wired when the proxy exists.",
    );
  }
}
