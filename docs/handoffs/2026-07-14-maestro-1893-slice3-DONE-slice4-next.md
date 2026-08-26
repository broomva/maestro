# Maestro loop handoff — FID-6 slice 3 (Settings) DONE → slice 4 (User) next

**Date:** 2026-07-14
**Ticket:** BRO-1893 (FID-6 full-page views) — kept **In Progress** (slice 4 remains)
**This slice:** FID-6 slice 3 — the Settings page (`MccSettings`)
**Merged:** PR #75, squash `3a873e0`, **main tip `3a873e0`**

## What shipped

Ported `MccSettings`: seven config sections (runners · credentials · autonomy · routines ·
notifications · appearance · members) in **two layouts** — a two-pane section nav and a one-scroll
editorial with a **scroll-spy** table of contents, toggled in the local top bar. `/settings` renders
the real `SettingsPage` (stub dropped); it lives in the shell main like History + Knowledge.

**Files (new unless noted):**
- `apps/app/src/components/settings/settings-page.tsx` — the page (store-free; local useState + theme)
- `apps/app/src/components/settings/controls.tsx` — Switch / Segmented / Stepper / Slider (accessible)
- `apps/app/src/components/settings/settings-data.ts` — the SAMPLE fixtures
- `apps/app/src/components/settings/settings-page.test.tsx`, `apps/app/tests/settings.pw.ts`
- `apps/app/src/styles/settings.css` (+ `@import` in globals.css) — the `.set-*` subset
- edited: `router.tsx`, `routes/stubs.tsx`, **`theme.ts`** (new `useThemeState` hook),
  **`components/theme-toggle.tsx`** (now uses the shared hook)

## Key decisions / lessons

1. **Canon: the orchestrator is NOT a settings page.** Settings is app/account config; dropped the
   prototype's "the engine room" framing per the disclosure ladder ("preferences and access").
2. **Honest data (the Knowledge sample pattern).** Appearance › Theme is the ONE live control (real
   theme module → `data-theme` on `<html>`). The engine-room sections have no runtime path → SAMPLE
   rows under a **"preview"** chip, controls local-only, never faked persisted. The fake "Saved"
   indicator became the "preview" chip.
3. **Store-free page renders directly** under `renderToStaticMarkup` (no container/view split — that
   split is only for store-reading components).
4. **NEW rule-worthy lesson — component-defined-in-render is a MAJOR remount trap.** `Section` was
   defined inside `SettingsPage`'s render and used as `<Section/>`; new function identity each render
   → React remounts the whole subtree on every interaction → lost segmented keyboard focus, broke
   slider drag, and **reset the uncontrolled workspace-name input to "Broomva"**. Both P20 AND
   CodeRabbit flagged it independently. Fix: `renderSection(id)` **called inline**
   (`{renderSection(active)}`), no reconciliation boundary. When a render helper closes over parent
   state, CALL it — never `<Comp/>`.

## P20 (4-dim, skeptic-verified): PASS, 0 surviving blocker/major

6 raised → 4 confirmed (1 major, 2 minor, 1 nit), all fixed in `d46d007`.
- MAJOR (Section remount) — mutation-proven (reintroduce a fresh-identity wrapper → keyboard-nav +
  input-retention pw tests fail) + **Strata-B delta re-verify PASS** (fresh-context adversary, 6 axes).
- MINOR (theme desync) — the segmented + the top-bar toggle were two independent mount-time mirrors
  over `<html data-theme>`; fixed with a shared `useThemeState()` MutationObserver hook in both.
  Mutation-proven (remove observer → sync test fails).
- MINOR (segmented keyboard nav untested) — added a pw test driving arrow keys.
- NIT (settings.css "Tokens-only" overclaim) — fixed by tokenizing all switch values.

## CodeRabbit: 4 threads, all resolved (`2350a05`)

Section remount (already fixed) · **hardcoded ToC → IntersectionObserver scroll-spy** (mutation-proven:
revert to `i===0` fails the scroll-spy test) · raw CSS → tokens (`--bv-gray-300/800`, `--bv-white`,
`--bv-shadow-edge`) · slider missing focus ring → `:focus-visible`.

## Gates
tsc + biome clean · **198 unit (+9) + 8 pw green** · CI 4/4 green · light + dark + both layouts dogfooded.

## NEXT — FID-6 slice 4: User (`MccUser`)
Port the prototype's User/account page. The `.usr-*` styles are ALREADY in the prototype's
`handoff/apps/maestro/settings.css` (lines ~173-287) — port that subset into a new
`apps/app/src/styles/user.css` (or extend settings.css). The prototype component is `ConceptUser.jsx`
(`MccUser`). It has: an identity header (avatar + name + role), an autonomy-score hero (3 stats +
weekly bars), and a two-column body of cards (recent sessions / preferences / security). Same honesty
rule: no user/account backend read-write path yet → SAMPLE/preview data labelled honestly; wire only
what has a real path. Wire `/account` → the real component (drop the `AccountView` stub in
`routes/stubs.tsx`). Same process: branch FIRST, build to fidelity, tsc+biome+bun test, `bun run build`
before pw, dogfood light+dark, P20 4-dim ≥7 & 0 surviving, fix mutation-proven, merge green via REST
squash, bookkeep, `ScheduleWakeup` the next iteration.

Arc after slice 4: tab-strip-integration → FID-7 (BRO-1894 palette/overlays) → FID-8 mobile/a11y,
THEN resume P3 (1805→1809→1813→1818→1821)+P4/P5. Deferred: **BRO-1895** (slice 2b interactive graph +
KG read-path seam).

Driver: `~/broomva/docs/maestro-orchestration-loop.md`. Topic file: `[[maestro-build-arc]]`.
