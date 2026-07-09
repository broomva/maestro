// Chat transport — Maestro's delta over the AI SDK v6 UI Message Stream.
//
// Chat is a PROJECTION of a session, never the owner of work (data-contract
// §"The work model", FLOWS F10: "closing the tab loses nothing").
//
// ARCHITECTURE (the correction that makes this seam swappable for real):
// Maestro does NOT invent a chat wire. It ADOPTS the Vercel AI SDK v6 UI Message
// Stream wholesale — the same protocol `@ai-sdk/react`'s `useChat` consumes and the
// runtime produces via the SDK's UI-message-stream helpers. The one swappable joint
// is therefore the SDK's OWN `ChatTransport`
// (`sendMessages(options) => Promise<ReadableStream<UIMessageChunk>>` +
// `reconnectToStream`), not a Maestro interface: the prototype's mock transports
// become SDK-shaped test doubles, and any transport satisfying ai's `ChatTransport`
// plugs into `useChat` unchanged.
//
// So this file declares ONLY Maestro's delta over that third-party protocol:
//   • the custom `data-*` part PAYLOADS Maestro streams (`data-tick`; `data-gate`
//     is owned by the gate-queue seam, BRO-1789) and the `MaestroDataParts` map that
//     parameterizes ai's generic `UIMessage<METADATA, DATA_TYPES>`;
//   • the wire constants (headers, protocol + SDK version pins, endpoint);
//   • the harness stdin control line (`ChatControlMessage`) that carries a UIMessage
//     client → runtime → child.
//
// It deliberately does NOT re-declare `UIMessage` / `UIMessageChunk` /
// `ChatTransport` / `ToolUIPart`: those are ai's types, imported directly by
// apps/runtime and apps/app (both depend on `ai`). Re-declaring ai's ~25-variant
// generic chunk union by hand is the drift trap this seam was REWRITTEN to avoid —
// a hand-mirror silently omits variants (the first draft omitted `tool-output-error`,
// so a failed tool call could not even be represented, and pinned the wrong transport
// shape). Adopting a versioned third-party protocol wholesale (pinned by AI_SDK_MAJOR
// + a type-level conformance test in apps/app where `ai` is present) is the opposite
// of drift: there is nothing mirrored to drift from.
//
// PATTERNS §10 ("no wire type describing the wire is declared outside this package")
// is honored: every wire type MAESTRO declares — the tick payload, the data-part
// map, the control line, the constants — lives here. The AI SDK UI Message Stream is
// not a Maestro-declared type; it is a dependency, vendored in from `ai`.
//
// Canon: data-contract §"The wire protocol", API.md §Chat + §Versioning, FLOWS F10,
// specs/HARNESS.md §2 (the child stdin `chat` line carries a UIMessage),
// canon-amendments D-NAME (x-maestro-protocol).

// ── Wire constants ───────────────────────────────────────────────────────────

/** The SSE header the runtime sets on a chat response (the AI SDK UI-message-stream marker). */
export const UI_MESSAGE_STREAM_HEADER = "x-vercel-ai-ui-message-stream" as const;
/** The UI Message Stream Protocol version literal — the real compatibility anchor. */
export const UI_MESSAGE_STREAM_VERSION = "v1" as const;
/** The Maestro protocol header carried on every request/stream; the relay passes it through (D-NAME). */
export const MAESTRO_PROTOCOL_HEADER = "x-maestro-protocol" as const;
/**
 * The AI SDK v6 major whose UI Message Stream Maestro adopts as its chat wire. The
 * runtime's stream producer and the client's `useChat` MUST share it — the child's
 * reader + the client's sender agree on the stream shape via this pin (HARNESS §2).
 */
export const AI_SDK_MAJOR = 6 as const;
/**
 * The `@ai-sdk/react` major that ships `useChat` for AI SDK v6. Pinned SEPARATELY
 * because the React binding versions independently of core `ai`: v6 core pairs with
 * react-binding v3 — NOT v2 (v2 is the ai@5 hook, and mis-pairing yields a `useChat`
 * whose transport contract does not match this wire).
 */
