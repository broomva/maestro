# Maestro — agent working rules

You are building **Maestro**, Broomva's work-orchestration AI agent product. This file is
the repo-root **always-on contract** — read it fully before writing code, and keep it loaded.
It carries two things: the **design canon** (below) and a pointer to the **governance stack**
(§Governance). The long-form rationale lives in
`handoff/design_handoff_maestro/build-docs/design-system/readme.md` and
`handoff/design_handoff_maestro/build-docs/design-system/SKILL.md` — read those once at the
start of a session; treat the rules below as canon.

> **Canon rule (START-HERE §2 wins).** Where two docs describe the same thing, the owner in
> `handoff/design_handoff_maestro/START-HERE.md §2` is the single source of truth; every other
> doc defers to it. Design tokens are consumed as the `@maestro/tokens` package — never copy
> raw values.

## What Maestro is

A **chat-first agent product** for orchestrating loops of agentic work under human control.
The product thesis, in one line: **the scarce resource is unsupervised hours** — how long an
agent runs before a human must look. Every design choice exists to make that legible and calm.

Core model (do not re-invent it — it shapes the data layer and the UI):

- **The workspace is the substrate.** Work is files + folders in an FS/sh environment. A folder
  is work at any scale (question → task → project → initiative); depth is meaning, not a fixed
  schema. Frontmatter (`kind`, `state`, `owner`, `budget`, `gate`) is the orchestration contract
  and lives in the files.
- **The session is the verb.** A running agent timeline of events (agent, user, tools). Work is
  the noun; sessions do the work.
- **Chat is a projection.** A session renders work; it never owns it. The same run can appear in
  a side panel, a thread, or a handoff page.
- **The orchestrator is an agent**, not a settings page — it has presence in the chrome, a
  session you can open, and a wake log. Schedules are set in a sentence.
- **The branch is the receipt.** Show evidence (`run/<id>`, diffstats, judge verdicts), never
  fake progress percentages.
- **The gate is the human's.** No loop auto-completes. Clean runs wait at **"Needs you."**
  Needing you is a gate, not a failure — render it in accent-blue, never red.

## Recommended stack (greenfield)

Nothing here is locked, but the design tokens are already shaped for this stack — deviating
costs you the free integration:

- **Vite + React + TypeScript** — one SPA delivered as web, PWA, and Tauri shell. Next.js is for
  the marketing site only (see `handoff/design_handoff_maestro/build-docs/ARCHITECTURE.md` and
  `handoff/design_handoff_maestro/build-docs/STACK.md`).
- **Tailwind CSS v4** with `@theme inline`. The tokens already expose the shadcn semantic layer
  (`--primary`, `--ring`, `--muted-foreground`, `--sidebar`, …) for light and dark. See
  `handoff/design_handoff_maestro/build-docs/TOKENS-INTEGRATION.md`.
