# Maestro handoff — BRO-1826 M4 slice A (chat data path) DONE → slice B (the view) next

**Date:** 2026-07-11
**Main tip:** `52e4413` (squash-merged PR #56, CI-gated, green-only)
**Linear:** BRO-1826 stays **In Progress** (slice A done; slice B = the view). Project `6c738b50`.

## M4 split (documented)

Full M4 = transport + `useBvChat` hook + feed (message/tool/typing/empty renderers) + Composer send/stop + DS classes + route + playwright — too large for one P20 surface. Split:
- **Slice A (this, DONE): the chat data path** — headless, design-doc-independent (determined by the reducer + wire), de-risks the UI.
- **Slice B (next): the view** — the React feed + composer + route + playwright that consumes slice A.

## What slice A shipped

- **`apps/app` gains `ai@^6`** — the wire's type source.
- **`apps/app/src/chat/transport.ts`**:
  - `ChatTransport` interface: `stream(messages, {signal}): AsyncGenerator<StreamChunk>` — mirrors the prototype's canon `BvChatTransport.stream(messages)`.
  - `RuntimeChatTransport`: POST the transcript to `CHAT_ENDPOINT` (`/api/sessions/:id/chat`) → `parseUiMessageStream` hand-parses the ai@6 UI Message Stream SSE (`data:` frames + `[DONE]`) → yields `StreamChunk`s. Typed `ChatTransportError` on non-OK (carries API §4 code/status/retryable). `FetchLike` test seam. Abort is a clean stop at every granularity (pre-fetch, in-flight fetch, between buffered frames, between reads); reader cancelled in `finally`.
  - **Holds NO transcript state** — folding is the shared pure `bvApplyChunk` (PATTERNS §9). The React `useBvChat` (slice B) folds into feature-local state. **Chat is a projection, not a store** (the M4 verify criterion). Hand-rolled the SSE parse rather than ai's `readUIMessageStream` (which folds internally and would compete with `bvApplyChunk`).
- **`apps/app/src/chat/conformance.test.ts`** — the type-level guard the reducer's module note promised: ai's real `UIMessage`/`UIMessageChunk` (emitted variants) assignable to the structural `ChatMessage`/`StreamChunk`, plus `Assignable<EmittedChunkType, UIMessageChunk["type"]>` (catches a renamed/removed emitted variant that `Extract<>` alone would silently miss).

Validation: 63 app / 188 protocol tests, typecheck + biome clean, CI green. **P11 dogfood**: transport drove a live mock-mode runtime (real supervisor + proxy + spawned child) via the F10 endpoint — streamed `start text-start text-delta text-end finish`, folded a real assistant turn.

## P20 (rule-of-many — the gate caught a real no-op abort + a vacuous test)

Round-1 FAIL 6/10 (3 confirmed), round-2 re-verify PASS 8/10 (0 confirmed):
- **MAJOR** — abort was a no-op for the **coalesced-read case**: the inner drain loop had no signal check, so an abort after folding frame N still yielded every remaining buffered frame. Common locally (Bun+Hono emits tool frames back-to-back; loopback TCP coalesces them into one read). The abort test was **vacuous** (one-frame-per-pull fixture never put two content frames in `buf`). Fixed: inner-loop `signal.aborted` check; test rewritten to one-read delivery + assert the tail drops.
- **MINOR** — abort during/before the POST threw instead of clean-return. Fixed: guard + try/catch mirroring the body-read path.
- **NIT** — conformance `Extract<>` silently shrank on a renamed variant. Fixed with the `Assignable<EmittedChunkType, ...>` assertion.
- Test-only follow-up commit closed the reviewer's coverage nit (non-abort `throw err` branches now tested — a real error PROPAGATES; production byte-identical → no re-fire).

**Reusable lessons** (also in [[maestro-build-arc]]):
- A stream reader must honor cancellation at every granularity it buffers — a per-network-read abort check misses frames coalesced into one read; check the signal in the inner frame-drain loop too. A one-item-per-read test fixture makes this class of bug invisible.
- A `type X = Extract<Union, {type: Name}>` conformance assertion silently passes when `Name` no longer inhabits `Union` — pair it with `Assignable<Name, Union["type"]>`.
- Abort must be a clean stop on both sides of the await (request AND body read).

## NEXT — BRO-1826 slice B (the view)

The React chat surface that consumes slice A's transport. Build to the design spec below (full spec: the prior handoff's Explore-agent output; sources: `handoff/design_handoff_maestro/build-docs/BUILD-PLAN.md §M4`, `docs/canon-map.md` PromptPlate/AiProtocol rows, `handoff/apps/maestro/AiProtocol.jsx` + `ConceptMaestroLoop.jsx` prototype, `COMPONENTS.md`, `LIVE-SIGNALS.md`).

