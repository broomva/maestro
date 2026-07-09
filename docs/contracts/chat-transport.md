# Contract — the ChatTransport (mock → runtime, 1:1 swap)

> **Seam BRO-1776.** A contract-writing ticket: this doc + the `chat.ts` types are agreed before the
> reducer port, the chat endpoint, and the M4 chat surface. It pins the one swappable joint between the
> AI-SDK `useChat` client and the runtime, so the prototype's mock transports are replaced 1:1 without
> touching the reducer or the components.
>
> **Types:** [`packages/protocol/src/chat.ts`](../../packages/protocol/src/chat.ts)
> · **Tests:** [`chat.test.ts`](../../packages/protocol/src/chat.test.ts)
> (`bun test packages/protocol -t transport`).
> **Canon:** data-contract §"The wire protocol" · API.md §Chat / §Versioning · FLOWS F10 / F6.5 ·
> specs/HARNESS.md §2 · START-HERE §5 seam 1 · canon-amendments D-NAME.

## 1. What the seam is

Chat is a **projection** of a session — it never owns the work ("closing the tab loses nothing",
FLOWS F10.4; data-contract §"The work model"). The `ChatTransport` is the single joint between the
AI-SDK `useChat` client and the backend. The prototype ships three mock transports
(`BvAnthropicTransport` / `BvOpenAITransport` / `BvHarnessTransport`); the real runtime transport
replaces them **1:1** behind the same interface, and the pure reducer (`bvApplyChunk`, ported as-is)
folds the stream identically regardless of which transport produced it (data-contract §wire; PATTERNS §9).

## 2. The HTTP surface

- `POST /api/sessions/:id/chat` — **session-addressed** (`CHAT_ENDPOINT`). Body = a single `UIMessage`;
  response = an SSE stream (API.md §Chat).
- Response header `x-vercel-ai-ui-message-stream: v1` (`UI_MESSAGE_STREAM_HEADER` +
  `UI_MESSAGE_STREAM_VERSION`); every request/stream carries `x-maestro-protocol: 1`
  (`MAESTRO_PROTOCOL_HEADER`, D-NAME) which the relay passes through untouched (API.md §Versioning).
- Auth: `Authorization: Bearer <runtime-credential>` direct/LAN, or a relay-forwarded signed identity
  (API.md §3). The Anthropic key never transits the relay; model calls originate on the runtime.

## 3. The `UIMessage` shape

`{ id, role, metadata?, parts[] }`; `role ∈ user | assistant | system`. Parts (`UIMessagePart`):
`text`, `reasoning` (both carry a `state: streaming | done`), `tool-${name}` (a state ladder
`input-streaming → input-available → output-available`), `data-${name}` (gen-UI), and `error`. It
mirrors the **AI SDK v6** `UIMessage` **structurally** — it is not imported from `ai` (PATTERNS §10: the
wire type lives here so it cannot drift; the package stays zero-runtime-dep). `metadata` stays generic
(`UIMessage<M = unknown>`); the app/runtime narrow it (e.g. `{ model }` on assistant, `{ time }` on
user) — a UI concern kept out of the wire type.

## 4. The chunk vocabulary (the UI Message Stream Protocol)

`UIMessageChunk` is the **closed** set the transport yields and the reducer folds (`UI_MESSAGE_CHUNK_TYPES`
lists it as data, so a new member must be added to both — a drift guard):

`start` · `text-start`/`text-delta`/`text-end` · `reasoning-start`/`reasoning-delta`/`reasoning-end` ·
`tool-input-start`/`tool-input-delta`/`tool-input-available` · `tool-output-available` · `data-*` ·
`error` · `finish` · `abort` · `start-step` · `finish-step`.

`finish`/`abort`/`start-step`/`finish-step` are lifecycle no-ops in the reducer. A `data-*` chunk with
`transient: true` **bypasses the transcript entirely** (surfaced via `onData`, never persisted).

## 5. Custom data parts (gen-UI)

Two product-defined `data-*` parts, reconciled across the transcript **by `id`**:

- **`data-tick`** — the orchestrator wake log (`TickReceipt { rows: TickRow[] }`). Always at the stable
  id `DATA_TICK_ID` (`"tick-log"`); a re-send updates the card **in place** (FLOWS F6.5). **Owned by this
  seam** (`TickDataPart`).
- **`data-gate`** — the F5 gate "look" card, reconciled by `id = gateId` across every open client. Its
  **payload shape is owned by the gate-queue seam (BRO-1789)**, which types it via
  `DataUIPart<GateCard>`. This seam keeps `data-*` **generic** (`DataUIPart<T = unknown>`) so it does not
  pre-empt that ownership — the transport machinery is here; the gate card is there.

## 6. Where the transport terminates

The **runtime** is the terminating endpoint and the only protocol speaker. Direct/LAN: client → runtime.
Relay: `https://relay/r/:runtimeId/api/sessions/:id/chat`, forwarded verbatim — the SSE passes through
unbuffered and the relay never parses work types (API.md §3). Relay = byte-mover; runtime = protocol
speaker.

