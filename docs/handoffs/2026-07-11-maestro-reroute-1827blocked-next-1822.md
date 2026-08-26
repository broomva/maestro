# Maestro loop — REROUTE: 1827 is blocked; NEXT = BRO-1822 (F10 chat endpoint, mounts the dispatch stack)

**TL;DR (P14 dep-chain reroute).** The prior handoff pointed at BRO-1827 (P2-exit), but a dep-chain check
shows **1827 is `blockedBy` BRO-1826 (M4) which is `blockedBy` BRO-1822 (F10 chat endpoint)**. 1822's own
blockers — BRO-1779 (supervisor spawn/tail/reap) and BRO-1776 (ChatTransport seam) — are BOTH Done, and its
child-side dependency (BRO-1862 stdin `chat`) shipped this session. **So BRO-1822 is the unblocked frontier.**
Rerouting to it (explicitly sanctioned: "reroute anything dependency-coupled, documenting the choice"). No
code was written this iteration — the deliverable is the re-scope + the foundational finding below.

## State of the world (P15)

- **maestro repo** `apps/maestro/` — `main` @ **`9761692`**, tree clean, no open PRs. broomva-child keystone
  COMPLETE (1855 + 1862 Done).
- **Linear** `6c738b50` dep-chain to P2-exit: **1822 (F10 chat) → 1826 (M4 chat projection) → 1827 (P2-exit)**.
  - 1822 blockers: 1779 ✅ done, 1776 ✅ done; child-side 1862 ✅ done → **1822 UNBLOCKED**.
  - 1826 blockers: 1822 (open), 1762 ✅, 1819 ✅ → unblocks once 1822 lands.
  - 1827 blockers: 1826 (open), 1806/1814/1824/1825 ✅ → unblocks once 1826 lands.
- Follow-ups BRO-1860 (child-loop hardening, ~12 refuted/deferred nits), 1847/1846/1844/1845/1841/1850/1853.

## THE FOUNDATIONAL FINDING (this is why 1822 is bigger than "an endpoint")

**The dispatch stack is NOT mounted in the running runtime.** `apps/runtime/src/index.ts` (206 lines) wires
only: open index → scan → watcher → crash-recovery (F9 `recoverOnStartup`) → `Bun.serve(createApp(...))`. It
does **not** instantiate `createSupervisor`, `createModelProxy`, or any dispatch trigger. The supervisor,
model proxy, and broomva-child all exist as **tested library code** but have never been assembled into the
24/7 process. So today the live runtime can serve the board (reads/SSE) but cannot dispatch a run.

1822 is where the mount happens. Its scope ("idle-node chat **auto-dispatches**") + done.check
(`bun test apps/runtime --filter chat`, a runtime test) mean: the chat endpoint dispatches a session if none
is live, then routes the UIMessage into the child. So building 1822 = mounting supervisor + proxy + dispatch.

### Pre-wired seams ready to use (don't rebuild)
- `createApp(config, startedAt, index, reconcile?, kill?)` — already takes a **`kill?`** seam ("Absent → kill
  intent 501s"). Wire `supervisor.kill` into it.
- `apps/runtime/src/api/intents.ts` — already has the **dispatch (new_mission) + kill intent SHAPES**; the
  kill path returns `unsupported_intent`/501 until `deps.kill` is present. `POST /api/intents` exists.
- `apps/runtime/src/supervisor/supervisor.ts` — `createSupervisor({db, factory, tokens, proxy, spawnChild,
  mintRunId, hostEnv, config})`; `.dispatch(nodeId)`, `.kill(runId)`, `.get(runId)` (F10 seam → the
  `RunEntry` whose `supervised.control` is the `ChildControl` with `.chat(msg)`/`.stop()`/`.ping()`).
- `apps/runtime/src/proxy/proxy.ts` — `createModelProxy({guard, tokens, upstream, apiKey, env})` +
  `serveProxy`. `createMockModel` (proxy/mock-model.ts) is the CI upstream (zero tokens).
- `apps/runtime/src/child/broomva-child.ts` — the child; its stdin `chat` control (1862) is the routing target.
- ChatTransport wire types: `packages/protocol/src/chat.ts` (UI_MESSAGE_STREAM_* consts, `ChatControlMessage`,
  `UIMessageEnvelope`) + `chunk-reducer.ts` (`bvApplyChunk`). `docs/contracts/chat-transport.md`.
- The P1-exit Playwright template (`apps/app/tests/p1-exit.pw.ts`) boots a real runtime over a temp workspace
  on port 4319 (the vite `/api` proxy target) — the pattern the eventual p2-exit.pw.ts will follow.

## BRO-1822 build plan (SPLIT — it is foundational + large)

Likely one slice per iteration; assess + split, documenting:
1. **Mount the dispatch runtime + a mock-model mode.** A config flag (`MAESTRO_MOCK_MODEL=1`, or default to
   mock when no Anthropic key) selects `createMockModel` vs the real upstream. In `index.ts` (guarded like the
   index open — the compiled binary caveat): instantiate the model proxy (in-process or served) +
   `createSupervisor`, pass `supervisor.kill` into `createApp`. Prove: an integration test boots the app,
   dispatches a triggered node (mock), asserts a child spawns + session events stream. Zero tokens.
2. **`POST /api/sessions/:id/chat`** (the endpoint): resolve the session → if none live, `supervisor.dispatch`
   (dispatch-then-chat) → route the UIMessage into `supervisor.get(runId).control.chat(msg)` → stream the AI
   SDK UI Message Stream Protocol out (header `x-vercel-ai-ui-message-stream: v1`; data-tick/data-gate parts).
   Error semantics: child dies mid-chat → a typed error part, session state honest. done.check
   `bun test apps/runtime --filter chat`: mock-model chat round-trip; idle-node chat auto-dispatches;
   mid-chat child death surfaces an error part.
3. (Then 1826 M4 chat-projection UI, then 1827 P2-exit E2E.)

**Watch for:** the mock upstream must model the tool/streaming preconditions ([[mock-fidelity-gap-false-green]]);
the child never holds the Anthropic key (only a bearer to the proxy); consecutive user turns / alternation on
chat injection (a slice-3 P20 note); the compiled-binary native-libSQL caveat when mounting new dynamic imports.

## The loop

Driver `~/broomva/docs/maestro-orchestration-loop.md`. One ticket/iteration: snapshot → **dep-chain (verify
the target is UNBLOCKED before building — this reroute is why)** → **branch FIRST + verify
`git branch --show-current`** → build (reuse the seams above) → bun test/tsc/biome green → dogfood (P11) →
**P20 ≥7 AND 0 surviving blocker/major, fix confirmed findings mutation-proven, re-verify on pushed code** →
PR via REST → resolve CodeRabbit → merge green via REST (full 40-char sha) → bookkeep → next handoff →
`ScheduleWakeup`. A clean terminal P20 pass merges; refuted/nits → BRO-1860. **Branch-slip is rule-of-THREE
(1819+1814+1862) — branch is the FIRST action after `git checkout main`.** Stop = `ScheduleWakeup({stop:true})`
when all P0–P5 non-OUTLOOK Done + the P2 exit (1827) passes.
