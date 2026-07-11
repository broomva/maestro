/// <reference types="bun" />
// chat.ts (BRO-1822 slice 2) — the F10 chat endpoint. `POST /api/sessions/:id/chat`:
// a UIMessage in → routed into the live child's stdin (dispatch-then-chat for idle work) →
// the session's events projected out as an AI SDK v6 UI Message Stream (FLOWS F10, API §Chat).
//
// Chat is a PROJECTION of a session (FLOWS F10.4): "closing the tab loses nothing" — the child's
// events land in session.jsonl (the truth) regardless of who is watching; this endpoint only renders
// them. So a disconnect ends the render, never the run.
//
// THE WIRE IS ADOPTED, NOT INVENTED (docs/contracts/chat-transport.md §1): Maestro does not declare a
// chat wire. It uses ai@6's `UIMessageChunk` type + `createUIMessageStream`/`createUIMessageStreamResponse`
// helpers directly, so there is nothing mirrored to drift. The one Maestro-owned piece — the
// EventType→UIMessageChunk mapping (contract §7) — is runtime logic and lives here (not in the zero-dep
// protocol package).
//
// SCOPE (slice 2): projects the DURABLE event stream (source 2 of contract §7) — coalesced `agent.said`
// turns, `tool.call`/`tool.result`, and the terminal run.* events. The live per-TOKEN delta stream
// (source 1) needs the child + proxy to stream tokens (they POST/read whole responses today) — a
// follow-up. The resulting message is well-formed either way: one assistant message that accretes text
// and tool parts as the agent works, finishing when the run reaches a terminal state (the "unsupervised
// hours" model — the assistant message spans the agent's work until it needs you / halts).

import {
  type ErrorCode,
  type ErrorResponse,
  EVENT_TYPES,
  MAESTRO_PROTOCOL_HEADER,
  MAESTRO_PROTOCOL_VERSION,
  type UIMessageEnvelope,
} from "@maestro/protocol";
import { createUIMessageStream, createUIMessageStreamResponse, type UIMessageChunk } from "ai";
import { and, asc, eq, gt, max } from "drizzle-orm";
import type { Context, Hono } from "hono";
import type { ContentfulStatusCode } from "hono/utils/http-status";
import type { IndexDb } from "../db/client";
import { event } from "../db/schema";
import type { DispatchRuntime } from "../dispatch";
import { parsePayload } from "./event-projection";
import { abortableSleep, DEFAULT_STREAM_POLL_MS } from "./stream";

/** Rows drained per tail query while projecting a session (matches the SSE stream's batch). */
const CHAT_DRAIN_BATCH = 500;

/** What the chat route needs: the open index (to tail) + a LAZY dispatch accessor (the supervisor is
 *  mounted after createApp runs, and only in mock-model mode — so this reads it at request time). */
export interface ChatDeps {
  db: IndexDb;
  /** The mounted dispatch runtime, or undefined when the runtime is read-only (no model loop). */
  dispatch: () => DispatchRuntime | undefined;
  /** Tail poll interval in ms (default {@link DEFAULT_STREAM_POLL_MS}). */
  pollMs?: number;
}

/** Emit an API §4 typed error envelope with the protocol header. */
function fail(
  c: Context,
  code: ErrorCode,
  message: string,
  status: ContentfulStatusCode,
  retryable = false,
): Response {
  const body: ErrorResponse = { error: { code, message, retryable } };
  c.header(MAESTRO_PROTOCOL_HEADER, String(MAESTRO_PROTOCOL_VERSION));
  return c.json(body, status);
}

/** Structural check for an ai `UIMessage` (id/role/parts) — protocol's `UIMessageEnvelope` shape. */
function isMessageLike(x: unknown): x is UIMessageEnvelope {
  if (!x || typeof x !== "object") return false;
  const m = x as { role?: unknown; parts?: unknown };
  return typeof m.role === "string" && Array.isArray(m.parts);
}

/** Normalize a message-like object to the minimal control-line envelope (id defaulted if the client
 *  omitted it — the child only reads `parts`). */
function toEnvelope(m: UIMessageEnvelope): UIMessageEnvelope {
  const id = (m as { id?: unknown }).id;
  return { id: typeof id === "string" ? id : "user", role: m.role, parts: m.parts };
}

/**
 * Pull the newest user message from a chat body. The SDK transport POSTs `{ messages: UIMessage[], … }`
 * (§9); API.md §Chat also allows a single bare `UIMessage`. The newest `role:"user"` message is the turn
 * to route into the child.
 */
