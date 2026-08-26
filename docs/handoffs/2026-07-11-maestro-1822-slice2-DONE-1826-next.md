# Maestro handoff — BRO-1822 slice 2 (F10 chat endpoint) DONE → BRO-1826 (M4) next

**Date:** 2026-07-11
**Main tip:** `55df1c7` (squash-merged PR #55, CI-gated automerge, green-only)
**Linear project:** `6c738b50-fc1f-4b4c-8704-139c4747c62a` (Broomva Maestro — build)

## What shipped this iteration

**BRO-1822 DONE (all slices).** Slice 2 = the F10 chat endpoint, the dispatch *trigger* that slice 1's mount was built for.

- `apps/runtime/src/api/chat.ts` (NEW) — `POST /api/sessions/:id/chat`:
  - `extractLatestUserMessage(body)` — SDK `{messages:[]}` or a bare UIMessage → newest `role:"user"`.
  - Resolve target: `sup.get(id)` → `sup.list().find(nodeId===id)` → idle-node `sup.dispatch(id)` (dispatch-then-chat, FLOWS F10.2). Typed errors per API §4: `unsupported_intent` 501 (no dispatch mounted), `not_found` 404, `lease_held` 409.
  - `startCursor = currentMaxSeq(db, runId)` → `entry.supervised.control.chat(msg)` → `createUIMessageStreamResponse({stream: createUIMessageStream({execute: streamSession})})`, header `x-maestro-protocol: 1`.
  - `streamSession` (exported) = the EventType→UIMessageChunk projector (contract §7): `start` → agent.said→text-start/delta/end (`text-${seq}`) · tool.call→tool-input-start/available (synthesized `tool-${seq}` toolCallId, strict call→result pairing, dangling-tool close on death) · run.finished→finish · run.failed/killed→error+errored finish. Adopts ai@6's wire wholesale.
- `apps/runtime/src/dispatch.ts` — `mountDispatch` now async; seeds `BudgetGuard.dayTotalUsd` via new `deriveDayTotalUsdFromIndex(db, now)` (today-only `budget.metered` sum, degrade-to-0). Slice-1 latent gap #1 (per-day cap resets on restart) CLOSED.
- `apps/runtime/src/app.ts` / `index.ts` — `createApp` gained a lazy `dispatch?: () => DispatchRuntime | undefined` accessor (6th param); registers the chat route under the `if(index)` gate; `index.ts` passes `() => dispatch` and awaits the async mount.

378 runtime tests (was 374), zero tokens through the REAL supervisor + proxy + child.

## P20 saga (the load-bearing part)

Round-1 **FAIL 5/10**, 3 confirmed findings; round-2 re-verify (fresh, on the pushed sha) **PASS 8/10, 0 confirmed**.

1. **[MAJOR] startCursor TOCTOU tail-forever hang.** A chat landing in a run's *finishing window* (its terminal event committed at/below `startCursor`, so the `gt(cursor)` live tail never sees it) tailed a dead run forever → client hangs, connection leaks. **Fix:** `streamSession` takes `isRunLive: () => sup.get(runId) !== null`; when caught up (drained < a full batch) on a run no longer in the supervisor registry, it does ONE guaranteed extra drain, then reads the terminal directly via `lastTerminal()` and finishes. Shared `applyTerminal()` renders a terminal identically whether streamed live or read post-reap. **Race-free by `supervisor.terminal` ordering** (emit terminal AWAITED → park → `registry.delete`): once `isRunLive()` flips false, the run's writes are durable; a reaped run emits no further events so the extra-drain can't spin.
2. **[MINOR] index-read false success.** The `drainOnce` catch emitted a clean `finish` → now writes a `{type:"error", errorText}` chunk + errored finish.
3. **[MINOR] day-total seed unasserted end-to-end.** New `dispatch.test.ts` behavioral test seeds over-cap prior spend ($6 vs a $5/day cap) → the mounted child's FIRST proxy call is refused in-path (402 → budget halt → exit 10 → **blocked**), mock served 0 calls.

All three mutation-proven: disable the reaped branch → both MAJOR tests time out (500ms watchdog); drop the error chunk → index-read test reds; drop `{dayTotalUsd}` at the mount → run completes `review` not `blocked`.

CodeRabbit's one thread (`ai@^6` → `^7`, v7 shipped 2026-06-25) **rejected-with-reason + filed BRO-1864** (out-of-scope dependency major; wire adopts v6 wholesale). Unix-socket proxy switch (slice-1 gap #2) still deferred + filed (only matters post-mock-mode).

## Reusable lessons

- **A projection that tails a live source must define termination for every way the source can vanish.** A "snapshot cursor, then tail `seq > cursor`" pattern has a TOCTOU where the terminal marker lands between snapshot and tail — the fix is a liveness oracle on the source + a direct read of the terminal marker, and it's only race-free if the source commits the terminal BEFORE going unlive (verify the ordering, don't assume it).
- **A hang bug is invisible to a test that doesn't bound its own runtime** — guard termination tests with a wall-clock watchdog race so a regressed hang resolves "timeout" fast instead of hanging the suite.
- **After a P20 round with a PRODUCTION-code fix, re-fire P20 fresh on the pushed sha** (unlike a test-only strengthening, which only needs a local mutation-proof).

