// The chat transport (BRO-1826 M4, slice A) — the ONE real ChatTransport the canon-map names
// (handoff/.../docs/canon-map.md: the prototype's Bv*Transport are "mocks a real backend replaces, same
// ChatTransport interface"). It speaks the runtime F10 chat endpoint (POST /api/sessions/:id/chat, API §Chat
// / FLOWS F10): send the transcript, receive the AI SDK v6 UI Message Stream, yield its chunks.
//
// It ONLY yields chunks — folding is the client's single pure reducer `bvApplyChunk` (PATTERNS §9: "no
// component folds chunks ad hoc"). The React binding `useBvChat` (slice B) drives it: append the user turn,
// iterate `stream([...messages, user])`, fold each chunk into feature-local transcript state. So the
// transport carries NO transcript state — chat is a projection, not a store (the M4 verify criterion).
//
// The wire is adopted, not invented: the runtime emits `createUIMessageStreamResponse` (ai@6) framing —
// SSE `data: <json>\n\n` lines + a terminal `data: [DONE]`, header `x-vercel-ai-ui-message-stream: v1`
// (chat-transport.md §1). This parses that framing into the reducer's `StreamChunk` input contract; an
// unrecognized variant folds as a no-op (the reducer's forward-compat rule), so the transport does not
// re-declare ai's full chunk union — it hands raw parsed frames to the reducer.

import {
  CHAT_ENDPOINT,
  type ChatMessage,
  type ErrorResponse,
  type StreamChunk,
} from "@maestro/protocol";

/**
 * A chat transport — yields the assistant's reply to `messages` as UI Message Stream chunks. The LAST
 * message is the just-sent user turn (`useBvChat` appends it before calling). Mirrors the prototype's
 * `BvChatTransport.stream(messages)` contract (canon), so a mock double and the real runtime transport are
 * interchangeable behind this one interface.
 */
export interface ChatTransport {
  stream(
    messages: readonly ChatMessage[],
    opts?: { signal?: AbortSignal },
  ): AsyncGenerator<StreamChunk, void, unknown>;
}

/** A typed chat HTTP failure (a non-2xx from the endpoint) — carries the API §4 error code + status so the
 *  hook can render an honest, specific message (e.g. 501 `unsupported_intent` → "chat is unavailable"). */
export class ChatTransportError extends Error {
  readonly status: number;
  readonly code?: string;
  readonly retryable: boolean;
  constructor(message: string, status: number, code?: string, retryable = false) {
    super(message);
    this.name = "ChatTransportError";
    this.status = status;
    this.code = code;
    this.retryable = retryable;
  }
}

/** The subset of `fetch` the transport uses — just the call signature (not the static `preconnect` etc.),
 *  so a plain function is a valid test double. The global `fetch` satisfies it. */
export type FetchLike = (input: string | URL | Request, init?: RequestInit) => Promise<Response>;

export interface RuntimeChatTransportOptions {
  /** The session/node id addressed by the endpoint path. */
  sessionId: string;
  /** Runtime origin, e.g. `http://localhost:4319`. Default `""` (same origin; the vite proxy forwards /api). */
  baseUrl?: string;
  /** Injected `fetch` (default the global) — the test seam. */
  fetchImpl?: FetchLike;
}

/** The `[DONE]` sentinel a UI Message Stream terminates with — distinct from a `null` (ignored) line. */
const DONE = Symbol("ui-message-stream-done");

/**
 * Parse one SSE line into a chunk. Returns the parsed `StreamChunk`, `null` for a line that is not stream
 * content (blank lines, `:` heartbeat comments, an unparseable frame — tolerated, never fatal), or `DONE`
 * for the terminal `data: [DONE]`. The reducer treats an unknown-typed chunk as a no-op, so a forward-compat
 * variant flows through untouched (chat-transport.md §1).
 */
function parseDataLine(line: string): StreamChunk | null | typeof DONE {
  const trimmed = line.trim();
  if (!trimmed.startsWith("data:")) return null; // blank line / `: hb` comment / `event:` field
  const payload = trimmed.slice(5).trim();
  if (payload === "") return null;
  if (payload === "[DONE]") return DONE;
  try {
    return JSON.parse(payload) as StreamChunk;
  } catch {
    return null; // a malformed frame must not tear down the stream
  }
}

/**
 * Read a UI Message Stream response body as `StreamChunk`s. Buffers across network chunks so a frame split
 * mid-line still parses; stops on `[DONE]`, on an abort (clean return, not a throw), or on body end. Cancels
 * the reader on any exit so an aborted stream never leaks the connection. A genuine read error (not an
 * abort) propagates so the caller can surface it. Exported for direct unit testing.
 */
