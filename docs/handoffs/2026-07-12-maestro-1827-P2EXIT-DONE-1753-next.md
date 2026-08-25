# Maestro handoff — BRO-1827 P2 EXIT DONE (P2 complete) → BRO-1753 next (P3 opens)

**Date:** 2026-07-12
**Main tip:** `2613987` (BRO-1827 P2 exit — #58 squash-merged)
**Loop:** autonomous maestro build governor (one ticket/slice per iteration; driver `~/broomva/docs/maestro-orchestration-loop.md`, memory [[maestro-build-arc]]).

---

## What just shipped (BRO-1827 — P2 EXIT, Loop 1 complete)

The ROADMAP §P2 phase gate, proven end to end. **`apps/app/tests/p2-exit.pw.ts`** — two Playwright tests over a REAL `MAESTRO_MOCK_MODEL=1` runtime (the only mode that mounts dispatch), booted on the fixed vite-proxy port 4319 over a temp **git** workspace (the worktree sandbox factory branches off it):
- **① live path** — `/session/runner`, send via the real `RuntimeChatTransport` (no fixture) → `POST /api/sessions/:id/chat` → dispatch-then-chat → supervisor + mock proxy + spawned child → the mock reply streams into the feed over SSE, then **SIGKILL** (hard crash) + restart → session + events survive.
- **② recovery path** — a still-`running` session seeded via `apps/runtime/scripts/seed-orphan.ts` (the sub-second mock can't be caught mid-flight, so seeding is the deterministic crash reproduction) → boot → `recoverOnStartup.parkOrphans` (F9.3) parks it `blocked` (plain-voice "Stuck") + one `run.orphaned`, pre-crash events intact.

**P11 caught a real latent browser bug** (the point of the E2E): `RuntimeChatTransport` called its default `fetch` unbound → native browser fetch threw "Illegal invocation" (`this !== window`). Fixed: `?? globalThis.fetch.bind(globalThis)`; mutation-proven unit guard. `playwright.config` `workers:1` (p1-exit + p2-exit both bind 4319). P20 8/10, 0 surviving blocker/major.

**⇒ P0 + P1 + P2 all COMPLETE. Loop 1 (sessions that run) is done.**

---

## NEXT: BRO-1753 — done: block parser + contract validation (P3 frontier)

**Why this ticket:** P3 = "Loop 2 + the gate: autonomy earns trust" (the verifier + the gate). Its dependency-first ticket is the **contract seam** — exactly like the P1 seams. BRO-1753 `blockedBy` only **BRO-1785** (packages/protocol v0, **Done**), and it `blocks` **BRO-1765** (the verifier child). It's protocol-only ("can start before P2 exit, parallel with P2 work" per its loop-note). Build it first so the verifier stages (1765/1778/1786) have a validated contract to consume.

**Goal (VERIFIER.md §1):** a parser/validator for the `done:` schema, living in **`packages/protocol`** (shared: the runtime validates at dispatch, the verifier validates at verify). The schema:
- **named checks** with commands;
- **timeout** defaults + caps;
- **protect globs** — an **extend-only floor**: `**/*.test.*`, `**/rubric.md`, `**/_work.md`. Contracts may ADD globs (e.g. `package.json`) but NEVER remove the floor. (This floor is what Stage 0 tamper-guard, BRO-1765, enforces.)
- **judge / rubric refs.**

**Validation rules:**
- reject `gate:auto` with an empty check list (a judge-only auto-gate is rejected — this is **D8 fixture 4**);
- reject malformed rubric refs.

**done.check:**
```bash
cd apps/maestro && bun test packages/protocol --filter done-schema
# a fixture matrix incl. the D8 fixture-4 rejection (judge-only gate:auto)
```
(Note: `--filter` is a no-op in `bun test`; use `bun test packages/protocol/src/<file>.test.ts` or `bun test -t done-schema` — see the P1 gotcha log.)

**Docs:** `handoff/design_handoff_maestro/build-docs/specs/VERIFIER.md §1` · `handoff/design_handoff_maestro/build-docs/specs/DECISIONS.md §D8 fixture 4`.

### Seams to respect (P14 dep-chain)
- **Upstream:** `packages/protocol` is the home (zero runtime deps — keep drizzle/libSQL out, mirror the BRO-1754 index-schema decision). The work-contract types + frontmatter parser already exist (BRO-1787, `packages/protocol/src/frontmatter.ts` → `resolveWorkContract`/`WorkContract`); the `done:` block is part of the work contract, so extend/reuse those types rather than redefining. Check how `WorkContract.done` is currently typed (`doneJson` is `JSON.parse`'d in the scanner + `nodeRowToContract` in the supervisor).
- **Downstream:** BRO-1765 (verifier child Stage 0 reads the protect globs + diff limits), BRO-1778 (Stage 1 runs the named checks), the runtime dispatch path (validates the contract before spawning). Grep for existing `done`/`protect`/`checks` handling in `packages/protocol` + `apps/runtime` before writing — some may already be stubbed.

### Watch-outs
- **Protocol stays browser-safe + zero-runtime-dep** — the app imports it too.
- **Anti-vacuity** ([[self-hosting-vacuous-pass]]): the fixture matrix must assert EXACT rejections (which rule fired), not just "threw". D8 fixture 4 (judge-only `gate:auto`) is the headline case — a test that passes even if the empty-check-list rule is removed is vacuous.
- **Branch FIRST** after `git checkout main` confirms clean, BEFORE any Read/Edit (rule-of-three slip 1819+1814+1862).
- This is protocol-only + likely small — probably one P20 surface, no split needed.

### Cycle reminder
P15 snapshot → P14 dep-chain → **branch FIRST** → build per `apps/maestro/CLAUDE.md` + reuse seams → `bun test`/`tsc`/`biome ci .` green → P11 dogfood → P20 dynamic Workflow ≥7, 0 surviving blocker/major, fixes mutation-proven, re-verify fresh on pushed code (test-only strengthening with production byte-identical to the P20-verified sha → local mutation-proof only, no re-fire) → PR via REST → resolve every CodeRabbit thread → merge green via REST (P20≥7 + 4 required jobs + 0 unresolved threads; full 40-char sha; squash) → bookkeep → `ScheduleWakeup` the next ticket.

**Stop condition:** `ScheduleWakeup({stop:true})` when ALL P0–P5 non-OUTLOOK tickets in project `6c738b50` are Done. P3 has 11 tickets (1753→…→1821), then P4 + P5 remain. No mid-arc stops; never ask back.

---

## State snapshot at handoff

- **Branch:** `main` @ `2613987`, clean tree (untracked `docs/handoffs/*` + `docs/conversations/*` only — bridge output, kept local like every prior ticket).
- **Linear:** BRO-1827 **Done**; BRO-1753 **Backlog**, unblocked (blockedBy 1785=Done), High. P3 milestone = "P3 — Loop 2 + the gate: autonomy earns trust".
- **P3 chain (build order):** 1753 (this) → 1765 (verifier child + Stage 0) → 1778 (Stage 1 checks) → 1786 (Stage 2 judge) → 1794 (verdict + feedback) → 1802 (approve = merge) → 1805 (F5 gate slice) → 1809 (M5 inspector) → 1813 (adversarial suite) → 1818 (autonomy ledger) → 1821 (P3 exit). Order within is dependency-gated — verify each ticket's blockedBy before starting.
- **Open follow-ups (Low/Med, not blocking):** 1865 (M4 polish) · 1864 (ai v6→v7) · 1860 (child loop) · 1847 (2-phase lease) · 1846 (inotify) · 1844/1845 (SSE resume) · 1841 (native libSQL) · 1850 (kill-TOCTOU) · 1853 (D4 hygiene).
- **CI:** 4 required jobs (quality/secrets/build/governance) + CodeRabbit advisory; CI-gated automerge; Playwright is a local-only P11 gate (no browser in CI).
