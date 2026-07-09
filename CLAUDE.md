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

---

> The full P1–P20 primitive contract every agent-run loop in this repo inherits. Ported
> from bstack (vendored at `bstack/`, BRO-1829); operational detail lives in `AGENTS.md`.

## Bstack Core Automation Primitives

Twenty irreducible building blocks that make this workspace self-operating. All are always active. Full specification in `AGENTS.md`.

Each primitive carries a **short name** for agent prose. When referencing a primitive in responses, PR bodies, commit messages, or comments, use the `Name (Pn)` form — *"applying Snapshot (P15)"*, *"via Dep-Chain (P14)"*, *"running Bookkeeping (P6)"* — not bare `Pn`. The number is the canonical identifier; the short name is the human-readable handle.

**Short-name index**: Bridge (P1) · Gate (P2) · Tickets (P3) · Pipeline (P4) · Fanout (P5) · Bookkeeping (P6) · Freshness (P7) · Janitor (P8) · Wait (P9) · Hygiene (P10) · Empirical (P11) · Persist (P12) · Dream (P13) · Dep-Chain (P14) · Snapshot (P15) · Crystallize (P16) · Lens (P17) · Audience (P18) · Orchestrate (P19) · Cross-Review (P20).

| # | Primitive | Mechanism | Invariant |
|---|-----------|-----------|-----------|
| P1 | **Bridge** — Conversation Bridge | Stop hook → JSONL → Obsidian docs → vault | Bridge stamp < 24h stale |
| P2 | **Gate** — Control Gate | PreToolUse hook → `.control/policy.yaml` | G1–G4 blocking, never bypassed |
| P3 | **Tickets** — Linear Tickets | Every work unit tracked Backlog → Done | No significant work without a ticket |
| P4 | **Pipeline** — PR Pipeline | Branch → PR → CI → merge → deploy | Never merge with failing checks |
| P5 | **Fanout** — Parallel Agents | Concurrent isolated agents via worktrees | No shared mutable file writes |
| P6 | **Bookkeeping** — Knowledge Bookkeeping | `bookkeeping run` → score → promote → entity pages → synthesize | `research/entities/` never contains unscored items; knowledge capture is a reflex, not a request, and **never a question** (file proactively, report after — never ask permission to document) |
| P7 | **Freshness** — Skill Freshness Check | SessionStart hook → reports stale-skill nudge if last update check ≥ 7d ago | Never blocks; closes silent-rot bug for `npx skills add` snapshots |
| P8 | **Janitor** — Branch + Worktree Janitor | `make janitor` → detects squash-merged branches + dead worktrees, removes safely | Default `--dry-run`; never touches protected branches |
| P9 | **Wait** — Productive Wait (`broomva/p9` skill) | wait-queue drains while a blocking operation runs (PR CI is the reference impl: `gh pr checks --watch` via `run_in_background` → classifier + evaluator self-heal). For non-PR waits (push-triggered deploys, builds), do a single direct check after kicking off next-priority work. | Never `sleep` on a blocking wait; merge defers to control metalayer |
| P10 | **Hygiene** — Worktree Hygiene Discipline | Reflexive rule: decide worktree-or-not before first file; keep `git status` clean; auto-run P8 janitor after every merge | A clean tree is the only reliable reset point |
| P11 | **Empirical** — Empirical Feedback Loop | Reflexive rule: validate by *interacting* — log-tails, browser E2E, screenshots, deploy verification, multi-level test composition | Reasoning isn't validation; interaction is |
| P12 | **Persist** — Persistent Loop Discipline (`broomva/persist` skill) | Reflexive rule: cross-context restart loop — state in filesystem (PROMPT.md + git tree), each iteration spawns a fresh agent context | At long-horizon work (>1h), in-context loops decay; restart fresh, backpressure from compilers/tests |
| P13 | **Dream** — Dream Cycle Discipline | Reflexive rule: any consolidation that crosses a cadence-tier boundary MUST follow the 5-phase shape (gather → replay → prune → consolidate → index) | Replay against frozen substrate is the runtime form of stop-gradient; without it, dense lower-tier signal corrupts sparse upper-tier rules |
| P14 | **Dep-Chain** — Dependency-Chain Reasoning Discipline | Reflexive rule: before any substantive write, enumerate upstream (files, functions, types, contracts, deployed state this depends on) and downstream (consumers, tests, CI gates, docs, in-flight PRs depending on this). Concrete file paths + function names in the response or PR body — not in the agent's head. | "Think deeply through chain of dependencies" without a concrete enumeration step is ritual. P14 makes it machine-checkable. |
| P15 | **Snapshot** — State-Snapshot Before Action | Reflexive rule: before any plan, the agent surfaces `git status` + branch + ahead/behind, in-flight PRs (`gh pr list`), Linear ticket state, bookkeeping/bridge freshness, last deploy state. The snapshot is *part of* the planning response — not deferred. | Plans built on stale state fail silently. P15 makes state-checking a cheap reflex, not a request the user has to make. |
| P16 | **Crystallize** — Crystallization Discipline (the Bstack Engine) | Meta-primitive — the loop that produces every other primitive. Pattern recurs ≥3 times across sessions → propose promotion to skill / SKILL.md / AGENTS.md section / `.control/policy.yaml` gate, gated by the four conditions: ≥3 instances, concrete mechanism, stated invariant, stated failure mode. | The crystallization loop must run inside the workspace, not in the user's head. P1–P15 are *outputs* of this loop. |
| P17 | **Lens** — Lens-Routed Request Articulation (`broomva/role-x` skill) | Reflexive rule: every substantive user input passes through `role/x` intake — select lens(es) from `roles/<name>.md` registry by scoring signals, load substantive context, decide mode (`augment` / `rewrite` / `decompose`); P5 fan-out becomes typed graph. | No `act as X` persona rewrites — lenses load substantive context only. Lens selection is logged. Mode decision is surfaced unless `augment`. |
| P18 | **Audience** — Format-Follows-Audience Discipline | Reflexive rule: format follows audience. Agent-readable (LLM, system-prompt loaded, in-repo reference) → **markdown**. Human-readable (decisions, review, exploration) → **HTML**. Both (README, CHANGELOG, GitHub-browseable) → markdown (GitHub renders). ASCII pseudo-diagrams + unicode-color-approximation + >100-line markdown specs without HTML companion are explicit anti-patterns. Specs/plans/ADRs land in `docs/specs/`, `docs/plans/`, `docs/adrs/` as `.html`. | Format follows audience, not habit. Markdown's expressiveness ceiling means humans bounce off agent-produced specs at ~100 lines; HTML's information density carries the load. The 2-4× HTML generation cost is paid only on artifacts a human will actually read. |
| P19 | **Orchestrate** — Orchestration-Mechanism Selection Discipline | At pre-flight of substantive autonomous work, apply the **2×2×2 mechanism cube** (session-scope × trigger-source × agent-count). **N=1 plane:** `/goal <condition>` (internal+in-session), Wait (P9) `p9 watch --background` (external+in-session), `/loop <interval>` (internal+across-session), Persist (P12) `persist iterate PROMPT.md` (external+across-session). **N>1 plane:** Fanout (P5) multi-`Agent` (external+in-session), **`bstack wave dispatch <plan...>`** (external+across-session — worktree per plan, JSONL state). Compose dynamically. | No autonomous-continuation work without explicit mechanism choice + cube-cell citation. "Continue please" / waiting for user prompts mid-arc is ritual and forbidden. |
| P20 | **Cross-Review** — Cross-Model Adversarial Review Gate (`broomva/cross-review` skill) | Before substantive PRs merge, fire cross-model adversarial gate. Three strata: A (true cross-vendor via `codex exec`), B (fresh-context subagent under devil's-advocate brief), C (composed adversarial-review skills — `superpowers:constructive-dissent`, `devils-advocate`, `pr-review-toolkit:*`, `critique`, `premortem`). Anti-slop score ≥7/10; max 3 fix rounds; verdict logged in PR comments + Linear ticket (if workspace uses Linear). Fires *before* P4 auto-merge. | Substantive PRs (>200 LOC OR public API OR multi-file OR governance-class) cannot merge without cross-model verdict ≥7/10. Self-review by the writing model as sole verdict is forbidden. |

> **Naming note.** Skill repo names are stable and don't always match primitive numbers. P6's skill repo is `broomva/bookkeeping` (named for the function). P9's skill repo is `broomva/p9` — name matches primitive number. Renaming any skill repo would break every `npx skills add` install, so when a skill repo carries a number, the primitive numbering commits to keeping it stable.

