# Contract — the chat transport (mock → runtime, 1:1 swap)

> **Seam BRO-1776.** A contract-writing ticket: this doc + the `chat.ts` types are agreed before the
> chat endpoint, the client transport, and the M4 chat surface. It pins the one swappable joint between
> the AI-SDK `useChat` client and the runtime, so the prototype's mock transports are replaced 1:1
> without touching the components.
>
> **Types:** [`packages/protocol/src/chat.ts`](../../packages/protocol/src/chat.ts)
> · **Tests:** [`chat.test.ts`](../../packages/protocol/src/chat.test.ts)
> (`bun test packages/protocol -t transport`).
> **Canon:** data-contract §"The wire protocol" · API.md §Chat / §Versioning · FLOWS F10 / F6.5 ·
> specs/HARNESS.md §2 · START-HERE §5 seam 1 · canon-amendments D-NAME.

## 1. What the seam is — and the architecture correction

Chat is a **projection** of a session — it never owns the work ("closing the tab loses nothing",
FLOWS F10.4; data-contract §"The work model").

**Maestro does not invent a chat wire. It adopts the Vercel AI SDK v6 UI Message Stream wholesale** —
the same protocol `@ai-sdk/react`'s `useChat` consumes and the runtime produces via the SDK's
UI-message-stream helpers. The **one swappable joint is therefore the SDK's own `ChatTransport`**
(`sendMessages` / `reconnectToStream`, §9), not a Maestro-defined interface. The prototype ships three
mock transports (`BvAnthropicTransport` / `BvOpenAITransport` / `BvHarnessTransport`); they become
**SDK-shaped test doubles**, and the real runtime transport replaces them **1:1** behind the SDK's
`ChatTransport` — any transport satisfying it plugs into `useChat` unchanged.

So `packages/protocol` declares **only Maestro's delta** over that third-party protocol: the custom
`data-*` part payloads, the wire constants, and the harness stdin control line. It deliberately does
**not** re-declare `UIMessage` / `UIMessageChunk` / `ChatTransport` / `ToolUIPart` — those are ai's,
imported directly by `apps/runtime` and `apps/app` (both depend on `ai`).

> **Why this replaced the first draft (the drift trap).** The first version hand-mirrored ai's
> ~25-variant generic chunk union and defined its own `ChatTransport { stream() }`. That mirror
> **silently omitted `tool-output-error`** (so a failed tool call could not even be represented — the
> tool part would hang at "running" forever) and pinned the **wrong transport shape** (`stream()` vs the
> real `sendMessages() → Promise<ReadableStream>`). A hand-mirror of a versioned SDK type is guaranteed
> to drift. Adopting the protocol wholesale — pinned by `AI_SDK_MAJOR` + a type-level conformance test
> in `apps/app` (§9) — is the opposite of drift: there is nothing mirrored to drift from.

**PATTERNS §10** ("no wire type describing the wire is declared outside this package") holds: every wire
type **Maestro** declares — the tick payload, the `MaestroDataParts` map, the control line, the
constants — lives in `protocol`. The AI SDK stream is not a Maestro-declared type; it is a **dependency**,
vendored in from `ai`.

## 2. The HTTP surface

- `POST /api/sessions/:id/chat` — **session-addressed** (`CHAT_ENDPOINT`). Response = an SSE stream of
  UI Message Stream chunks (API.md §Chat).
- Response header `x-vercel-ai-ui-message-stream: v1` (`UI_MESSAGE_STREAM_HEADER` +
  `UI_MESSAGE_STREAM_VERSION`); every request/stream carries `x-maestro-protocol: 1`
  (`MAESTRO_PROTOCOL_HEADER`, D-NAME) which the relay passes through untouched (API.md §Versioning).
- Auth: `Authorization: Bearer <runtime-credential>` direct/LAN, or a relay-forwarded signed identity
  (API.md §3). The Anthropic key never transits the relay; model calls originate on the runtime.

The request body is shaped by the SDK transport's `sendMessages` options (§9), not a bespoke Maestro
body — the client is the real `useChat` default/custom transport.

## 3. The message shape — ai's `UIMessage`, the params Maestro pins

The message is ai's **`UIMessage<METADATA, DATA_PARTS, TOOLS>`** — `{ id, role, metadata?, parts[] }`,
`role ∈ user | assistant | system`, with ai's part union (`text`, `reasoning`, `tool-${name}`,
`data-${name}`, `source-url`, `source-document`, `file`, …). **Maestro does not re-declare `UIMessage`.**
It is a **three-parameter** generic; Maestro pins the two that have a fixed Maestro shape and deliberately
defers the third:

