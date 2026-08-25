# Maestro loop handoff — FID-6 slice 2a (Knowledge) DONE → slice 3 (Settings) next

**Date:** 2026-07-14
**Ticket:** BRO-1893 (FID-6 full-page views) — kept **In Progress** (slices remain)
**This slice:** FID-6 slice 2a — the Knowledge page (`MccKnowledge`)
**Merged:** PR #74, squash `4798e97`, **main tip `4798e97`**

## What shipped

The prototype's Knowledge full-page view: *the context engine made visible as a graph that is
itself the filesystem* — every entity a node, every `related:` link an edge, every folder a node
you can enter (descending re-scopes the graph). `/knowledge` now renders the real `KnowledgePage`
(replaced the stub); scope nav is in-page (breadcrumb crumbs + folder nodes) so the shared shell
sidebar is untouched.

**Files (all new unless noted):**
- `apps/app/src/lib/kg.ts` — pure KG math: `kgEdges` (undirected dedup, drops self-loops + out-of-scope),
  `kgDegree`, `kgHash` (FNV-1a, layout seed), `kgLayout` (340-step **deterministic** force layout,
  computed once + memoised by scope — NOT a running sim), `kgPath` (breadcrumb + cyclic guard),
  `kgNeighbors`, `kgBacklinks`, `kgScore`/`kgCategory`/`kgIsScope`/`kgTypeColor`, `KG_TYPE`, `KG_GOLD`.
- `apps/app/src/lib/kg-data.ts` — `KG_SCOPES` + `KG_FRESH` **SAMPLE** fixture (8 scopes). Labelled
  `sample` in the UI; primitive labels sanitized of P-jargon per §Voice.
- `apps/app/src/components/knowledge/{kg-graph,kg-inspector,kg-list,kg-rail,knowledge-page}.tsx`
- `apps/app/src/styles/knowledge.css` (+ `@import` in globals.css)
- Tests: `lib/kg.test.ts`, `components/knowledge/knowledge-page.test.tsx`, `tests/knowledge.pw.ts`
- `router.tsx` + `routes/stubs.tsx` edited (wire `/knowledge`, drop the stub).

## Key decisions / lessons (carry forward)

1. **Honest data, rule-of-FOUR → now FIVE.** The client store has NO knowledge-graph read path
   (same as no-session-read-path). So the graph uses `KG_SCOPES` sample data, labelled `sample` —
   never faked as live, no counts, no progress %. Swaps behind the same components when a real KG
   read path lands (**BRO-1895**).
2. **KnowledgePage is store-FREE** (only `useState`) → it renders directly under
   `renderToStaticMarkup`. No HistoryPage-style container/view split was needed (that split exists
   only because a *store-reading* component pins the initial snapshot under SSR). Test the page
   directly; test pure sub-components (KgInspector/KgListView/KgGraph) by props.
3. **The "derived view disagrees with itself" trap struck again (RULE-OF-FIVE).** P20 caught
   `kgBacklinks` counting a reflexive `related:` link as a self-backlink while `kgEdges` drops
   self-loops — the two derived views of the same data would disagree once real frontmatter (which
   can carry reflexive links) replaces the fixture. Guard: `n.id !== node.id`. Latent, caught past
   green CI. (Prior four: FID-1 badge over-count, FID-4 decode crash, FID-5 canceled fabricated
   progression, FID-6 History sessionId-keyed emptiness.)

## P20 (4-dim, skeptic-verified): PASS, 0 surviving blocker/major

11 raised → 10 confirmed, all **minor/nit**, all fixed in `6bd45fd`. Two substantive ones
**empirically mutation-proven** (revert source → new test fails):
- `kgBacklinks` self-loop guard — unit test fails against pre-fix (returns `["self"]`).
- graph type-filter a11y parity (off-type nodes get `tabIndex=undefined` + `aria-hidden`, matching
  the List which drops filtered rows) — component test fails against pre-fix (11 tab stops, expected 2).
Plus: drawer focus mgmt (focus→Close on open, Escape closes, restore on close; pw-proven),
tablist/tabpanel association, §Voice em-dash in the `sample` tooltip, rail React key `::` separator
(latent collision — scope/node ids share a flat namespace), a vacuous-assertion replacement, LEGEND
tuple collapse, per-chip KG_TYPE double-lookup hoist.

## Gates
tsc + biome clean · **189 unit (+3) + 5 pw (+1) green** · CI 4/4 green · light+dark+drawer dogfooded
(the 10 fixes change no rendered pixels — a11y/correctness/dedup/tooltip only).

## NEXT — FID-6 slice 3: Settings (`MccSettings`)
Port the prototype's Settings full-page view — the **two-pane section nav** (left section list,
right pane). The **orchestrator is NOT a settings page** (canon) — Settings is app/account config,
not orchestrator control. Same process: branch FIRST, build to fidelity, tsc+biome+bun test,
`bun run build` before pw, dogfood light+dark, P20 4-dim ≥7 & 0 surviving, fix mutation-proven,
merge green via REST squash, bookkeep, `ScheduleWakeup` the next iteration.

Arc queue after slice 3: slice 4 User → tab-strip-integration → FID-7 (BRO-1894 palette/overlays) →
FID-8 mobile/a11y, THEN resume P3 (1805→1809→1813→1818→1821)+P4/P5.
Deferred: **BRO-1895** (slice 2b interactive graph + KG read-path seam).

Driver: `~/broomva/docs/maestro-orchestration-loop.md`. Topic file: `[[maestro-build-arc]]`.
