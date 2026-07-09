# Data contract — the work model and the wire

Everything here is canon: implement these shapes. Demo *values* live in `apps/maestro/WorkData.jsx` and `apps/maestro/AiProtocol.jsx`.

## The work model

A work item is an object with a lifecycle. A folder is work at any scale (question, task, project, initiative) — depth is meaning, not schema. The orchestration contract is frontmatter living in the files:

```yaml
kind: task | routine | project | initiative | question
state: proposed | queued | running | blocked | review | done
owner: <agent or person>
budget: <unsupervised-hours allowance>
gate: you | none        # who must look before Done
related: [<paths>]      # knowledge-graph edges
```

`kind: routine` + `gate: none` = a standing loop: never closes, spends zero human hours until a run flags something.

## The state machine

System enums are a developer surface; **plain voice is canon in the UI**:

| system state | plain voice | tone token | group hint |
|---|---|---|---|
| `proposed` | (spec) | `muted` | Specs not yet dispatched |
| `queued` | Queued | `muted` | Actionable on the next tick |
| `running` | Running | `active` (`--bv-info`) | Dispatched, live in a worktree |
| `blocked` | Stuck | `warn` (`--bv-warning`) | A worker is stuck · unblock it |
| `review` | Needs you | `--bv-blue-accent` | Clean runs waiting at your gate |
| `done` | Done | `--bv-success` | The branch is the receipt |
| (routine) | Standing | — | standing pulse, never closes |

Rules:
- Attention-first ordering: `review, blocked, running, queued, proposed, done` (`WK_GROUP_ORDER`); `review` + `blocked` are the attention set.
- **No transition auto-enters `done`.** A clean run lands at `review` ("Needs you"); approving is the human verb. Send back returns to `queued` with the feedback attached.
- "Needs you" renders accent-blue (235), never red. `blocked` (Stuck) is the only warning tone.
- Never render progress percentages. Receipts only: `run/<id>` branch, diffstat, judge verdict, event timeline.

## The work item shape

From `WK_ITEMS` (trim demo-only fields as needed):

```ts
{
  id: string,
  state: "proposed"|"queued"|"running"|"blocked"|"review"|"done",
  time: string,                       // relative age of last event
  title: string,
  initiative: string, project: string,
  worker: { name: string, where: "local worktree" | "cloud sandbox" },
  run: `run/${string}`,               // the branch IS the receipt
  verdict?: string,                   // judge output
  reason?: string,                    // blocked cause
  look?: {                            // the gate compression
    ran: string,                      // "2h 14m unsupervised · 41 events"
    decided: string[],
    ask: string,
  },
  events: { g: string, verb: string, detail: string|node, t: string, tone?: string }[],
  chat: (                             // the same stream, as conversation
    | { from: "user", text: string }
    | { from: "run", phase: string, run: string, live?: boolean, lines: [string, string][] }
    | { from: "assistant", html: string }
  )[],
}
```

The unsupervised-hours ledger (autonomy scoreboard: hours today, a notch per human look) derives from events — never stored as a percentage.

## The wire protocol (chat)

`apps/maestro/AiProtocol.jsx` implements the **Vercel AI SDK UIMessage shape (v5/v6)** and the **UI Message Stream Protocol** (SSE, `x-vercel-ai-ui-message-stream: v1`):

- A message is `{ id, role, metadata, parts[] }`; parts are `text`, `reasoning`, `tool-NAME`, `data-NAME`.
- Stream chunks: `start`, `text-start/delta/end`, `reasoning-*`, `tool-input-*`, `tool-output-available`, `data-*`, `finish` — folded by `bvApplyChunk` (a pure reducer; port it as-is).
- **Gen-UI is data parts**: the tick receipt is `data-tick` with a stable id (`tick-log`) so re-sends update the card in place; gate cards are `data-gate` parts reconciled by id across the transcript.
- Any backend that speaks the protocol plugs in: `streamText().toUIMessageStreamResponse()` for model providers, or a custom `ChatTransport` for the agentic harness. The prototype's three transports (`BvAnthropicTransport`, `BvOpenAITransport`, `BvHarnessTransport`) are mocks with the production interface.
- The dispatch rail (model · harness · effort · scope · autonomy) reads `BV_MODEL_CATALOG` / `BV_HARNESSES` / `BV_EFFORT_SCALE` — replace the catalog with your real registry, keep the rail contract.

## The tick

Wakes have causes; the loop must be legible. `MCC_TICK_WAKES` (in `apps/maestro/WorkPanel.jsx`) is the vocabulary: a worker returning · your message · an interval · a self-set routine. Every tick renders *why it woke* + what it did (the `data-tick` receipt).

## The knowledge graph

Files with frontmatter are nodes; `related:` links are edges; folders are scope nodes you can enter (re-scopes the graph). Demo scopes in `KG_SCOPES` (`apps/maestro/ConceptKnowledge.jsx`); node types/colors in `KG_TYPE` (`apps/maestro/KgGraph.jsx`). Production source: index the workspace frontmatter.