## 7. Event stream → UIMessage stream (the transport is NOT a pure `EventEnvelope→chunk`)

The runtime folds **two** sources into the chat stream (HARNESS §6):

1. **The live model token stream** — `text`/`reasoning` deltas, ephemeral, **not** in the event log
   (`session.jsonl` stores coalesced *turns*, one event per completed block; per-token deltas go over the
   chat stream — F10).
2. **Projected events** — `tool.call` → `tool-input-*`, `tool.result` → `tool-output-available`,
   `gate.opened` → `data-gate`, orchestrator narrative → `data-tick`, `run.exiting`/`run.finished` →
   `finish`, `run.failed` → `error`.

The concrete `EventType → chunk` mapping is **runtime logic** (it also folds the ephemeral token stream),
so it lives in `apps/runtime`, not in the zero-dep protocol package. This table is documentation, not
code:

| Source | Chunk |
|---|---|
| model text token | `text-delta` (bracketed by `text-start`/`text-end`) |
| model reasoning token | `reasoning-delta` |
| `tool.call` | `tool-input-start` → `tool-input-available` |
| `tool.result` | `tool-output-available` |
| `gate.opened` | `data-gate` (payload = BRO-1789 `GateCard`) |
| orchestrator narrative | `data-tick` (id `tick-log`) |
| `run.exiting` / `run.finished` | `finish` |
| `run.failed` | `error` |

## 8. The AI-SDK version pin

Standardize on **`ai@^6`** (+ `@ai-sdk/react@^2` for `useChat`) — the v6 line, matching the workspace
(chatOS). No `package.json` under `apps/maestro` pins `ai` yet; the deps land in `apps/app` (client) +
`apps/runtime` (server stream helpers) when those tickets build. **`packages/protocol` adds no dep** — it
mirrors the shape structurally. `AI_SDK_MAJOR = 6` and the `v1` stream-protocol literal are recorded as
consts; the `v1` literal is the real compatibility anchor (the wire contract depends only on the major).

## 9. The 1:1 swap guarantee

The interface is `ChatTransport { stream(messages: UIMessage[]): AsyncIterable<UIMessageChunk> }`. Any
transport yielding the §4 vocabulary plugs into `useChat` unchanged; the reducer ports as-is. The
prototype's `Bv*Transport` classes become **test doubles** behind this shape; the real transport is an
AI-SDK HTTP transport that speaks the runtime SSE. `chat.test.ts` proves this: a mock `ChatTransport`
whose `stream()` yields `start → text-start → text-delta → text-end → finish` reduces to the same
`UIMessage` the real transport would — the machine-checkable form of "mock and runtime speak one interface".

## 10. Idle-work addressing — dispatch-then-chat (resolves F10)

The endpoint is session-addressed, but F10.2 says a chat to **idle work** "spawns a session". **Decision
(client-side dispatch-then-chat):** a chat aimed at a node with no live session first POSTs a `dispatch`
intent (F2), awaits the session row via the stream (`node.updated` + the new session), **then** POSTs the
chat to the new `sessionId`. This keeps the endpoint purely session-addressed (no `nodeId` overload) and
reuses the intents→events round-trip (PATTERNS §3). The alternative (runtime-side auto-dispatch on a
`nodeId`-addressed chat) would change the API.md §Chat endpoint contract — not taken.

## 11. Child-harness stdin (cross-dep HARNESS §2)

The same `UIMessage` travels client → runtime (HTTP) → supervisor → child stdin as an NDJSON control line
`{ "type": "chat", "message": <UIMessage> }` (`ChatControlMessage`). **HARNESS §2 owns the full control
union** (`chat`/`stop`/`ping`); this seam contributes only the `chat` variant's `message` typing — which
is exactly the protocol `UIMessage`, so both sides agree by construction. The §8 version pin is
load-bearing here: the child's Agent-SDK reader and the client's `useChat` sender must be the same AI-SDK
major.

## 12. Reconciliations / boundaries

- **`GateKind` widening to add `"question"`** (HARNESS §4 exit-20) is **BRO-1789's** (the gate-queue
  seam), not this one. This seam references `GateKind` nowhere directly; the `data-gate` payload it leaves
  generic carries the gate `kind` via BRO-1789's `GateCard`.
- **Prototype card `kind: "gate" | "warn"`** is a *display* discriminator, distinct from the `gate` table
  `GateKind` row value — the gate-queue contract keeps them separate.
- **`data-gate` payload shape** → BRO-1789 (`GateCard`). **The board-derived `look { ran, decided[], ask }`**
  → work-item.ts (BRO-1764). Two projections of the same F5 gate, `gateId`-linked.
- **The full stdin control union** (`stop`/`ping`) → HARNESS §2; this seam types only `chat`.

---

_Contract for `seam-chat-transport` (BRO-1776). Provenance: data-contract / API / FLOWS / HARNESS /
START-HERE under `handoff/design_handoff_maestro/`. Structurally mirrors AI SDK v6 without importing it
(PATTERNS §10); leaves the `data-gate` card payload to BRO-1789 to avoid a cross-seam type collision._
