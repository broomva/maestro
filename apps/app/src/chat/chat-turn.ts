// chat-turn (BRO-1826 M4, slice B) — the PURE heart of a chat turn, extracted from the React hook so
// the fold + status machine + transient-data policy are unit-testable WITHOUT a DOM (the repo tests
// React via renderToStaticMarkup + pure calls; interaction is dogfooded in Playwright). `useBvChat` is a
// thin React wrapper that drives this generator and pushes each step into feature-local state.
//
// It composes the two already-tested pure units: `transport.stream` (slice A) yields UI Message Stream
// chunks; `bvApplyChunk` (protocol) folds them into the transcript. This adds only the turn contract:
// append the user turn (submitted) → fold each non-transient chunk (streaming) → settle (ready). Chat is
// a projection — this holds no state; the caller owns the transcript.

import { bvApplyChunk, type ChatMessage, type StreamChunk } from "@maestro/protocol";
import type { ChatTransport } from "./transport";

/** The lifecycle of a send, mirroring ai's `useChat` status (canon `AiProtocol.jsx` useBvChat). */
export type ChatStatus = "ready" | "submitted" | "streaming" | "error";

/** A transient `data-${name}` chunk — surfaced to `onData` and NOT folded into the transcript (it is
 *  ephemeral gen-UI signal, e.g. a live tick, not conversation state). Structural, mirrors the reducer. */
export interface TransientDataChunk {
  type: `data-${string}`;
  transient: true;
  data?: unknown;
}

/** True for a transient data chunk (a `data-*` chunk flagged `transient`). `.startsWith` doesn't narrow
 *  a template-literal type, so this is the structural guard the fold loop routes on. */
export function isTransientData(chunk: StreamChunk): chunk is TransientDataChunk {
  return chunk.type.startsWith("data-") && (chunk as { transient?: boolean }).transient === true;
}

/** One observable step of a turn: the transcript so far + the status to show. */
export interface ChatTurnStep {
  messages: readonly ChatMessage[];
  status: ChatStatus;
}

/**
 * Settle any still-"streaming" text/reasoning part of the LAST assistant message to "done". On a clean
 * finish the reducer already flipped them (text-end / reasoning-end arrived); on an ABORT — the stop verb,
 * or a truncated stream — those end chunks never arrive, so the trailing part would keep its blinking
 * caret forever, falsely signalling "still typing" after the turn is settled (P20 slice-B round-2 MAJOR).
 * Pure; returns the SAME reference when nothing was streaming (reference-stability for React). Only bare
 * "streaming" parts (text/reasoning) are touched — a tool part uses "input-streaming"/… and an interrupted
 * tool honestly stays mid-state, so it is left alone.
 */
export function finalizeStreamingParts(messages: readonly ChatMessage[]): readonly ChatMessage[] {
  const li = messages.length - 1;
  const last = messages[li];
  if (!last || last.role !== "assistant") return messages;
  if (!last.parts.some((p) => p.state === "streaming")) return messages;
  const next = messages.slice();
  next[li] = {
    ...last,
    parts: last.parts.map((p) => (p.state === "streaming" ? { ...p, state: "done" } : p)),
  };
  return next;
}

/** Options for a turn — the abort signal (the stop verb) and the transient-data sink. */
export interface RunChatTurnOptions {
  signal?: AbortSignal;
  onData?: (chunk: TransientDataChunk) => void;
}

/**
 * Drive one chat turn as a pure async generator of `{messages, status}` steps. The caller (`useBvChat`)
 * applies each step to React state, so the order of yields IS the status timeline the UI shows:
 *
 *   1. `submitted` with `[...history, user]` — the user turn appears instantly, before any network work.
 *   2. `streaming` on every folded chunk — the assistant message grows part by part.
 *   3. `ready` once the stream ends (clean end OR abort — an aborted stream returns cleanly from slice A).
 *
 * A transient data chunk is handed to `onData` and skipped (never folded). A genuine transport error
 * (e.g. `ChatTransportError`) is NOT caught here — it propagates so the hook can decide the error policy
 * (render an error part, set `error` status). `submitted` is always yielded first, so a turn that fails
 * on the very first read still shows the user's message.
 */
export async function* runChatTurn(
  transport: ChatTransport,
  history: readonly ChatMessage[],
  user: ChatMessage,
  opts: RunChatTurnOptions = {},
): AsyncGenerator<ChatTurnStep, void, unknown> {
  let messages: readonly ChatMessage[] = [...history, user];
  yield { messages, status: "submitted" };
  for await (const chunk of transport.stream(messages, { signal: opts.signal })) {
    if (isTransientData(chunk)) {
      opts.onData?.(chunk);
      continue;
    }
    messages = bvApplyChunk(messages, chunk);
    yield { messages, status: "streaming" };
  }
  // On loop exit — clean finish OR an abort that dropped the trailing text-end — settle any part left
  // "streaming" so the caret stops (P20 round-2 MAJOR). Idempotent when the stream ended normally.
  yield { messages: finalizeStreamingParts(messages), status: "ready" };
}
