/// <reference types="bun" />
// conformance.test.ts (BRO-1826 M4, slice A) — the type-level guard the reducer's module note promised:
// "apps/app WILL carry the type-level conformance test asserting ai's real UIMessage / UIMessageChunk are
// assignable to these [structural] shapes (§9)". packages/protocol is zero-dep + structural ON PURPOSE
// (it never imports `ai`); this is where the wire's real types are pinned against those shapes, so an ai
// version bump that renames a field or reshapes a variant fails at tsc HERE, not silently at runtime.

import { describe, expect, test } from "bun:test";
import {
  AI_SDK_MAJOR,
  AI_SDK_REACT_MAJOR,
  type ChatMessage,
  type StreamChunk,
} from "@maestro/protocol";
import type { UIMessage, UIMessageChunk } from "ai";

type Expect<T extends true> = T;
/** True iff `From` is assignable to `To` (tuple-wrapped so a union `From` is checked whole, not distributed). */
type Assignable<From, To> = [From] extends [To] ? true : false;

// ai's UIMessage is a valid transcript container for the reducer (id / role / parts / metadata).
type _MsgConforms = Expect<Assignable<UIMessage, ChatMessage>>;

// Every UI Message Stream chunk variant the F10 endpoint emits (chat.ts streamSession) narrows to the
// reducer's `StreamChunk` input, so the transport hands raw parsed frames straight to `bvApplyChunk`.
type EmittedChunkType =
  | "start"
  | "text-start"
  | "text-delta"
  | "text-end"
  | "tool-input-start"
  | "tool-input-available"
  | "tool-output-available"
  | "tool-output-error"
  | "error"
  | "finish";
type AiEmittedChunk = Extract<UIMessageChunk, { type: EmittedChunkType }>;
type _ChunkConforms = Expect<Assignable<AiEmittedChunk, StreamChunk>>;

// `Extract<>` above SILENTLY shrinks if ai renames/removes an emitted variant (the literal becomes
// uninhabited → `never` drops out of the union → the smaller union is still assignable). So assert every
// emitted type NAME is still a member of ai's chunk-type union: this fails tsc if ai drops or renames a
// variant the endpoint emits (the drift `_ChunkConforms` alone cannot see).
type _EmittedNamesExist = Expect<Assignable<EmittedChunkType, UIMessageChunk["type"]>>;

// Reference the assertions so the file self-documents (a bare `type _X` is elided by the compiler).
const _conformance: [_MsgConforms, _ChunkConforms, _EmittedNamesExist] = [true, true, true];

describe("chat wire conformance (BRO-1826 M4) — ai v6 types satisfy the reducer's structural shapes", () => {
  test("apps/app is pinned to the AI SDK majors the runtime's stream producer emits (chat-transport.md §1)", () => {
    expect(AI_SDK_MAJOR).toBe(6);
    expect(AI_SDK_REACT_MAJOR).toBe(3);
    // The [true, true, true] tuple only typechecks if all three conformance assertions above hold.
    expect(_conformance).toEqual([true, true, true]);
  });
});
