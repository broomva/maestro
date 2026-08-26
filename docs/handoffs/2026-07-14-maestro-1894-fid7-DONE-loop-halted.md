# Maestro loop handoff — FID-7 (⌘K palette + feedback drawer) DONE → LOOP HALTED

**Date:** 2026-07-14
**Ticket:** BRO-1894 (FID-7: command palette + overlays) — **DONE**
**Merged:** PR #78, squash `5bc515b`, **main tip `5bc515b`**
**Loop status:** **HALTED** at the user's request ("stop on the next arc that closes"). No next-iteration wakeup fired. Resume by re-issuing `/loop` with the next ticket (FID-8).

## What shipped

The prototype's transient **overlay layer** — the ⌘K command palette + the feedback drawer — mounted once
via `OverlayHost` in the shell, driven by the existing `bv:command-open` / `bv:feedback-open` events
(⌘K keydown, top-bar command field, sidebar footer).

- **Command palette** (`components/overlays/command-palette.tsx`) — earned **glass** overlay (one of the
  three sanctioned glass places, §Glass) anchored under the top-bar command field via `data-cmdk-anchor`;
  type-to-filter with match highlight, arrow-key nav, Enter runs, Esc closes + **returns focus to the
  trigger** (WAI-ARIA combobox: `role=combobox` + `aria-activedescendant` + `role=listbox`/`option`).
  Portals to `<body>`; returns null before the portal when closed (SSR-safe). Split **container /
  pure-view** so §Voice + markup are unit-tested on the presentational half.
- **Feedback drawer** (`components/overlays/feedback-drawer.tsx`) — right-docked **matte** panel (a
  drawer is not glass); real local composer with an honest **preview** chip (delivery not wired) + a
  labelled **sample** thread history; focus-trapped, Esc-closes, focus-returns.
- **Honest data** (the load-bearing rule): the palette ships a **real command registry only** — navigate
  to a real route / toggle the real theme / open the real drawer; nothing that no-ops. No fabricated
  "start a session" / "wake maestro" / recent-artifact rows the store can't back. §Voice: "configure
  Maestro", not the prototype's "the engine room".

**Files:** new `components/overlays/{commands.ts, command-palette.tsx, feedback-drawer.tsx, focus-trap.ts,
overlay-host.tsx}` (+ 3 test files), `styles/{command.css, feedback.css}`; edited `components/shell.tsx`
(mount OverlayHost + `data-cmdk-anchor` gate), `components/shell/top-bar.tsx` (`data-cmdk-anchor`),
`styles/globals.css` (imports), `src/m0.test.ts` (matte-guard exemption). `tests/overlays.pw.ts` (9).

## P20 (4-dim, skeptic-verified): PASS, 0 surviving blocker/major

10 raised → **3 confirmed (2 distinct issues, both in the drawer)** → all fixed mutation-proven; 7 refuted.
- **MAJOR — feedback text-wipe.** The drawer's open effect keyed on `[open, onClose]`; `onClose` is a
  fresh inline arrow from `OverlayHost`, which re-renders on every SSE `server` update (Shell subscribes
  to the live slice) → the effect re-ran on ordinary background traffic and **wiped in-progress feedback
  text + reset the form + yanked focus**. Fix: key on `[open]` alone + read the latest `onClose` via an
  `onCloseRef`. The palette was already immune (its effect keys on `[open, place]`). **CodeRabbit
  independently found the same defect.** Locked by a non-vacuous pw test (holds the SSE stream, fires one
  `node.updated` mid-typing, asserts text survives + a guard the event actually applied).
- **MAJOR — no focus-return-to-trigger** on drawer close (WAI-ARIA). Fix: `restoreFocusRef` capturing the
  opener + restoring in cleanup, mirroring the palette. Locked by a pw test that waits for the textarea to
  take focus first (so the assertion isn't vacuous).
- **Strata-B delta re-verify: PASS (9/10)** — both fixes sound, no new regressions, palette immunity
  confirmed.

## CodeRabbit: 3 comments, all resolved

- text-wipe (same as P20 — fixed `d9f7688`);
- explicit named React type imports instead of the global `React.*` namespace (`2bb4ae7`);
- **a real canon catch**: the ported `.cmdk-item.is-active::before` blue **left-border strip** — CLAUDE.md
  §Color forbids colored left-border strips. Removed (+ the now-unused `position: relative`); the
  frosted-blue background (`var(--bv-frost-12)`) + the ↵ hint carry the active state.

## Gates
tsc + biome clean · **240 unit (+29)** pass · `overlays.pw.ts` (9) + routing + shell pw green · light +
dark dogfooded vs the prototype (faithful glass palette, matte drawer, honest labels).

## Load-bearing lessons

1. **Canon > prototype fidelity when they conflict.** The prototype's own `command.css` carried a canon
   violation (the left-border strip); porting faithfully is not the same as porting the canon. When the
   prototype's literal CSS conflicts with a §rule (CLAUDE.md), the §rule wins. (Also: "configure Maestro"
   over the prototype's "engine room" §Voice violation.)
2. **A modal `role=dialog` on the live surface must not key an effect on an inline parent callback.** Shell
   re-renders on SSE, so any child dialog whose reset/lifecycle effect depends on an inline `onClose` will
   re-fire on background traffic. Read the callback via a ref; key the effect on `[open]`. (P20 + CodeRabbit
   both caught it independently — a rule-of-two toward a possible lint/pattern.)
3. **Focus-return-to-trigger is a distinct leg** from focus-trap + Esc; easy to ship the first two and drop
   the third. Mirror the sibling that got it right, and make the pw test wait for focus to leave the
   trigger first or it passes vacuously.

## NEXT (when the loop resumes) — FID-8 (mobile / a11y)

The prototype's mobile/sheet form factor (`mobile.css` is canon per `docs/canon-map.md`): the sidebar
becomes a sheet nav, and the palette + drawer adapt to the bottom-sheet form. Also the broader a11y sweep.
After FID-8 the fidelity arc is complete → **resume P3** (1805 → 1809 → 1813 → 1818 → 1821) + P4/P5.

Deferred follow-ups: **BRO-1895** (Knowledge slice 2b interactive graph + KG read-path seam),
**BRO-1889** (DS reconciliation — incl. a Button `destructive` variant), BRO-1892/1887/1885. Possible new
candidate: a lint/pattern for lesson (2) above (inline-callback in a live-surface dialog effect).

Driver: `~/broomva/docs/maestro-orchestration-loop.md`. Topic file: `[[maestro-build-arc]]`.
