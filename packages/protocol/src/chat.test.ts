/// <reference types="bun" />
import { describe, expect, test } from "bun:test";
import {
  AI_SDK_MAJOR,
  CHAT_ENDPOINT,
  type ChatControlMessage,
  type ChatTransport,
  DATA_TICK_ID,
  isDataChunk,
  isTickPart,
  MAESTRO_PROTOCOL_HEADER,
  type TickDataPart,
  UI_MESSAGE_CHUNK_TYPES,
  UI_MESSAGE_STREAM_HEADER,
  UI_MESSAGE_STREAM_VERSION,
  type UIMessage,
  type UIMessageChunk,
} from "./chat";

// done.check for seam-chat-transport (BRO-1776): `bun test packages/protocol
// --filter transport`. `--filter` is a no-op in bun test (only `-t` filters by
// name); every describe carries "transport" so `-t transport` isolates the suite.

describe("chat transport · wire constants", () => {
  test("the UI message stream header + version are pinned", () => {
    expect(UI_MESSAGE_STREAM_HEADER).toBe("x-vercel-ai-ui-message-stream");
    expect(UI_MESSAGE_STREAM_VERSION).toBe("v1");
  });
  test("the maestro protocol header is set (D-NAME)", () => {
    expect(MAESTRO_PROTOCOL_HEADER).toBe("x-maestro-protocol");
  });
  test("the chat endpoint is session-addressed", () => {
    expect(CHAT_ENDPOINT).toBe("/api/sessions/:id/chat");
  });
  test("the AI SDK major is pinned to 6", () => {
    expect(AI_SDK_MAJOR).toBe(6);
  });
  test("the tick receipt uses the stable id tick-log", () => {
    expect(DATA_TICK_ID).toBe("tick-log");
  });
});

describe("chat transport · UIMessage round-trips through JSON", () => {
  test("a user message with a text part survives a JSON round-trip", () => {
    const msg: UIMessage = {
      id: "m1",
      role: "user",
      parts: [{ type: "text", text: "ship it" }],
    };
    expect(JSON.parse(JSON.stringify(msg))).toEqual(msg);
  });

  test("an assistant message with text + reasoning + tool + data parts round-trips", () => {
    const msg: UIMessage = {
      id: "m2",
      role: "assistant",
      metadata: { model: "claude-opus-4-8" },
      parts: [
        { type: "reasoning", text: "planning", state: "done" },
        { type: "text", text: "done", state: "done" },
        { type: "tool-edit", toolCallId: "t1", state: "output-available", output: { ok: true } },
        { type: "data-tick", id: DATA_TICK_ID, data: { rows: [] } },
      ],
    };
    expect(JSON.parse(JSON.stringify(msg))).toEqual(msg);
  });
});

describe("chat transport · custom data parts", () => {
  test("the tick part carries the stable id and its rows", () => {
    const tick: TickDataPart = {
      type: "data-tick",
      id: DATA_TICK_ID,
      data: { rows: [{ g: "g1", cause: "cron", label: "nightly triage", t: "06:00" }] },
    };
    expect(isTickPart(tick)).toBe(true);
    expect(tick.id).toBe(DATA_TICK_ID);
    expect(tick.data.rows).toHaveLength(1);
  });

  test("a transient data chunk is flagged and bypasses the transcript", () => {
    const chunk: UIMessageChunk = { type: "data-progress", data: { pct: 1 }, transient: true };
    expect(isDataChunk(chunk)).toBe(true);
    // narrow to read transient without an unsafe cast
    if (chunk.type.startsWith("data-")) {
      expect((chunk as Extract<UIMessageChunk, { type: `data-${string}` }>).transient).toBe(true);
    }
  });
});

describe("chat transport · the chunk vocabulary is closed", () => {
  test("one sample of every chunk type is representable, and the count matches the catalog", () => {
    const chunks: UIMessageChunk[] = [
      { type: "start" },
      { type: "text-start", id: "t" },
      { type: "text-delta", id: "t", delta: "hi" },
      { type: "text-end", id: "t" },
      { type: "reasoning-start", id: "r" },
      { type: "reasoning-delta", id: "r", delta: "…" },
      { type: "reasoning-end", id: "r" },
      { type: "tool-input-start", toolCallId: "c", toolName: "edit" },
      { type: "tool-input-delta", toolCallId: "c", inputTextDelta: "{" },
      { type: "tool-input-available", toolCallId: "c", toolName: "edit", input: {} },
      { type: "tool-output-available", toolCallId: "c", output: {} },
      { type: "data-tick", data: {} },
      { type: "error", errorText: "boom" },
      { type: "finish" },
      { type: "abort" },
      { type: "start-step" },
      { type: "finish-step" },
    ];
    // catalog and samples agree in count (drift guard: a new union member must be added to both)
    expect(chunks).toHaveLength(UI_MESSAGE_CHUNK_TYPES.length);
    for (const c of chunks) expect(typeof c.type).toBe("string");
  });
});

describe("chat transport · the 1:1 swap interface", () => {
  // A minimal port of the reducer's happy path — proves a transport's chunk
  // stream folds into a UIMessage regardless of which transport produced it.
  async function reduce(transport: ChatTransport, input: UIMessage[]): Promise<UIMessage> {
    const out: UIMessage = { id: "a1", role: "assistant", parts: [] };
    let text = "";
    for await (const c of transport.stream(input)) {
      if (c.type === "start" && c.messageId) out.id = c.messageId;
      else if (c.type === "text-delta") text += c.delta;
    }
    if (text) out.parts.push({ type: "text", text, state: "done" });
    return out;
  }

  test("a mock ChatTransport yields the chunk vocabulary and reduces to a UIMessage", async () => {
    const mock: ChatTransport = {
      async *stream() {
        yield { type: "start", messageId: "a-42" } as UIMessageChunk;
        yield { type: "text-start", id: "x" };
        yield { type: "text-delta", id: "x", delta: "hel" };
        yield { type: "text-delta", id: "x", delta: "lo" };
        yield { type: "text-end", id: "x" };
        yield { type: "finish" };
      },
    };
    const msg = await reduce(mock, [
      { id: "u1", role: "user", parts: [{ type: "text", text: "hi" }] },
    ]);
    expect(msg.id).toBe("a-42");
    expect(msg.parts).toEqual([{ type: "text", text: "hello", state: "done" }]);
  });

  test("the child stdin chat control line wraps a UIMessage", () => {
    const line: ChatControlMessage = {
      type: "chat",
      message: { id: "u1", role: "user", parts: [{ type: "text", text: "go" }] },
    };
    expect(JSON.parse(JSON.stringify(line))).toEqual(line);
    expect(line.message.role).toBe("user");
  });
});
