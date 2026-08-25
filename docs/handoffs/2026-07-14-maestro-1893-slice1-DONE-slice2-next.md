# Maestro handoff — FID-6 slice 1 (History) DONE → slice 2 (Knowledge) next

**Date:** 2026-07-14
**Loop:** autonomous self-firing design-fidelity arc (ultracode). One ticket/slice per iteration.
**Main tip:** `4483f71` (PR #73 squash-merged).

## What just shipped — FID-6 slice 1: the History page (BRO-1893)

The first of FID-6's four full-page views. `/history` now renders the real `HistoryPage`
(replaced the stub). Ported the prototype `MccHistory`: search bar + organizing-axis toggle
(by day / work / agent) + you/autonomous filter, over a grouped list of run rows.

### Files
- `apps/app/src/store/project.ts` — `selectHistory(s)` + `HistorySession` iface + `HISTORY_RUN_STATES` set.
- `apps/app/src/store/project.test.ts` — `selectHistory` describe block (+8 tests).
- `apps/app/src/components/history/history-page.tsx` — `HistoryPage` (store container) + pure `HistoryView`.
- `apps/app/src/components/history/history-page.test.tsx` — `HistoryView` render tests.
- `apps/app/src/styles/history.css` — verbatim `.mcc-hist`/`.mcc-hrow`/`.mcc-hgroup` port (matte, cool-axis).
- `apps/app/src/styles/globals.css` — `@import "./history.css"`.
- `apps/app/src/router.tsx` — `historyRoute` wires `HistoryPage`.
- `apps/app/src/routes/stubs.tsx` — `HistoryView` stub removed.
- `apps/app/tests/history.pw.ts` — 6 integration tests.

### The KEY decision (special-case (a): "derived view silently disagrees with itself")
The prototype `MccHistory` is a session browser, but **the client store has NO session read path**:
`/api/tree` hydrates *nodes only* (`stream.ts:118` passes `{nodes}`), and **no SSE event ever creates a
`session` row** (`reducer.ts` — the `default` case only records `lastEventAt[sessionId]`, never
`s.sessions[id]`). So a `sessionId`-keyed history is **permanently empty in production**. Confirmed by
the P20 correctness skeptic against `reducer.ts` + `stream.ts`.

**Resolution:** `selectHistory` derives from **work STATE** — a leaf in `running`/`blocked`/`review`/`done`
(`HISTORY_RUN_STATES`) is the real, hydrated signal that a run happened. Captures you- AND loop-started
runs (the tick/wake log `s.ticks` is only a subset — scheduled fires). Forward-compatible: the run branch
(`it.run`) + deep per-run detail fill in behind the same rows when the session-history read path lands.
Excludes containers (initiative/project) + never-run backlog (`proposed`/`reviewing`/`triggered`/`canceled`
— no fabricated receipts, same discipline as the FID-5 canceled rail).

> **NOTE for later:** the session read path (populate `s.sessions` from `/api/node/:id` or a stream
> event) is a real pending seam — it lights up the run branch + deep run detail on History AND the
> inspector. Not this UI slice. Worth a follow-up ticket when a P1/P3 backend slice touches sessions.

### Structure note (reusable)
`HistoryPage` (store-reading container) / `HistoryView` (pure, prop-driven) **split** — because
**zustand's `useStore` reads the pinned INITIAL snapshot under `renderToStaticMarkup`** (getServerSnapshot),
so a store-reading component can't be seeded via `setState` in a plain `bun test`. Proven empirically
(a probe rendered `nodes:0` after `setState` set 1 node). The fix/idiom: test the pure view by props
(the Inspector precedent takes `item` as a prop too). Selector logic → `project.test.ts`; rendering →
prop-driven `*.test.tsx`; interaction → `*.pw.ts`.

### P20 (4-dim dynamic workflow) — PASS, 0 surviving blocker/major
6 raw findings → 4 confirmed (all lower-severity), all fixed + mutation-proven in `13a5587`:
1. **react-patterns (minor):** axis control claimed `role="tablist"` but broke the WAI-ARIA tabs keyboard
   model (arrows moved selection without focus; no roving tabindex). Fixed to match board `PlaneToggle`
   (roving tabindex + Arrow/Home/End move selection AND focus). **Empirically mutation-proven** (revert →
   the new pw test fails).
2. **correctness (nit):** empty-result end note said "…this search." even when the you/loop chip emptied
   the list → "No runs match these filters." (pw-proven).
3. **design-canon (nit):** empty-state title at weight 500 → `font-semibold` (§Type reserves 600 for
   empty-state titles; unit-proven).
4. **anti-slop (nit):** redundant ternary `{s.kind === "you" ? "you" : "loop"}` → `{s.kind}`.

CodeRabbit: 0 inline threads. CI: build/secrets/governance/quality all green.

## Bookkeeping done
- Linear BRO-1893 → In Progress + slice-1-DONE comment (kept open — 3 slices remain).
- Linear **BRO-1894 created** = FID-7 (command palette ⌘K `MccCommandPalette` + `MccFeedback` drawer +
  glass dialog/popover primitives — glass is canon there).
- Local main synced to `4483f71`; feature branch deleted.

## NEXT — FID-6 slice 2: Knowledge (BRO-1893, same umbrella ticket)
Port `ConceptKnowledge.jsx` (`MccKnowledge`) + `KgGraph.jsx` (`KgGraph`/`KgMiniGraph` force-directed
scope graph) + `KnowledgeApp.jsx` (`KG_SCOPES` demo data). Wire to `/knowledge` (replace the stub) + the
sidebar nav. **Honest data:** the store has no KG read path, so the scope graph uses `KG_SCOPES`
demo/fixture data — label it honestly (a "demo"/"sample" affordance), never present fixture as live.
Force-directed layout: watch perf + `prefers-reduced-motion` (stop the simulation). Same gates: branch
FIRST, tests + tsc + biome, dogfood light+dark, P20 4-dim ≥7 & 0 blocker/major, resolve CodeRabbit, merge
via REST squash.

Then: slice 3 Settings (`MccSettings` two-pane; orchestrator is NOT a settings page), slice 4 User
(`MccUser`), then the tab-strip integration (full-page views as chrome tabs). Then FID-7 (BRO-1894),
FID-8, then resume P3 (1805→1809→1813→1818→1821) + P4/P5.

## Gotchas reconfirmed this iteration
- vite preview serves prebuilt `dist` → **`bun run build` before every pw / screenshot** (run build from
  `apps/app`, the root has no `build` script).
- GitHub GraphQL rate-limited (`gh pr create` fails) → create PR + merge + comments all via **REST**
  (`gh api`). CodeRabbit posted 0 inline threads (satisfied).
- after `biome check --write`, the file is reformatted → re-check before commit (caught a one-line
  `.filter({hasText})` wrap).
