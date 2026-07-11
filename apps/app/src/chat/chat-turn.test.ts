/// <reference types="bun" />
// chat-turn.test.ts (BRO-1826 M4, slice B) — the pure turn engine. Anti-vacuity
// [[self-hosting-vacuous-pass]]: assert the EXACT status timeline AND the folded transcript, drive a
// FAKE transport (not the real one — that's transport.test.ts), and prove transient data is surfaced but
// NOT persisted + an error propagates AFTER submitted. A vacuous version would assert "some steps came
// back"; these assert the precise contract useBvChat depends on.

import { describe, expect, test } from "bun:test";
import type { ChatMessage, StreamChunk } from "@maestro/protocol";
import {
  type ChatTurnStep,
  finalizeStreamingParts,
  isTransientData,
  runChatTurn,
} from "./chat-turn";
import type { ChatTransport } from "./transport";

const user: ChatMessage = { id: "u1", role: "user", parts: [{ type: "text", text: "list" }] };

/** A transport that yields a fixed chunk list (optionally throwing partway) — the test double. */
function fakeTransport(chunks: StreamChunk[], throwAfter?: number): ChatTransport {
  return {
    async *stream() {
      for (let i = 0; i < chunks.length; i++) {
        if (throwAfter !== undefined && i === throwAfter)
          throw Object.assign(new Error("boom"), { code: "unsupported_intent" });
        yield chunks[i] as StreamChunk;
      }
    },
  };
}

async function collect(gen: AsyncGenerator<ChatTurnStep>): Promise<ChatTurnStep[]> {
  const steps: ChatTurnStep[] = [];
  for await (const s of gen) steps.push(s);
  return steps;
}

describe("runChatTurn — the pure turn engine", () => {
  test("yields submitted (with the user turn) → streaming per chunk → ready, folding a real assistant message", async () => {
    const chunks: StreamChunk[] = [
      { type: "start", messageId: "r1" },
      { type: "text-start", id: "t" },
      { type: "text-delta", id: "t", delta: "Listed " },
      { type: "text-delta", id: "t", delta: "the files." },
      { type: "text-end", id: "t" },
      { type: "finish" },
    ];
    const steps = await collect(runChatTurn(fakeTransport(chunks), [], user));

    // The status timeline: submitted first, then one streaming per FOLDED chunk (finish folds to no-op
    // but still steps), then a terminal ready. `finish` is a reducer no-op → same messages ref.
    expect(steps.map((s) => s.status)).toEqual([
      "submitted",
      "streaming", // start
      "streaming", // text-start
      "streaming", // text-delta
      "streaming", // text-delta
      "streaming", // text-end
      "streaming", // finish (no-op fold, still a step)
      "ready",
    ]);
    // submitted carries the user turn INSTANTLY (before any network work).
    expect(steps[0]?.messages).toEqual([user]);
    // The final transcript: the user turn + a folded assistant message with the completed text.
    const final = steps.at(-1)?.messages ?? [];
    expect(final).toHaveLength(2);
    expect(final[1]).toMatchObject({ id: "r1", role: "assistant" });
    expect(final[1]?.parts.find((p) => p.type === "text")).toMatchObject({
      text: "Listed the files.",
      state: "done",
    });
  });

  test("a transient data chunk is surfaced to onData and NEVER folded into the transcript", async () => {
    const tick = {
      type: "data-tick",
      transient: true,
      data: { rows: 1 },
    } as unknown as StreamChunk;
    const chunks: StreamChunk[] = [
      { type: "start", messageId: "r1" },
      tick,
      { type: "text-start", id: "t" },
      { type: "text-delta", id: "t", delta: "hi" },
      { type: "text-end", id: "t" },
      { type: "finish" },
    ];
    const seen: unknown[] = [];
    const steps = await collect(
      runChatTurn(fakeTransport(chunks), [], user, { onData: (c) => seen.push(c) }),
    );

    // onData saw the tick exactly once.
    expect(seen).toEqual([tick]);
    // No step ever carried a data-tick part — the transient chunk bypassed the transcript entirely.
    for (const step of steps)
      for (const m of step.messages)
        expect(m.parts.some((p) => p.type === "data-tick")).toBe(false);
    // And it did NOT produce a streaming step (it `continue`d before yielding).
    expect(steps.map((s) => s.status)).toEqual([
      "submitted",
      "streaming", // start
      "streaming", // text-start
      "streaming", // text-delta
      "streaming", // text-end
      "streaming", // finish
      "ready",
    ]);
  });

  test("a transport error propagates AFTER submitted was already yielded (the user turn survives)", async () => {
    // Throw on the very first stream read — submitted must still have been emitted, so the hook can keep
    // the user's message on screen while it renders the failure.
    const gen = runChatTurn(fakeTransport([{ type: "start" }], 0), [], user);
    const first = await gen.next();
    expect(first.value).toMatchObject({ status: "submitted" });
    expect(first.value?.messages).toEqual([user]);
    await expect(gen.next()).rejects.toThrow("boom");
  });

  test("history is preserved and the user turn is appended (not replacing prior context)", async () => {
    const prior: ChatMessage = {
      id: "a0",
      role: "assistant",
      parts: [{ type: "text", text: "hey" }],
    };
    const gen = runChatTurn(fakeTransport([{ type: "finish" }]), [prior], user);
    const first = await gen.next();
    expect(first.value?.messages).toEqual([prior, user]);
  });
});

