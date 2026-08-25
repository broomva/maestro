# Maestro loop — BRO-1822 slice 1 DONE (dispatch MOUNTED); NEXT = slice 2 (the chat endpoint)

**TL;DR.** BRO-1822 was split. **Slice 1 (this) mounted the dispatch loop into the running runtime** — the
supervisor + model proxy + real child were tested library code that `index.ts` had never assembled into the
24/7 process. Merged PR #54 → main **`0ef897d`**, P20 8/10, 0 blocker/major. **BRO-1822 stays In Progress**;
slice 2 is the actual endpoint (`POST /api/sessions/:id/chat`) that TRIGGERS a dispatch and routes chat.

## State of the world (P15)

- **maestro repo** `apps/maestro/` — `main` @ **`0ef897d`**, tree clean, no open PRs. Branch janitored.
- **Linear** `6c738b50`: **1822 In Progress** (slice 1 of 2 done — see the slice-1 comment on the ticket).
  Dep-chain to P2-exit unchanged: **1822 → 1826 (M4) → 1827 (P2-exit)**.
- P2 done: runtime chain 11 + UI 1825/1824 + child KEYSTONE (1854/1856/1855/1862) + **1822 slice 1**.
- Follow-ups: BRO-1860 (child-loop hardening), 1847/1846/1844/1845/1841/1850/1853.

## What slice 1 shipped (the mount)

- **`apps/runtime/src/dispatch.ts`** (new) — `mountDispatch(deps): DispatchRuntime`. Assembles:
  `SessionTokenRegistry` + `BudgetGuard(db, fsJournalSink())` [DURABLE session.jsonl — the BRO-1811 lesson]
  → `createModelProxy` + `serveProxy({port:0})` [loopback-only] → `createSupervisor({… spawnChild: devSpawnChild …})`.
  Supervisor assembly is wrapped in `try { … } catch (err) { proxyServer.stop(); throw err }` so a partial-init
  throw doesn't leak the already-listening proxy. `devSpawnChild` = `bun run broomva-child.ts` (no compiled bin yet).
  Returns `DispatchRuntime { supervisor, proxyServer, kill(runId), shutdown() }`.
- **`apps/runtime/src/index.ts`** — `let dispatch: DispatchRuntime | undefined` (module scope); `createApp(config,
  startedAt, index, watcher?.nudge, (runId) => dispatch?.kill(runId) ?? false)` (deferred kill closure); after
  crash-recovery, `if (index && config.mockModel) { try { dispatch = mountDispatch({db: index, config, hostEnv:
  process.env}); … } catch { dispatch = undefined /* degrade to reads */ } }`; `dispatch?.shutdown()` in the
  shutdown hook (before `watcher?.stop()`).
- **`apps/runtime/src/config.ts`** — `mockModel?: boolean`, `MAESTRO_MOCK_MODEL === "1"`. This is the ONLY mode
  that mounts dispatch today (no real Anthropic upstream exists — the sole `ModelUpstream` is `createMockModel`).
  Unset ⇒ runtime stays read-only, `dispatch` undefined, kill intent 501s.
- **`apps/runtime/src/dispatch.test.ts`** (new) — 2 integration tests, ZERO tokens, spawning the REAL child:
  (1) mounted dispatch runs a real child to completion → events teed to the index + **budget metered durably to
  session.jsonl** (mutation-proven vs a MemoryEventSink swap); (2) F8 kill of a live child mid-model-call →
  canceled + run.killed (parked mid-call via the mock's `delayMs` hung on a never-resolving promise — no shell
  grandchild, no leaked timer).

Verified: 365 runtime tests, tsc + biome clean, P11 dogfood (`MAESTRO_MOCK_MODEL=1` boot logged
`dispatch mounted (mock-model) · proxy http://127.0.0.1:…`).

## BRO-1822 SLICE 2 build plan (the endpoint — the actual F10 chat)

