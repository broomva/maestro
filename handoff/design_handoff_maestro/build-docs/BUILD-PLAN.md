# Build plan

A sequence for a Claude Code session building Broomva greenfield. Each milestone is shippable and verifiable on its own — don't skip ahead, the later milestones lean on the earlier primitives. Read `CLAUDE.md` first; it's the contract every milestone must hold to.

**The pixel/behavior target is the Maestro prototype** — `apps/maestro/` at the design-project root, with `design_handoff_maestro/docs/canon-map.md` saying per file which exports are canon vs exploration, and `design_handoff_maestro/docs/porting-notes.md` saying how prototype code becomes production components (module translation, the state taxonomy, the `MccMaestroLoopV2` decomposition map, hardening bar). Per milestone: M2 shell → `WorkShell.jsx` (`McSidebar`) + `WorkPanel.jsx` (`McvTopBar`); M3 board → `WorkPlanes.jsx`/`WorkFeed.jsx`; M4 chat → `AiProtocol.jsx` + `PromptPlate.jsx`; M5 inspector → `WorkDetail.jsx`; M6 palette/overlays → `LiveCommand.jsx`.

---

## M0 — Scaffold & foundation
**Goal:** an empty app that already *feels* Broomva.

- Scaffold **Vite + React + TypeScript** + Tailwind v4 + shadcn. (The app is a SPA per `ARCHITECTURE.md` — Next.js is marketing-only. See `STACK.md`.)
- Wire the tokens per `TOKENS-INTEGRATION.md`: import `styles.css`, add the `@theme inline` block, set `data-theme` theme switching (light default, dark via attribute), keep the CalSans `@font-face` in `typography.css` with its font path correct (display only).
- Add Lucide.
- **Verify:** `bg-background text-foreground` is white / barely-blue ink in light and deep blue-purple in dark; focus rings are ai-blue; borders are whispers; nothing outside overlays has `backdrop-filter`.

## M1 — Primitives
**Goal:** the eight core components, matching their contracts.

- Build in order: Button → IconButton → Input → Avatar → StatusBadge → DotComet → Card → Composer (per `COMPONENTS.md`). Keep prop names identical to the `.d.ts` files. Use CVA for variants.
- Port the live signals to components per `LIVE-SIGNALS.md` (Undertow → `<Card running>`, tidepool → `<DotComet>`), reduced-motion preserved.
- Build a `/kitchen-sink` route rendering every variant/state.
- **Verify:** side-by-side against the primitives as used across `apps/maestro/` (see canon-map). Hover = frost or one-step lighten, never scale. Composer is the only thing with the halo.

## M2 — The shell
**Goal:** the chrome the whole product lives in. **It never scrolls.**

- 200px fixed sidebar (`--sidebar`, matte, cool-gray) + 52px top bar + flex main. Inner panels scroll, the shell doesn't.
- Sidebar: workspaces/agents nav, sentence-case items, Lucide icons, the blackhole brand mark on a dark/frosted surface (don't recolor it).
- Top bar: holds the orchestrator's **presence** (a live bench/chip using the tidepool dot — not a settings button) and the theme toggle.
- **Verify:** resize to small viewport — chrome holds, only inner panels scroll. The orchestrator reads as an agent with presence, not a menu.

## M3 — The board (work as the noun)
**Goal:** the mission board — each card is a unit of work.

- `<Card>` grid; running cards wear the Undertow + a `<DotComet>` status line; states use plain-voice `<StatusBadge>` (Queued · Running · Stuck · Needs you · Done · Standing).
- **Needs you** is the gate: accent-blue, surfaced first — never red, never a failure tone. No progress percentages anywhere.
- Selection drives the inspector (M5).
- **Verify:** a glance answers what changed · what it decided · what it asks. No fake progress.

## M4 — Chat (the projection)
**Goal:** the conversation surface — chat renders a session, never owns the work.

- Chat column max 768px. Feed of messages (agent/user/tool events), typing dots, the glass `<Composer>` pinned at the bottom with the halo.
- Inline assistant link pills (one of the five sanctioned color uses); bold real-world nouns, not UI elements.
- **Verify:** the same run could appear here or in a side panel — chat is a view, not the store.

## M5 — The inspector (receipts)
**Goal:** the right panel — the contract and the sessions doing the work. Rung 3: for verifying, not operating.

- ~45% viewport, 380px min. Selection-driven: item → look/chat/activity · folder → frontmatter + sessions · routine → frontmatter + runs.
- Show **receipts**: `run/<id>`, diffstats, judge verdicts, the event timeline. Never worktrees, `index.db`, or the engine room.
- Control is **verbs** (approve, send back, grant, point) — approving is the one human gate; no loop auto-completes.
- **Verify:** you can operate the product entirely from rungs 1–2 (feed + the gate); the inspector only confirms.

## M6 — Overlays, command palette, motion polish
- Dialogs + command palette on `.bv-glass-heavy` over `.bv-scrim`; popovers/menus/tooltips on `.bv-glass`. Each carries the inner light line.
- Enter/exit transitions (`opacity:0,y:8 → 1,0` / exit `y:-8`), morph transitions for panel resize / theme change. All under the reduced-motion gate.
- **Verify:** glass appears in exactly three contexts (overlays, popovers, composer) and nowhere else.

---

## Definition of done (every milestone)
- Holds every hard rule in `CLAUDE.md` (monochrome-by-default, glass earned, OKLCH only, plain voice, sentence case, no percentages, gates in accent-blue).
- Light **and** dark both correct.
- `prefers-reduced-motion` stops all animation while leaving state legible.
- Matches the Maestro prototype (`apps/maestro/` + canon-map) where a canon export exists, ported per `porting-notes.md` — not transplanted (no window globals, state in its taxonomy home, error boundaries + a11y bar met).
- No emoji in chrome, no Title Case, no warm grays, no invented hues.
