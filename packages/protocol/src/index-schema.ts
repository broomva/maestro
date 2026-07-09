// Index schema — the derived control-plane index as a ROW-SHAPE contract.
//
// FS is the system of record; the index is a derived, transactional projection
// that never writes truth back (ARCHITECTURE §3, PATTERNS §1-2 "two-store,
// one-way authority"). This module pins the SHAPES of the seven index tables +
// the scan cursor — the *contract*. The drizzle-orm/libsql table definitions and
// migrations live in apps/runtime (BRO-1796), never here: `@maestro/protocol`
// ships to the browser bundle and must stay dependency-free, and DATA-MODEL §B is
// explicit that "types are the contract, columns a sketch".
//
// Full prose + the derived-vs-authoritative split per table: docs/contracts/fs-index.md.
// Canon: DATA-MODEL §B.1/§B.3/§B.5, ARCHITECTURE §3/§7, PATTERNS §1/§2,
// canon-amendments D-DURABILITY / D-ORDER.

import type { EventEnvelope } from "./events";
import type { Kind, TriggerKind } from "./intents";
import type { GateMode, GateVerdict, OrchState } from "./state";
import type { GateKind, SessionStatus } from "./work";

// ── The tables + the authority split (DATA-MODEL §B.1) ───────────────────────

/** The seven control-plane index tables (DATA-MODEL §B.3). */
export type IndexTable =
  | "node"
  | "session"
  | "event"
  | "gate"
  | "schedule"
  | "run_budget"
  | "lease";

export const INDEX_TABLES = [
  "node",
  "session",
  "event",
  "gate",
  "schedule",
  "run_budget",
  "lease",
] as const satisfies readonly IndexTable[];

/**
 * The authority axis (DATA-MODEL §B.1, ARCHITECTURE §3b):
 *  - `fs-derived`  — a cache with teeth: drop it and re-scan the workspace + git
 *    + FS event journal to rebuild it identically.
 *  - `authoritative` — live operational state not in the FS (budget counters,
 *    leases). Journaled to the FS as events (D-DURABILITY) so a rebuild can
 *    replay them; reconciled against receipts on crash.
 */
export type IndexAuthority = "fs-derived" | "authoritative";

export const TABLE_AUTHORITY = {
  node: "fs-derived",
  session: "fs-derived",
  event: "fs-derived",
  gate: "fs-derived",
  schedule: "fs-derived",
  run_budget: "authoritative",
  lease: "authoritative",
} as const satisfies Record<IndexTable, IndexAuthority>;

export const REBUILDABLE_TABLES: readonly IndexTable[] = INDEX_TABLES.filter(
  (t) => TABLE_AUTHORITY[t] === "fs-derived",
);

export const AUTHORITATIVE_TABLES: readonly IndexTable[] = INDEX_TABLES.filter(
  (t) => TABLE_AUTHORITY[t] === "authoritative",
);

/**
 * How a full index rebuild recovers each table (ARCHITECTURE §3b, D-DURABILITY):
 *  - `fs-scan`        — walk the workspace, parse `_work.md` frontmatter.
 *  - `git-scan`       — derive from `run/<id>` branches + diffstats.
 *  - `journal-replay` — replay `session.jsonl` + the workspace synthetic journal
 *    in canonical order (see `compareReplay`).
 *  - `reconcile`      — NOT FS-recoverable; reconcile against receipts on crash.
 *    A fresh runtime holds no leases and they expire, so identity excludes them.
 */
export type RebuildSource = "fs-scan" | "git-scan" | "journal-replay" | "reconcile";

export const TABLE_REBUILD = {
  node: ["fs-scan"],
  session: ["fs-scan", "git-scan"],
  event: ["journal-replay"],
  // gate.opened / gate.decided are journaled (D-DURABILITY), so decided gates
  // survive the index — the rebuild guarantee stays unqualified.
  gate: ["journal-replay"],
  schedule: ["fs-scan"],
  // budget.* is journaled (D-DURABILITY): spend counters replay from the log.
  run_budget: ["journal-replay"],
  lease: ["reconcile"],
} as const satisfies Record<IndexTable, readonly RebuildSource[]>;

