# Handoff: Broomva Maestro

> **New here? Read `START-HERE.md` first.** It's the map across all the docs — canon
> ownership, the build spine, and which lens (this doc = design, `build-docs/` = build) you
> want. This file is the **design lens**: the screens, interactions, and fidelity target.

## Overview

Maestro is Broomva's work-orchestration app: a chat-first surface where an orchestrator agent ("the maestro") schedules agent sessions against a filesystem workspace. Work is the noun (folders with frontmatter contracts), sessions are the verb, chat is a projection. The user's job is to command, observe, and decide — approvals at "the gate" are the one human verb.

The handoff is the whole project: the working design prototype (`apps/maestro/`), the full design system (tokens, components, guidelines), and the contract docs in `design_handoff_maestro/docs/`.

## About the design files

The prototype (`apps/maestro/`) and the design system around it are **design references built in HTML** — prototypes showing intended look and behavior, not production code to copy directly. The app runs React 18 dev builds with in-browser Babel transpilation; that is a design-iteration rig, not an architecture. **The task is to recreate these designs in a real build environment** (Vite/Next + React is the natural fit since the prototypes are already React JSX, but any stack works) using proper modules, a bundler, and production React.

That said, the JSX is deliberately close to portable: components are plain function components, styling is CSS custom properties + class-based CSS, and the data/wire contracts are already specified (see `docs/data-contract.md`). Much of the component logic can be lifted with mechanical changes (globals → imports).

## Fidelity

**High-fidelity.** Colors, typography, spacing, radii, shadows, motion, and copy are final and token-driven. Recreate pixel-perfectly. The one caveat: seed data (work items, chat transcripts, knowledge graph) is demo content — shapes are canon (see data contract), values are not.

## Where to start

1. `docs/canon-map.md` — what each file is, which exports are shipped vs exploration.
1b. `docs/porting-notes.md` — how prototype code becomes production components: the mechanical translation table, the state taxonomy (store / persisted prefs / local) with the `MccMaestroLoopV2` decomposition map, and the hardening bar (error boundaries, memoization, a11y, tests).
2. `docs/data-contract.md` — the work model, state machine, and AI wire protocol.
3. `docs/production-notes.md` — decisions to make before/while building (service worker, icons, CSS layering, asset gaps).
4. `readme.md` — the full design language (philosophy, color, type, glass, motion, voice).
4b. `Maestro Architecture.html` — the system diagram: 24/7 runtime · thin relay · projection clients, the four loops, FS-as-truth + derived index.
4c. `build-docs/` — the complete Claude Code build bundle: `ARCHITECTURE.md`, `AUTONOMY.md`, `STACK.md`, `DATA-MODEL.md`, `FLOWS.md`, `API.md`, `ROADMAP.md`, `BUILD-PLAN.md`, plus the design ruleset (`CLAUDE.md`, `PATTERNS.md`, `COMPONENTS.md`, `TOKENS-INTEGRATION.md`, `LIVE-SIGNALS.md`) and packaged tokens/fonts (`design-system/`). Start at `build-docs/README.md`.
4d. `build-docs/specs/` — the deep build specs closing the bundle's gaps: `VERIFIER.md` (Loop 2: done-schema, tamper guard, verdict/rubric formats), `HARNESS.md` (supervisor↔child seam, model proxy, stdio protocol), `ORCHESTRATOR.md` (the tick: policy, tools, grants), `DECISIONS.md` (git-on-approve, the data-model open questions, notifications, threat model, testing strategy).
5. Open `apps/maestro/index.html` to see the living reference (serve the project root statically; paths resolve in place). **This prototype is the sole pixel/behavior target** — the old `reference/desktop` shell kit was dropped to avoid drift; `docs/canon-map.md` says which exports are canon, and `docs/porting-notes.md` says how prototype code becomes production components.

## Screens / views

All desktop views share one chrome: 200px fixed sidebar (workspace tree + autonomy footer), 52px top bar (bench + tick timer + command field), flex main. **The shell never scrolls; inner panels scroll.** Chat column max 768px. Right panel ~45% viewport, 380px min. All fixed columns drag-resize.

- **Maestro (home)** — `ConceptMaestroLoop.jsx` (`MccMaestroLoopV2`, `initialMode="mission"`). Two states of one layout: *mission* mode grows the work plane (feed / board / list toggle) with chat docked right; clicking a workspace/folder collapses the plane to a dock and the conversation takes center. Tab strip: sessions on the left, files open on the right from the FS pane. The gate queue (approval cards) rides above the composer.
- **The composer** — `PromptPlate.jsx`. Two storeys: text + ⌘L hint above, dispatch rail (model · effort · scope · autonomy) below. 28px radius, glass, the frosted-blue halo (`--bv-shadow-composer`) — the single dramatic depth cue in the app.
- **Work detail / inspector** — `WorkDetail.jsx`. One object, three projections: lifecycle rail (proposed → queued → running → review → done), activity timeline (receipts), chat. Approve / Send back are the only gate controls.
- **Knowledge** — `KnowledgeApp.jsx` + `KgGraph.jsx`. The context engine as a graph that *is* the filesystem: files are nodes, `related:` frontmatter links are edges, folders are enterable scope nodes (zoom-morph). Search drives the graph live and doubles as a command palette. Detail drawer with neighborhood mini-graph, filter chips, graph⇄list toggle, minimap.
- **History** — `ConceptHistory.jsx`. All sessions, yours and the loop's. Four organizing axes (day · work · agent · lineage) + a you/autonomous filter.
- **Settings** — `ConceptSettings.jsx`. "The engine room": runners, credentials, autonomy, routines, notifications, appearance, members. Note: the *orchestrator* never lives here — it's an agent with presence in the chrome, not a settings page.
- **Account** — `ConceptUser.jsx`. Identity + the autonomy score (unsupervised hours), Overview and Account views.
- **Feedback** — `ConceptFeedback.jsx`. Right-docked drawer; feedback is a tracked thread handed to the loop (one item shown already running with the Undertow).
- **⌘K palette** — `LiveCommand.jsx`. Glass, anchored under the command field, keyboard-driven, actually navigates.
- **Mobile / tablet shell** — `MobileShell.jsx`. Chat-first; gate queue above the prompt; Chat · Mission · Files surfaces. Ship the **sheet** nav model (chat is the canvas, Mission/Files rise as pull-up sheets) — the pager/menu/edge variants in the Tweaks panel are explorations.

