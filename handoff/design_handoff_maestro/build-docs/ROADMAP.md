# Roadmap

Phases across all tiers. `BUILD-PLAN.md` (M0–M6) is the **UI track** and stays authoritative for it; this document sequences the whole system around it. Each phase has an exit test you can run, and each is written so its tasks decompose directly into `_work.md` folders — because from P5 on, **Maestro's own backlog lives in a Maestro workspace**.

Cascade discipline throughout (`AUTONOMY.md` §6): make each loop stable before automating the loop outside it.

The deep specs behind these phases live in `specs/`: `HARNESS.md` (P2's supervisor↔child seam), `VERIFIER.md` (P3's Loop 2), `ORCHESTRATOR.md` (P4's tick), `DECISIONS.md` (git-on-approve, the data-model open questions — now closed, notifications, threat model, testing).

---

## P0 — Foundations
**Goal:** the repo exists and the contract is code.

- Bun workspaces monorepo (`STACK.md`): `packages/tokens · ui · protocol`, `apps/runtime · relay · app · marketing`.
- `packages/protocol`: OrchState + transitions, Intent union, event envelope, work-contract types (incl. the full `done:` schema from `VERIFIER.md` §1), error codes — ported from `DATA-MODEL.md`/`API.md`.
- `packages/tokens` from `design-system/`; UI track **M0** (Vite scaffold + tokens).
- CI: typecheck, test, `bun build --compile` producing the runtime binary.

**Exit:** the binary runs and serves a health route; the SPA renders tokens correctly light + dark.

## P1 — The spine (read-only)
**Goal:** see a real workspace in the app. No agents yet.

- Runtime: workspace scanner + fs watcher → index (`node` from frontmatter); libSQL schema from `DATA-MODEL.md` B.3; startup rebuild (F9 steps 1–2).
- Read API + global SSE stream with resume cursor (`API.md` §1); `new_mission` as the first intent (F1).
- UI track **M1–M2** (primitives, shell) against real endpoints; board renders real nodes in attention order.

**Exit:** edit a `_work.md` by hand; the board updates over the stream without reload. Kill the index file; it rebuilds identical.

## P2 — Loop 1: sessions that run
**Goal:** dispatch real work and watch it, with the guardrails already in place. **No unattended anything yet.**

- Runner port + Claude Agent SDK adapter; Sandbox port + worktree adapter; supervisor spawn/tail/reap (F2, F3) — the whole seam per `HARNESS.md`: spawn/env contract, NDJSON stdio, exit codes, liveness.
- **Guardrails before the first run:** budget-in-path via the model proxy (`HARNESS.md` §3 — the key never touches a child), iteration cap, no-progress halt, kill switch (F8). These precede features — a loop without them never ships.
- The mock-model server behind the proxy (`DECISIONS.md` D8 layer 1) lands here too — CI drives full flows with zero tokens from P2 on.
- `session.jsonl` → event projection → stream; chat endpoint speaking the UI Message Stream Protocol (F10).
- UI track **M3–M4** (board with live Undertow, chat); crash recovery (F9 complete).

**Exit:** dispatch a contract from the app, watch events stream, kill it mid-run, restart the runtime — nothing lost, orphan parked at Stuck.

## P3 — Loop 2 + the gate: autonomy earns trust
**Goal:** "done" is a hard check and a human verb. *This is where to spend the most time.*

- Verifier child process per `VERIFIER.md`: tamper/diff guard (Stage 0), deterministic checks, LLM judge as supplement (F4); `verdict.md` + `rubric.md` formats, structured feedback into `fix_plan.md`.
- Approve = squash-merge with the verdict-freshness rule and "Rebase & re-verify" (`DECISIONS.md` D1).
- Gate table + the four verdicts wired to intents (F5); `data-gate` parts; the state-machine rule enforced: no auto-done.
- UI track **M5** (inspector, receipts, lifecycle rail).
- The adversarial verifier eval suite (`DECISIONS.md` D8 layer 2) — this phase's exit test, as fixtures.
- Autonomy ledger: unsupervised hours + a notch per human look, derived from events.

**Exit:** a clean run parks at "Needs you"; approve merges the branch; send-back redispatches with feedback; a run that games its check is caught by the verifier (test: delete a failing test in-run — verdict must fail it).

## P4 — Loop 3: the loop becomes legible
**Goal:** triggers and the orchestrator — Maestro starts scheduling itself.

- Scheduler on the index (`schedule`, `next_fire_at`); idempotency leases (F7); trigger taxonomy: manual → cron → hook (goal triggers last — they're where tokens burn).
- The tick (F6): the orchestrator per `ORCHESTRATOR.md` — briefing, ordered decision policy, intent subset (gate verbs rejected server-side for `agent:*`), zero default capabilities, the `data-tick` wake log, tick lease.
- Notification delivery designed now, shipped in P5 (`DECISIONS.md` D6).
- Routines / Standing work end-to-end; UI track **M6** (overlays, palette, motion polish).

**Exit:** a nightly routine fires exactly once (kill the runtime mid-fire and restart — still once), and the wake log answers *why it woke* every time.

## P5 — Dogfood: Maestro builds Maestro
**Goal:** the backlog moves into a Maestro workspace and the product becomes its own first customer. This is a phase, not a stunt — it's how Loop 4 gets its data.

- Migrate this roadmap's remaining tasks into `_work.md` folders with real contracts (`done.check` = the repo's own test suite; `gate: human` on **everything**).
- Run Maestro's build tasks through Maestro: dispatch from the app, verify with Loop 2, approve at the gate. Every merged PR is a `run/<id>` branch with a verdict.
- **Loop 4, human-grade:** weekly trace review across `event` history — tune budgets, prompts, `done` contracts by hand, committed to frontmatter. Automating the rewrite waits until P6+ and changes rarely.
- Notification delivery ships here — Tauri native + Web Push (`DECISIONS.md` D6): unsupervised hours nobody notices are wasted hours.
- Honest scoping (`production-notes` §8 still applies): auth/multi-user and error surfaces beyond blocked-cards are *not* required to dogfood on localhost.
- Grow orchestrator autonomy via readable `capabilities:` grants (`ORCHESTRATOR.md` §5), one at a time, as the ledger earns them.

**Exit:** a majority of Maestro commits in a given week originated as gated Maestro runs, and the autonomy ledger shows unsupervised hours trending up at constant human looks.

## P6 — Distribution
**Goal:** other people (and other products) can use it.

- Relay + Clerk auth + the grant table (`API.md` §3); runtime dials out — self-host with no inbound port.
- PWA manifest + service worker (network-first HTML, per `production-notes` §2); Tauri 2 shell (tray presence, native gate notifications, keychain credential).
- Managed-runtime tier: same binary, our infra; libSQL → Turso only if/when db-per-tenant economics demand it (`DATA-MODEL.md` B).
- Phase-2 isolation when exposure demands it: container/microVM behind the Sandbox port — the one component where Rust enters (`STACK.md`).

**Exit:** a second machine runs a self-hosted runtime reachable through the relay; the same account sees both runtimes; a stranger's onboarding never touches a terminal (managed tier).

---

## After P6 — other products

"Leverage Maestro to build other products" is not a new feature: a product is a workspace. New repo → new workspace → contracts + routines. What matures per-product is the `done` library (oracles per stack) and the Loop 4 tuning — both live in files, both portable.

## Standing rules across all phases

- Nothing runs unattended before its guardrails exist (P2's list is the floor).
- Every phase ships light + dark, reduced-motion safe, holding every hard rule in `CLAUDE.md`.
- When a task here conflicts with a pattern in `PATTERNS.md`, the pattern wins or the pattern gets amended — no silent exceptions.
