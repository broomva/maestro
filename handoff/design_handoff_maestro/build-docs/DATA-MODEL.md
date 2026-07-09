# Data model

A sketch, not a final schema — enough for Claude Code to scaffold against and for us to argue about specifics. It realizes the two-store model from `ARCHITECTURE.md` §3: **the filesystem is the system of record; the control-plane index is a derived, transactional projection.** Enums and naming match the existing scaffold (`apps/broomva/.../maestro/lib.ts`) so this is continuous with code that already exists.

> **The authority rule, restated:** every fact about *work* originates in a file. The index never invents truth — it indexes, coordinates, and answers fast queries. If the index is lost, rebuild it by scanning the workspace + git. The only state that is *not* rebuildable is live operational coordination (budget counters, leases) — and losing that is a crash you reconcile against receipts, not a lost record.

---

## Part A — The filesystem (system of record)

### A.1 The workspace
A workspace is a **git repository** the runtime owns. Work is folders and files inside it. **A folder is a unit of work at any scale** — depth is meaning, not a fixed schema. A question, a task, a project, an initiative are all just folders that happen to nest.

```
workspace/                         # a git repo — the substrate
  growth/                          # a folder = work (here: an initiative)
    _work.md                       # the work contract + brief (frontmatter below)
    notes/                         # memory the agent writes, freely
      decisions.md
    seo-refresh/                   # nested work (a project) — depth = meaning
      _work.md
      fix-meta-tags/               # nested again (a task)
        _work.md
        runs/
          run-7f3a/                # one session's receipt
            session.jsonl          # the event log (append-only, durable truth)
            progress.md            # disk memory, rewritten every iteration
            fix_plan.md            # the task list the loop reads + updates
            verdict.md             # the verifier's judge verdict + evidence
  routines/
    nightly-triage/
      _work.md                     # kind: routine, with a trigger block
```

The actual code changes a run produces do **not** sit in `runs/` — they live on a **git branch `run/<id>`** (the worktree). The branch *is* the receipt; `runs/run-<id>/` holds the session's narrative + memory + verdict alongside it.

### A.2 The work contract (`_work.md` frontmatter)
This is the orchestration contract — the single most important data structure in Broomva. It's YAML frontmatter on every work folder's `_work.md`, readable by agent and human alike.

```yaml
---
id: 7f3a9c                 # stable UUID — survives renames/moves (sync-ready)
kind: task                 # question | task | project | initiative | routine
state: running             # orch-state enum (see B.2) — the system state
owner: "@alex"             # human (@handle) or agent (agent:maestro) responsible
gate: human                # human | auto — does *done* require the human gate?

budget:                    # the guardrail contract (enforced in the request path)
  per_run_usd: 5
  per_day_usd: 20
  max_iterations: 40

done:                      # the verifiable success function (Loop 2 / AUTONOMY.md)
  check: "pnpm test && pnpm lint"   # deterministic oracle — preferred gate
  judge: rubric.md                   # optional LLM-judge rubric, for non-test work
  stop_on: [cap, no_progress, budget] # the three independent stop conditions

trigger:                   # only for kind: routine / event-driven work (Loop 3)
  on: cron                 # heartbeat | cron | hook | goal
  at: "0 6 * * *"
  idempotency: nightly-triage-{{date}}

created: 2026-06-25
updated: 2026-06-25
---

# Fix meta tags
<!-- the brief: what this work is, in plain language. The "look" the gate shows. -->
```

Field notes:
- **`kind`** is meaning, not mechanism — the runtime treats all kinds the same; the label is for humans and for how the UI groups. `routine` is the only kind with runtime-special behavior (it carries a `trigger`).
- **`gate: human`** is the default for anything irreversible. It's *why* a clean run parks at "Needs you" instead of completing.
- **`done`** is mandatory before a loop runs unattended. Deterministic `check` first; `judge` is the weaker fallback for subjective work (`AUTONOMY.md` §2).
- **`budget` + `done.stop_on`** are the guardrail contract — the runtime reads them, the index enforces them.

### A.3 The session log (`session.jsonl`)
Append-only JSONL, one event per line — the **durable truth** for what happened in a run. The index's `event` table is a queryable projection of these files.

```jsonl
{"ts":"2026-06-25T06:00:01Z","actor":"system","type":"run.started","run":"7f3a","branch":"run/7f3a"}
{"ts":"2026-06-25T06:00:14Z","actor":"agent","type":"tool.call","tool":"edit","path":"src/head.tsx"}
{"ts":"2026-06-25T06:01:02Z","actor":"tool","type":"check.result","check":"pnpm test","ok":true}
{"ts":"2026-06-25T06:01:09Z","actor":"agent","type":"verdict","ok":true,"by":"agent:judge","evidence":"verdict.md"}
{"ts":"2026-06-25T06:01:10Z","actor":"system","type":"gate.opened","gate":"human","reason":"clean, awaiting approval"}
```

