/// <reference types="bun" />
// transport.test.ts (BRO-1826 M4, slice A) — the real RuntimeChatTransport against the F10 UI Message
// Stream. Anti-vacuity [[self-hosting-vacuous-pass]]: fold every yielded chunk through the SHARED reducer
// `bvApplyChunk` and assert the EXACT resulting transcript (parts, states, output), and assert the POST
// actually carried the transcript — so a dropped route / mis-parsed frame fails, not just "a stream came
// back". Covers frame-reassembly across network boundaries, clean abort, and a typed HTTP error.

import { describe, expect, test } from "bun:test";
import {
  bvApplyChunk,
  type ChatMessage,
  type ErrorResponse,
  type StreamChunk,
} from "@maestro/protocol";
import {
  ChatTransportError,
  type FetchLike,
  parseUiMessageStream,
  RuntimeChatTransport,
} from "./transport";

/** One SSE frame as the runtime's `createUIMessageStreamResponse` emits it (`data: <json>\n\n`). */
const frame = (obj: unknown) => `data: ${JSON.stringify(obj)}\n\n`;
const DONE_FRAME = "data: [DONE]\n\n";

/** A 200 UI Message Stream response whose body enqueues `slices` (bytes) in order, then closes. */
function streamResponse(slices: string[], status = 200): Response {
  const enc = new TextEncoder();
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const s of slices) controller.enqueue(enc.encode(s));
      controller.close();
    },
  });
  return new Response(body, { status, headers: { "content-type": "text/event-stream" } });
}

// The chunk sequence the F10 endpoint emits for a run that runs one tool then answers (chat.ts
// streamSession: start → tool.call → tool.result → agent.said → run.finished).
const RUN_CHUNKS: StreamChunk[] = [
  { type: "start", messageId: "r1" },
  { type: "tool-input-start", toolCallId: "tool-3", toolName: "shell" },
  {
    type: "tool-input-available",
    toolCallId: "tool-3",
    toolName: "shell",
    input: { command: "ls" },
  },
  { type: "tool-output-available", toolCallId: "tool-3", output: "file.txt" },
  { type: "text-start", id: "text-5" },
  { type: "text-delta", id: "text-5", delta: "Listed the files." },
  { type: "text-end", id: "text-5" },
  { type: "finish" },
];