export function extractLatestUserMessage(body: unknown): UIMessageEnvelope | undefined {
  const list =
    body && typeof body === "object" && Array.isArray((body as { messages?: unknown }).messages)
      ? ((body as { messages: unknown[] }).messages as unknown[])
      : isMessageLike(body)
        ? [body]
        : [];
  for (let i = list.length - 1; i >= 0; i--) {
    const m = list[i];
    if (isMessageLike(m) && m.role === "user") return toEnvelope(m);
  }
  return undefined;
}

/** The current maximum event seq for a session — the point AFTER which this chat's events will land. */
async function currentMaxSeq(db: IndexDb, sessionId: string): Promise<number> {
  const rows = await db
    .select({ m: max(event.seq) })
    .from(event)
    .where(eq(event.sessionId, sessionId));
  return rows[0]?.m ?? 0;
}

/**
 * The EventType→UIMessageChunk projection (contract §7) as a per-row tail. Writes ONE assistant message:
 * `start` → text/tool parts as the child tees them → `finish` on the terminal run event. A run that dies
 * mid-turn (`run.failed`/`run.killed`) surfaces a stream-level `error` chunk + closes any dangling tool
 * part with `tool-output-error` (the tool never hangs at "running" — contract §4). The child tees
 * tool.call then tool.result strictly paired (broomva-child.ts), and neither carries a toolCallId, so we
 * synthesize one from the tool.call's seq and correlate the immediately-following tool.result to it.
 */
async function streamSession(
  writer: { write(chunk: UIMessageChunk): void },
  opts: { db: IndexDb; runId: string; startCursor: number; pollMs: number; signal: AbortSignal },
): Promise<void> {
  const { db, runId, startCursor, pollMs, signal } = opts;
  writer.write({ type: "start", messageId: runId });

  let cursor = startCursor;
  // The most-recent unresolved tool.call — the next tool.result closes it (strict pairing). Tracked so a
  // run that dies between call and result still closes the part (tool-output-error), never leaves it open.
  let openToolCallId: string | undefined;
  let finished = false;
  let errored = false;

  const closeDanglingTool = (errorText: string) => {
    if (openToolCallId) {
      writer.write({ type: "tool-output-error", toolCallId: openToolCallId, errorText });
      openToolCallId = undefined;
    }
  };

  while (!signal.aborted && !finished) {
    let rows: (typeof event.$inferSelect)[];
    try {
      rows = await db
        .select()
        .from(event)
        .where(and(eq(event.sessionId, runId), gt(event.seq, cursor)))
        .orderBy(asc(event.seq))
        .limit(CHAT_DRAIN_BATCH);
    } catch {
      // The index became unreadable mid-tail — end the render rather than spin (the run is untouched).
      break;
    }

    for (const row of rows) {
      cursor = row.seq;
      const payload = parsePayload(row.payload) as Record<string, unknown> | undefined;
      switch (row.type) {
        case EVENT_TYPES.AGENT_SAID: {
          const text = typeof payload?.text === "string" ? payload.text : "";
          if (text === "") break;
          const id = `text-${row.seq}`;
          writer.write({ type: "text-start", id });
          writer.write({ type: "text-delta", id, delta: text });
          writer.write({ type: "text-end", id });
          break;
        }
        case EVENT_TYPES.TOOL_CALL: {
          const toolCallId = `tool-${row.seq}`;
          const toolName = typeof payload?.tool === "string" ? payload.tool : "tool";
          writer.write({ type: "tool-input-start", toolCallId, toolName });
          writer.write({
            type: "tool-input-available",
            toolCallId,
            toolName,
            input: payload?.input,
          });
          openToolCallId = toolCallId;
          break;
        }
        case EVENT_TYPES.TOOL_RESULT: {
          // Correlate to the open tool.call (strict pairing). A stray result (no open call) is ignored.
          if (!openToolCallId) break;
          const toolCallId = openToolCallId;
          openToolCallId = undefined;
          if (payload?.ok === false) {
            const errorText =
              typeof payload?.summary === "string" ? payload.summary : "tool failed";
            writer.write({ type: "tool-output-error", toolCallId, errorText });
          } else {
            writer.write({
              type: "tool-output-available",
              toolCallId,
              output: payload?.summary ?? null,
            });
          }
          break;
        }
        case EVENT_TYPES.RUN_FINISHED:
          finished = true;
          break;
        case EVENT_TYPES.RUN_FAILED: {
          const reason = typeof payload?.reason === "string" ? payload.reason : "the run failed";
          closeDanglingTool(reason);
          writer.write({ type: "error", errorText: reason });
          errored = true;
          finished = true;
          break;
        }
        case EVENT_TYPES.RUN_KILLED:
          closeDanglingTool("the session was stopped");
          writer.write({ type: "error", errorText: "the session was stopped" });
          errored = true;
          finished = true;
          break;
        default:
          break; // run.beat / budget.* / gate.* / synthetics: not chat content in slice 2
      }
      if (finished) break;
    }

    if (finished) break;
    // A full batch means the backlog is still draining — re-query immediately, no sleep.
    if (rows.length === CHAT_DRAIN_BATCH) continue;
    await abortableSleep(pollMs, signal);
  }

  // The client went away — stop rendering; the run is untouched (chat is a projection). Writing after an
  // abort is harmless but pointless.
  if (signal.aborted) return;
  // A clean finish (or an index-read failure that broke the loop): close any dangling tool part, then
  // finish so the assistant message is well-formed even if the run ended between a call and its result.
  closeDanglingTool("run ended before the tool returned");
  writer.write({ type: "finish", ...(errored ? { finishReason: "error" as const } : {}) });
}