`actor` is always one of `agent | user | tool | system`. `type` is namespaced (`run.* | tool.* | check.* | gate.* | budget.*`). This log is the audit trail the guardrails require and the source the UI's activity timeline renders.

---

## Part B — The control-plane index (derived, transactional)

An embedded SQLite-class store, local to the runtime. Shown as Drizzle-flavored TypeScript; treat types as the contract, columns as a sketch.

> **Storage engine: libSQL embedded (local file), via `drizzle-orm/libsql`. Not Turso Cloud — yet.** The index is runtime-local, single-writer, and rebuildable from the FS; the hot path is a transactional budget read-modify-write before each model call — SQLite's exact sweet spot. Turso Cloud's differentiators (edge replication, embedded replicas, db-per-tenant) solve problems this topology doesn't have, and a hosted DB cuts against the self-host trust story. Use **libSQL** (Turso's drop-in SQLite fork) as the driver from day one so adopting Turso Cloud later is a swap, not a migration — it earns its place only in two futures: the **managed multi-tenant runtime tier** (db-per-workspace economics) or **multi-runtime-per-workspace** team coordination (a shared lease primary — open question #3).

### B.1 What's derived vs. authoritative
- **Derived & rebuildable from FS** (a cache with teeth): `node`, `session`, `event`, `gate`, `schedule`. Drop them and re-scan the workspace.
- **Authoritative live operational state** (not in the FS, must be transactional): `run_budget`, `lease`. These coordinate concurrent runs; on crash, reconcile against receipts.

### B.2 The orch-state enum (matches existing code)
```ts
// mirrors apps/.../maestro/lib.ts — do not rename without updating the board
export type OrchState =
  | "proposed" | "reviewing" | "triggered"   // backlog → about to run
  | "running"                                 // active
  | "blocked" | "review"                      // ATTENTION — needs the human
  | "done" | "canceled";                      // terminal

export const ATTENTION_STATES = ["blocked", "review"] as const;
```

**System enum → plain voice** (the UI never shows the left column; see `CLAUDE.md`):

| OrchState | Plain voice | Dot |
|---|---|---|
| `proposed`, `reviewing` | Queued | neutral gray |
| `triggered`, `running` | Running | live tidepool (DotComet) |
| `blocked` | Stuck | warning |
| `review` | **Needs you** | **accent-blue** (the gate, never red) |
| `done` | Done | success |
| `canceled` | Canceled | neutral gray |
| routine between fires | Standing | pulse dot |

### B.3 Tables

```ts
// ── node: every work folder, indexed from its _work.md frontmatter ──────────
export const node = sqliteTable("node", {
  id:        text("id").primaryKey(),            // = frontmatter id (stable UUID)
  path:      text("path").notNull().unique(),    // workspace-relative folder path
  parentId:  text("parent_id"),                  // nesting = the work tree
  kind:      text("kind").notNull(),             // question|task|project|initiative|routine
  state:     text("state").$type<OrchState>().notNull(),
  owner:     text("owner"),                       // @handle | agent:name
  gate:      text("gate").$type<"human"|"auto">().notNull().default("human"),
  budgetJson:text("budget_json"),                 // snapshot of the budget contract
  doneJson:  text("done_json"),                   // snapshot of the success function
  title:     text("title"),                       // first heading of _work.md
  updatedAt: integer("updated_at", { mode: "timestamp" }).notNull(),
});

// ── session: one agent run against a node, in a worktree ────────────────────
export const session = sqliteTable("session", {
  id:        text("id").primaryKey(),            // = run id, e.g. "7f3a"
  nodeId:    text("node_id").notNull(),
  branch:    text("branch").notNull(),           // git worktree branch: run/<id>
  status:    text("status").$type<"running"|"blocked"|"review"|"done"|"canceled">().notNull(),
  startedAt: integer("started_at", { mode: "timestamp" }).notNull(),
  endedAt:   integer("ended_at",   { mode: "timestamp" }),
  diffstat:  text("diffstat_json"),              // receipt: files/+/- — shown in inspector
});

// ── event: queryable projection of every session.jsonl line (the stream) ────
export const event = sqliteTable("event", {
  seq:       integer("seq").primaryKey({ autoIncrement: true }), // fan-out cursor
  sessionId: text("session_id").notNull(),
  ts:        integer("ts", { mode: "timestamp" }).notNull(),
  actor:     text("actor").$type<"agent"|"user"|"tool"|"system">().notNull(),
  type:      text("type").notNull(),             // run.* | tool.* | check.* | gate.* | budget.*
  payload:   text("payload_json"),
});

// ── gate: pending + decided human decisions (Org-Control-Layer verdicts) ────
export const gate = sqliteTable("gate", {
  id:        text("id").primaryKey(),
  sessionId: text("session_id").notNull(),
  kind:      text("kind").notNull(),             // e.g. "completion" | "irreversible-action"
  proposal:  text("proposal_json"),              // what the agent wants to do
  verdict:   text("verdict").$type<"approve"|"revise"|"block"|"escalate">(), // null = pending
  decidedBy: text("decided_by"),                 // @handle
  decidedAt: integer("decided_at", { mode: "timestamp" }),
});

// ── schedule: routines / triggers (Loop 3) ──────────────────────────────────
export const schedule = sqliteTable("schedule", {
  id:        text("id").primaryKey(),
  nodeId:    text("node_id").notNull(),
  triggerKind: text("trigger_kind").$type<"heartbeat"|"cron"|"hook"|"goal">().notNull(),
  spec:      text("spec").notNull(),             // cron expr | interval | hook selector
  nextFireAt:integer("next_fire_at", { mode: "timestamp" }),
  enabled:   integer("enabled", { mode: "boolean" }).notNull().default(true),
});

// ── run_budget: AUTHORITATIVE — checked transactionally BEFORE each call ─────
export const runBudget = sqliteTable("run_budget", {
  sessionId: text("session_id").primaryKey(),
  spentUsd:    real("spent_usd").notNull().default(0),
  iterations:  integer("iterations").notNull().default(0),
  lastCallAt:  integer("last_call_at", { mode: "timestamp" }),
  // guard reads per_run_usd / per_day_usd / max_iterations from node.budgetJson
});

// ── lease: AUTHORITATIVE — idempotency + locks (no double-fire, no storms) ──
export const lease = sqliteTable("lease", {
  key:       text("key").primaryKey(),           // e.g. node id, or schedule idempotency key
  holder:    text("holder").notNull(),           // runtime/worker id
  acquiredAt:integer("acquired_at", { mode: "timestamp" }).notNull(),
  expiresAt: integer("expires_at", { mode: "timestamp" }).notNull(),
});
```

### B.4 How the loops touch the data (`AUTONOMY.md`)
- **Loop 1 (agent)** appends to `session.jsonl`, rewrites `progress.md`/`fix_plan.md`, commits to `run/<id>`.
- **Before each model call**: transactional read-modify-write on `run_budget`; refuse if over `per_run/per_day/max_iterations`. This is the budget-in-path guard.
- **Loop 2 (verify)** runs `done.check`; writes `verdict.md`; emits a `verdict` event; sets `session.status`.
- **Loop 3 (trigger)** acquires a `lease` on the schedule's idempotency key before firing (kills hook/heartbeat storms); the **governance gate** writes a `gate` row whose `verdict` is one of the four Org-Control-Layer values = Broomva's verbs.
- **Loop 4 (hill-climbing)** reads `event` history across many sessions; proposes edits to `done`/`budget`/prompts (writes back to `_work.md` frontmatter — FS stays the source of truth).

### B.5 Reactive queries the clients need
- **"Needs you" headline** → `count(node where state in ('blocked','review'))`.
- **The board** → `node` grouped by `state`, attention order (`blocked, review, running, triggered, reviewing, proposed, done, canceled`).
- **Activity timeline** → `event where session_id = ? order by seq`.
- **The orchestrator's bench** → `schedule where enabled` + their next fire + any live sessions.
- The change feed for SSE fan-out (`ARCHITECTURE.md` §4) is `event.seq` as the cursor.

---

## Open questions to resolve before locking

> **All four are now decided in `specs/DECISIONS.md` (D2–D5).** Kept here for the reasoning trail.
1. **Workspace = repo, or repo-inside-workspace?** Sketch assumes the workspace *is* a git repo. If a workspace must span multiple repos (cross-repo work), `node` needs a `repo` dimension.
2. **Event log scale.** JSONL-per-run is clean but a heartbeat routine could produce huge logs — decide a rotation/summarization policy (the deep dive's "summarize every 10–20 steps" applies here too).
3. **Multi-runtime per workspace** (team tier) — leases already assume a `holder`; confirm one workspace is owned by exactly one runtime, or design lease arbitration.
4. **Budget reconciliation on crash** — define the startup reconcile: re-derive `spent_usd` from `event` `budget.*` entries if `run_budget` is stale.