`POST /api/sessions/:id/chat` per API.md §1 / FLOWS.md §F10 / `docs/contracts/chat-transport.md`:

1. **Resolve the session/node.** For an idle node with no live run → **dispatch-then-chat**:
   `supervisor.dispatch(nodeId)` then route into the new run. For a live run → route directly.
2. **Route the UIMessage into the child.** `supervisor.get(runId).control.chat(msg)` (the 1862 stdin `chat`
   control — folds the `ChatControlMessage` UIMessage envelope into the next beat's user turn).
3. **Stream the AI SDK UI Message Stream Protocol out** (header `x-vercel-ai-ui-message-stream: v1`;
   data-tick / data-gate parts per the seam) — tail the session's events (reuse the SSE projection /
   `event-projection.ts`) and map to UI message parts.
4. **Error semantics:** child dies mid-chat → a typed error part; session state stays honest.
5. **done.check:** `bun test apps/runtime --filter chat` — mock-model chat round-trip; idle-node chat
   auto-dispatches; mid-chat child death surfaces an error part.

**Wire the two latent gaps the slice-1 P20 surfaced (they belong to slice 2, when the trigger goes live):**
- **Day-total budget seed** — at the mount, `deriveDayTotal(readMeteredEvents(...), dayStartMs)` →
  `BudgetGuardOptions.dayTotalUsd`, so the per-day cap is NOT reset on every runtime restart (BudgetGuard already
  has the `dayTotalUsd` opt + `deriveDayTotal`/`deriveSpentBySession` in budget.ts; the D5 recovery re-derive
  exists — reuse it at mount time).
- **Unix-socket proxy** — move the loopback proxy from TCP (`serveProxy({port:0})`) to a unix socket
  (`serveProxy({unixSocket})`, already supported in proxy.ts) BEFORE a real Anthropic key ever sits behind a
  local TCP listener. Mock-mode TCP is fine; the real-upstream follow-up must not ship a keyed local-TCP surface.

**Seams ready (don't rebuild):** `DispatchRuntime` from `mountDispatch`; `dispatch` module var + the deferred
kill closure in index.ts; `supervisor.dispatch(nodeId)` / `.get(runId).control.chat(msg)`; ChatTransport wire
types in `packages/protocol/src/chat.ts` + `chunk-reducer.ts`; the mock upstream (`createMockModel`, with the
tool/streaming fidelity from 1855). The mount is gated on `config.mockModel` — slice-2 CI tests set
`MAESTRO_MOCK_MODEL=1` and inject a mock script (zero tokens).

**Watch for:** consecutive-user-turn / alternation on chat injection (a slice-3 note); the child never holds the
Anthropic key (only a per-session bearer to the proxy); [[mock-fidelity-gap-false-green]] — the mock must model
the streaming/tool preconditions; keep the endpoint session-addressed (§10 idle-dispatch is client-side).

## The loop

Driver `~/broomva/docs/maestro-orchestration-loop.md`. One ticket/iteration: snapshot → **dep-chain (verify
UNBLOCKED)** → **branch FIRST + verify `git branch --show-current`** → build (reuse the seams above) → bun
test/tsc/biome green → dogfood (P11) → **P20 ≥7 AND 0 surviving blocker/major, fix confirmed findings
mutation-proven, re-verify on pushed code** → PR via REST → resolve CodeRabbit → merge green via REST (full
40-char sha) → bookkeep → next handoff → `ScheduleWakeup`. A clean terminal P20 pass merges; a test-only
strengthening that closes the sole confirmed finding with production code byte-identical to the P20-verified sha
does NOT need a full re-fire (mutation-prove locally). refuted/defense-in-depth nits → BRO-1860. **Branch-slip is
rule-of-THREE (1819+1814+1862) — branch is the FIRST action after `git checkout main`.** Stop =
`ScheduleWakeup({stop:true})` when all P0–P5 non-OUTLOOK Done + the P2 exit (1827) passes.