export const AI_SDK_REACT_MAJOR = 3 as const;
/** The session-addressed chat endpoint (API.md §Chat). */
export const CHAT_ENDPOINT = "/api/sessions/:id/chat" as const;

// ── Maestro custom data parts (the delta over ai's UIMessage) ─────────────────
//
// ai's `UIMessage<METADATA, DATA_TYPES>` is generic over a DATA_TYPES map; each key
// NAME yields a `data-${NAME}` part carrying that payload (ai's `DataUIPart`). Maestro's
// map is `MaestroDataParts`; apps/app composes `UIMessage<MaestroMetadata,
// MaestroDataParts>` (where `ai` is present). This file owns the PAYLOAD types + the map.

/** The `data-tick` part NAME — ai keys the DATA_TYPES map by the bare name; the part `type` is `data-<name>`. */
export const DATA_TICK_NAME = "tick" as const;
/** The full part `type` string for the tick — `data-tick` (what a guard / renderer matches). */
export const DATA_TICK_PART = "data-tick" as const;
/** The stable part `id` the orchestrator wake receipt always uses — re-sends update it in place (FLOWS F6.5). */
export const DATA_TICK_ID = "tick-log" as const;

/** One row of the orchestrator wake log (data-contract §"The tick"). */
export interface TickRow {
  g: string;
  cause: string;
  causeColor?: string;
  label: string;
  t: string;
}

/** The `data-tick` payload — carried at id `DATA_TICK_ID`; re-sends update the card in place (F6.5). */
export interface TickReceipt {
  rows: TickRow[];
}

/**
 * Maestro's DATA_TYPES map for ai's generic `UIMessage<METADATA, DATA_TYPES>`. Each
 * key NAME becomes a `data-${NAME}` part. `tick` is owned here; the gate-queue seam
 * (BRO-1789) adds a `gate: GateCard` member to THIS interface — extending the single
 * canonical map rather than re-declaring a data part elsewhere, so data-part ownership
 * stays single-sourced (this seam leaves the gate payload out, not generic-typed).
 */
export interface MaestroDataParts {
  tick: TickReceipt;
}

/**
 * The tick as an ai `DataUIPart`: `{ type: "data-tick"; id?; data: TickReceipt }`. A
 * Maestro-owned NARROWING of ai's data part for the one part this seam types — not a
 * re-declaration of ai's part union.
 */
export interface TickDataPart {
  type: typeof DATA_TICK_PART;
  id?: string;
  data: TickReceipt;
}

// ── Harness stdin control line (cross-dep HARNESS §2) ─────────────────────────
//
// The same UIMessage travels client → runtime (HTTP) → supervisor → child stdin as an
// NDJSON control line. HARNESS §2 owns the full control union (`chat` / `stop` / `ping`);
// this seam contributes the `chat` variant. Its `message` is an AI SDK `UIMessage`
// (ai's type, resolved on the runtime side); protocol types it against the MINIMAL
// structural envelope the control line needs — id / role / parts — NOT a re-declaration
// of ai's full part union. The runtime narrows `parts` to ai's `UIMessagePart[]`.

/** The minimal UIMessage envelope the harness control line needs; ai's `UIMessage` satisfies it structurally. */
export interface UIMessageEnvelope {
  id: string;
  role: "user" | "assistant" | "system";
  parts: unknown[];
}

/** The `chat` harness control line (HARNESS §2) — carries a UIMessage to the child's stdin. */
export interface ChatControlMessage {
  type: "chat";
  message: UIMessageEnvelope;
}

// ── Guards ────────────────────────────────────────────────────────────────────

/**
 * True for the orchestrator wake-log part (the one data part this seam types).
 * Operates on ai's `UIMessagePart` structurally (`{ type: string }`) so it needs no
 * `ai` import; narrows to Maestro's `TickDataPart`.
 */
export const isTickDataPart = (part: { type: string }): part is TickDataPart =>
  part.type === DATA_TICK_PART;
