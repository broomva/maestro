// The stream-folding reducer (PATTERNS §9, data-contract §"The wire protocol", BRO-1819) — ported
// AS-IS from the prototype's `bvApplyChunk` (handoff/apps/maestro/AiProtocol.jsx). This is the client's
// ONLY stream-state logic: pure, testable, shared by every projection (PATTERNS §9 "no component folds
// chunks ad hoc; new part types extend the reducer"). It folds an AI-SDK v6 UI-Message-Stream chunk into
// the running `UIMessage[]` transcript.
//
// ZERO-DEP, structurally typed (chat-transport.md §1, §4): `packages/protocol` never imports `ai`, so
// this operates on MINIMAL STRUCTURAL shapes — a `ChatMessage`/`ChatPart` container and a `StreamChunk`
// input contract covering ONLY the variants this fold handles. This is NOT a re-declaration of ai's full
// ~25-variant `UIMessageChunk` union (the §1 drift trap): an unhandled variant is a clean no-op (the
// reducer simply doesn't fold it until extended — PATTERNS §9). `apps/app` does not yet depend on `ai`;
// when it does (BRO-1782/1826, the chat wiring), it WILL carry the type-level conformance test asserting
// ai's real `UIMessage`/`UIMessageChunk` are assignable to these shapes (§9) — that test is the guard,
// not this comment. Pinned to the v6 wire by `AI_SDK_MAJOR` (chat.ts).
//
// PURITY: the reducer is a pure `(state, chunk) → state` with no ambient clock — the only id it ever has
// to synthesize (a `start` chunk that omits `messageId`, which the runtime normally sets) is derived
// deterministically from the transcript length, not the prototype's `Date.now()`-based `bvUid`. This is
// the one deliberate deviation from the AS-IS port, required to keep the fold pure + deterministically
// testable (the no-ambient-clock discipline the rest of the runtime follows).

// ── The structural transcript shapes (ai's `UIMessage`/`UIMessagePart` satisfy these) ──

/** A part of a message — the structural subset the fold reads/writes. ai's `UIMessagePart` union members
 *  (text / reasoning / `tool-${name}` / `data-${name}` / error) all satisfy this. */
export interface ChatPart {
  /** `text` · `reasoning` · `tool-${name}` · `data-${name}` · `error`. */
  type: string;
  /** Block id for text/reasoning/data parts (data parts reconcile by `type`+`id`). */
  id?: string;
  /** Tool-call id for `tool-${name}` parts (reconciled by `toolCallId`). */
  toolCallId?: string;
  text?: string;
  state?: string;
  inputText?: string;
  input?: unknown;
  output?: unknown;
  data?: unknown;
  errorText?: string;
}

/** A message in the transcript — ai's `UIMessage` satisfies this structurally. */
export interface ChatMessage {
  id: string;
  role: "user" | "assistant" | "system";
  metadata?: unknown;
  parts: ChatPart[];
}

// ── The reducer input contract — the SUBSET of ai's `UIMessageChunk` this fold handles ──
// NOT a mirror of ai's full v6 union (chat-transport.md §1/§4 drift trap): only the variants the reducer
// folds are named; anything else no-ops. apps/app's conformance test guards assignability from ai's type.

/** A `data-${name}` part chunk (gen-UI). `transient` bypasses the transcript (surfaced via onData). */
export interface DataChunk {
  type: `data-${string}`;
  id?: string;
  data?: unknown;
  transient?: boolean;
}

/** The lifecycle + content chunks the fold recognizes (the closed subset — see the module note). */
export type StreamChunk =
  | { type: "start"; messageId?: string; messageMetadata?: unknown }
  | { type: "finish" | "abort" | "start-step" | "finish-step" | "message-metadata" }
  | { type: "text-start"; id: string }
  | { type: "text-delta"; id: string; delta: string }
  | { type: "text-end"; id: string }
  | { type: "reasoning-start"; id: string }
  | { type: "reasoning-delta"; id: string; delta: string }
  | { type: "reasoning-end"; id: string }
  | { type: "tool-input-start"; toolCallId: string; toolName: string }
  | { type: "tool-input-delta"; toolCallId: string; inputTextDelta: string }
  | { type: "tool-input-available"; toolCallId: string; toolName?: string; input: unknown }
  | { type: "tool-input-error"; toolCallId: string; errorText: string }
  | { type: "tool-output-available"; toolCallId: string; output: unknown }
  | { type: "tool-output-error"; toolCallId: string; errorText: string }
  | { type: "error"; errorText: string }
  | DataChunk;

