// Chat transport — the UIMessage wire + the UI Message Stream Protocol.
//
// Chat is a PROJECTION of a session, never the owner of work (data-contract
// §"The work model", FLOWS F10: "closing the tab loses nothing"). The
// `ChatTransport` is the ONE swappable joint between the AI-SDK `useChat` client
// and the backend: the prototype ships three mock transports; the real runtime
// transport replaces them 1:1 behind this same interface (START-HERE §5 seam 1,
// data-contract §"The wire protocol").
//
// This mirrors the Vercel AI SDK v6 `UIMessage` + UI-Message-Stream shapes
// STRUCTURALLY — it does NOT `import … from "ai"`: the wire type lives here so it
// cannot drift, and `@maestro/protocol` stays zero-runtime-dep (PATTERNS §10). The
// concrete payloads of custom `data-*` parts that other seams own are left generic
// here: `data-tick` (the orchestrator wake log) is typed below; the `data-gate`
// card payload is owned by the gate-queue seam (BRO-1789), which types it via
// `DataUIPart<GateCard>` — this file keeps data parts generic so it does not
// pre-empt that ownership.
//
// Canon: data-contract §"The wire protocol", API.md §Chat + §Versioning, FLOWS
// F10, specs/HARNESS.md §2 (the child stdin `chat` line carries this UIMessage),
// canon-amendments D-NAME (x-maestro-protocol).

// ── Wire constants ───────────────────────────────────────────────────────────

/** The SSE header the runtime sets on a chat response (data-contract §wire, API.md §Chat). */
export const UI_MESSAGE_STREAM_HEADER = "x-vercel-ai-ui-message-stream" as const;
/** The UI Message Stream Protocol version literal — the real compatibility anchor. */
export const UI_MESSAGE_STREAM_VERSION = "v1" as const;
/** The Maestro protocol header carried on every request/stream; the relay passes it through (D-NAME). */
export const MAESTRO_PROTOCOL_HEADER = "x-maestro-protocol" as const;
/** The AI SDK major this wire mirrors — the child's reader + the client's sender must share it (§HARNESS §2). */
export const AI_SDK_MAJOR = 6 as const;
/** The session-addressed chat endpoint (API.md §Chat). */
export const CHAT_ENDPOINT = "/api/sessions/:id/chat" as const;
/** The stable id the orchestrator wake receipt always uses — re-sends update it in place (FLOWS F6.5). */
export const DATA_TICK_ID = "tick-log" as const;

// ── UIMessage ────────────────────────────────────────────────────────────────

export type UIMessageRole = "user" | "assistant" | "system";

/** A streaming lifecycle marker on text / reasoning parts (the reducer folds start→delta→end). */
export type PartState = "streaming" | "done";

export interface TextUIPart {
  type: "text";
  id?: string;
  text: string;
  state?: PartState;
}

export interface ReasoningUIPart {
  type: "reasoning";
  id?: string;
  text: string;
  state?: PartState;
}

/** The tool part state ladder: input streams in, then the output arrives. `type` is `tool-${toolName}`. */
export type ToolPartState = "input-streaming" | "input-available" | "output-available";

export interface ToolUIPart {
  type: `tool-${string}`;
  toolCallId: string;
  state: ToolPartState;
  inputText?: string;
  input?: unknown;
  output?: unknown;
}

/** A gen-UI data part. `type` is `data-${name}`; reconciled across the transcript by `id`. */
export interface DataUIPart<T = unknown> {
  type: `data-${string}`;
  id?: string;
  data: T;
}

/** The error part the reducer pushes on an `error` chunk. */
export interface ErrorUIPart {
  type: "error";
  errorText: string;
}

export type UIMessagePart = TextUIPart | ReasoningUIPart | ToolUIPart | DataUIPart | ErrorUIPart;

/** A chat message — the shape sent to the runtime AND rendered from the stream (metadata stays app-narrowed). */
export interface UIMessage<M = unknown> {
  id: string;
  role: UIMessageRole;
  metadata?: M;
  parts: UIMessagePart[];
}