**Build list:**
1. **`useBvChat` hook** (canon, `AiProtocol.jsx:517-547`): `{transport, initialMessages, onData}` → `{messages, status, sendMessage({text})}`. Appends a user message, sets `submitted`, iterates `transport.stream([...msgs, user])`, folds each chunk via `bvApplyChunk` (transient `data-*` → `onData`, not persisted), sets `streaming`/`ready`. Feature-local state (NOT the global store) — this IS "chat is a view". Note: the prototype uses `Date.now()`-based ids; keep ids deterministic/pure where reasonable or accept a client-only impurity (no-ambient-clock is a runtime discipline; the client can use crypto.randomUUID for message ids). `apps/app` now has no React testing lib — add `@testing-library/react` (or test the hook via a thin render) if unit-testing the hook.
2. **The feed** (`MccMaestroChat`/`MccMessage`/`MccToolPart`, `ConceptMaestroLoop.jsx:421-452`, `AiProtocol.jsx:550-598`): 768px column (canon, NOT the prototype's 760px), inner scroller (`overflow-y:auto; overscroll-behavior:contain`; `:first-child{margin-top:auto}` bottom-anchor; the shell never scrolls). Renderers per part type: user row (right-aligned), assistant `text` (+ blinking caret while `state==="streaming"`), `reasoning` (italic muted), `tool-*` (matte card: check-square icon + `<b>name</b>` + state label "streaming input…"→"running"→"done"; input/output monospace), `error`, `data-tick` (optional, defer if large). `data-gate` parts → `return null` (rendered by the gate queue, which is BRO-1789/1805 territory — likely defer). Empty state = a calm plain-voice greeting ("A fresh session on {layer}", sentence case, no emoji).
3. **Composer send/stop** — extend the `@maestro/ui` `Composer` with a `busy`/`onStop` prop (the send arrow flips to a stop square while `status !== "ready"`; `aria-label` "Send"↔"Stop"). It's the ONE glass surface (halo, 28px, inner light line — already built in M1). Wire `onSend` → `sendMessage`; `onStop` → abort the transport's signal.
4. **DS classes** — `bv-msg`/`bv-chat-feed`/`bv-typing`/`bv-link-pill` are compiled-DS-bundle classes NOT in the vendored prototype CSS (canon-map §51). Author them in `@maestro/ui` or `@maestro/tokens` (typing dots = `bv-typing` three-dot animation, stops under `prefers-reduced-motion`).
5. **Inline assistant link pills** — `.bv-link-pill` is 1 of the 5 sanctioned color uses; bold real-world nouns (`<b>Run</b>`), never UI chrome words.
6. **Route/shell wiring** — where the chat column lives (a route or a session-focused surface; check the shell + `router.tsx` — the board routes are `/`, `/knowledge`, `/history`, `/settings`, `/account`; the chat surface likely rides the board→session flow or a new route). The inspector (`board/inspector.tsx`) is the M5 side-panel — the "same run in a side panel" verify can be satisfied by both surfaces rendering the same session via the same reducer.
7. **Design canon** (CLAUDE.md hard rules): glass only on the composer; matte feed; accent-blue "Needs you"; no progress %; plain voice, sentence case, no em-dash/emoji; motion <300ms, everything stops under `prefers-reduced-motion`.

**done.check (the ticket):** `playwright test chat-m4.spec` — mock-model conversation streams into the feed; composer halo unique; same session renders in panel + thread.

**Dep-chain (P14):** upstream ready = the transport (slice A, `52e4413`), `bvApplyChunk`/`bvSelectGate`/`bvLastUserText` (protocol), the `@maestro/ui` Composer/Card/DotComet (M1), the F10 endpoint (BRO-1822), the SSE read API + M3 board + routing/error-boundaries. Downstream = BRO-1827 (P2 exit — dispatch-from-app → watch → kill → restart) needs the UI to drive a session.

**Validation (P11):** the app has a light/dark playwright smoke harness + bun static tests. Dogfood by driving a real chat against a `MAESTRO_MOCK_MODEL=1` runtime and watching the feed render text + tool parts + finish; capture a screenshot (light + dark). This is the full live-UI dogfood slice A deferred.

**Then:** BRO-1827 (P2 exit). Stop the loop (`ScheduleWakeup({stop:true})`) only when 1827 passes AND all P0–P5 non-OUTLOOK tickets in `6c738b50` are Done (many P3/P4/P5 remain + their exit tickets 1821/1817/1781).

## Branch discipline

No slip on 1826 slice A (branched first). Keep the reflex: first action after `git checkout main` + clean confirm = create the feature branch, before any Read/Edit.