## Interactions & behavior

- **Selection drives both surfaces**: picking a tree rung scopes the plane and retunes the inspector (root → meta-contract, folder → contract + sessions, item → look/chat/activity).
- **The gate**: clean runs land at "Needs you", never auto-Done. Gate cards compress a run to *what changed · what it decided · what it asks*, with Approve / Send back. Halts are accent-blue, never red.
- **Live signals**: running work wears the Undertow (contained 4px breathing halo + faint 9s orbit — `tokens/motion.css`); status dots wear the tidepool dot. Presence, not progress; no spinners, no percentages.
- **Motion**: feedback < 300ms (200ms common, `--bv-ease-standard`); enter `opacity 0, y8 → 1, 0`; morphs (panel resize, theme) 500ms `--bv-ease-morph`. Everything stops under reduced motion.
- **Hover** = frosted-blue fill (`--bv-frost-8`) or one-step lighten; pressed one step deeper; no scale/rotation/glow.
- **Streaming chat**: messages stream per the AI SDK UI Message Stream Protocol; the tick receipt is a `data-tick` part that updates in place; gate cards are `data-gate` parts reconciled by id. See `docs/data-contract.md`.
- **Keyboard**: ⌘K palette (global), ⌘L focus composer. Full arrow-key nav inside the palette.
- **Theme**: light/dark toggle; `data-theme="dark"` on the root; PWA `theme-color` meta kept in step.
- **Responsive**: `useBvViewport` picks desktop / tablet / phone; below desktop the mobile shell replaces the chrome entirely.

## State management

Prototype state lives in React component state + globals; production needs:

- **Work items** — the canonical store (shape in `docs/data-contract.md`); selection state (tree rung, open item, open session); plane view mode (feed/board/list) per scope.
- **Chat sessions** — UIMessage arrays per session, driven by a `ChatTransport` (the prototype ships mock Anthropic/OpenAI/harness transports in `AiProtocol.jsx` that a real backend replaces 1:1).
- **The gate queue** — derived from work items in `review`/`blocked`, ordered attention-first.
- **User prefs** — theme, density (`data-density`), persisted; the prototype uses localStorage via the Tweaks panel.
- **Tweaks panel knobs** (`MaestroApp.jsx`) are design-review controls, not product features — except density and theme, which map to Settings → Appearance. Chat length (short/stress/extreme) selects seed transcripts for stress-testing; drop in production.

## Design tokens

`styles.css` imports the complete set (158 tokens): `tokens/colors.css` (ink, cool grays at hue 265, ai-blue 260 / accent-blue 235, frost, borders, status, shadows, full dark theme), `tokens/typography.css` (system stack, 12/14/16/18/22/24/28 scale, CalSans display opt-in), `tokens/spacing.css` (4px base, radius ladder 0.25rem → 28px composer, heights, durations), `tokens/glass.css` (glass utilities + light line + scrim), `tokens/motion.css` (Undertow, tidepool dot, standing pulse). **Consume tokens, never raw values.** All colors are OKLCH; tint with `color-mix(in oklab, …)`.

## Assets

- `assets/broomva-blackhole-logo.png` — brand mark (raster; **request the SVG**, see production notes).
- `fonts/CalSans-SemiBold.ttf` — display font, hero/marketing only, semibold-drawn at any weight.
- Icons: Lucide, but the app uses **inline path data** (`McIcon` in `apps/maestro/WorkData.jsx`), no CDN. See production notes.

## Files

The download is the whole project; this folder holds the docs.

- `apps/maestro/` — the Maestro prototype. Entry: `index.html`. File-by-file triage in `docs/canon-map.md`.
- `styles.css` + `tokens/` — the token source of truth.
- `components/` — the standard components (core, forms, navigation, overlays, work), each with `.jsx`, `.d.ts`, and a `.prompt.md` usage doc.
- `guidelines/` — specimen pages for every foundation and primitive.
- `ui_kits/desktop/styles.css` — app-shell base classes Maestro depends on.
- `assets/` + `fonts/` — brand mark, CalSans.
- `readme.md` + `SKILL.md` — the design-language canon.
- `design_handoff_maestro/docs/` — canon map, porting notes, data contract, production notes.

Ignore: `uploads/` (research scraps), `_ds_bundle.js` / `_ds_manifest.json` / `_adherence.oxlintrc.json` (design-tool compiler output), `Design System.html`, `templates/` (design-tool starting points).