## NEXT — BRO-1826 (M4 — chat, the projection)

The SPA UI that consumes the F10 endpoint just shipped. `track:ui`, phase:P2, milestone "P2 — Loop 1: sessions that run", `apps/app` (Vite + React + TanStack Router + Tailwind v4 + shadcn base + @maestro/tokens).

Scope (per the ticket title + CLAUDE.md canon): the chat column — **768px max-width**, an **event feed** (render the UI Message Stream the endpoint emits: text parts, tool call/result parts, terminal), and the **glass Composer** (one of the three glass surfaces — overlays/popovers/composer; carries the inner light line; 28px radius; send + stop verbs). Plain voice, no progress %, receipts not fake progress.

**Dep-chain to verify before building (P14):**
- Upstream (ready): the F10 endpoint (`POST /api/sessions/:id/chat`, `55df1c7`); the SSE read/stream API (`/api/stream`, `/api/sessions/:id/stream`) for hydrate+tail; `@maestro/protocol` chat types (`UIMessageEnvelope`, `MaestroDataParts`, `MAESTRO_PROTOCOL_HEADER`); the M3 board (BRO-1825) + routing/error-boundaries (BRO-1824) already in `apps/app`; tokens + shadcn base.
- The client should use ai@6's `useChat`/`readUIMessageStream` (the transport the endpoint speaks) OR consume the stream directly — check what `apps/app` already wires. The wire is ai's, so ai's React hooks are the intended consumer.
- Downstream: BRO-1827 (P2 exit E2E — dispatch-from-app → watch → kill → restart) depends on M4 being able to drive a session from the UI.
- Design canon (CLAUDE.md hard rules): glass only on the composer here; matte feed; accent-blue "Needs you"; Undertow live signal; no em dashes / no emoji / sentence case; everything stops under `prefers-reduced-motion`.

**Validation (P11):** the app has a light/dark playwright smoke harness + bun static tests. Dogfood by driving a real chat against a `MAESTRO_MOCK_MODEL=1` runtime (the endpoint auto-dispatches an idle node) and watching the feed render text + tool parts + finish. Capture a screenshot.

**Then:** BRO-1827 (P2 exit). Stop the loop (`ScheduleWakeup({stop:true})`) only when 1827 passes AND all P0–P5 non-OUTLOOK tickets in `6c738b50` are Done (many P3/P4/P5 remain — verifier stages, orchestrator, scheduler, routines, VPS, plus their exit tickets 1821/1817/1781).

## Branch discipline reminder

Rule-of-THREE branch-to-main slip (1819+1814+1862). **First action after `git checkout main` + clean confirm = create the feature branch, BEFORE any Read/Edit.** No slip on 1822 (both slices branched first) — keep it that way.