// ── The one product data-part this seam owns: the tick (orchestrator wake log) ─

/** One row of the orchestrator wake log (data-contract §"The tick"). */
export interface TickRow {
  g: string;
  cause: string;
  causeColor?: string;
  label: string;
  t: string;
}

/** The `data-tick` payload — always carried at id `DATA_TICK_ID`; re-sends update the card in place (F6.5). */
export interface TickReceipt {
  rows: TickRow[];
}

export type TickDataPart = DataUIPart<TickReceipt> & { type: "data-tick"; id: typeof DATA_TICK_ID };

// ── The chunk vocabulary (UI Message Stream Protocol) ────────────────────────
//
// The closed set the transport yields and the reducer folds. `finish` / `abort` /
// `start-step` / `finish-step` are lifecycle no-ops in the reducer; a `data-*`
// chunk with `transient: true` bypasses the transcript entirely (surfaced via
// onData, never persisted).

export type UIMessageChunk =
  | { type: "start"; messageId?: string; messageMetadata?: unknown }
  | { type: "text-start"; id: string }
  | { type: "text-delta"; id: string; delta: string }
  | { type: "text-end"; id: string }
  | { type: "reasoning-start"; id: string }
  | { type: "reasoning-delta"; id: string; delta: string }
  | { type: "reasoning-end"; id: string }
  | { type: "tool-input-start"; toolCallId: string; toolName: string }
  | { type: "tool-input-delta"; toolCallId: string; inputTextDelta: string }
  | { type: "tool-input-available"; toolCallId: string; toolName: string; input: unknown }
  | { type: "tool-output-available"; toolCallId: string; output: unknown }
  | { type: `data-${string}`; id?: string; data: unknown; transient?: boolean }
  | { type: "error"; errorText: string }
  | { type: "finish" }
  | { type: "abort" }
  | { type: "start-step" }
  | { type: "finish-step" };

/** The chunk `type` discriminators the reducer handles — the closed vocabulary, as data (drift guard). */
export const UI_MESSAGE_CHUNK_TYPES = [
  "start",
  "text-start",
  "text-delta",
  "text-end",
  "reasoning-start",
  "reasoning-delta",
  "reasoning-end",
  "tool-input-start",
  "tool-input-delta",
  "tool-input-available",
  "tool-output-available",
  "data-*",
  "error",
  "finish",
  "abort",
  "start-step",
  "finish-step",
] as const;

// ── The transport interface (the 1:1 swap seam) ──────────────────────────────

/**
 * The one swappable joint (START-HERE §5 seam 1). The prototype's
 * `BvAnthropicTransport` / `BvOpenAITransport` / `BvHarnessTransport` become test
 * doubles behind THIS shape; the real transport is an AI-SDK HTTP transport that
 * speaks the runtime SSE. Any transport yielding `UIMessageChunk` plugs into
 * `useChat` unchanged, and the pure reducer (`bvApplyChunk`, ported as-is) folds
 * the stream identically regardless of which transport produced it.
 */
export interface ChatTransport {
  stream(messages: UIMessage[]): AsyncIterable<UIMessageChunk>;
}

// ── Child-harness stdin control (cross-dep HARNESS §2) ───────────────────────
//
// The same UIMessage travels client → runtime (HTTP) → supervisor → child stdin as
// an NDJSON control line. HARNESS §2 owns the full control union (`chat`/`stop`/
// `ping`); this seam contributes only the `chat` variant's `message` typing, which
// is exactly the protocol UIMessage (both sides agree because the type lives here).

export interface ChatControlMessage {
  type: "chat";
  message: UIMessage;
}

// ── Guards ───────────────────────────────────────────────────────────────────

/** True for any `data-*` chunk (the gen-UI parts, incl. transient). */
export const isDataChunk = (
  c: UIMessageChunk,
): c is Extract<UIMessageChunk, { type: `data-${string}` }> =>
  typeof c.type === "string" && c.type.startsWith("data-");

/** True for the orchestrator wake-log part (the one data part this seam types). */
export const isTickPart = (p: UIMessagePart): p is TickDataPart => p.type === "data-tick";
