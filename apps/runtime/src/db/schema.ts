// The control-plane index — drizzle-orm/sqlite-core table definitions (BRO-1796).
//
// This is the ORM binding of the row-shape contract in
// `@maestro/protocol/src/index-schema.ts`. The contract lives in the protocol
// package (it ships to the browser bundle and must stay dependency-free);
// the drizzle-orm ORM binding is a server dep, so the physical tables live HERE, in the
// runtime, and never reach the client (fs-index.md §7). The `$inferSelect` of
// every table is asserted structurally equal to its protocol row shape in
// schema.test.ts — that assertion is the seam: change a column and the contract
// test fails at `tsc`.
//
// The authority rule (fs-index.md §1, DATA-MODEL §B.1): the filesystem is the
// system of record; this index is a derived, transactional projection that never
// writes truth back. Each table below is tagged with its authority category.
//
// Three §4 pins this file MUST uphold (they are easy to get wrong):
//  1. Every `*At` / `ts` column is `integer(col, { mode: "number" })` storing
//     `Date.now()` (epoch ms). NOT `{ mode: "timestamp" }` (infers `Date`,
//     persists Unix *seconds* — loses ms and breaks the `number` pin) nor
//     `{ mode: "timestamp_ms" }` (also `Date`-typed).
//  2. `node.path` is a PARTIAL unique index scoped to `WHERE deleted_at IS NULL`,
//     never a global `.unique()` — a retained tombstone must not collide with a
//     delete-then-recreate of the same path.
//  3. `SyncFields` (updatedAt + deletedAt) ride only the syncable derived rows
//     (node, session, gate, schedule). The append-only `event` log and the
//     per-runtime authoritative rows (run_budget, lease) carry neither.
//
// Canon: DATA-MODEL §B.3 (the sketch these columns realize) · fs-index.md §3/§4/§5.

import type {
  Actor,
  EventType,
  GateKind,
  GateMode,
  GateVerdict,
  Kind,
  OrchState,
  SessionStatus,
  TriggerKind,
} from "@maestro/protocol";
import { sql } from "drizzle-orm";
import { index, integer, real, sqliteTable, text, uniqueIndex } from "drizzle-orm/sqlite-core";

// ── node — FS-DERIVED. Every work folder, indexed from its `_work.md`. ────────
// Rebuild source: fs-scan. Drop it and re-parse the workspace frontmatter.
export const node = sqliteTable(
  "node",
  {
    // = frontmatter `id` (stable UUID). FS-AUTHORED: survives rename/move; the
    // runtime never mints it (fs-index.md §4 "ID ownership").
    id: text("id").primaryKey(),
    // workspace-relative folder path. Unique among LIVE rows only (see index).
    path: text("path").notNull(),
    // nesting = the work tree; null at the workspace root.
    parentId: text("parent_id"),
    kind: text("kind").$type<Kind>().notNull(),
    state: text("state").$type<OrchState>().notNull(),
    owner: text("owner"),
    gate: text("gate").$type<GateMode>().notNull().default("human"),
    budgetJson: text("budget_json"),
    doneJson: text("done_json"),
    title: text("title"),
    // FS-derived (frontmatter `created`), so it is deterministic and part of the
    // rebuild-identity dump — unlike wall-clock `updatedAt`.
    createdAt: integer("created_at", { mode: "number" }).notNull(),
    // ── SyncFields (fs-index.md §4) ──
    // Index-assigned mutation clock (`Date.now()` at write), NOT frontmatter
    // `updated:` — the last-writer-wins clock for the team tier.
    updatedAt: integer("updated_at", { mode: "number" }).notNull(),
    // Soft delete: a vanished FS node tombstones (a peer runtime must learn it is
    // gone, never silently re-learn it as present). null = live.
    deletedAt: integer("deleted_at", { mode: "number" }),
  },
  (t) => [
    // §4 PIN: partial unique — scoped to live rows so a tombstone + recreate of
    // the same path does not collide.
    uniqueIndex("node_path_live_unique").on(t.path).where(sql`${t.deletedAt} IS NULL`),
    // B.5 hot paths: tree walk (parentId) + board grouping (state).
    index("node_parent_idx").on(t.parentId),
    index("node_state_idx").on(t.state),
  ],
);

// ── session — FS-DERIVED. One agent run against a node, in a `run/<id>` tree. ─
// Rebuild source: fs-scan + git-scan (runs/run-<id>/ + the branch diffstat).
export const session = sqliteTable(
  "session",
  {
    id: text("id").primaryKey(), // = run id, e.g. "7f3a"
    nodeId: text("node_id").notNull(),
    branch: text("branch").notNull(), // git worktree branch: run/<id>
    status: text("status").$type<SessionStatus>().notNull(),
    startedAt: integer("started_at", { mode: "number" }).notNull(),
    endedAt: integer("ended_at", { mode: "number" }),
    diffstatJson: text("diffstat_json"), // receipt: { files, plus, minus }
    // SyncFields
    updatedAt: integer("updated_at", { mode: "number" }).notNull(),
    deletedAt: integer("deleted_at", { mode: "number" }),
  },
  (t) => [index("session_node_idx").on(t.nodeId)],
);