describe("runChatTurn — abort settles the trailing streaming part (P20 round-2 MAJOR)", () => {
  test("a truncated stream (text-end never arrives) still settles the text part to done on the final ready step", async () => {
    // The transport ends WITHOUT a text-end/finish — exactly what an abort produces (slice A returns
    // cleanly, dropping the tail). The last text part is left state:"streaming" by the reducer; the
    // final `ready` step MUST settle it so the caret stops.
    const chunks: StreamChunk[] = [
      { type: "start", messageId: "r1" },
      { type: "text-start", id: "t" },
      { type: "text-delta", id: "t", delta: "half a th" }, // no text-end, no finish
    ];
    const steps = await collect(runChatTurn(fakeTransport(chunks), [], user));
    const final = steps.at(-1);
    expect(final?.status).toBe("ready");
    const textPart = final?.messages[1]?.parts.find((p) => p.type === "text");
    // Mid-stream steps saw it streaming; the terminal ready step settles it to done.
    expect(textPart).toMatchObject({ text: "half a th", state: "done" });
  });
});

describe("finalizeStreamingParts", () => {
  test("flips a streaming text/reasoning part of the last assistant message to done", () => {
    const msgs: ChatMessage[] = [
      user,
      {
        id: "r1",
        role: "assistant",
        parts: [
          { type: "reasoning", text: "hmm", state: "streaming" },
          { type: "text", text: "typing", state: "streaming" },
        ],
      },
    ];
    const out = finalizeStreamingParts(msgs);
    expect(out[1]?.parts.every((p) => p.state === "done")).toBe(true);
    // The user turn is untouched (same reference).
    expect(out[0]).toBe(user);
  });

  test("leaves a tool part's own state alone (only bare 'streaming' is settled)", () => {
    const msgs: ChatMessage[] = [
      {
        id: "r1",
        role: "assistant",
        parts: [{ type: "tool-shell", toolCallId: "c1", state: "input-streaming" }],
      },
    ];
    const out = finalizeStreamingParts(msgs);
    expect(out[0]?.parts[0]?.state).toBe("input-streaming"); // an interrupted tool stays honest
    expect(out).toBe(msgs); // nothing streaming (bare) → same reference (React stability)
  });

  test("returns the same reference when nothing is streaming / no assistant tail", () => {
    const done: ChatMessage[] = [
      { id: "r1", role: "assistant", parts: [{ type: "text", text: "x", state: "done" }] },
    ];
    expect(finalizeStreamingParts(done)).toBe(done);
    expect(finalizeStreamingParts([user])).toEqual([user]); // last msg is user → untouched
    expect(finalizeStreamingParts([])).toEqual([]);
  });
});

describe("isTransientData", () => {
  test("true only for a data-* chunk flagged transient", () => {
    expect(isTransientData({ type: "data-tick", transient: true } as unknown as StreamChunk)).toBe(
      true,
    );
    expect(isTransientData({ type: "data-tick" } as unknown as StreamChunk)).toBe(false);
    expect(isTransientData({ type: "text-delta", id: "t", delta: "x" })).toBe(false);
    expect(isTransientData({ type: "finish" })).toBe(false);
  });
});