// ── Sync-ready invariants (ARCHITECTURE §7) ──────────────────────────────────

/**
 * Sync-ready invariants on every syncable derived row (ARCHITECTURE §7): a stable
 * UUID primary key (in the row's `id`/`path`/`key`), `updatedAt` for
 * last-writer-wins ordering, and a soft delete so a vanished row *tombstones*
 * instead of disappearing (a peer runtime in the team tier must learn it is gone,
 * never silently re-learn it as present). Append-only rows (`event`) and the
 * authoritative operational rows (`run_budget`, `lease`) do NOT carry these —
 * events are immutable and the operational rows are per-runtime, never synced.
 */
export interface SyncFields {
  /** epoch ms; monotone per row — the last-writer-wins clock for the team tier. */
  updatedAt: number;
  /** epoch ms of soft delete, or null when live. A vanished FS node tombstones. */
  deletedAt: number | null;
}

// ── The seven table row shapes (DATA-MODEL §B.3) ─────────────────────────────

/** `node` — every work folder, indexed from its `_work.md` frontmatter. */
export interface NodeRow extends SyncFields {
  /** = frontmatter `id` (stable UUID; survives rename/move — runtime never mints it). */
  id: string;
  /** workspace-relative folder path; unique among live (non-tombstoned) rows. */
  path: string;
  /** nesting = the work tree; null at the workspace root. */
  parentId: string | null;
  kind: Kind;
  state: OrchState;
  /** `@handle` | `agent:name`. */
  owner: string | null;
  gate: GateMode;
  /** snapshot of the `budget:` contract (JSON string). */
  budgetJson: string | null;
  /** snapshot of the `done:` success function (JSON string). */
  doneJson: string | null;
  /** first heading of `_work.md`. */
  title: string | null;
  /** epoch ms — frontmatter `created` (the age the board comparator groups by). */
  createdAt: number;
}

/** `session` — one agent run against a node, in a `run/<id>` worktree. */
export interface SessionRow extends SyncFields {
  /** = run id, e.g. "7f3a". */
  id: string;
  nodeId: string;
  /** git worktree branch: `run/<id>`. */
  branch: string;
  status: SessionStatus;
  startedAt: number;
  endedAt: number | null;
  /** receipt: `{ files, plus, minus }` (JSON string) — shown in the inspector. */
  diffstatJson: string | null;
}

/**
 * `event` — the queryable projection of every `session.jsonl` line plus the
 * workspace synthetic journal. The row shape IS the wire envelope
 * (`EventEnvelope`, events.ts); PATTERNS §10 forbids re-declaring a wire type, so
 * this is an alias, not a copy. Append-only and immutable — no `SyncFields`
 * (an event never updates or soft-deletes; it is the log). `seq` is the global
 * autoincrement total order and the SSE resume cursor (DATA-MODEL §B.5);
 * `sessionId` is null for synthetics (D-DURABILITY). Storage MAY hold `ts` as
 * epoch ms and project it to ISO-8601 at the wire boundary — a storage detail
 * owned by BRO-1796, not a second protocol type.
 */
export type EventRow = EventEnvelope;

/** `gate` — pending + decided human decisions (Org-Control-Layer verdicts). */
export interface GateRow extends SyncFields {
  id: string;
  sessionId: string;
  /**
   * `completion` | `irreversible-action` today; seam-gate-queue (BRO-1789)
   * widens `GateKind` to add `question` and closes the enum.
   */
  kind: GateKind;
  /** what the agent wants to do (JSON string) — the source of the gate-card payload. */
  proposalJson: string | null;
  /** null = pending (opened, not yet decided). */
  verdict: GateVerdict | null;
  /** `@handle`. */
  decidedBy: string | null;
  openedAt: number;
  decidedAt: number | null;
}

