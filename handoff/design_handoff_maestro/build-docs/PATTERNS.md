# Patterns

The named design patterns Broomva is built on, where each applies, and the line it must not cross. These are the "why" behind the shapes in `ARCHITECTURE.md` / `DATA-MODEL.md` / `FLOWS.md` — when a new feature doesn't fit one of these, that's a design conversation, not a workaround.

## 1. Two-store, one-way authority (derived projection)

FS = system of record; index = transactional projection. Every fact about work originates in a file; the index only coordinates and answers fast. **Line:** the index never writes truth back to the FS except through a flow that commits (Loop 4's contract edits go to `_work.md`, then re-index).

## 2. Event log + cursor subscription (event-sourcing lite)

`session.jsonl` per run is the durable log; the index `event` table is its queryable projection with a total-order `seq`; clients subscribe with a resume cursor. Not full event sourcing — current state lives in `node`/`session` rows, not replayed on every read; the log is for audit, timeline, recovery, and fan-out. **Line:** never emit an event that didn't happen on disk first.

## 3. Intents in, events out (CQRS-lite)

Clients issue **intents** (commands in the human's vocabulary: approve, send back, kill); the runtime validates, acts, and the result arrives as **events on the stream** — never in the POST response. One write path, every projection consistent for free, and the orchestrator uses the same intents as the human. **Line:** no client-only state that matters; no REST endpoint that mutates outside the intent union.

## 4. Supervisor + process-per-run

The runtime is a small supervisor; every agent loop and every verifier is a child process in its own worktree. Crash isolation, `SIGKILL` as the kill switch, fresh-context restarts by respawn, OS-level parallelism. **Line:** no agent code executes in the supervisor's process, ever — the supervisor schedules, tails, and enforces; it never *thinks*.

## 5. Ports & adapters (hexagonal), exactly three ports

- **Runner** — `dispatch(contract, session) → events`. Adapters: Claude Agent SDK now; other harnesses later. The dispatch rail's registry is this port's catalog.
- **Sandbox** — "a place a run executes": git worktree now, container/microVM later (`ARCHITECTURE.md` §5). A worktree and a container are the same abstraction.
- **ChatTransport** — the AI SDK transport interface; mock in the prototype, runtime-backed in production, swappable 1:1.

**Line:** don't invent a fourth port until two real implementations exist for it. Premature ports are how frameworks metastasize.

## 6. Governance interceptor (the gate)

Every completion and every irreversible action is intercepted **after generation, before execution**, producing a `gate` row that a human resolves with one of four verdicts (approve / revise / block / escalate). Straight from the Org-Control-Layer result (`AUTONOMY.md` §Loop 3): separating "what the agent suggests" from "what the platform executes." **Line:** no code path executes an irreversible action without passing this interceptor — including the orchestrator's.

## 7. Explicit state machine, no auto-done

`OrchState` transitions are enumerated in one module; illegal transitions throw. The one rule that outranks all others: `review → done` requires a gate verdict when `gate: human`. **Line:** no timer, retry count, or verifier output transitions state on its own.

## 8. Guard-in-path (budgets, leases)

Guardrails are checked transactionally in the request path — budget before each model call, lease before each dispatch/fire — not audited after. Agents reach the model only through a runtime-provided handle, so the guard is structural. **Line:** any new resource an agent can spend gets a guard *before* the first unattended run uses it.

## 9. Pure reducer for stream folding

`bvApplyChunk(state, chunk) → state` — the client's only stream-state logic, pure and testable, shared by every projection. **Line:** no component folds chunks ad hoc; new part types extend the reducer.

## 10. Protocol as a package

`packages/protocol` holds every wire type (events, intents, contracts, error codes); runtime and clients import the same source. **Line:** no type describing the wire is declared outside this package — duplicated types are how projections drift.

## Anti-patterns (each has already tried to happen)

- Logic in the relay. It routes bytes; it grows no opinions.
- Truth in the client. Optimistic UI is fine; authoritative client state is not.
- Progress percentages. Receipts only — they'd be fake anyway.
- The writer grading its own homework — verifier is a separate process, different model where possible.
- A "quick" FS write from the index side. Authority flows one way.
- A fourth port, a second write path, a special-cased orchestrator.