- **shadcn/ui** as the primitive base, restyled to Broomva. Variants via **CVA**.
- **Lucide** icons (already the system's icon set).
- CalSans via the `@font-face` in `typography.css` (display only). System stack for everything else.

Model the domain around **workspace / session / orchestrator**, with the 8-state OrchState enum
(`proposed · reviewing · triggered · running · blocked · review · done · canceled`) mapped to
plain-language UI states (Queued · Running · Stuck · Needs you · Done · Standing). Runtime is
Bun + Hono compiled to a single binary; the index is libSQL embedded via `drizzle-orm/libsql`.

## The hard rules (these are easy to violate — don't)

**Color**
- Barely-blue ink `oklch(0.175 0.022 265)` on white, never pure `#000`. Never pure-white text in dark mode.
- **Monochrome by default.** Every neutral sits on the cool axis (hue 265, chroma ≤ 0.01). No warm grays.
- Color earns its place in exactly five situations: the running glow, status pills, agent avatar accents, inline assistant link pills, and the frosted-blue interaction layer (hover / selected / focus / glow).
- **One hue family:** ai-blue (260). accent-blue (235) only when two accents must coexist — it owns "Needs you." No decorative color, no gradient washes, no colored left-border strips.
- **All colors are OKLCH.** Tint with `color-mix(in oklab, …)` or alpha. Never invent a hex.

**Glass** — appears in **exactly three places**: overlays (dialogs, command palette), popovers (menus, dropdowns, tooltips), and the composer. Every glass surface carries the inner light line `inset 0 1px 0 var(--glass-light-line)`. **Cards, panels, sidebars, headers are matte. Always.**

**Shadows** — two tiers, all blue-tinted: the 1px edge shadow at rest, and the composer halo (the one dramatic depth cue). Hover lifts cards to a diffuse blue-tinted shadow. No inner shadows except the glass light line.

**Radii** — Houston's ladder, unchanged: chips `0.25rem` → inputs `0.375rem` → rows/icon-buttons `0.5rem` → cards `0.75rem` → dialogs `1rem` → composer `28px` → pills `full`. **Pill radius is for buttons and avatars only — never cards.**

**Type** — system stack; 400 default, 500 for buttons/section headers, 600 for empty-state titles only. Tight scale: **12 / 14 / 16 / 18 / 22 / 24 / 28**. Nothing else. CalSans is opt-in for hero/marketing headings only (`data-display-font="calsans"`) — **never in app chrome.**

**Motion** — feedback under 300ms (200ms common). Enter `opacity:0, y:8 → 1, 0`; exit `y:-8`. The signature live signal is the **Undertow** (see `handoff/design_handoff_maestro/build-docs/LIVE-SIGNALS.md`). Calm is load-bearing: motion encodes presence, not urgency. No bouncy entrances, no scale/rotate on hover. **Everything stops under `prefers-reduced-motion`.**

**Work states** — plain voice is canon: **Queued · Running · Stuck · Needs you · Done** (plus **Standing** for routines). The system enums (Todo, InProgress, Blocked, InReview) are a developer surface only. The dot carries the color: gray / info / warning / accent-blue / success. **Never show progress percentages** — show receipts (branch, diffstat, judge verdict, timeline).

**Voice** — plain-language, second person, lead with the verb. "Needs you," not "In Review." "New mission," not "Create task." Sentence case everywhere; no Title Case buttons, no UPPERCASE eyebrows, no wide letterspacing. **No emoji in chrome. No em dashes in user-facing copy.** No "Welcome!", no celebration, no marketing superlatives.

**Icons** — Lucide only, 20px standard / 16px inline / 24px empty-state, stroke 2px, round caps, `currentColor`. No fill icons, no mixed libraries, no emoji-as-icons.

**Layout** — 200px fixed sidebar + 52px header + flex main. Chat column max 768px. Right panel ~45% viewport, 380px min. 4px spacing base. **The shell never scrolls; inner panels do.**

**The disclosure ladder** — the substrate is for agents; the user gets signals, verbs, and receipts. Never expose worktrees, `index.db`, or the engine room in the UI. Rung 1 ambient (feed, chip, bench, Undertow) → rung 2 the gate (the look; control is verbs: approve, send back, grant, point) → rung 3 receipts (inspector, for verifying not operating).

## Don't

- Don't put glass on cards, sidebars, or chrome.
- Don't invent hues, or add orange/yellow to live signals (Maestro runs blue → indigo → cyan → ice).
- Don't show progress percentages. Show receipts.
- Don't give the orchestrator a settings page — it's an agent.
- Don't use CalSans in app chrome. Don't use warm grays. Don't use scale/rotate/glow on hover.
- Don't write Title Case labels, UPPERCASE eyebrows, or wide letterspacing.
- Don't mark "Needs you" / halt states in red — they're accent-blue.

## Where to look

All handoff docs are vendored under `handoff/design_handoff_maestro/`.

| Need | File |
|---|---|
| Full philosophy + decision log | `handoff/design_handoff_maestro/build-docs/design-system/readme.md` |
| Condensed agent manifest | `handoff/design_handoff_maestro/build-docs/design-system/SKILL.md` |
| System topology (runtime/relay/clients, state) | `handoff/design_handoff_maestro/build-docs/ARCHITECTURE.md` |
| Stack (tool choices + reasoning) | `handoff/design_handoff_maestro/build-docs/STACK.md` |
| Autonomy / loop logic (the four loops, guardrails) | `handoff/design_handoff_maestro/build-docs/AUTONOMY.md` |
| Data model (work-contract frontmatter + index tables) | `handoff/design_handoff_maestro/build-docs/DATA-MODEL.md` |
| Tokens (source of truth) | `@maestro/tokens` package · `handoff/design_handoff_maestro/build-docs/design-system/tokens/*.css`, `.../styles.css` |
| Wiring tokens into Tailwind v4 | `handoff/design_handoff_maestro/build-docs/TOKENS-INTEGRATION.md` |
| Per-component build specs | `handoff/design_handoff_maestro/build-docs/COMPONENTS.md` |
| Undertow / dot / glow | `handoff/design_handoff_maestro/build-docs/LIVE-SIGNALS.md` |
| Build order | `handoff/design_handoff_maestro/build-docs/ROADMAP.md` (infra) · `.../BUILD-PLAN.md` (UI) |
| Deep specs (harness / verifier / orchestrator / decisions) | `handoff/design_handoff_maestro/build-docs/specs/*.md` |
| Which prototype exports are real | `handoff/design_handoff_maestro/docs/canon-map.md` |
| Data shapes, state machine, wire protocol | `handoff/design_handoff_maestro/docs/data-contract.md` |
| Working pixel reference | `handoff/apps/maestro/` (running prototype) |

## Monorepo

Bun workspaces (`STACK.md §Monorepo`):

```
packages/tokens      the design tokens (source of truth)     → BRO-1773
packages/ui          the component library
packages/protocol    shared types: events, intents, work items (both sides) → BRO-1785
apps/runtime         the 24/7 engine (Bun + Hono, bun build --compile) → BRO-1790
apps/relay           the thin broker (placeholder; P6)
apps/app             the Vite SPA (web / PWA / Tauri) → BRO-1782
apps/marketing       Next.js site (placeholder dir only; D-SCOPE)
```

`packages/protocol` is the point of the single language: the wire contract is the same code on
both sides, not a codegen seam that drifts. **bun** is the only package manager; **Biome** is
lint/format (never ESLint/Prettier).

## Governance

Maestro is governed by the **bstack** control-systems metalayer, wired into this repo from init
(see **BRO-1829** `p0-bstack-init`). Governance is present from the first commit and grows
organically as the runtime's own guardrails (budget-in-path proxy, key-confinement, kill-switch)
land as `.control/policy.yaml` shields — the governance layer formalizes the very system it
governs.

| File | Role |
|---|---|
| `CLAUDE.md` (this file) | Invariants — design canon + always-on ruleset |
| `AGENTS.md` | Operational rules + the P1–P20 primitive contract every agent-run loop inherits |
| `METALAYER.md` | Control-systems manifest (plant / controller / shields / feedback) |
| `.control/policy.yaml` | Machine-readable gates (G1–G4) + setpoints |
| `.claude/settings.json` | Claude Code hooks: Stop→conversation-bridge (P1), PreToolUse→control-gate (P2), SessionStart→skill-freshness (P7) |

Run `bstack doctor` to verify primitive-contract compliance. The merge gate is **human on every
merge** during the build — no PR auto-completes; clean runs park at the gate for approval.