// ── event — FS-DERIVED, APPEND-ONLY. The queryable projection of every ────────
// `session.jsonl` line + the workspace synthetic journal. Rebuild source:
// journal-replay. No SyncFields — an event never updates or soft-deletes; it IS
// the log. `seq` is the global total order + the SSE resume cursor (its VALUES
// are rebuild-scoped, renumbered in `compareReplay` order on a rebuild).
export const event = sqliteTable(
  "event",
  {
    seq: integer("seq").primaryKey({ autoIncrement: true }),
    // NULLABLE (D-DURABILITY): synthetics (node.updated, gate.opened/decided,
    // schedule.fired) have no session and are still persisted with sessionId null.
    sessionId: text("session_id"),
    // epoch ms (the runtime formats it to ISO at the wire boundary — EventEnvelope.ts).
    ts: integer("ts", { mode: "number" }).notNull(),
    actor: text("actor").$type<Actor>().notNull(),
    type: text("type").$type<EventType>().notNull(),
    // raw payload_json TEXT (the runtime parses it when it projects row → wire).
    payload: text("payload_json"),
  },
  // B.5 per-session timeline: `event where session_id = ? order by seq`.
  (t) => [index("event_session_seq_idx").on(t.sessionId, t.seq)],
);

// ── gate — FS-DERIVED. Pending + decided human decisions (the Org-Control ─────
// verdicts). Rebuild source: journal-replay (gate.opened / gate.decided, which
// are journaled — D-DURABILITY — so decided gates survive an index loss).
export const gate = sqliteTable("gate", {
  id: text("id").primaryKey(),
  sessionId: text("session_id").notNull(),
  kind: text("kind").$type<GateKind>().notNull(),
  proposalJson: text("proposal_json"), // the gate-card payload source
  verdict: text("verdict").$type<GateVerdict>(), // null = pending
  decidedBy: text("decided_by"), // @handle
  openedAt: integer("opened_at", { mode: "number" }).notNull(),
  decidedAt: integer("decided_at", { mode: "number" }),
  // SyncFields
  updatedAt: integer("updated_at", { mode: "number" }).notNull(),
  deletedAt: integer("deleted_at", { mode: "number" }),
});

// ── schedule — FS-DERIVED. Routines / triggers (Loop 3). Rebuild source: ──────
// fs-scan (routine `_work.md` trigger blocks).
export const schedule = sqliteTable(
  "schedule",
  {
    id: text("id").primaryKey(),
    nodeId: text("node_id").notNull(),
    triggerKind: text("trigger_kind").$type<TriggerKind>().notNull(),
    spec: text("spec").notNull(), // cron expr | interval | hook selector | goal condition
    nextFireAt: integer("next_fire_at", { mode: "number" }),
    enabled: integer("enabled", { mode: "boolean" }).notNull().default(true),
    // SyncFields
    updatedAt: integer("updated_at", { mode: "number" }).notNull(),
    deletedAt: integer("deleted_at", { mode: "number" }),
  },
  // B.5 orchestrator bench: `schedule where enabled`.
  (t) => [index("schedule_enabled_idx").on(t.enabled)],
);

// ── run_budget — AUTHORITATIVE. Read-modify-write transactionally BEFORE each ─
// model call (the budget-in-path guard, PATTERNS §8). Journal-backed: rebuilt by
// replaying `budget.*` events (D-DURABILITY). Per-runtime — never synced, so NO
// SyncFields.
export const runBudget = sqliteTable("run_budget", {
  sessionId: text("session_id").primaryKey(), // the session this budget meters
  spentUsd: real("spent_usd").notNull().default(0),
  iterations: integer("iterations").notNull().default(0),
  lastCallAt: integer("last_call_at", { mode: "number" }),
  // The guard reads per_run_usd / per_day_usd / max_iterations from node.budgetJson.
});

// ── lease — AUTHORITATIVE. Idempotency + locks (no double-fire, no heartbeat ──
// storms, PATTERNS §8). NOT FS-recoverable — a fresh runtime holds none and they
// expire; reconcile against receipts on crash. Per-runtime — never synced, so NO
// SyncFields.
export const lease = sqliteTable("lease", {
  key: text("key").primaryKey(), // a node id | a schedule idempotency key
  holder: text("holder").notNull(), // the runtime/worker id holding it
  acquiredAt: integer("acquired_at", { mode: "number" }).notNull(),
  expiresAt: integer("expires_at", { mode: "number" }).notNull(),
});

// ── scan_cursor — INDEX-INTERNAL (fs-index.md §5). The incremental-scan ───────
// high-water mark: one row per journal file, the byte offset the watcher has
// consumed so an incremental scan tails only new bytes. Losing it forces a full
// re-scan, never lost truth — excluded from the rebuild-identity dump.
export const scanCursor = sqliteTable("scan_cursor", {
  path: text("path").primaryKey(), // the journal file, workspace-relative
  byteOffset: integer("byte_offset").notNull(), // bytes consumed — resume the tail here
  lastSeq: integer("last_seq").notNull(), // highest event.seq produced from this file
  updatedAt: integer("updated_at", { mode: "number" }).notNull(),
});

/** Every table in the control-plane index — the schema object drizzle binds. */
export const indexSchema = {
  node,
  session,
  event,
  gate,
  schedule,
  runBudget,
  lease,
  scanCursor,
} as const;
