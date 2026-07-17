# Stack

The concrete tool choices for the `ARCHITECTURE.md` topology, and the reasoning. Nothing here changes the shape — runtime · relay · clients, FS-as-truth, derived index. This settles what each tier is built *with*.

## Summary

| Tier | Choice |
|---|---|
| Runtime | **Bun + TypeScript + Hono**, compiled to a single binary (`bun build --compile`) |
| Runtime index | **`bun:sqlite` embedded** (local file) via `drizzle-orm/bun-sqlite` — compiles INTO the single binary. **Decision (BRO-1841):** libSQL/`@libsql/client` was the original choice (Turso-swap story), but its native addon (`@libsql/<platform>.node`) cannot embed in `bun build --compile`, so the compiled self-host crashed opening the index. The single-binary self-host is the day-one deliverable, so the driver swapped to `bun:sqlite` (compiled into Bun). Turso-cloud adoption stays a driver swap for a later team tier — the schema is driver-agnostic `drizzle-orm/sqlite-core` and the `$inferSelect ≡ protocol` seam is unchanged (see `DATA-MODEL.md` Part B) |
| Agent loop (Loop 1) | Claude Agent SDK, spawned as a child process per run |
| Verifier (Loop 2) | Separate child process — writer ≠ judge is a process boundary |
| Relay | Small Hono service (Fly.io or similar); auth via Clerk (WorkOS if teams/SSO arrive early) |
| Realtime | SSE fan-out down; plain intents up. WebSocket only if client→runtime streaming is ever needed |
| App client | **Vite + React + TypeScript SPA** — one build, three targets: browser, PWA, Tauri 2 shell |
| App client libs | TanStack Router · Zustand (fed by the event-log subscription) · AI SDK `useChat` with a custom transport · Tailwind v4 `@theme` over the tokens · shadcn restyled · `lucide-react` pinned |
| Marketing | Next.js on Vercel — the only Next.js in the system |
| Isolation | git worktrees now; containers/microVMs later behind the sandbox interface (§ Rust below) |

## Monorepo

Bun workspaces:

```
packages/tokens      the design tokens (source of truth)
packages/ui          the component library
packages/protocol    shared types: events, intents, work items — imported by BOTH runtime and client
apps/runtime         the 24/7 engine
apps/relay           the thin broker
apps/app             the Vite SPA (web / PWA / Tauri)
apps/marketing       Next.js site
```

`packages/protocol` is the point of the single language: the wire contract is the same code on both sides, not a codegen seam that drifts.

## The runtime is a supervisor

"Many agents in parallel" does not stress the language — an agent loop spends its life awaiting model APIs or running tool calls, and tool calls are child processes the OS parallelizes regardless. The runtime is therefore a small supervisor:

- **One process per run.** The Agent SDK loop runs as a child in its worktree; the verifier runs as a *separate* child. A crashed or runaway agent cannot take down the runtime; the kill switch is a signal to a pid; memory-on-disk means a child restarts fresh-context losing nothing.
- **Correctness lives in the index, not in threads.** The parallel failure modes (`AUTONOMY.md` §4–5: budget races, hook storms) are data-integrity problems — solved by transactions on the control-plane index (budgets checked in-path, leases, idempotency keys), the same in any language.
- **The ceiling is budget, not compute.** N parallel agents hit token spend long before the supervisor breaks a sweat. If hundreds of concurrent runs ever matter, the answer is more runtimes / container-per-run (phase 2), not a faster supervisor.

## Why TypeScript, not Rust

Considered seriously (Codex CLI's Rust rewrite is the obvious precedent). Their reasons — zero-dependency install at mass-distribution scale, per-MB server economics, existing Rust sandboxing bindings — are real and mostly don't transfer:

1. **The workload is I/O-bound orchestration.** One long-lived process per user, tens of concurrent async waits. Rust's wins (throughput, no GC) buy nothing here; its iteration cost is paid daily.
2. **The Claude Agent SDK is TypeScript.** Loop 1 in Rust means hand-rolling the agent harness — rebuilding the highest-risk layer instead of importing it.
3. **The wire is TypeScript.** The AI SDK UIMessage stream protocol and reducer exist as TS; `packages/protocol` makes the contract literal across runtime and client.
4. **The deployment win is already covered.** `bun build --compile` gives the single self-host binary — Rust's headline advantage.
5. **Sequencing.** Codex shipped TypeScript first for velocity and rewrote after the product was proven. Same order applies here, solo.

**Where Rust does enter:** the Tauri shell (free), and later the **phase-2 sandbox supervisor** — the small security-critical component doing physical isolation (containers/microVMs, resource limits, syscall filtering). The sandbox interface in `ARCHITECTURE.md` §5 is the pre-drawn boundary where it lands without touching anything above.

## The dev loop is the self-host tier

"Always-on" doesn't mean cloud-only. Developing = runtime on localhost with the direct/LAN path bypassing the relay. The trust tier gets built by default; managed hosting is the same binary on different infra later.

## Where the risk actually lives

Not in any choice above — they're boring and reversible. The two hard builds are **Loop 2 (the verifier)** and the **guardrails** (budget-in-path, three stop conditions, kill switch). Per `AUTONOMY.md` §6: nothing runs unattended until those exist. Spend the novelty budget there.
