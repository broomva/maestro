// fixture-transport (BRO-1826 M4, slice B) — a ChatTransport that replays a recorded mock-model turn
// WITHOUT a backend. It exists so the feed's done.check (chat-m4.pw.ts) runs in the static playwright
// harness (vite preview serves the built SPA, no runtime), and so the session route has a deterministic
// demo when reached with `?fixture=1`. It is INERT unless explicitly selected — production always uses
// the real `RuntimeChatTransport`. Mirrors the F10 chunk order (chat.ts streamSession): start →
// reasoning → one tool → text → finish, with a small per-chunk delay so streaming states are observable.

import type { ChatMessage, StreamChunk } from "@maestro/protocol";
import type { ChatTransport } from "./transport";

/** The recorded reply — one run that reasons, runs a shell tool, then answers with an inline run ref
 *  (a backtick span → the link pill). Kept short + deterministic for a fast, stable test. */
const FIXTURE_CHUNKS: StreamChunk[] = [
  { type: "start", messageId: "r-fixture" },
  { type: "reasoning-start", id: "re" },
  { type: "reasoning-delta", id: "re", delta: "Checking the workspace before I answer." },
  { type: "reasoning-end", id: "re" },
  { type: "tool-input-start", toolCallId: "call-1", toolName: "shell" },
  {
    type: "tool-input-available",
    toolCallId: "call-1",
    toolName: "shell",
    input: { command: "ls" },
  },
  { type: "tool-output-available", toolCallId: "call-1", output: "plan.md\nnotes.md" },
  { type: "text-start", id: "t1" },
  { type: "text-delta", id: "t1", delta: "Listed the files. " },
  { type: "text-delta", id: "t1", delta: "Judged `run/7c2f1a` clean and " },
  { type: "text-delta", id: "t1", delta: "queued it to your gate." },
  { type: "text-end", id: "t1" },
  { type: "finish" },
];

/** Per-chunk delay (ms) — long enough that submitted → streaming → ready is observable, short enough
 *  the whole turn settles well inside a test timeout. */
const STEP_MS = 35;

const wait = (ms: number, signal?: AbortSignal): Promise<void> =>
  new Promise((resolve) => {
    if (signal?.aborted) return resolve();
    const id = setTimeout(resolve, ms);
    signal?.addEventListener("abort", () => {
      clearTimeout(id);
      resolve();
    });
  });

/**
 * The fixture transport. Ignores the sent messages (it always replays the same reply) and honors the
 * abort signal at every step (the stop verb works against it exactly like the real transport). The
 * per-chunk delay is configurable so a test can widen the streaming window to click Stop deterministically
 * mid-stream (the abort test) without slowing the default fast replay.
 */
export class FixtureChatTransport implements ChatTransport {
  readonly #stepMs: number;
  constructor(stepMs: number = STEP_MS) {
    this.#stepMs = stepMs;
  }
  async *stream(
    _messages: readonly ChatMessage[],
    opts?: { signal?: AbortSignal },
  ): AsyncGenerator<StreamChunk, void, unknown> {
    for (const chunk of FIXTURE_CHUNKS) {
      if (opts?.signal?.aborted) return;
      await wait(this.#stepMs, opts?.signal);
      if (opts?.signal?.aborted) return;
      yield chunk;
    }
  }
}

/** True when the current URL opts into the fixture transport (`?fixture=1`) — the demo/test seam,
 *  inert otherwise. Mirrors the router's `?crash` probe convention (BRO-1824). */
export function fixtureRequested(): boolean {
  if (typeof window === "undefined") return false;
  return new URLSearchParams(window.location.search).has("fixture");
}

/** Optional per-chunk delay (ms) from `?step=<n>` — lets a test widen the streaming window (to click Stop
 *  mid-stream). Undefined (the default) → the fast STEP_MS. Ignored outside fixture mode. */
export function fixtureStepMs(): number | undefined {
  if (typeof window === "undefined") return undefined;
  const raw = new URLSearchParams(window.location.search).get("step");
  if (raw === null) return undefined;
  const n = Number(raw);
  return Number.isFinite(n) && n >= 0 ? n : undefined;
}