- **`METADATA`** → Maestro's **`MaestroMetadata`** (`{ model?, time? }`). It **rides the wire**: the
  runtime emits it on the `start` / `message-metadata` chunk's `messageMetadata`, and the client folds it
  into `message.metadata` (§7). ai types METADATA as `unknown` at that boundary, so declaring it **once
  here** is what stops the emitter (BRO-1790) and the reader (BRO-1782) from drifting on the shape
  (`{ model }` vs `{ modelId }` would compile on both sides yet render the model label blank). `model`
  on an assistant message, `time` on a user message — one flat type (ai carries a single METADATA per
  message, not per-role).
- **`DATA_PARTS`** → Maestro's **`MaestroDataParts`** map (§5). Each key `NAME` yields a `data-${NAME}`
  part.
- **`TOOLS`** → **intentionally deferred** to ai's `UITools` default (dynamic tools) — a stated design
  decision, not silence. Maestro's child agents call an OPEN-ENDED tool set (shell, edit, read, arbitrary
  MCP tools), so tool parts are `DynamicToolUIPart` with `input` / `output` typed `unknown` — there is no
  fixed Maestro tool schema to pin, and `unknown` is the *accurate* model here (unlike METADATA, which
  has a fixed shape that WOULD drift if left unowned). BRO-1790 emits, BRO-1782 renders, both read tool
  output defensively.

