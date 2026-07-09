# Live signals

The running treatment is signature and the easiest thing in the system to get subtly wrong. It's a calm, contained weather effect — **presence, not progress.** All of it lives in `design-system/tokens/motion.css`; this file explains intent and the rules so you can package it as components without flattening it.

> **Principle:** calm is load-bearing. Live signals *breathe*; they never spin for attention. Motion encodes that an agent is present and working — not urgency, and never a percentage. Broomva runs **blue → indigo → cyan → ice**. No orange, no yellow, no red.

## The Undertow — THE running signal (cards)

A contained halo frame sitting **4px proud** of a card, holding three layers on three rhythms so the pattern never quite repeats:

- **Nebula pools** (`::before`, 4.2s) — two pools of blue/cyan light breathing and drifting down.
- **Tide band** (`::after`, 3.4s) — a band of ice-blue rising in **counter-phase** to the pools.
- **Orbit current** (`.bv-undertow-orbit`, 9s linear) — a faint conic sweep folded in as a slow current (this is the old border comet, retired into weather).

The card itself **stays matte** — the halo is the signal, not a glowing border. The frame borrows horizontal gutter (`margin: 0 -4px`) so card edges stay flush with siblings, while vertical space is real so stacked running cards never collide.

```html
<div class="bv-undertow">
  <span class="bv-undertow-orbit"></span>
  <!-- the matte card -->
</div>
```

Package as `<Card running>` (see `COMPONENTS.md`). Don't re-time the layers — the three different periods (3.4 / 4.2 / 9s) are what make it feel alive rather than looped.

## The tidepool dot — the running signal at dot scale

The same weather folded inside a 15px circle: a blue→ice gradient drifting vertically (`.bv-dot-live`, 3.2s). One motion language at every scale — use it for list rows, chips, status lines, the bench, and the orchestrator's presence chip in the chrome.

```html
<span class="bv-dot-live"></span>
```

Package as `<DotComet />`. `.bv-dot-comet` is kept as a compatible alias. **Only ever marks running/live work** — never a generic status dot.

## The pulse — the standing / listening dot

For states that are alive but asleep between beats (standing routines, the orchestrator listening): `.bv-dot--pulse`, a quiet 1s opacity breath. Quieter than the tidepool — it's waiting, not working.

## Hard rules

- **Everything stops under `prefers-reduced-motion: reduce`.** `motion.css` already gates all of it — preserve that when you port to components. A reduced-motion user must still be able to *tell* something is running (keep the static colored state visible), just without animation.
- **Blue → indigo → cyan → ice only.** Never add orange/yellow (those were Houston's; they're recolored out here) and never red.
- **No progress bars or percentages anywhere near a running signal.** The signal says "alive"; receipts (branch, diffstat, judge verdict, event timeline) say "how far".
- Retired and must not return: the **border comet** for running cards, and the orbiting **dot comet** as a separate element. Running cards wear the Undertow; dots wear the tidepool.
- Don't stack the Undertow on a glass surface or a pill — it's for matte cards.

## Reference
See the running (Undertow-wearing) live card in the Maestro prototype — `apps/maestro/WorkPlanes.jsx` — for the Undertow in context, and the prototype source in `design-system/tokens/motion.css` for exact keyframes.
