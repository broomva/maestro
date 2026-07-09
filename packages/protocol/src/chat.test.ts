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
  MAESTRO_PROTOCOL_VERSION,
  type MaestroDataParts,
  type MaestroMetadata,
  type TickDataPart,
  type TickReceipt,
  UI_MESSAGE_STREAM_HEADER,
  UI_MESSAGE_STREAM_VERSION,
  type UIMessageEnvelope,
} from "./chat";
import { MAESTRO_PROTOCOL_VERSION as VERSION_SRC, X_MAESTRO_PROTOCOL } from "./version";

// done.check for seam-chat-transport (BRO-1776): `bun run typecheck && bun test
// packages/protocol -t transport`. `--filter` is a no-op in bun test (only `-t` filters
// by name); every describe carries "transport" so `-t transport` isolates. typecheck is
// REQUIRED: `bun test` strips types, so the compile-time witnesses (MaestroDataParts /
// MaestroMetadata composition) only bite under `tsc --noEmit`, not `bun test` alone.
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
  test("the maestro protocol header + version are the SINGLE source (re-exported from version.ts, not re-declared)", () => {
    expect(MAESTRO_PROTOCOL_HEADER).toBe("x-maestro-protocol");
    // Single-source proof: chat's export IS version.ts's constant, not a second literal
    // that could drift on the next header rename (the header was already renamed once).
    expect(MAESTRO_PROTOCOL_HEADER).toBe(X_MAESTRO_PROTOCOL);
    expect(MAESTRO_PROTOCOL_VERSION).toBe(VERSION_SRC);
    expect(MAESTRO_PROTOCOL_VERSION).toBe(1);
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

  test("the tick guard does not mis-fire on a sibling data part or a failed-tool part (guard specificity)", () => {
    // ai's own parts, shaped structurally (protocol does not import them): a sibling
    // data part and the `output-error` tool part that carries a failed tool call. The
    // tick guard must claim ONLY `data-tick`. SDK conformance — that these ARE valid ai
    // parts, incl. `tool-output-error`'s representability — is apps/app's type-level
    // test (where `ai` is a dep), not asserted here on a hand-built literal.
    const gatePart = { type: "data-gate", id: "g", data: {} };
    const failedTool = { type: "tool-edit", state: "output-error", errorText: "boom" };
    expect(isTickDataPart(gatePart)).toBe(false);
    expect(isTickDataPart(failedTool)).toBe(false);
  });

  test("isTickDataPart is TAG-ONLY — it ACCEPTS a malformed data-tick, so consumers read data.rows defensively", () => {
    // The guard matches on `type` alone; it narrows the type but does NOT validate `data`.
    // A malformed data-tick (reachable via the loose `parts: unknown[]` control line) passes
    // the guard with a bad `data`. This pins the obligation on BRO-1782/BRO-1790: never trust
    // the narrow blindly, read `part.data?.rows ?? []`.
    const malformed = { type: "data-tick", id: DATA_TICK_ID, data: "not-a-receipt" };
    expect(isTickDataPart(malformed)).toBe(true); // tag matched, data NOT validated
    // the defensive read the contract requires of consumers:
    const rows = (malformed as { data?: { rows?: unknown[] } }).data?.rows ?? [];
    expect(rows).toEqual([]);
  });
});

describe("chat transport · the two UIMessage type params Maestro owns (METADATA + DATA_PARTS; TOOLS deferred)", () => {
  test("the DATA_TYPES half — MaestroDataParts['tick'] is the TickReceipt payload (compile-checked)", () => {
    // A compile-time assertion: MaestroDataParts["tick"] must be TickReceipt. If the
    // map or the payload drifts apart, this assignment fails `tsc --noEmit`.
    const receipt: MaestroDataParts["tick"] = { rows: [] };
    const asTick: TickReceipt = receipt;
    expect(asTick.rows).toEqual([]);
  });

  test("the METADATA half — MaestroMetadata carries the wire model/time (compile-checked)", () => {
    // The symmetric witness: metadata rides the message-metadata chunk (server-emitted,
    // §7), so protocol owns its shape too — otherwise the emitter (BRO-1790) and reader
    // (BRO-1782) drift on an `unknown`-typed generic. `model` on assistant, `time` on
    // user; both optional. If a field is renamed here, apps/app's composition breaks.
    const assistant: MaestroMetadata = { model: "claude-opus-4-8" };
    const user: MaestroMetadata = { time: "12m" };
    expect(assistant.model).toBe("claude-opus-4-8");
    expect(user.time).toBe("12m");
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
    // SHAPE-ILLUSTRATION only (a self-satisfying literal, acceptable by design): protocol
    // is zero-dep, so it cannot import `ai` to prove real assignability. The binding
    // acceptance criterion — `const _: UIMessageEnvelope = {} as import("ai").UIMessage` —
    // is asserted in apps/runtime + apps/app (BRO-1790/1782) where `ai` is a dependency.
    // Here we only illustrate the envelope is the structural subset the harness line needs.
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
