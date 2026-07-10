# Architecture

How Broomva is built and deployed. This is the system topology and the state model. The *autonomy* logic that runs on top of it is in `AUTONOMY.md`; the UI rules are in `CLAUDE.md`.

> **The one-line shape:** a 24/7 **runtime** owns the workspace and runs the loops; a thin **relay** brokers connections; **clients** (desktop / web / PWA) are projections that subscribe. Nothing important lives in the client.

---

## 1. Why this topology (it falls out of the product thesis)

Three facts force the design:

1. **Unsupervised hours.** Agents run for hours — the user closes the lid and walks away. A runtime that dies with the client can't deliver that. → **the runtime must be always-on, off the client.**
2. **Chat is a projection.** The same run must render identically in a desktop window, a browser tab, and a phone. → **clients subscribe to one runtime; they don't own state.**
3. **The workspace is the substrate.** Work is files + git in a real FS/sh environment, and memory lives on disk (see `AUTONOMY.md` — this is the loop-engineering "memory on disk, not in context" principle). → **the runtime owns a real filesystem; that FS is the system of record.**

This retires the earlier "Tauri sidecar daemon on the user's laptop" idea. The daemon moves onto the always-on runtime; Tauri becomes purely a client shell.

## 2. The three tiers

### Runtime (always-on)
The engine. Owns a workspace (FS + git + sh), runs the four loops, spawns agent processes into isolated `run/<id>` git worktrees, holds the control-plane index, emits the event stream. TypeScript (Bun or Node) + the Claude Agent SDK. **All agent logic lives here and only here.**

