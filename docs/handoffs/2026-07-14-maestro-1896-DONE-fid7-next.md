# Maestro loop handoff — tab-strip work-surface scoping DONE → FID-7 (⌘K palette) next

**Date:** 2026-07-14
**Ticket:** BRO-1896 (tab strip is work-surface chrome) — **DONE**
**Merged:** PR #77, squash `da6769f`, **main tip `da6769f`**

## The pivot (read this first)

The slice-4 handoff queued "tab-strip integration" as *"full-page views also open as a chrome tab."*
**That is NOT prototype-faithful.** Before building, I verified against `handoff/apps/maestro/`:

- Every full-page view (`ConceptHistory.jsx` `MccHistory`, `ConceptKnowledge` `MccKnowledge`,
  `ConceptSettings.jsx` `MccSettings`, `ConceptUser.jsx` `MccUser`) renders its **own standalone frame**
  — `BvNavTree active="…" inApp onNav={onOpenView}` + `McvTopBar`, **NO tab strip**.
- The `mcc-ftabs` strip lives ONLY in the loop / mission shell (`ConceptMaestroLoop.jsx`
  `MccMaestroLoopV2`) and carries chat/session + file tabs. There is **never a "view" tab** —
  `onOpenView(id)` swaps the *whole frame* (leaves the loop shell entirely).

So "views-as-tabs" would be building *something different* → forbidden by the canon rule. The faithful
move was the **opposite**: remove the tab strip from where it doesn't belong.

## What shipped

Our single-shell SPA (BRO-1824: one `ShellLayout`, all views as child routes, one shared SSE stream)
was rendering `<TabStrip/>` + the FS pane on **every** route — so they leaked onto `/history`,
`/knowledge`, `/settings`, `/account`, where the prototype frames the view with no tab strip (and the
strip had no active tab there — a pinned-but-inactive Maestro tab + stale lingering file tabs).

**Fix:** `shell.tsx` gates both on `isWorkSurface(pathname)`:

```ts
function isWorkSurface(pathname: string): boolean {
  return pathname === "/" || pathname.startsWith("/file/") || pathname.startsWith("/session/");
}
// …
const workSurface = isWorkSurface(pathname);
const showFs = workSurface && fsOpen;   // FS pane is work-surface chrome too
// …
{workSurface ? <TabStrip /> : null}
```

The **sidebar + top bar stay on every route** (the way back to Maestro). `fsOpen` + open-file state
live in the store, so returning to the work surface restores the tabs — no loss.

**Files (all edits, no new files):**
- `apps/app/src/components/shell.tsx` — the `isWorkSurface` gate + doc comment
- `apps/app/src/components/shell.test.tsx` — +2 tests (a new "work-surface chrome" describe): present on
  `/`, `/file/$`, `/session/$` (incl. `fs-rpane`); ABSENT on the 4 view routes (tab strip AND `fs-rpane`),
  but `brand-mark` + `cmd-field` stay
- `apps/app/tests/routing.pw.ts` — +1 test: tab strip + FS pane on the board, gone on `/settings`, both
  return when you navigate back to Maestro
- `apps/app/src/components/shell/top-bar.tsx` — added `data-testid="cmd-field"` (a stable sentinel for
  the ⌘K field, replacing a brittle `class="mcc-cmd"` string match)

## P20 (4-dim, skeptic-verified): PASS, 0 surviving blocker/major, anti-slop 9/10

3 raised → 2 confirmed (both NITS, fixed + mutation-proven), 1 refuted.
- NIT — the unit ABSENT test asserted only tab-strip absence; the **FS-pane** `workSurface` gate had NO
  unit coverage (only pw). Added `not.toContain("fs-rpane")`. Discriminates because `fsOpen` defaults
  true → mutation-proven: a `showFs = fsOpen` regression leaks the pane onto views and the unit fails.
- NIT — `toContain('class="mcc-cmd"')` sentinel was brittle + weak → `data-testid="cmd-field"`.
- REFUTED — claimed the "⌘K field present on /account" assertion "needs revising when slice4 builds it";
  refuted because slice4 (`MccUser`) ALREADY shipped in #76, and the ⌘K field comes from the persistent
  shell `<TopBar>` (renders on every route), not the account view's own identity bar (which faithfully
  omits ⌘K). No change needed.

## CodeRabbit: clean, 0 comments (`mergeable_state: clean`)

## Gates
tsc + biome ci clean · **shell unit 9** + routing/fs/all-4-view pw green · full suite **1079 pass** ·
board-vs-Settings dogfooded (tab strip present on the board, absent on Settings, sidebar/header stay,
returns on nav back). Both gates mutation-proven (revert `workSurface` → unit ABSENT fails on tab strip;
revert `showFs` → unit ABSENT fails on `fs-rpane`; pw fails on both).

## Lesson (RULE-worthy)

**When an arc/ticket item's NAME implies a behavior, verify it against the prototype BEFORE building —
the faithful move can be the OPPOSITE of the name.** Here "tab-strip integration" read as "add view
tabs," but the prototype wanted the tab strip *removed* from the nav-view frames. Prototype > aspirational
done.check text when they conflict (CLAUDE.md "match the prototype" / START-HERE §2 wins).

## NEXT — FID-7 (BRO-1894): ⌘K command palette + overlays

Build `MccCommandPalette` — the ⌘K palette (`handoff/apps/maestro/` command-palette specimen) + any
overlay surfaces (feedback drawer). The plumbing is **already there**: `shell.tsx` dispatches
`bv:command-open` on ⌘K (global keydown) and on the top-bar `cmd-field` click, and `bv:feedback-open` on
the sidebar feedback affordance — the palette + drawer just need to mount and listen. The palette is a
**glass overlay** (one of the three sanctioned glass places) with the inner light line; it navigates via
TanStack `<Link>`/`useNavigate` (never raw `<a>`); §Voice + sample-labels apply to any inert command rows.

Arc after: FID-8 mobile / a11y (the sheet nav), THEN resume P3 (1805→1809→1813→1818→1821) + P4/P5.

Deferred follow-ups: **BRO-1895** (Knowledge slice 2b interactive graph + KG read-path seam),
**BRO-1889** (DS reconciliation — incl. a possible Button `destructive` variant), BRO-1892/1887/1885.

Driver: `~/broomva/docs/maestro-orchestration-loop.md`. Topic file: `[[maestro-build-arc]]`.