/** The `POST /api/sessions/:id/chat` handler — resolve/dispatch the target run, route the message in,
 *  stream the projected session out. */
async function handleChat(c: Context, deps: ChatDeps): Promise<Response> {
  const rt = deps.dispatch();
  if (!rt) {
    // No model loop mounted (read-only runtime, or no mock-model mode) — the same shape the F8 kill
    // intent returns when the supervisor is absent (API §4 / intents.ts).
    return fail(
      c,
      "unsupported_intent",
      "chat is unavailable (no model loop mounted; set MAESTRO_MOCK_MODEL=1)",
      501,
    );
  }
  const id = c.req.param("id");
  if (!id) return fail(c, "invalid_intent", "missing session/node id in the path", 400);

  let userMessage: UIMessageEnvelope | undefined;
  try {
    userMessage = extractLatestUserMessage(await c.req.json());
  } catch {
    return fail(c, "invalid_intent", "chat body must be JSON (a UIMessage or { messages })", 400);
  }
  if (!userMessage) return fail(c, "invalid_intent", "no user message in the chat body", 400);

  // Resolve the target run: (1) a live run addressed by its id; (2) a live run for the addressed NODE;
  // (3) an idle node → dispatch-then-chat (FLOWS F10.2 "spawns a session if the target is idle work").
  const sup = rt.supervisor;
  let entry = sup.get(id) ?? sup.list().find((e) => e.nodeId === id) ?? null;
  if (!entry) {
    const out = await sup.dispatch(id);
    if (!out.dispatched) {
      return out.reason === "node_not_found"
        ? fail(c, "not_found", `no live session and no node: ${id}`, 404)
        : fail(c, "lease_held", `node ${id} is busy (its lease is held)`, 409, true);
    }
    entry = sup.get(out.runId);
    if (!entry)
      return fail(c, "not_found", `run ${out.runId} ended before the message could route`, 404);
  }
  const runId = entry.runId;

  // Only project what THIS chat produces: snapshot the tail before routing the message in.
  const startCursor = await currentMaxSeq(deps.db, runId);
  // Route the message into the child's stdin (F10 step 2). control.chat never throws (best-effort — a
  // dead child is the reap path's concern); the tail below then surfaces the child's response or its death.
  await entry.supervised.control.chat(userMessage);

  const signal = c.req.raw.signal;
  const pollMs = deps.pollMs && deps.pollMs > 0 ? deps.pollMs : DEFAULT_STREAM_POLL_MS;
  const stream = createUIMessageStream({
    execute: ({ writer }) =>
      streamSession(writer, { db: deps.db, runId, startCursor, pollMs, signal }),
    // Surface the message; the SDK default masks it to avoid leaking server internals, but this is a
    // self-hosted runtime and an honest error is the point (a masked "An error occurred" hides the cause).
    onError: (err) => (err instanceof Error ? err.message : "chat stream error"),
  });
  return createUIMessageStreamResponse({
    stream,
    headers: { [MAESTRO_PROTOCOL_HEADER]: String(MAESTRO_PROTOCOL_VERSION) },
  });
}

/**
 * Register the F10 chat route on `app`. Called by `createApp` under the same `if (index)` gate as the
 * reads/stream routes (the tail needs the open index); when dispatch is not mounted the handler returns a
 * typed `unsupported_intent` 501 rather than 404, so a client can tell "no model loop" from "no route".
 */
export function registerChatRoutes(app: Hono, deps: ChatDeps): void {
  app.post("/api/sessions/:id/chat", (c) => handleChat(c, deps));
}
