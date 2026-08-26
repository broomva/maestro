# Maestro loop handoff — FID-6 slice 4 (User) DONE → tab-strip integration next

**Date:** 2026-07-14
**Ticket:** BRO-1893 (FID-6 full-page views) — **DONE** (last slice; the whole ticket closes here)
**This slice:** FID-6 slice 4 — the account / User page (`MccUser`)
**Merged:** PR #76, squash `7294e62`, **main tip `7294e62`**

## What shipped

Ported `MccUser` (`handoff/apps/maestro/ConceptUser.jsx`) — the account page. Two views in a local
top bar: an **Overview** dashboard (identity header + the autonomy-score hero + a two-column body:
Your sessions + Preferences) and an editable **Account** page (identity form · personal preferences ·
security · "where you're signed in" · sign out). Renders in the shell main like History / Knowledge /
Settings.

**Files (new unless noted):**
- `apps/app/src/components/account/account-page.tsx` — `AccountPage` (store container) + pure
  `AccountView` + module-level `OverviewPanel` / `AccountPanel` / `IdentityHeader`
- `apps/app/src/components/account/account-page.test.tsx` — renders BOTH panels in isolation
- `apps/app/tests/account.pw.ts` — hermetic integration (real shell, mocked tree/stream)
- `apps/app/src/styles/user.css` (+ `@import` in globals.css) — the `.usr-*` subset, tokens-only
- edited: `router.tsx` (wire `/account` → `AccountPage`), deleted `routes/stubs.tsx` (fully superseded)

## Key decisions / lessons

1. **Honest data (the Knowledge/Settings sample pattern).** Two real paths: *Your sessions* via
   `selectHistory(server)` (real work-state-derived runs, honest empty state), and *Theme* via
   `useThemeState` (live write-through, in sync with the top-bar toggle). Everything else is
   sample/preview and **labelled**: the autonomy hero is sample, the Overview Preferences card + the two
   inert account-view rows carry "preview" chips, the identity form says **"not saved yet"** (NOT the
   prototype's false "syncs to your profile"), receipts never a progress %.
2. **Store-read → SPLIT** (History/Inspector lesson): the store read lives in `AccountPage`; the pure
   `AccountView` is prop-driven. The two view branches are **module-level** `OverviewPanel` /
   `AccountPanel` (stable identity, no remount trap) so each half renders in isolation and the §Voice
   guards cover BOTH — not just the default Overview.
3. **`--bv-danger` red only for genuine destructive actions** (revoke a session, sign out); a
   needs-you / halt is never red. The weekly peak bar must NOT use `--bv-blue-accent` (hue 235,
   reserved for "Needs you") — use `--bv-blue` at full opacity (a value contrast on the same hue).
4. **Internal nav is TanStack `<Link>`**, never a raw `<a href>` — a raw anchor hard-reloads the SPA and
   tears down the shell's single shared SSE stream (BRO-1824). This was a P20 MAJOR.

## P20 (4-dim, skeptic-verified): PASS, 0 surviving blocker/major

9 raised → 7 confirmed → all fixed mutation-proven; 2 refuted (radius-2xl on the hero cards — the port
faithfully reproduces the prototype AND `--bv-radius-2xl` is token-sanctioned for "large cards").
- MAJOR — raw `<a href="/history">` hard-reload → `<Link to="/history">`. pw window-marker survives the
  nav (mutation: revert→a wipes it). **Strata-B PASS**.
- MAJOR — Overview Preferences "this device" receipt faked persistence over 4 inert toggles → honest
  "preview" chip. unit mutation: reintroduce "this device" fails the honesty test. **Strata-B PASS**.
- MINOR ×3 — peak bar hue; over-scoped "sample" tooltip (broadened); account-view Personal-prefs mixed
  live/inert (per-row "preview" chips).
- NIT — §Voice guards covered only the Overview → extracted the panels + added an `AccountPanel` §Voice
  describe. unit mutation: an em dash in the account-view copy now fails (previously never rendered).

## CodeRabbit: 2 minor, all threads resolved (`28f5a56`)

- No-op Sign out button styled destructive without a sample affordance → wrapped in a `.usr-signout` row
  with its own "sample" receipt (it sits outside the sample-labelled cards). Noted: `@maestro/ui` Button
  has no `destructive` CVA variant — a DS-reconciliation candidate (BRO-1889), not worth a
  governance-class variant for a sample no-op, so `--bv-danger` stays an inline one-off.
- Live-vs-sample disclosure was hover-`title`-only → added `aria-describedby` → a visually-hidden note
  (screen-reader announced) while the visible chip text stays exactly "sample".

## Gates
tsc + biome ci clean · **11 unit (+3) + 6 pw (+1)** green · full suite **1076 pass** · light + dark
dogfooded (peak bar + preview chip confirmed post-fix).

## NEXT — tab-strip integration

The FID-6 full-page views (History / Knowledge / Settings / Account) currently open from the sidebar nav
as routed pages. FID-4 shipped the chrome tab strip (`components/fs/tab-strip.tsx`, placement B). The
integration: a full-page view should also **open as a chrome tab** (BRO-1893 done.check: "opens both
from the sidebar nav AND as a chrome tab"). Wire the sidebar-nav / palette navigation to the tab strip
so History/Knowledge/Settings/Account register as tabs (like the FS file tabs), sharing the tab model.
Check whether this needs its own ticket or folds into FID-7.

Arc after: FID-7 (**BRO-1894** ⌘K palette / overlays — the palette navigates to these pages) → FID-8
mobile / a11y (the sheet nav opens these), THEN resume P3 (1805→1809→1813→1818→1821) + P4/P5.

Deferred follow-ups: **BRO-1895** (Knowledge slice 2b interactive graph + KG read-path seam),
**BRO-1889** (DS reconciliation — incl. a possible Button `destructive` variant), BRO-1892/1887/1885.

Driver: `~/broomva/docs/maestro-orchestration-loop.md`. Topic file: `[[maestro-build-arc]]`.