/** `schedule` — routines / triggers, Loop 3. */
export interface ScheduleRow extends SyncFields {
  id: string;
  nodeId: string;
  triggerKind: TriggerKind;
  /** cron expr | interval | hook selector | goal condition (`Trigger.at`). */
  spec: string;
  nextFireAt: number | null;
  enabled: boolean;
}

/**
 * `run_budget` — AUTHORITATIVE. Read-modify-write transactionally BEFORE each
 * model call (the budget-in-path guard, PATTERNS §8). Journal-backed: rebuilt by
 * replaying `budget.*` events (D-DURABILITY). Per-runtime — never synced.
 */
export interface RunBudgetRow {
  /** pk — the session this budget meters. */
  sessionId: string;
  spentUsd: number;
  iterations: number;
  lastCallAt: number | null;
  // the guard reads per_run_usd / per_day_usd / max_iterations from node.budgetJson.
}

/**
 * `lease` — AUTHORITATIVE. Idempotency + locks: no double-fire, no heartbeat
 * storms (PATTERNS §8). NOT FS-recoverable — a fresh runtime holds no leases and
 * they expire; reconcile against receipts on crash. Per-runtime — never synced.
 */
export interface LeaseRow {
  /** pk — a node id | a schedule idempotency key. */
  key: string;
  /** the runtime/worker id holding it. */
  holder: string;
  acquiredAt: number;
  expiresAt: number;
}

// ── The high-water mark (this seam pins it — not in the DATA-MODEL sketch) ────

/**
 * The incremental-scan high-water mark. Two coordinates the seam pins:
 *  - GLOBAL: the max `event.seq` the index has assigned — the SSE resume cursor
 *    and the event total order (DATA-MODEL §B.5).
 *  - PER-FILE: the byte offset into each `session.jsonl` the watcher has consumed,
 *    so an incremental scan (p1-watcher) tails only new bytes.
 * Index-internal operational state: a full rebuild resets every offset to 0 and
 * re-replays, so `scan_cursor` is excluded from the rebuild-identity dump.
 */
export interface ScanCursorRow {
  /** pk — the journal file, workspace-relative (e.g. `runs/run-7f3a/session.jsonl`). */
  path: string;
  /** bytes consumed — resume the tail from here. */
  byteOffset: number;
  /** the highest `event.seq` produced from this file so far. */
  lastSeq: number;
  updatedAt: number;
}

// ── The rebuild-identity core: the canonical replay total order ──────────────

/**
 * A journal event awaiting replay — the minimal key the rebuild orders by. On a
 * full rebuild the runtime assigns `event.seq` in `compareReplay` order, so two
 * rebuilds of the same workspace produce a byte-identical `event` table (the
 * identity guarantee named here, implemented in p1-rebuild-invariant). Ordering
 * by `(ts, sourcePath, line)` is a STRICT TOTAL order because `(sourcePath, line)`
 * is unique across all journal lines and breaks `ts` ties deterministically.
 */
export interface ReplayKey {
  /** epoch ms of the event (parsed from the journal line). */
  ts: number;
  /** the journal file this line came from, workspace-relative. */
  sourcePath: string;
  /** 0-based line number within `sourcePath`. */
  line: number;
}

/**
 * The canonical replay comparator — a strict total order over journal lines that
 * makes `event.seq` assignment deterministic, hence the rebuild byte-identical.
 * Returns <0 / 0 / >0. Returns 0 iff the two keys denote the same journal line.
 */
export function compareReplay(a: ReplayKey, b: ReplayKey): number {
  if (a.ts !== b.ts) return a.ts - b.ts;
  if (a.sourcePath !== b.sourcePath) return a.sourcePath < b.sourcePath ? -1 : 1;
  return a.line - b.line;
}

/** True when two replay keys denote the same journal line (`compareReplay` === 0). */
export function replayKeyEqual(a: ReplayKey, b: ReplayKey): boolean {
  return compareReplay(a, b) === 0;
}
