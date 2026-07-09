# Broomva — Claude Code handoff

> **New here? Read `../START-HERE.md` first** for the map across all the docs. This bundle is
> the **build lens** (greenfield architecture + roadmap); the design lens is `../README.md`.

Everything a Claude Code session needs to build **Broomva** (a work-orchestration AI agent product) greenfield, in the Broomva visual language: Houston's calm monochrome philosophy on an Arcan-blue axis.

## How to use this

1. Drop this folder's contents at the **root of the new repo** (or alongside your scaffold). `CLAUDE.md` is written to live at the repo root so Claude Code loads it automatically.
2. Start a Claude Code session and point it at **`ROADMAP.md`**. Tell it: *"Read `CLAUDE.md`, then build P0 from `ROADMAP.md`."*
3. Work phase by phase. Each one is shippable and has its own exit test.

## What's here

| File | What it is | Read it when |
|---|---|---|
| **`CLAUDE.md`** | The always-on ruleset — product model + every hard design rule. Lives at repo root. | First, always. |
| **`ARCHITECTURE.md`** | System topology — 24/7 runtime + relay + thin clients, FS-as-truth + derived index, isolation, stack. | Before any infra. |
| **`STACK.md`** | The concrete tool choices and why — Bun runtime as supervisor, Vite SPA, the Rust boundary, the parallelism model. | With `ARCHITECTURE.md`. |
| **`AUTONOMY.md`** | The loop logic that *is* Maestro — four nested loops, verifier, the gate, guardrails. | The autonomy work. |
| **`DATA-MODEL.md`** | The work-contract frontmatter + the control-plane index tables (FS-as-truth, derived index). | Scaffolding the data layer. |
| **`FLOWS.md`** | Sequence specs for every core flow — dispatch, the agent beat, verify, the gate, the tick, triggers, kill, recovery, chat. | Implementing any flow. |
| **`API.md`** | The wire surface — runtime routes, the event stream + cursor, the intent union, chat protocol, relay envelope. | The API layer; `packages/protocol`. |
| **`PATTERNS.md`** | The design patterns and the line each must not cross, plus the anti-pattern list. | Before deviating from anything. |
| **`ROADMAP.md`** | Phases P0–P6 across all tiers with exit tests — including P5, where Maestro builds Maestro. | Throughout — the master sequence. |
| **`TOKENS-INTEGRATION.md`** | How `design-system/tokens/*` become `globals.css` + Tailwind v4 `@theme`. | M0. |
| **`COMPONENTS.md`** | Build spec for the eight core primitives (variant → token → state). | M1. |
| **`LIVE-SIGNALS.md`** | The Undertow / tidepool dot / pulse — the running signal, with its rules. | M1, M3. |
| **`BUILD-PLAN.md`** | The UI track: sequenced milestones M0–M6, each with a verify step. Slots into `ROADMAP.md`'s phases. | The UI work. |
| **`specs/VERIFIER.md`** | Loop 2 in full — the `done:` schema, tamper/diff guard, verdict + rubric formats, the feedback wire. | P3; before anything runs unattended. |
| **`specs/HARNESS.md`** | The supervisor↔child contract — spawn/env, stdio protocol, the budget-metering model proxy, exit codes. | P2; the process seam. |
| **`specs/ORCHESTRATOR.md`** | The maestro's tick — briefing, decision policy, intent subset, capability grants, wake log. | P4. |
| **`specs/DECISIONS.md`** | The decision record — git-on-approve, the data-model open questions, notifications, threat model, testing. | Before arguing with a default. |
| `design-system/` | **Source of truth.** Tokens, fonts, logo, full philosophy (`readme.md`), agent manifest (`SKILL.md`), and the typed component contracts (`components/core/*.d.ts` + `*.prompt.md`). | Continuously. |
| `reference/` *(removed)* | The old desktop-shell kit was **dropped** — it predated the Maestro loop and drifted from canon. The pixel/behavior target is the Maestro prototype: `apps/maestro/` at the design-project root, mapped export-by-export in `design_handoff_maestro/docs/canon-map.md`. | M1–M6, to match. |

## The 30-second version

- **Light-first**, white canvas, **barely-blue ink** `oklch(0.175 0.022 265)` that reads as black until you look. Cool monochrome (every gray at hue 265). Dark mode is fully specified — deep blue-purple, never black.
- **Color earns its place:** ai-blue (260) only, for focus / hover-frost / the composer halo / the running glow / status. No decorative color. All OKLCH.
- **Glass is earned** — overlays, popovers, and the composer only. Everything else is matte.
- **Voice:** plain-language, second person, sentence case. "Needs you," not "In Review." No emoji, no em dashes, no Title Case.
- **Product thesis:** the scarce resource is unsupervised hours. Work is the noun; sessions do it; the orchestrator is an agent; the gate is the human's. Show receipts, never percentages.

## Notes & open items

- **Stack is a recommendation, not a lock** (Vite + React SPA + Tailwind v4 + shadcn + Lucide; Next.js for marketing only) — but the tokens are already shaped for it, so deviating costs you the free integration. See `STACK.md` and `TOKENS-INTEGRATION.md`.
- The `components/core/*.jsx` files are **Babel-runtime prototypes** — read them for markup, but re-author as real app primitives (the `.d.ts` defines the API). Don't ship them as-is.
- **The blackhole logo is raster only** (`design-system/assets/broomva-blackhole-logo.png`). A vector version would help small sizes — request the SVG from Broomva.
- **CalSans ships SemiBold only** and is display/marketing-only — never in app chrome.