/** True for a `data-${name}` chunk (structural — `.startsWith` doesn't narrow a template-literal type). */
function isDataChunk(chunk: StreamChunk): chunk is DataChunk {
  return chunk.type.startsWith("data-");
}

/**
 * Fold one UI-Message-Stream chunk into the transcript — pure `(state, chunk) → state` (returns a new
 * array on any change; the input is never mutated). Ported AS-IS from the prototype `bvApplyChunk`, plus
 * `tool-input-error` / `tool-output-error` handling (chat-transport.md §4 — a failed tool flips to the
 * `output-error`/`input-error` state carrying `errorText`, so it renders as failed rather than hanging
 * at "running"; this closes the §1 drift-trap gap the prototype predated).
 */
export function bvApplyChunk(
  state: readonly ChatMessage[],
  chunk: StreamChunk,
): readonly ChatMessage[] {
  const li = state.length - 1;

  // ── No-op chunks return the SAME array reference ─────────────────────────────
  // Reference stability on no-ops is the load-bearing property of a reducer feeding React: a memoized
  // consumer (a zustand selector, a React.memo'd list) must NOT re-render on the FREQUENT chunks that
  // change nothing — the lifecycle framing (finish, step markers, message-metadata), a transient data
  // chunk (surfaced via onData, never persisted), or any content/data chunk before the first `start`
  // (nothing to attach to). Cloning upfront (as the prototype did) would churn a new array every chunk.
  switch (chunk.type) {
    case "finish":
    case "abort":
    case "start-step":
    case "finish-step":
    case "message-metadata":
      return state;
  }
  if (isDataChunk(chunk) && chunk.transient) return state;
  if (chunk.type !== "start" && li < 0) return state;

  // ── Mutating paths: clone (copy-on-write; the input is never mutated) ────────
  const msgs = state.slice();
  /** Clone message `i` (and its parts array) for an in-place edit — `i` is always in range here. */
  const touch = (i: number): ChatMessage => {
    const cur = msgs[i];
    if (cur === undefined) throw new RangeError(`bvApplyChunk.touch(${i}): out of range`);
    const m: ChatMessage = { ...cur, parts: cur.parts.slice() };
    msgs[i] = m;
    return m;
  };
  /** Replace part `j` of `m` via `fn` — guarded, so a not-found index (`-1`) is a clean no-op. */
  const updatePart = (m: ChatMessage, j: number, fn: (p: ChatPart) => ChatPart): void => {
    const part = m.parts[j];
    if (part !== undefined) m.parts[j] = fn(part);
  };

  // start → open a new assistant message. A missing messageId falls back to a DETERMINISTIC id derived
  // from the transcript length (pure; the runtime normally supplies messageId).
  if (chunk.type === "start") {
    msgs.push({
      id: chunk.messageId ?? `msg-${msgs.length}`,
      role: "assistant",
      metadata: chunk.messageMetadata,
      parts: [],
    });
    return msgs;
  }

  // data-${name} — gen-UI parts reconciled across the WHOLE transcript by (type, id). A re-send at the
  // same id updates the card IN PLACE (data-tick at "tick-log" F6.5; data-gate at its gateId F5). A
  // pre-start / transient data chunk already returned above.
  if (isDataChunk(chunk)) {
    for (let i = 0; i < msgs.length; i++) {
      const parts = msgs[i]?.parts ?? [];
      const j = parts.findIndex(
        (p) => p.type === chunk.type && chunk.id != null && p.id === chunk.id,
      );
      if (j >= 0) {
        const m = touch(i);
        updatePart(m, j, (p) => ({ ...p, data: chunk.data }));
        return msgs;
      }
    }
    const m = touch(li);
    m.parts.push({ type: chunk.type, id: chunk.id, data: chunk.data });
    return msgs;
  }

  // Everything else appends to / updates the LAST message's parts (li >= 0 guaranteed above).
  const m = touch(li);
  const findBlock = (type: string, id: string): number =>
    m.parts.findIndex((p) => p.type === type && p.id === id);
  const findCall = (id: string): number => m.parts.findIndex((p) => p.toolCallId === id);

  switch (chunk.type) {
    case "text-start":
      m.parts.push({ type: "text", id: chunk.id, text: "", state: "streaming" });
      break;
    case "text-delta":
      updatePart(m, findBlock("text", chunk.id), (p) => ({
        ...p,
        text: (p.text ?? "") + chunk.delta,
      }));
      break;
    case "text-end":
      updatePart(m, findBlock("text", chunk.id), (p) => ({ ...p, state: "done" }));
      break;
    case "reasoning-start":
      m.parts.push({ type: "reasoning", id: chunk.id, text: "", state: "streaming" });
      break;
    case "reasoning-delta":
      updatePart(m, findBlock("reasoning", chunk.id), (p) => ({
        ...p,
        text: (p.text ?? "") + chunk.delta,
      }));
      break;
    case "reasoning-end":
      updatePart(m, findBlock("reasoning", chunk.id), (p) => ({ ...p, state: "done" }));
      break;
    case "tool-input-start":
      m.parts.push({
        type: `tool-${chunk.toolName}`,
        toolCallId: chunk.toolCallId,
        state: "input-streaming",
        inputText: "",
      });
      break;
    case "tool-input-delta":
      updatePart(m, findCall(chunk.toolCallId), (p) => ({
        ...p,
        inputText: (p.inputText ?? "") + chunk.inputTextDelta,
      }));
      break;
    case "tool-input-available":
      updatePart(m, findCall(chunk.toolCallId), (p) => ({
        ...p,
        state: "input-available",
        input: chunk.input,
      }));
      break;
    case "tool-input-error":
      // A malformed tool call — flip to output-error so it renders failed (chat-transport.md §4).
      updatePart(m, findCall(chunk.toolCallId), (p) => ({
        ...p,
        state: "output-error",
        errorText: chunk.errorText,
      }));
      break;
    case "tool-output-available":
      updatePart(m, findCall(chunk.toolCallId), (p) => ({
        ...p,
        state: "output-available",
        output: chunk.output,
      }));
      break;
    case "tool-output-error":
      // A failed tool call — flip to output-error carrying errorText; it must not hang at "running".
      updatePart(m, findCall(chunk.toolCallId), (p) => ({
        ...p,
        state: "output-error",
        errorText: chunk.errorText,
      }));
      break;
    case "error":
      m.parts.push({ type: "error", errorText: chunk.errorText });
      break;
  }
  return msgs;
}

