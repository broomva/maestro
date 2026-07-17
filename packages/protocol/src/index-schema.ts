// Index schema — the derived control-plane index as a ROW-SHAPE contract.
//
// FS is the system of record; the index is a derived, transactional projection
// that never writes truth back (ARCHITECTURE §3, PATTERNS §1-2 "two-store,
// one-way authority"). This module pins the SHAPES of the seven index tables +
// the scan cursor — the *contract*. The drizzle-orm/sqlite-core table definitions and
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
  /**
   * epoch ms — the INDEX-assigned mutation clock (`Date.now()` at index write), NOT
   * the frontmatter `updated:` field (which is intentionally not indexed). Monotone
   * per row; the last-writer-wins clock for the team tier. Because it is wall-clock,
   * the rebuild identity (§6) holds only *modulo* `updatedAt`.
   */
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
  /**
   * epoch ms — frontmatter `created`, the node's creation timestamp. NOT implicitly the
   * board sort key: the board within-group tiebreak is keyed on RECENCY of attention
   * (`updatedAt` / a gate's `openedAt`) and owned by seam-gate-queue (BRO-1789) — sorting
   * the attention board by creation time would bury freshly-actionable old work. A
   * creation-date display is a separate, optional concern (WorkItem.created).
   */
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
 * workspace synthetic journal. `EventRow` is the STORAGE row: it reuses the wire
 * envelope (`EventEnvelope`, events.ts) but differs on the THREE fields the storage
 * representation changes:
 *  - `ts` — epoch ms, not the ISO wire string (BRO-1796 stores it via
 *    `integer(ts, {mode:"number"})` — see §4; the runtime formats it to ISO at the
 *    wire boundary, `EventEnvelope.ts: string`);
 *  - `payload` — the raw `payload_json` TEXT, not the rehydrated object;
 *  - `sessionId` — required-nullable (`string | null`), because a stored row is
 *    never `undefined`; the wire type leaves it optional. Null for synthetics
 *    (D-DURABILITY).
 * `EventEnvelope` stays THE wire type (PATTERNS §10); the runtime parses `payload` +
 * formats `ts` when it projects a row to the wire (BRO-1796). Append-only + immutable
 * — no `SyncFields` (an event never updates or soft-deletes; it is the log). `seq` is
 * the global total order + the live SSE resume cursor (DATA-MODEL §B.5); its VALUES
 * are rebuild-scoped, not preserved across a rebuild (see `compareReplay`).
 */
export type EventRow = Omit<EventEnvelope, "ts" | "payload" | "sessionId"> & {
  sessionId: string | null;
  ts: number;
  payload: string | null;
};

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
 * full rebuild the runtime assigns `event.seq` in `compareReplay` order, so **two
 * rebuilds** of the same workspace produce a byte-identical `event` table — the
 * rebuild-vs-rebuild identity the property test proves (the full kill/rebuild/diff
 * is p1-rebuild-invariant).
 *
 * This is CANONICAL re-derivation, NOT reproduction of the pre-loss LIVE index:
 * live `event.seq` is ingest-ordered (byte-arrival — the SSE cursor), and under
 * concurrent sessions writing separate `run/<id>/session.jsonl` files, ingest
 * order differs from `(ts, sourcePath, line)` order. So seq VALUES are
 * rebuild-scoped and an SSE cursor does not survive a rebuild — a rebuild is a
 * stream reset (docs/contracts/fs-index.md §6). What a rebuild reproduces is every
 * derived query answer (DATA-MODEL §B.5), not the live seq integers.
 *
 * Ordering by `(ts, sourcePath, line)` is a STRICT TOTAL order because
 * `(sourcePath, line)` is unique across all journal lines and breaks `ts` ties
 * deterministically.
 *
 * Two preconditions BRO-1796's scanner must uphold for the byte-identity + the
 * per-session query-answer guarantees (fs-index.md §6):
 *  - `sourcePath` is BYTE-CANONICAL — one fixed Unicode normalization form, no
 *    `./` / symlink / alias spellings. `compareReplay` compares it raw, so two
 *    spellings of one physical file split into two keys and break rebuild identity.
 *  - per-session `ts` is MONOTONE non-decreasing in file order — the Loop-1 append
 *    path (BRO-1790) clamps each event's `ts` to `max(prev_ts, now)`. Only then
 *    does the within-session `(ts, line)` order equal write/file order (fs-index.md
 *    §6.2); without it a clock step-back would re-order a session's timeline.
 */
export interface ReplayKey {
  /** epoch ms of the event — a FINITE number (the parser rejects malformed lines before a key is built). */
  ts: number;
  /** the journal file, BYTE-CANONICAL workspace-relative (fixed Unicode form, no alias/symlink spellings). */
  sourcePath: string;
  /** 0-based line number within `sourcePath`. */
  line: number;
}

/**
 * The canonical replay comparator — a strict total order over journal lines that
 * makes `event.seq` assignment deterministic, hence two rebuilds byte-identical.
 * Returns <0 / 0 / >0; 0 iff the two keys denote the same journal line.
 *
 * A non-finite `ts` (a corrupt line the parser should have rejected) is normalized
 * to +Infinity, so it sorts LAST and ties only other non-finite keys — which keeps
 * the comparator a *genuine* total order. (Comparing a raw NaN with `<`/`>` would be
 * non-transitive: NaN ties every finite `ts` on both sides, so finite keys would
 * order by `ts` while a NaN ordered only by path — `n < lo < hi` yet `n > hi` — and
 * `Array.sort` would then make `event.seq` depend on input order, breaking the
 * byte-identical rebuild.)
 */
export function compareReplay(a: ReplayKey, b: ReplayKey): number {
  const at = Number.isFinite(a.ts) ? a.ts : Number.POSITIVE_INFINITY;
  const bt = Number.isFinite(b.ts) ? b.ts : Number.POSITIVE_INFINITY;
  if (at < bt) return -1;
  if (at > bt) return 1;
  if (a.sourcePath < b.sourcePath) return -1;
  if (a.sourcePath > b.sourcePath) return 1;
  if (a.line < b.line) return -1;
  if (a.line > b.line) return 1;
  return 0;
}

/** True when two replay keys denote the same journal line (`compareReplay` === 0). */
export function replayKeyEqual(a: ReplayKey, b: ReplayKey): boolean {
  return compareReplay(a, b) === 0;
}
