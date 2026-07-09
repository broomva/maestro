# API

The wire surface. Three contracts: the **runtime API** (the only one with substance), the **relay protocol** (routing envelope around it), and the **client stream**. Every type here lives in `packages/protocol` and is imported by both sides — the contract is code, not documentation.

Versioning: every request/stream carries `x-broomva-protocol: 1`. Bump on breaking change; the relay passes it through untouched.

---

## 1. Runtime API

Hono routes on the runtime. Auth: `Authorization: Bearer <runtime-credential>` (direct/LAN) or the relay's forwarded identity (see §3). All bodies JSON.

### Reads

```
GET  /api/tree                     the work tree (node rows, nested)
GET  /api/node/:id                 one node: contract + sessions + gates
GET  /api/node/:id/brief           the _work.md body (the look's source)
GET  /api/sessions/:id             session row + diffstat receipt
GET  /api/sessions/:id/events?after=<seq>   event page (timeline hydration)
GET  /api/board                    nodes grouped in attention order (B.5)
GET  /api/schedules                the bench: routines + next fires
```

Reads are cheap index queries. Clients hydrate once, then live off the stream.

### The event stream

```
GET /api/stream            SSE, global — every event, node change, gate arrival
GET /api/sessions/:id/stream   SSE, one session
```

- Envelope: `{ seq, sessionId?, ts, actor, type, payload }` — the `event` table row, verbatim.
- **Resume cursor:** client sends `Last-Event-ID: <seq>`; runtime replays from there. `seq` is the index's autoincrement — total order, no gaps, no client-side sorting.
- Synthetic types beyond `session.jsonl`: `node.updated`, `gate.opened`, `gate.decided`, `schedule.fired` — projected by the runtime so clients never poll.

### Intents (the only writes)

```
POST /api/intents
```

One endpoint, a discriminated union — mirrors the gate verbs and the human's vocabulary, nothing else:

```ts
type Intent =
  | { type: "new_mission"; parentPath: string; title: string; brief: string; kind: Kind }
  | { type: "dispatch";    nodeId: string }
  | { type: "approve";     gateId: string }
  | { type: "revise";      gateId: string; feedback: string }        // send back
  | { type: "block";       gateId: string; reason?: string }
  | { type: "escalate";    gateId: string; to: string }              // point
  | { type: "grant";       gateId: string; capability: string }
  | { type: "kill";        sessionId: string }
  | { type: "set_routine"; nodeId: string; trigger: Trigger }        // a sentence, parsed upstream
  | { type: "set_state";   nodeId: string; state: OrchState }        // human override, audited
  | { type: "tick";        cause: "interval" | "hook" | "manual" }
```

- Header `Idempotency-Key: <uuid>` required on every intent; the runtime stores it in `lease` — a retried POST is a no-op, not a double dispatch.
- Response is `202 { accepted: true }` or a typed refusal. **Results arrive on the stream**, never in the response body — intents in, events out. This keeps every projection consistent for free.

### Chat

```
POST /api/sessions/:id/chat       body: UIMessage
```

Streams the response per the **Vercel AI SDK UI Message Stream Protocol** (`x-vercel-ai-ui-message-stream: v1`): `start`, `text-*`, `reasoning-*`, `tool-*`, `data-*`, `finish`. Gen-UI parts: `data-tick` (stable id `tick-log`, updates in place) and `data-gate` (reconciled by id). The client folds chunks with the pure reducer; any backend speaking the protocol plugs into `useChat` unchanged.

---

## 2. Direct / LAN mode

Self-host default and the dev loop. The client talks straight to the runtime with the runtime credential (Tauri keeps it in the OS keychain). No relay in the path; identical API. Nothing above this section knows which path was taken.

## 3. Relay protocol

The relay never parses work types — it moves opaque bytes between authenticated parties.

- **Runtime side (dials out):** the runtime opens a persistent outbound connection to the relay and authenticates with its runtime key. Self-hosting never requires an open inbound port — this is load-bearing for the trust tier.
- **Client side:** `https://relay/r/:runtimeId/*` → forwarded verbatim to that runtime over its tunnel, response streamed back. SSE passes through unbuffered.
- **Auth:** client ↔ relay via Clerk session; the relay checks the user is authorized for `:runtimeId` (a `user ↔ runtime` grant table — the relay's *only* table) and forwards identity as a signed header the runtime verifies.
- The Anthropic key never transits the relay; model calls originate on the runtime.

## 4. Error shape

```ts
{ error: { code: string, message: string, retryable: boolean } }
```

Codes are part of `packages/protocol` (`budget_exhausted`, `lease_held`, `gate_required`, `not_found`, `unauthorized`). The UI renders them in plain voice; raw codes are a developer surface, like the state enums.
