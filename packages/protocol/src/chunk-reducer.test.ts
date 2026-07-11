/// <reference types="bun" />
// chunk-reducer.test.ts — BRO-1819 done.check `bun test packages/protocol --filter reducer`. Recorded
// chunk sequences fold through the pure `bvApplyChunk` reducer; the FULL vocabulary incl. data-tick
// in-place update + data-gate id-reconciliation. Anti-vacuity [[self-hosting-vacuous-pass]]: every case
// asserts the EXACT resulting transcript (parts, ids, states, text) — swap a branch and a test fails.

import { describe, expect, test } from "bun:test";
import {
  bvApplyChunk,
  bvLastUserText,
  bvSelectGate,
  type ChatMessage,
  type StreamChunk,
} from "./chunk-reducer";

/** Fold a whole recorded sequence from an initial transcript. */
function fold(init: ChatMessage[], chunks: StreamChunk[]): ChatMessage[] {
  return chunks.reduce<ChatMessage[]>((s, c) => bvApplyChunk(s, c), init);
}

describe("chunk-reducer — start / lifecycle", () => {
  test("start opens an assistant message with its id + metadata", () => {
    const s = bvApplyChunk([], {
      type: "start",
      messageId: "m1",
      messageMetadata: { model: "opus" },
    });
    expect(s).toEqual([{ id: "m1", role: "assistant", metadata: { model: "opus" }, parts: [] }]);
  });

  test("start without messageId gets a DETERMINISTIC id from transcript length (pure, no clock)", () => {
    let s = bvApplyChunk([], { type: "start" });
    expect(s[0]?.id).toBe("msg-0");
    s = bvApplyChunk(s, { type: "start" });
    expect(s[1]?.id).toBe("msg-1");
  });

  test("finish / abort / start-step / finish-step / message-metadata are no-ops", () => {
    const start: ChatMessage[] = [{ id: "m1", role: "assistant", parts: [] }];
    for (const type of [
      "finish",
      "abort",
      "start-step",
      "finish-step",
      "message-metadata",
    ] as const) {
      expect(bvApplyChunk(start, { type })).toEqual(start);
    }
  });

  test("an unknown/future chunk variant is a clean no-op (forward-compatible)", () => {
    const start: ChatMessage[] = [{ id: "m1", role: "assistant", parts: [] }];
    const weird = { type: "source-url", url: "x" } as unknown as StreamChunk;
    expect(bvApplyChunk(start, weird)).toEqual(start);
  });

  test("a content chunk before any start is dropped (nothing to fold onto)", () => {
    expect(bvApplyChunk([], { type: "text-delta", id: "t", delta: "hi" })).toEqual([]);
  });
});

describe("chunk-reducer — text + reasoning lifecycle", () => {
  test("text-start/delta/delta/end folds one part: streaming→done, deltas concatenated", () => {
    const s = fold(
      [{ id: "m1", role: "assistant", parts: [] }],
      [
        { type: "text-start", id: "t1" },
        { type: "text-delta", id: "t1", delta: "Hel" },
        { type: "text-delta", id: "t1", delta: "lo" },
        { type: "text-end", id: "t1" },
      ],
    );
    expect(s[0]?.parts).toEqual([{ type: "text", id: "t1", text: "Hello", state: "done" }]);
  });

  test("reasoning folds the same way, on its own part", () => {
    const s = fold(
      [{ id: "m1", role: "assistant", parts: [] }],
      [
        { type: "reasoning-start", id: "r1" },
        { type: "reasoning-delta", id: "r1", delta: "think" },
        { type: "reasoning-end", id: "r1" },
      ],
    );
    expect(s[0]?.parts).toEqual([{ type: "reasoning", id: "r1", text: "think", state: "done" }]);
  });
});

describe("chunk-reducer — tool lifecycle (incl. the §4 error path)", () => {
  test("tool input → output folds one tool part through its states", () => {
    const s = fold(
      [{ id: "m1", role: "assistant", parts: [] }],
      [
        { type: "tool-input-start", toolCallId: "c1", toolName: "dispatch" },
        { type: "tool-input-delta", toolCallId: "c1", inputTextDelta: '{"g":' },
        { type: "tool-input-delta", toolCallId: "c1", inputTextDelta: '"x"}' },
        { type: "tool-input-available", toolCallId: "c1", toolName: "dispatch", input: { g: "x" } },
        { type: "tool-output-available", toolCallId: "c1", output: { queued: true } },
      ],
    );
    expect(s[0]?.parts).toEqual([
      {
        type: "tool-dispatch",
        toolCallId: "c1",
        state: "output-available",
        inputText: '{"g":"x"}',
        input: { g: "x" },
        output: { queued: true },
      },
    ]);
  });

  test("tool-output-error flips the part to output-error with errorText (never hangs at running)", () => {
    const s = fold(
      [{ id: "m1", role: "assistant", parts: [] }],
      [
        { type: "tool-input-start", toolCallId: "c1", toolName: "sh" },
        { type: "tool-input-available", toolCallId: "c1", input: { cmd: "boom" } },
        { type: "tool-output-error", toolCallId: "c1", errorText: "exit 1" },
      ],
    );
    expect(s[0]?.parts[0]).toMatchObject({
      type: "tool-sh",
      toolCallId: "c1",
      state: "output-error",
      errorText: "exit 1",
    });
  });

  test("tool-input-error also flips to output-error (malformed call)", () => {
    const s = fold(
      [{ id: "m1", role: "assistant", parts: [] }],
      [
        { type: "tool-input-start", toolCallId: "c1", toolName: "sh" },
        { type: "tool-input-error", toolCallId: "c1", errorText: "bad json" },
      ],
    );
    expect(s[0]?.parts[0]).toMatchObject({ state: "output-error", errorText: "bad json" });
  });
});