// ── Selectors — derived UI state from the transcript (pure; ported AS-IS) ──

/** An open gate card, structurally — the fields `bvSelectGate` reads (the full payload is the gate-queue
 *  seam's `GateCard`, BRO-1789; this reads it structurally so the selector stays zero-dep). Exported so
 *  a consumer can name `bvSelectGate`'s element type. */
export interface GateCardLike {
  id?: string;
  resolved?: boolean;
}

/**
 * The open gates in the transcript — every `data-gate` part reconciled by `id` (last write wins), then
 * filtered to the unresolved. This is the "attention" derivation the board/inspector read (F5).
 *
 * DEVIATION from the AS-IS prototype: an id-LESS `data-gate` part is skipped rather than collapsed under
 * a single `undefined` key. This is both a typed-Map necessity (the key is `string`, `ChatPart.id` is
 * `string | undefined`) and more correct — F5 keys every gate by its `gateId`, so an id-less gate is
 * out-of-contract input the reconciliation can't address anyway.
 */
export function bvSelectGate(messages: readonly ChatMessage[]): GateCardLike[] {
  const map = new Map<string, GateCardLike>();
  for (const m of messages) {
    for (const p of m.parts) {
      if (p.type === "data-gate" && p.data != null && p.id != null) {
        // Keyed by the part id (the gateId), so a re-sent gate card overwrites its prior state in place.
        map.set(p.id, p.data as GateCardLike);
      }
    }
  }
  return [...map.values()].filter((g) => g && !g.resolved);
}

/** The most recent user turn's text (joined across its text parts) — the transport's "what did they ask". */
export function bvLastUserText(messages: readonly ChatMessage[]): string {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i] as ChatMessage;
    if (m.role === "user") {
      return m.parts
        .filter((p) => p.type === "text")
        .map((p) => p.text ?? "")
        .join(" ");
    }
  }
  return "";
}