`apps/app` composes `UIMessage<MaestroMetadata, MaestroDataParts>` (leaving TOOLS at ai's default); the
`UIMessage` container itself is ai's. This keeps PATTERNS §10 whole — every wire type with a **fixed**
Maestro shape is declared here; the one genuinely-dynamic parameter is deferred by explicit decision.

## 4. The chunk vocabulary — ai's `UIMessageChunk` (adopted, not re-declared)

The stream is ai's **`UIMessageChunk`** union (v6), the closed set the SDK reducer folds. `protocol` does
**not** re-declare it — a partial hand-copy is the drift trap of §1. The full v6 vocabulary the runtime
emits and the client folds:

- lifecycle — `start` · `start-step` · `finish-step` · `finish` · `abort` · `message-metadata`
- text — `text-start` / `text-delta` / `text-end`
- reasoning — `reasoning-start` / `reasoning-delta` / `reasoning-end`
- tools — `tool-input-start` / `tool-input-delta` / `tool-input-available` / **`tool-input-error`** ·
  `tool-approval-request` · `tool-output-available` / **`tool-output-error`** / `tool-output-denied`
- data / sources / files — `data-${name}` · `source-url` · `source-document` · `file`
- `error` (stream-level)

**A failed tool call is first-class:** `tool-output-error { toolCallId, errorText }` (and
`tool-input-error` for a malformed call) flips the tool part to ai's `ToolUIPart` state
**`output-error`** carrying `errorText` — the tool renders as failed, it does not hang at "running". This
representability is *inherited* by adopting the SDK vocabulary; it was the specific gap the §1 rewrite
closed. A `data-*` chunk with `transient: true` bypasses the transcript (surfaced via `onData`, never
persisted).

## 5. Custom data parts (gen-UI) — the `MaestroDataParts` map

Maestro's `data-*` parts, reconciled across the transcript **by `id`**, are the members of
`MaestroDataParts`:

- **`data-tick`** — the orchestrator wake log (`TickReceipt { rows: TickRow[] }`). Always at the stable
  id `DATA_TICK_ID` (`"tick-log"`); a re-send updates the card **in place** (FLOWS F6.5). **Owned by this
  seam** — `MaestroDataParts["tick"] = TickReceipt`, with the `TickDataPart` narrowing + `isTickDataPart`
  guard and the `DATA_TICK_NAME` / `DATA_TICK_PART` constants.
- **`data-gate`** — the F5 gate "look" card, reconciled by `id = gateId` across every open client. Its
  payload is **owned by the gate-queue seam (BRO-1789)**, which adds a `gate: GateCard` member to
  `MaestroDataParts` via **TypeScript module augmentation** from `gate.ts`
  (`declare module "./chat" { interface MaestroDataParts { gate: GateCard } }`) — so `chat.ts` is **not**
  edited (no `export *` barrel collision, no forked map) and the map stays single-sourced. This seam
  **leaves the gate member out**, so it does not pre-empt that ownership.
  **Precondition (BRO-1789 must satisfy):** the augmentation only reaches the composition site if `gate.ts`
  is part of the compiled package — so BRO-1789 MUST also add `export * from "./gate"` to
  `packages/protocol/src/index.ts`. Without that barrel line the `declare module "./chat"` is dead (the
  file is never loaded), `MaestroDataParts` keeps only `tick`, and `data-gate` silently fails to typecheck
  at the composition site in apps/app.

## 6. Where the transport terminates

The **runtime** is the terminating endpoint and the only protocol speaker. Direct/LAN: client → runtime.
Relay: `https://relay/r/:runtimeId/api/sessions/:id/chat`, forwarded verbatim — the SSE passes through
unbuffered and the relay never parses work types (API.md §3). Relay = byte-mover; runtime = protocol
speaker.

## 7. Event stream → UIMessage stream (the transport is NOT a pure `EventEnvelope→chunk`)

The runtime folds **two** sources into the chat stream (HARNESS §6):

1. **The live model token stream** — `text`/`reasoning` deltas, ephemeral, **not** in the event log
   (`session.jsonl` stores coalesced *turns*; per-token deltas go over the chat stream — F10).
2. **Projected events** — folded into ai chunks.

The concrete `EventType → chunk` mapping is **runtime logic** (it also folds the ephemeral token stream
and produces ai's `UIMessageChunk` via the SDK helpers), so it lives in `apps/runtime`, not in the
zero-dep protocol package. This table is documentation, not code:

| Source | Chunk |
|---|---|
| model text token | `text-delta` (bracketed by `text-start` / `text-end`) |
| model reasoning token | `reasoning-delta` |
| `tool.call` | `tool-input-start` → `tool-input-available` |
| `tool.result` (ok) | `tool-output-available` |
| `tool.result` (failed) | **`tool-output-error`** (`{ toolCallId, errorText }` → part state `output-error`) |
| `gate.opened` | `data-gate` (payload = BRO-1789 `GateCard`) |
| orchestrator narrative | `data-tick` (id `tick-log`) |
| `run.exiting` / `run.finished` | `finish` |
| `run.failed` | `error` (stream-level) |

## 8. The AI-SDK version pin

Standardize on **`ai@^6`** for core, **`@ai-sdk/react@^3`** for `useChat` — the v6 line, matching the
workspace. **`@ai-sdk/react` versions independently of core `ai`:** v6 core pairs with react-binding
**v3**, *not* v2 (v2 is the ai@5 hook; mis-pairing yields a `useChat` whose transport contract does not
match this wire). Recorded as consts: `AI_SDK_MAJOR = 6`, `AI_SDK_REACT_MAJOR = 3`, and the `v1`
stream-protocol literal (the real compatibility anchor — the wire contract depends only on the major).

No `package.json` under `apps/maestro` pins `ai` yet; the deps land in `apps/app` (client, `@ai-sdk/react`)
+ `apps/runtime` (server stream helpers, `ai`) when those tickets build. **`packages/protocol` adds no
dep** — it adopts the SDK by pinning the major, not by importing it, and stays zero-runtime-dep.

## 9. The 1:1 swap guarantee — ai's `ChatTransport`

The swap joint is ai's own interface (v6):

```ts
// Illustrative reproduction (the installed ai@6 .d.ts declares these as property/arrow
// signatures — `sendMessages: (options) => Promise<...>` — structurally equivalent to
// the method syntax shown here; apps/app's `satisfies ChatTransport` check is authoritative).
interface ChatTransport<UI_MESSAGE extends UIMessage> {
  sendMessages(options: {
    trigger: 'submit-message' | 'regenerate-message';
    chatId: string;
    messageId: string | undefined;
    messages: UI_MESSAGE[];
    abortSignal: AbortSignal | undefined;
  } & ChatRequestOptions): Promise<ReadableStream<UIMessageChunk>>;
  reconnectToStream(options: { chatId: string } & ChatRequestOptions): Promise<ReadableStream<UIMessageChunk> | null>;
}
```

Any transport implementing it plugs into `useChat({ transport })` unchanged. The prototype's `Bv*Transport`
classes become **SDK-shaped test doubles** implementing `sendMessages`/`reconnectToStream`; the real
transport is a Maestro fetch/SSE `ChatTransport` speaking the runtime endpoint (§2).

**Conformance is enforced where `ai` lives, not here.** `apps/app` (BRO-1782) carries a type-level test
(`ai` is a dep there) asserting (a) the Maestro transport satisfies `ChatTransport<MaestroUIMessage>`, and
(b) `MaestroDataParts` composes a valid `UIMessage` (each `data-${NAME}` is a real `DataUIPart`). Because
`protocol` re-declares none of these SDK types, there is nothing here to drift — the conformance test
guards the *composition*, not a mirror. `protocol`'s own `chat.test.ts` covers the Maestro delta:
constants (incl. the react-major pin), the tick payload + guard, both owned generic halves
(`MaestroDataParts["tick"]` + the `MaestroMetadata` witness), and the control line.

## 10. Idle-work addressing — dispatch-then-chat (resolves F10)

The endpoint is session-addressed, but F10.2 says a chat to **idle work** "spawns a session." **Decision
(client-side dispatch-then-chat):** a chat aimed at a node with no live session first POSTs a `dispatch`
intent (F2), awaits the session row via the stream (`node.updated` + the new session), **then** POSTs the
chat to the new `sessionId`. This keeps the endpoint purely session-addressed (no `nodeId` overload) and
reuses the intents→events round-trip (PATTERNS §3). The alternative (runtime-side auto-dispatch on a
`nodeId`-addressed chat) would change the API.md §Chat endpoint contract — not taken.

**Ordering (must, or the message races):** the client MUST be **subscribed to / resuming the event stream
from its `seq` cursor BEFORE it POSTs the `dispatch` intent** — otherwise the `node.updated` + new-session
events can arrive before the subscription is live and the client waits forever (or drops the user's
queued message). Subscribe → dispatch → await session → chat, never dispatch-then-subscribe.
**Dispatch-rejected path:** if the `dispatch` intent is refused (4xx / a typed `ErrorResponse` —
`gate_required`, `lease_held`, `budget_exhausted`), the client surfaces the refusal in plain voice and
**does NOT** POST the chat (there is no session to address); the queued message stays editable, not lost.

## 11. Child-harness stdin (cross-dep HARNESS §2)

The same UIMessage travels client → runtime (HTTP) → supervisor → child stdin as an NDJSON control line
`{ "type": "chat", "message": <UIMessage> }` (`ChatControlMessage`). **HARNESS §2 owns the full control
union** (`chat` / `stop` / `ping`); this seam contributes only the `chat` variant. Its `message` is an ai
`UIMessage`; `protocol` types it against the **minimal structural envelope** the control line needs —
`UIMessageEnvelope { id, role, parts: unknown[] }` — which ai's `UIMessage` satisfies, and which the
runtime narrows to ai's full `UIMessagePart[]`. Typing it minimally (not re-declaring ai's part union)
keeps `protocol` zero-dep while still pinning the control-line shape both sides agree on. The §8 version
pin is load-bearing here: the child's Agent-SDK reader and the client's `useChat` sender must be the same
AI-SDK major.

## 12. Reconciliations / boundaries

- **`data-gate` payload shape** → BRO-1789 adds `gate: GateCard` to `MaestroDataParts` (this seam leaves
  it out). **The board-derived `look { ran, decided[], ask }`** → work-item.ts (BRO-1764). Two projections
  of the same F5 gate, `gateId`-linked.
- **`GateKind` widening to add `"question"`** (HARNESS §4 exit-20) is **BRO-1789's**; this seam references
  `GateKind` nowhere. **Prototype card `kind: "gate" | "warn"`** is a *display* discriminator, distinct
  from the `gate` table `GateKind` — the gate-queue contract keeps them separate.
- **The full stdin control union** (`stop` / `ping`) → HARNESS §2; this seam types only `chat`.
- **The `EventType → chunk` fold + the transport implementation** → `apps/runtime` (BRO-1790) and
  `apps/app` (BRO-1782); this seam pins the contract, not the fold.

---

_Contract for `seam-chat-transport` (BRO-1776). Provenance: data-contract / API / FLOWS / HARNESS /
START-HERE under `handoff/design_handoff_maestro/`. Adopts the AI SDK v6 UI Message Stream wholesale
(zero-dep: pins the major, does not import it); declares only Maestro's delta — the tick payload, the
`MaestroDataParts` map, the control line, the constants — and leaves the `data-gate` card payload to
BRO-1789 to avoid a cross-seam type collision._
