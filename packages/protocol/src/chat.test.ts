/// <reference types="bun" />
import { describe, expect, test } from "bun:test";
import {
  AI_SDK_MAJOR,
  AI_SDK_REACT_MAJOR,
  CHAT_ENDPOINT,
  type ChatControlMessage,
  DATA_TICK_ID,
  DATA_TICK_NAME,
  DATA_TICK_PART,
  isTickDataPart,
  MAESTRO_PROTOCOL_HEADER,
  type MaestroDataParts,
  type TickDataPart,
  type TickReceipt,
  UI_MESSAGE_STREAM_HEADER,
  UI_MESSAGE_STREAM_VERSION,
  type UIMessageEnvelope,
} from "./chat";

// done.check for seam-chat-transport (BRO-1776): `bun test packages/protocol -t
// transport`. `--filter` is a no-op in bun test (only `-t` filters by name); every
// describe carries "transport" so `-t transport` isolates the suite.
//
// This seam declares Maestro's DELTA over the AI SDK v6 UI Message Stream (adopted
// wholesale) — the data-part payloads, the control line, the constants. The AI SDK
// types themselves (`UIMessage` / `UIMessageChunk` / `ChatTransport` / `ToolUIPart`,
// including the `tool-output-error` chunk + `output-error` tool state that carry a
// FAILED tool call) are ai's, not re-declared here; their conformance is asserted by
// a type-level test in apps/app (BRO-1782) where `ai` is a dependency. Testing them
// here would mean hand-mirroring the SDK — the exact drift trap this seam removed.

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
  test("the adopted AI SDK core major is pinned to 6", () => {
    expect(AI_SDK_MAJOR).toBe(6);
  });
  test("the @ai-sdk/react major is pinned to 3 — the v6-core hook, NOT the v5 (v2) one", () => {
    // The blocker this guards: pairing ai@6 with @ai-sdk/react@2 gives a `useChat`
    // whose transport contract does not match this wire.
    expect(AI_SDK_REACT_MAJOR).toBe(3);
  });
});

describe("chat transport · the tick data part (the one part this seam owns)", () => {
  test("the part name and part type agree — `data-<name>`", () => {
    expect(DATA_TICK_PART).toBe(`data-${DATA_TICK_NAME}`);
    expect(DATA_TICK_NAME).toBe("tick");
  });

  test("the tick receipt uses the stable id tick-log (re-sends update in place, F6.5)", () => {
    expect(DATA_TICK_ID).toBe("tick-log");
  });

  test("a tick part carries the stable id and its rows, and the guard matches it", () => {
    const tick: TickDataPart = {
      type: "data-tick",
      id: DATA_TICK_ID,
      data: { rows: [{ g: "g1", cause: "cron", label: "nightly triage", t: "06:00" }] },
    };
    expect(isTickDataPart(tick)).toBe(true);
    expect(tick.id).toBe(DATA_TICK_ID);
    expect(tick.data.rows).toHaveLength(1);
    expect(JSON.parse(JSON.stringify(tick))).toEqual(tick);
  });

  test("the guard rejects other data parts and a failed-tool part (guard specificity)", () => {
    // ai's own parts, shaped structurally (protocol does not import them). The guard
    // must not mis-fire on a sibling data part or on the `output-error` tool part that
    // carries a failed tool call — proving the tick guard is the only Maestro claim.
    const gatePart = { type: "data-gate", id: "g", data: {} };
    const failedTool = { type: "tool-edit", state: "output-error", errorText: "boom" };
    expect(isTickDataPart(gatePart)).toBe(false);
    expect(isTickDataPart(failedTool)).toBe(false);
    // the failed-tool shape (ai's `tool-output-error` → `output-error` state) IS
    // representable — it is ai's, inherited by adopting the SDK vocabulary.
    expect(failedTool.errorText).toBe("boom");
  });
});

describe("chat transport · the MaestroDataParts map parameterizes ai's UIMessage", () => {
  test("the map's `tick` member is the TickReceipt payload (compile-checked)", () => {
    // A compile-time assertion: MaestroDataParts["tick"] must be TickReceipt. If the
    // map or the payload drifts apart, this assignment fails `tsc --noEmit`.
    const receipt: MaestroDataParts["tick"] = { rows: [] };
    const asTick: TickReceipt = receipt;
    expect(asTick.rows).toEqual([]);
  });
});

describe("chat transport · the child stdin control line (HARNESS §2)", () => {
  test("the chat control line wraps a minimal UIMessage envelope and round-trips", () => {
    const line: ChatControlMessage = {
      type: "chat",
      message: { id: "u1", role: "user", parts: [{ type: "text", text: "go" }] },
    };
    expect(JSON.parse(JSON.stringify(line))).toEqual(line);
    expect(line.message.role).toBe("user");
  });

  test("an ai-shaped UIMessage (id/role/parts) satisfies the minimal envelope structurally", () => {
    // Stands in for the real assignability (`ai`'s UIMessage → UIMessageEnvelope),
    // which apps/runtime asserts with `ai` present. Here we prove the envelope is the
    // structural subset the harness line needs — id + role + a parts array.
    const aiShaped = {
      id: "m2",
      role: "assistant" as const,
      metadata: { model: "claude-opus-4-8" },
      parts: [
        { type: "reasoning", text: "planning", state: "done" },
        { type: "text", text: "done", state: "done" },
        { type: "tool-edit", toolCallId: "t1", state: "output-available", output: { ok: true } },
        { type: "data-tick", id: DATA_TICK_ID, data: { rows: [] } },
      ],
    };
    const envelope: UIMessageEnvelope = aiShaped;
    expect(envelope.parts).toHaveLength(4);
    expect(envelope.role).toBe("assistant");
  });
});
