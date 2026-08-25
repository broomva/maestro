# Maestro handoff — BRO-1826 M4 DONE (both slices) → BRO-1827 P2-exit next

**Date:** 2026-07-12
**Main tip:** `52a30e4` (BRO-1826 M4 slice B — the chat view — #57 squash-merged)
**Loop:** autonomous maestro build governor (one ticket/slice per iteration; see `~/broomva/docs/maestro-orchestration-loop.md` + [[maestro-build-arc]] / [[maestro-loop-self-fires-via-schedulewakeup]]).

---

## What just shipped (BRO-1826 M4 COMPLETE)

M4 = the chat surface, split into A (data path, #56 `52e4413`) + B (the view, #57 `52a30e4`). **BRO-1826 is DONE** (slice B completes the whole ticket).

**Slice B — the chat view** (`apps/app/src/chat/`), canon "chat is a projection, not a store":

- `chat-turn.ts` — the **pure turn engine** `runChatTurn(transport, history, user, opts)` (async generator: append user → `submitted`, per-folded-chunk → `streaming`, terminal → `ready`) folding via the shared `bvApplyChunk` reducer. Bun-testable with **no DOM** (repo convention: React tested via `renderToStaticMarkup` + pure calls; interaction dogfooded in playwright `*.pw.ts`). `finalizeStreamingParts(messages)` settles the trailing bare-`streaming` part of the last assistant message → the caret fix (see below). `ChatStatus = ready|submitted|streaming|error`.
- `use-bv-chat.ts` — thin React glue: feature-local `messages`/`status`, a per-turn `AbortController`, stable-identity `sendMessage` (empty-dep `useCallback` + latest-value refs), `stop()`, and **unmount-abort** (`useEffect(()=>()=>abort, [])`). Error path: `setMessages((m)=>appendErrorMessage(finalizeStreamingParts(m), err))`.
- `message-list.tsx` — user / assistant-text / reasoning / tool / error renderers; `AssistantText` tokenizes backtick spans → `.bv-link-pill`; `TypingDots` (`role="status"`); `EmptyState`.
- `chat-feed.tsx` — `role="log"` scroll container + bottom-anchor effect.
- `format.ts` — `tokenizeAssistantText` (backtick→pill) + `emptySessionGreeting`.
- `fixture-transport.ts` — `FixtureChatTransport` / `FixtureErrorChatTransport`, selected by `?fixture=1` / `?fixture=error` / `?step=<ms>` — the demo/test seam, **inert in production**.
- `routes/session.tsx` — `/session/$sessionId` route; `SessionView` reads the param and renders `<SessionChat key={sessionId} …/>` (**the key forces a full remount on a client-side session switch** so session A's transcript never bleeds into B). `SessionChat` binds ONE transport (fixture/error/runtime via `useMemo`), drives ONE `useBvChat`, paints a thread (768px column + glass Composer) + a read-only side-panel mirror (`data-testid=session-panel`, `xl:flex`).
- `packages/ui/composer.tsx` — extended with `busy`/`onStop` (Send↔Stop verb; Enter inert while busy).
- `packages/ui/chat.css` — new DS classes (`bv-msg`/`bv-chat-feed`/`bv-typing`/`bv-link-pill`/`bv-reasoning`/`bv-toolpart`; caret blink; all motion stops under `prefers-reduced-motion`).

**P20:** 4 rounds, terminal PASS **8/10, 0 surviving blocker/major** (`wf_ed141dd4-718`). The load-bearing catch — **caret-forever**: a streaming text part keeps its blinking caret unless explicitly settled, and it recurred on each of the THREE terminal exit paths as I fixed them one at a time (`[[fix-in-edge-dense-file-regresses]]`). Fix = `finalizeStreamingParts` at **every** terminal transition (`runChatTurn` ready-yield · `stop()` · the hook catch), each mutation-proven (reverting reds a specific playwright test).

**CodeRabbit:** 3 self-tagged 🔵 Trivial nitpicks (smart-scroll, optional-chaining, reasoning null-guard) — none correctness defects → filed to **BRO-1865** (deferred to keep the merge byte-identical to the P20-verified sha `f3904d3`).

**Validation:** 735 bun + 24 playwright green; tsc + `biome ci .` clean; P11 dogfood of the real `RuntimeChatTransport → runChatTurn → MessageList` against a live `MAESTRO_MOCK_MODEL=1` runtime + light/dark screenshots.

---

## NEXT: BRO-1827 — P2 exit (phase gate, Urgent, now UNBLOCKED)

All five blockers Done: **1826** (M4, just merged) · **1806** (mock-model server) · **1814** (crash recovery F9 + D4 lock + D5 reconcile) · **1824** (routing + error boundaries) · **1825** (M3 board live). BRO-1827 `blocks` BRO-1765 (verifier child) and opens all P3 dispatch-dependent work.

**Goal (ROADMAP §P2 Exit, verbatim):** dispatch a contract **from the app**, watch events stream, **kill it mid-run**, **restart the runtime** — **nothing lost, orphan parked at Stuck.**

**done.check:**
```bash
bun test:loops && playwright test p2-exit.spec   # scripted kill-mid-run + restart; evidence attached
```
Plus: run once **live locally** with a real SDK child (not just mock) + attach a screen recording + an event-log excerpt as evidence (scope bullet).

**This is a phase-gate / integration ticket, not new subsystem code.** Everything it exercises already exists and is tested in isolation — the job is to prove the *end-to-end seam* holds and produce evidence. Likely mostly a new `p2-exit.spec` playwright + a `test:loops`-level scripted scenario + wiring, not a large feature.

### The seams it composes (all shipped)

- **Dispatch from the app:** `apps/runtime/src/dispatch.ts` `mountDispatch()` (BRO-1822 slice 1) mounts the supervisor + model proxy + child spawn behind `MAESTRO_MOCK_MODEL=1`. The app triggers a run via `POST /api/sessions/:id/chat` (`apps/runtime/src/api/chat.ts`, BRO-1822 slice 2): resolve target → dispatch-then-chat for idle nodes → `entry.supervised.control.chat(msg)` → `createUIMessageStreamResponse`. The UI side is the M4 chat view (`RuntimeChatTransport` POSTs there).
- **Watch events stream:** `/api/stream` + `/api/sessions/:id/stream` (BRO-1816, poll-the-tail single-writer cursor) → the store (BRO-1775 `connectStream`) → the board (M3, BRO-1825) + the chat feed (M4).
- **Kill mid-run:** the F8 `kill` seam exposed by `mountDispatch` → supervisor SIGKILL → `canceled` + `run.killed` (proven in `dispatch.test.ts` test 2).
- **Restart → nothing lost, orphan at Stuck:** crash recovery F9 (BRO-1814) — on boot, `scanIntoIndex` + recovery reconciles; a run whose child died with no clean terminal is parked (orphan → **Stuck** / blocked), and durably-journaled events survive (the `fsJournalSink` session.jsonl is the durable truth, the 1811 lesson). D4 runtime singleton lock + D5 budget reconcile are part of 1814.
- **Guardrails transitively required:** mock-model (1806) + crash-recovery certify "no unattended anything without guardrails" (iteration cap / no-progress / fresh-context = 1795, kill-switch = 1801, budget = 1788).

### Watch-outs (from the arc)

- **`MAESTRO_MOCK_MODEL=1` is the ONLY mode that mounts dispatch today** (no real Anthropic upstream wired yet). CI runs mock; the "once live locally" run needs a real SDK child.
- **The compiled `bun build --compile` binary can't load the native `@libsql/client` addon** and degrades to `/health`-only — dispatch/reads/stream are all behind the `if(index)` gate. Per-PR CI only *compiles* the binary (never runs it); **dogfood the actual running process**, not just the compiled artifact.
- **`bun test:loops`** scenarios spawn the REAL `broomva-child.ts` via `makeChildSpawn` driven by a mock script + runtime `config` (not `--scenario` flags). A kill-mid-run scenario must bound the child command (short `sleep`) — SIGKILL does **not** cascade to `sh -c` grandchildren.
- **A phase-exit scenario whose success path is reachable without the thing-under-test firing is vacuous** — the discriminator must red when *only* the exit invariant regresses (e.g. assert the orphan is at Stuck AND the pre-kill events survived the restart, not just that the process restarted).
- **Branch FIRST** after `git checkout main` confirms clean, BEFORE any Read/Edit (rule-of-three slip: 1819+1814+1862). Make `git checkout -b feature/bro-1827-…` the literal first action.
- **P20 uses local `git diff main...HEAD`** (GraphQL is rate-limited 5k/hr). PR create/merge via REST.

### Cycle reminder (the loop contract)

P15 snapshot → P14 dep-chain → **branch FIRST** → build per `apps/maestro/CLAUDE.md` canon + reuse shipped seams → `bun test`/`tsc`/`biome ci .` green → P11 dogfood (interact + capture evidence) → **P20 dynamic Workflow ≥7 with 0 surviving blocker/major**, every CONFIRMED finding mutation-proven, re-verify P20 fresh on the pushed code → PR via REST → resolve every CodeRabbit thread → merge green via REST once P20≥7 + 4 required CI jobs green + 0 unresolved threads (full 40-char sha, squash) → bookkeep → `ScheduleWakeup` the next ticket.

**Stop condition:** when **BRO-1827 passes AND all P0–P5 non-OUTLOOK tickets in project `6c738b50` are Done** → `ScheduleWakeup({stop:true})`. (BRO-1827 opens P3, so after it, sweep P3/P4/P5.) No mid-arc stops; never ask back.

---

## State snapshot at handoff

- **Branch:** `main` @ `52a30e4`, clean tree (untracked `docs/handoffs/*` + `docs/conversations/*` only — bridge output, not part of any merge).
- **Linear:** BRO-1826 **Done** (2026-07-12); BRO-1827 **Backlog**, unblocked, Urgent. New follow-up **BRO-1865** (M4 chat-view polish nits, Low, Backlog).
- **Open follow-ups (Low/Med, not blocking):** 1865 (M4 polish) · 1864 (ai SDK v6→v7) · 1860 (child loop hardening) · 1847 (2-phase lease) · 1846 (inotify) · 1844/1845 (SSE resume) · 1841 (native libSQL driver) · 1850 (kill-TOCTOU) · 1853 (D4 hygiene).
- **CI:** 4 required jobs (quality/secrets/build/governance) + CodeRabbit advisory; CI-gated automerge; `enforce_admins:false`.