export async function* parseUiMessageStream(
  body: ReadableStream<Uint8Array>,
  signal?: AbortSignal,
): AsyncGenerator<StreamChunk, void, unknown> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buf = "";
  try {
    while (true) {
      if (signal?.aborted) return;
      let done: boolean;
      let value: Uint8Array | undefined;
      try {
        ({ done, value } = await reader.read());
      } catch (err) {
        // An abort surfaces as a rejected read — a clean stop, not an error to bubble.
        if (signal?.aborted || (err as { name?: string })?.name === "AbortError") return;
        throw err;
      }
      if (done) break;
      buf += decoder.decode(value, { stream: true });
      let nl: number;
      // biome-ignore lint/suspicious/noAssignInExpressions: idiomatic line-buffer drain
      while ((nl = buf.indexOf("\n")) >= 0) {
        // Honor abort BETWEEN buffered frames, not only between network reads. One read routinely
        // coalesces many frames (Bun+Hono emits tool call/result back-to-back; loopback TCP merges them),
        // so without this an abort after folding frame N still yields every remaining buffered frame — the
        // stop verb would be a no-op for a short reply (P20 slice-A MAJOR).
        if (signal?.aborted) return;
        const parsed = parseDataLine(buf.slice(0, nl));
        buf = buf.slice(nl + 1);
        if (parsed === DONE) return;
        if (parsed !== null) yield parsed;
      }
    }
    // Flush a trailing complete frame with no final newline.
    const parsed = parseDataLine(buf);
    if (parsed !== null && parsed !== DONE) yield parsed;
  } finally {
    try {
      await reader.cancel();
    } catch {
      // already closed — nothing to release
    }
  }
}

/** Build a typed error from a non-OK chat response (API §4 `ErrorResponse` envelope, best-effort). */
async function chatHttpError(res: Response): Promise<ChatTransportError> {
  let message = `chat request failed (${res.status})`;
  let code: string | undefined;
  let retryable = false;
  try {
    const body = (await res.json()) as ErrorResponse;
    if (body?.error) {
      if (typeof body.error.message === "string") message = body.error.message;
      code = body.error.code;
      retryable = body.error.retryable ?? false;
    }
  } catch {
    // non-JSON error body — keep the status-derived message
  }
  return new ChatTransportError(message, res.status, code, retryable);
}

/**
 * The real runtime chat transport — POSTs the transcript to the F10 endpoint and streams the reply back.
 * Idle-node addressing (dispatch-then-chat) is the endpoint's concern (FLOWS F10.2); the client just posts
 * to the session/node id.
 */
export class RuntimeChatTransport implements ChatTransport {
  readonly #sessionId: string;
  readonly #baseUrl: string;
  readonly #fetch: FetchLike;

  constructor(opts: RuntimeChatTransportOptions) {
    this.#sessionId = opts.sessionId;
    this.#baseUrl = opts.baseUrl ?? "";
    // Bind the default to the global: native `fetch` requires `this === window`, and calling it as a
    // private field (`this.#fetch(...)`) would pass `this === undefined` → the browser throws
    // "Failed to execute 'fetch' on 'Window': Illegal invocation". An injected `fetchImpl` (a plain
    // function test double) doesn't care about `this`, so it's unaffected. This only bites the real
    // browser path — never exercised until a live-browser E2E drove the un-injected transport (BRO-1827).
    this.#fetch = opts.fetchImpl ?? globalThis.fetch.bind(globalThis);
  }

  async *stream(
    messages: readonly ChatMessage[],
    opts?: { signal?: AbortSignal },
  ): AsyncGenerator<StreamChunk, void, unknown> {
    // Abort is a clean stop everywhere, including the request window: a pre-aborted signal or an abort
    // in-flight during the POST returns without yielding, matching the body-read path (so a consumer that
    // stops during the F10 dispatch-then-chat latency never sees an uncaught AbortError).
    if (opts?.signal?.aborted) return;
    const path = CHAT_ENDPOINT.replace(":id", encodeURIComponent(this.#sessionId));
    let res: Response;
    try {
      res = await this.#fetch(`${this.#baseUrl}${path}`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ messages }),
        signal: opts?.signal,
      });
    } catch (err) {
      if (opts?.signal?.aborted || (err as { name?: string })?.name === "AbortError") return;
      throw err;
    }
    if (!res.ok) throw await chatHttpError(res);
    if (!res.body) return; // no stream body (shouldn't happen for a 200) — a clean, empty reply
    yield* parseUiMessageStream(res.body, opts?.signal);
  }
}
