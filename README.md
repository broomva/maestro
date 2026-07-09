# Maestro

**Maestro** is Broomva's work-orchestration AI agent — a chat-first product for running loops of
agentic work under human control.

The scarce resource is **unsupervised hours**: how long an agent runs before a human must look.
Work is the noun (folders with frontmatter contracts), sessions are the verb, chat is a
projection. A 24/7 runtime schedules agent sessions against a filesystem workspace; the
filesystem is truth and a derived index makes it queryable. The human's one verb is **the gate** —
approvals on work that lands at "Needs you." Calm, light-first, barely-blue monochrome; color
earns its place; glass is earned; show receipts, never percentages.

## Canon

Where two docs describe the same thing, the owner named in
[`handoff/design_handoff_maestro/START-HERE.md` §2](handoff/design_handoff_maestro/START-HERE.md)
wins; every other doc defers to it. The repo-root [`CLAUDE.md`](CLAUDE.md) is the always-on
ruleset (design canon + governance pointer). Design tokens are consumed as the `@maestro/tokens`
package — never copy raw values.

The full design handoff (design system, build docs, running prototype, contracts) is vendored
as-is under [`handoff/`](handoff/).

## Monorepo

Bun workspaces — one language across runtime and client, wire contract shared as code:

```
packages/tokens      design tokens (source of truth)
packages/ui          component library (shadcn restyled to Broomva, CVA, Lucide)
packages/protocol    shared types: events, intents, work items (imported by both sides)
apps/runtime         the 24/7 engine — Bun + Hono, compiled to a single binary
apps/relay           the thin broker (placeholder; P6 Distribution)
apps/app             the Vite + React SPA — web / PWA / Tauri
apps/marketing       Next.js marketing site (placeholder dir only)
```

- **Package manager:** bun (only). **Lint/format:** Biome (never ESLint/Prettier).
- **Runtime:** Bun + TypeScript + Hono, `bun build --compile`. **Index:** libSQL via `drizzle-orm/libsql`.
- **Client:** Vite + React + TypeScript + Tailwind v4, one build → three targets.

```bash
bun install
bun run typecheck   # tsc --noEmit across the workspaces
bun run lint        # biome check
```

## Build spine

Sequenced so each phase is verifiable before the next depends on it (roadmap phases P0–P6 in
`handoff/design_handoff_maestro/build-docs/ROADMAP.md`; UI track M0–M6 rides inside):

| Phase | What lands | Exit test |
|---|---|---|
| **P0** Foundations | monorepo, tokens, protocol v0, runtime `/health`, app shell, governance | binary serves `/health`; SPA renders tokens light + dark; `bstack doctor` green |
| **P1** The spine (read-only) | FS-as-truth + derived index, scanner, watcher, read API | hand-edit a `_work.md` → board updates over the stream; kill the index → rebuilds identical |
| **P2** Loop 1: sessions that run | dispatch, harness, guardrails (budget / stop-conditions / kill-switch) | dispatch from the app, kill mid-run, restart → nothing lost, orphan at Stuck |
| **P3** Loop 2 + the gate | verifier (writer ≠ judge), gate queue, approve / send-back | clean run parks at Needs you; approve merges; a gamed check is caught |
| **P4** Loop 3: legible | orchestrator agent, routines, wake log | nightly routine fires exactly once across a mid-fire restart |
| **P5** Dogfood | Maestro builds Maestro | majority of a week's commits are gated Maestro runs |
| **P6** Distribution | relay, multi-machine, no-terminal onboarding | second machine through the relay; one account, both runtimes |

## Definition of done

The standing bar every ticket and phase holds — light + dark, reduced-motion safe, tokens-only,
holding every hard rule in `CLAUDE.md`, with bstack governance green — is written once in
[`docs/definition-of-done.md`](docs/definition-of-done.md) and referenced by every phase-exit ticket.

## Governance

Wired from init via **bstack** (BRO-1829) — see [`AGENTS.md`](AGENTS.md), [`METALAYER.md`](METALAYER.md),
[`.control/policy.yaml`](.control/policy.yaml). The merge gate is **human on every merge** during
the build.

## License

See [`LICENSE`](LICENSE) once added. © Broomva.