describe("RuntimeChatTransport — the F10 chat wire", () => {
  test("streams the UI Message Stream → folds into a well-formed assistant message; POSTs the transcript", async () => {
    let captured: { url: string; init: RequestInit } | undefined;
    const fetchImpl: FetchLike = async (url, init) => {
      captured = { url: String(url), init: init ?? {} };
      return streamResponse([...RUN_CHUNKS.map(frame), DONE_FRAME]);
    };
    const t = new RuntimeChatTransport({ sessionId: "n0", fetchImpl });
    const user: ChatMessage = { id: "u1", role: "user", parts: [{ type: "text", text: "list" }] };

    let msgs: readonly ChatMessage[] = [user];
    const seen: StreamChunk[] = [];
    for await (const c of t.stream(msgs)) {
      seen.push(c);
      msgs = bvApplyChunk(msgs, c);
    }

    // The transport yielded the content chunks in order; the [DONE] sentinel is consumed, never yielded.
    expect(seen.map((c) => c.type)).toEqual([
      "start",
      "tool-input-start",
      "tool-input-available",
      "tool-output-available",
      "text-start",
      "text-delta",
      "text-end",
      "finish",
    ]);
    // Folded transcript: the user turn + one assistant message carrying a completed tool part + the text.
    expect(msgs).toHaveLength(2);
    const assistant = msgs[1];
    expect(assistant?.id).toBe("r1");
    expect(assistant?.role).toBe("assistant");
    expect(assistant?.parts.find((p) => p.type === "tool-shell")).toMatchObject({
      toolCallId: "tool-3",
      state: "output-available",
      input: { command: "ls" },
      output: "file.txt",
    });
    expect(assistant?.parts.find((p) => p.type === "text")).toMatchObject({
      text: "Listed the files.",
      state: "done",
    });
    // ROUTING PROOF (anti-vacuity): the POST hit the session's chat path carrying the transcript.
    expect(captured?.url).toBe("/api/sessions/n0/chat");
    expect(captured?.init.method).toBe("POST");
    expect(JSON.parse(String(captured?.init.body))).toEqual({ messages: [user] });
  });

  test("reassembles a frame split across network chunks", async () => {
    const full =
      frame({ type: "start", messageId: "r1" }) +
      frame({ type: "text-start", id: "t" }) +
      frame({ type: "text-delta", id: "t", delta: "hello" }) +
      frame({ type: "text-end", id: "t" }) +
      frame({ type: "finish" }) +
      DONE_FRAME;
    // Split the whole payload at arbitrary byte offsets so a `data:` line straddles two network reads.
    const slices = [full.slice(0, 9), full.slice(9, 40), full.slice(40)];
    const fetchImpl: FetchLike = async () => streamResponse(slices);
    const t = new RuntimeChatTransport({ sessionId: "n0", fetchImpl });

    let msgs: readonly ChatMessage[] = [];
    for await (const c of t.stream([])) msgs = bvApplyChunk(msgs, c);
    expect(msgs[0]?.parts.find((p) => p.type === "text")).toMatchObject({
      text: "hello",
      state: "done",
    });
  });

  test("abort between frames buffered in ONE network read stops delivery (the coalesced-read case)", async () => {
    // ALL frames arrive in a single network read — the common case: Bun+Hono emits them back-to-back and
    // loopback TCP coalesces them, so the whole tail sits in `buf` after one reader.read(). The consumer
    // folds the FIRST delta then aborts; the remaining BUFFERED frames MUST NOT be delivered (the stop verb
    // is not a no-op). A one-frame-per-read fixture would make this pass vacuously — the missing inner-loop
    // abort check is only exercised when `buf` holds more than one frame at abort time.
    const all =
      frame({ type: "start", messageId: "r1" }) +
      frame({ type: "text-start", id: "t" }) +
      frame({ type: "text-delta", id: "t", delta: "AA" }) +
      frame({ type: "text-delta", id: "t", delta: "BB" }) +
      frame({ type: "text-end", id: "t" }) +
      frame({ type: "finish" }) +
      DONE_FRAME;
    const ac = new AbortController();
    const fetchImpl: FetchLike = async () => streamResponse([all]); // one enqueue → one reader.read()
    const t = new RuntimeChatTransport({ sessionId: "n0", fetchImpl });

    let msgs: readonly ChatMessage[] = [];
    const seen: string[] = [];
    for await (const c of t.stream([], { signal: ac.signal })) {
      seen.push(c.type);
      msgs = bvApplyChunk(msgs, c);
      if (c.type === "text-delta") ac.abort(); // after the FIRST delta ("AA"); "BB"/end/finish are buffered
    }
    // Stopped mid-buffer: start + text-start + the first delta only — the rest of the coalesced read dropped.
    expect(seen).toEqual(["start", "text-start", "text-delta"]);
    const text = msgs[0]?.parts.find((p) => p.type === "text");
    expect(text).toMatchObject({ text: "AA", state: "streaming" }); // never got "BB" nor text-end
  });

  test("a pre-aborted signal returns cleanly, never even POSTing", async () => {
    const ac = new AbortController();
    ac.abort();
    let called = false;
    const fetchImpl: FetchLike = async () => {
      called = true;
      return streamResponse([frame({ type: "start" }), DONE_FRAME]);
    };
    const t = new RuntimeChatTransport({ sessionId: "n0", fetchImpl });
    const seen: StreamChunk[] = [];
    for await (const c of t.stream([], { signal: ac.signal })) seen.push(c);
    expect(seen).toEqual([]);
    expect(called).toBe(false);
  });

  test("an abort that rejects the in-flight POST returns cleanly (no uncaught AbortError)", async () => {
    const ac = new AbortController();
    const fetchImpl: FetchLike = async () => {
      ac.abort();
      throw Object.assign(new Error("aborted"), { name: "AbortError" });
    };
    const t = new RuntimeChatTransport({ sessionId: "n0", fetchImpl });
    let threw = false;
    try {
      for await (const _ of t.stream([], { signal: ac.signal })) {
        /* no frames expected */
      }
    } catch {
      threw = true;
    }
    expect(threw).toBe(false);
  });

  test("a non-OK response throws a typed ChatTransportError with the endpoint's code + message", async () => {
    const errBody: ErrorResponse = {
      error: {
        code: "unsupported_intent",
        message: "chat is unavailable (no model loop mounted; set MAESTRO_MOCK_MODEL=1)",
        retryable: false,
      },
    };
    const fetchImpl: FetchLike = async () =>
      new Response(JSON.stringify(errBody), {
        status: 501,
        headers: { "content-type": "application/json" },
      });
    const t = new RuntimeChatTransport({ sessionId: "n0", fetchImpl });

    let caught: unknown;
    try {
      for await (const _ of t.stream([])) {
        /* should throw before yielding */
      }
    } catch (e) {
      caught = e;
    }
    expect(caught).toBeInstanceOf(ChatTransportError);
    expect((caught as ChatTransportError).status).toBe(501);
    expect((caught as ChatTransportError).code).toBe("unsupported_intent");
    expect((caught as ChatTransportError).message).toContain("unavailable");
  });

  test("parseUiMessageStream tolerates blank lines and `:` heartbeat comments between frames", async () => {
    const enc = new TextEncoder();
    const payload =
      ": hb\n\n" +
      frame({ type: "start", messageId: "r1" }) +
      "\n" +
      frame({ type: "finish" }) +
      DONE_FRAME;
    const body = new ReadableStream<Uint8Array>({
      start(c) {
        c.enqueue(enc.encode(payload));
        c.close();
      },
    });
    const seen: StreamChunk[] = [];
    for await (const c of parseUiMessageStream(body)) seen.push(c);
    expect(seen.map((c) => c.type)).toEqual(["start", "finish"]);
  });
});