describe("chunk-reducer — data parts (gen-UI)", () => {
  test("data-tick reconciles by id: a re-send updates the card IN PLACE (F6.5), not a 2nd part", () => {
    const s = fold(
      [{ id: "m1", role: "assistant", parts: [] }],
      [
        { type: "data-tick", id: "tick-log", data: { rows: [1] } },
        { type: "data-tick", id: "tick-log", data: { rows: [1, 2] } },
      ],
    );
    expect(s[0]?.parts).toEqual([{ type: "data-tick", id: "tick-log", data: { rows: [1, 2] } }]);
  });

  test("a data part reconciles across the WHOLE transcript, not just the last message", () => {
    // tick lands on message m1, then a NEW assistant message opens; a re-send still updates m1's part.
    const s = fold(
      [{ id: "m1", role: "assistant", parts: [] }],
      [
        { type: "data-tick", id: "tick-log", data: { rows: [1] } },
        { type: "start", messageId: "m2" },
        { type: "data-tick", id: "tick-log", data: { rows: [1, 2, 3] } },
      ],
    );
    expect(s[0]?.parts).toEqual([{ type: "data-tick", id: "tick-log", data: { rows: [1, 2, 3] } }]);
    expect(s[1]?.parts).toEqual([]); // the re-send updated m1 in place, did NOT push onto m2
    void s;
  });

  test("a transient data chunk bypasses the transcript (surfaced via onData, never persisted)", () => {
    const start: ChatMessage[] = [{ id: "m1", role: "assistant", parts: [] }];
    const s = bvApplyChunk(start, {
      type: "data-tick",
      id: "tick-log",
      data: { rows: [1] },
      transient: true,
    });
    expect(s[0]?.parts).toEqual([]);
  });
});

describe("chunk-reducer — data-gate id-reconciliation + bvSelectGate", () => {
  test("two gates by id fold as two parts; a re-send updates one in place; bvSelectGate returns the open", () => {
    let s = fold(
      [{ id: "m1", role: "assistant", parts: [] }],
      [
        { type: "data-gate", id: "g1", data: { id: "g1", resolved: false } },
        { type: "data-gate", id: "g2", data: { id: "g2", resolved: false } },
      ],
    );
    expect(s[0]?.parts).toHaveLength(2);
    expect(bvSelectGate(s).map((g) => (g as { id?: string }).id)).toEqual(["g1", "g2"]);

    // Resolve g1 by re-sending its card at the same id → updates in place (still 2 parts).
    s = bvApplyChunk(s, { type: "data-gate", id: "g1", data: { id: "g1", resolved: true } });
    expect(s[0]?.parts).toHaveLength(2);
    expect(bvSelectGate(s).map((g) => (g as { id?: string }).id)).toEqual(["g2"]);
  });
});

describe("chunk-reducer — purity + selectors + a full recorded sequence", () => {
  test("the input transcript is never mutated (copy-on-write)", () => {
    const original: ChatMessage[] = [{ id: "m1", role: "assistant", parts: [] }];
    const snapshot = structuredClone(original);
    const out = bvApplyChunk(original, { type: "text-start", id: "t1" });
    expect(original).toEqual(snapshot); // input untouched
    expect(out).not.toBe(original); // a new array
    expect(out[0]).not.toBe(original[0]); // a new message object
  });

  test("bvLastUserText joins the most recent user turn's text parts", () => {
    const msgs: ChatMessage[] = [
      { id: "u1", role: "user", parts: [{ type: "text", text: "first" }] },
      { id: "a1", role: "assistant", parts: [{ type: "text", text: "reply" }] },
      {
        id: "u2",
        role: "user",
        parts: [
          { type: "text", text: "port" },
          { type: "text", text: "this" },
        ],
      },
    ];
    expect(bvLastUserText(msgs)).toBe("port this");
    expect(bvLastUserText([])).toBe("");
  });

  test("a realistic stream (start → reasoning → tool → text → finish) folds to the expected transcript", () => {
    const s = fold(
      [],
      [
        { type: "start", messageId: "a1", messageMetadata: { model: "harness" } },
        { type: "reasoning-start", id: "r" },
        { type: "reasoning-delta", id: "r", delta: "checking queue" },
        { type: "reasoning-end", id: "r" },
        { type: "tool-input-start", toolCallId: "c", toolName: "dispatch" },
        { type: "tool-input-available", toolCallId: "c", input: { goal: "g" } },
        { type: "tool-output-available", toolCallId: "c", output: { queued: true } },
        { type: "data-tick", id: "tick-log", data: { rows: ["woke"] } },
        { type: "text-start", id: "t" },
        { type: "text-delta", id: "t", delta: "Routed." },
        { type: "text-end", id: "t" },
        { type: "finish" },
      ],
    );
    expect(s).toHaveLength(1);
    expect(s[0]?.metadata).toEqual({ model: "harness" });
    expect(s[0]?.parts).toEqual([
      { type: "reasoning", id: "r", text: "checking queue", state: "done" },
      {
        type: "tool-dispatch",
        toolCallId: "c",
        state: "output-available",
        inputText: "",
        input: { goal: "g" },
        output: { queued: true },
      },
      { type: "data-tick", id: "tick-log", data: { rows: ["woke"] } },
      { type: "text", id: "t", text: "Routed.", state: "done" },
    ]);
  });
});