- **Hosting is a choice, and it's where the trust + cost story lives:**
  - **Self-hosted runtime** (user's VPS / home server / their own cloud) — code never leaves their infrastructure, *they* pay the always-on cost. This is the trust-tier and the differentiator. Make it first-class.
  - **Managed runtime** (we host) — easy onboarding tier on top. Same binary, our infra.
  - The relay and clients must be **agnostic** to which — a runtime is a runtime.

### Relay (always-on, thin)
The connection broker between clients and runtimes, and the only public endpoint. **Keep it dumb on purpose:** auth, routing a client to its runtime(s), and fan-out of the event stream. **No agent logic, no work state.** Reasons: it's the trust boundary (smallest possible attack/liability surface), and if agent logic crept in it would become the bottleneck and couple the clients to us even in self-host mode. A self-hosted runtime may even expose a direct/LAN connection that bypasses the relay entirely.

### Clients (desktop / web / PWA — projections)
All three render the **same React UI on the same tokens** (the design system). They hold no source-of-truth state — they subscribe to a runtime via the relay and issue intents (approve, send back, grant, point, new mission, set a routine).

- **Web / PWA** — Vite + React SPA. The PWA is the installable/mobile form of the same SPA. Primary surface for "check on my runs from anywhere."
- **Desktop (Tauri)** — the *same* SPA in a Tauri 2 shell, adding OS-native value: tray presence for the orchestrator, native "Needs you" notifications, keychain for the runtime credential. It is a client, not a runtime.
- **Not Next.js for the app.** SSR/RSC/edge are meaningless for a client that talks to a runtime over a socket. Keep Next.js only for the marketing site (`ui_kits/landing`). The component layer is shared across all of it, so this is a build-target split, not a design split.

```
   self-host or managed
   ┌─────────────────────────────┐
   │  RUNTIME (24/7)             │   owns: FS workspace (git, sh),
   │  loops · agents · worktrees │          control-plane index,
   │  FS = system of record      │          event stream
   └──────────────┬──────────────┘
                  │  (or direct/LAN in self-host)
            ┌─────┴─────┐
            │   RELAY    │  thin: auth · routing · stream fan-out
            └─────┬─────┘
        ┌─────────┼─────────┐
     ┌──┴──┐   ┌──┴──┐   ┌──┴──┐
     │ web │   │ PWA │   │Tauri│   projections — no source-of-truth state
     └─────┘   └─────┘   └─────┘
```

## 3. State model — FS is the truth, the index is derived

This is the most important architectural decision, and it is **two stores with one direction of authority.**

### (a) Filesystem = system of record (AI-native)
Everything the agent reads or produces lives as plain files in the workspace, git-versioned:

- work + structure (folders = work at any scale; frontmatter = `kind/state/owner/budget/gate`),
- memory (progress docs, task boards, `fix_plan.md`, specs — the loop-engineering "state on disk" that lets fresh-context restarts skip done work),
- receipts (git history, `run/<id>` branches, diffstats, attached verification evidence).

Why FS and not a DB for this: it's what agents are already excellent at (the Letta benchmark — filesystem memory **74%** beats graph memory **68.5%**); it's git-versionable (the branch *is* the receipt); it's inspectable and portable; and it survives the runtime — rebuild everything else from it.

### (b) Control-plane index = derived, transactional, rebuildable
An embedded transactional store (**SQLite-class**, local to the runtime) for the things a filesystem does *badly* and which must be correct under concurrency:

- **budget counters** — checked transactionally **before each model call** (the loop-engineering budget-in-path guard; FS files lose updates when parallel worktrees race, which is the $86k/day failure),
- **leases / locks / idempotency keys** — so a webhook or heartbeat can't fire the same run twice (the "heartbeat storm" failure mode),
- **the run queue + scheduler timers** (routines, cron, "Standing" work),
- **fast reactive queries** — "every run that Needs you," the board, the orchestrator's bench — and the change feed the event stream is built from.

**Authority is one-directional: the index indexes the FS; it never owns truth.** If the index is lost, rebuild it by scanning the workspace + git. Treat it as a cache with teeth, not a database of record. This is a concrete operator command (BRO-1808): **`maestro-runtime --rebuild`** deletes `index.db` and rescans the workspace from the FS, then exits. The guarantee is enforced by an identity test — build the index, kill it, rebuild, and the derived `node` rows are byte-identical modulo the index-assigned `updatedAt` clock (`apps/runtime/src/db/rebuild.test.ts`); it composes the scanner's own determinism ("same workspace → identical set") one level up.

**Durability addendum (D-DURABILITY).** So the rebuild guarantee holds *unqualified*, the two index-only facts that a plain FS scan can't recover are journaled to the filesystem as events: **`budget.*`** (spend/metering) and **`gate.decided`** (verdicts) are written to `session.jsonl` (or the workspace journal for synthetics). The `event` table stays a pure **projection** of those journals — rebuilding by replaying them recovers spend counters and decided gates, so no state is lost with the index. `run_budget`/`lease` remain the authoritative in-path guards at runtime; the journal is their durable shadow.

## 4. Realtime — the event stream

The runtime emits an append-only event log (agent/user/tool events). The relay fans it out; clients subscribe over **SSE** (or WebSocket if you need client→runtime streaming beyond plain intents). Because every client is a subscriber and holds no truth, "the same run in a side panel, a thread, and a handoff page" is the default, not plumbing. The event log is also the audit trail the guardrails (`AUTONOMY.md`) require.

## 5. Isolation — phased, behind one interface

Phase-1 isolation is **logical: git worktrees + `run/<id>` branches** on the runtime host (your stated starting point). Honest limit: worktrees share the host shell, so a run can touch the host. Acceptable when the runtime is the user's own (self-host) — it's their trust boundary. Phase-2 is **physical** (containers / microVMs per run). **Draw the runtime↔execution boundary now so a worktree and a container are the same abstraction** ("a sandbox I run in"), making phase-2 a swap, not a rewrite.

## 6. Auth & identity

- Client ↔ relay: real auth (Clerk or WorkOS — WorkOS if teams/SSO matter early; both audiences, individuals first).
- Client ↔ self-hosted runtime: a runtime credential the user holds (Tauri stores it in the OS keychain).
- The Anthropic key lives **on the runtime**, never in the client, never proxied through the relay — matches the "your keys, nothing proxied" posture.

## 7. Team tier later, cheap if planned now

Design the FS + index **sync-ready from day one**: stable UUIDs, `updated_at`, soft deletes, runtime-owned IDs. Then "shared workspace / multiple humans on one runtime" is additive (more subscribers on the stream + the gate routed to the right person), not a migration.

## 8. Stack summary

| Concern | Choice |
|---|---|
| Runtime | TypeScript (Bun/Node) + Claude Agent SDK, always-on, self-host or managed |
| System of record | Filesystem (git, sh) — AI-native |
| Control-plane index | Embedded SQLite-class store — derived, transactional, rebuildable |
| Relay | Thin TS service — auth · routing · stream fan-out only |
| Realtime | Append-only event log → SSE fan-out |
| Clients | Vite + React SPA → web, PWA, and Tauri 2 shell (shared design system) |
| Marketing | Next.js on Vercel (`ui_kits/landing`) |
| Isolation | git worktrees now; containers later, behind one sandbox interface |
| Auth | Clerk / WorkOS (client↔relay); keychain runtime credential (client↔runtime) |

See `AUTONOMY.md` for what runs *inside* the runtime.
