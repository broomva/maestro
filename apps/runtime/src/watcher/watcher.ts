// FS watcher — the live half of the P1 exit (BRO-1804, ROADMAP §P1, DATA-MODEL §B.1).
//
// A `_work.md` edit on disk → re-scan → reconcile into the `node` table (reusing the BRO-1800
// scanner + sync) → append a `node.updated` synthetic event per changed live node → the SSE
// change feed (BRO-1816 poll-the-tail over `event.seq`) carries it to the board with no reload.
//
// Authority stays one-way (fs-index.md §1): the FS is truth, this only projects it. The
// reconcile is a FULL scan (idempotent — an unchanged re-scan writes nothing and emits nothing),
// debounced so a burst of editor saves coalesces into one pass. `node.updated` carries the full
// LiveNode row (store/types.ts: a convergent upsert); tombstones never cross the wire.

import { watch } from "node:fs";
import { EVENT_TYPES } from "@maestro/protocol";
import { inArray } from "drizzle-orm";
import type { IndexDb } from "../db/client";
import { projectLiveNode } from "../db/project";
import { event, node } from "../db/schema";
import { SKIP_DIRS, type SyncSummary, scanWorkspace, syncNodes } from "../scanner";

/** git worktrees for `run/<id>` branches — their `_work.md` copies must never re-index. */
const WORKTREE_SEGMENT = "run";
/**
 * Session-receipt dir `runs/run-<id>/` (DATA-MODEL §A.1). During an active run this churns
 * hard — session.jsonl appended per event, progress.md rewritten per iteration, checks/*.log —
 * and holds NO `_work.md` (produces no nodes), so it must never wake a reconcile. NOTE this is
 * PLURAL `runs`, distinct from the singular `run/<id>` worktree branch above; suppressing only
 * `run` (an earlier bug) left the real receipt churn unfiltered. The scanner still descends into
 * `runs/` (it is not in SKIP_DIRS — a deliberate BRO-1800 choice); trimming that scan footprint
 * is tracked in BRO-1846.
 */
const RECEIPTS_DIR = "runs";

export interface ReconcileResult {
  summary: SyncSummary;
  /** node.updated synthetics appended this pass. */
  emitted: number;
}

/**
 * Re-scan the workspace, reconcile it into the index, and append one `node.updated` event per
 * inserted/changed LIVE node. Reuses `scanWorkspace` + `syncNodes` (which does the content diff
 * and reports `changedIds`); re-reads the upserted rows so the payload is the exact indexed
 * shape (with the assigned `updatedAt`). Not wrapped in `db.transaction()` — libsql's tx opens a
 * separate connection, so a `:memory:` db tx hits an empty database (same reason as syncNodes).
 */
export async function reconcileAndEmit(
  db: IndexDb,
  root: string,
  now: number = Date.now(),
): Promise<ReconcileResult> {
  const { nodes, complete } = await scanWorkspace(root);
  const summary = await syncNodes(db, nodes, now, { tombstone: complete });
  if (summary.changedIds.length === 0) return { summary, emitted: 0 };

  const rows = await db.select().from(node).where(inArray(node.id, summary.changedIds));
  const events = rows
    .filter((r) => r.deletedAt === null) // tombstones never cross the wire (store/types.ts)
    .map((r) => ({
      sessionId: null,
      ts: now,
      actor: "system" as const,
      type: EVENT_TYPES.NODE_UPDATED,
      payload: JSON.stringify(projectLiveNode(r)),
    }));
  if (events.length > 0) await db.insert(event).values(events);
  return { summary, emitted: events.length };
}

/**
 * Should this changed path wake a reconcile? Wake on ANYTHING outside the known-noisy areas —
 * the reconcile is a full idempotent re-scan (an unchanged scan writes nothing and emits
 * nothing), so a spurious wake costs one cheap no-op pass. We do NOT try to require the changed
 * file be a `_work.md`, because Bun's recursive `fs.watch` on macOS reports only the TOP path
 * segment (`child`, not `child/_work.md`) — so the exact filename is not reliably available.
 * The reconcile, not the filename, decides what actually changed.
 *
 * Suppressed (never wake): a skipped dir (`.git`/`node_modules`/`.maestro`/`dist` — index
 * internals, deps, build output); the `runs/run-<id>/` session-receipt churn (DATA-MODEL §A.1 —
 * high-frequency session.jsonl/progress.md writes, and NO `_work.md`); and a `run/<id>`
 * git-worktree branch. `runs/` never holds a node, so it is suppressed at ANY depth. For the
 * worktree, Bun gives us only the top segment, so a top-level `run` is treated as churn; a user
 * folder literally named `run`/`runs` still gets indexed by the startup full scan, just not on
 * a live edit.
 */
export function isWatchedChange(relPath: string): boolean {
  const parts = relPath.split(/[/\\]/).filter(Boolean);
  if (parts.length === 0) return false;
  if (parts.some((seg) => SKIP_DIRS.has(seg))) return false; // under a skipped dir
  // Session receipts hold no node — ignore `runs/…` at any depth (the real run-churn source,
  // PLURAL `runs`, distinct from the singular `run/<id>` worktree below).
  if (parts.some((seg) => seg === RECEIPTS_DIR)) return false;
  if (parts[0] === WORKTREE_SEGMENT) return false; // top-level `run` (Bun-truncated worktree)
  // On a platform that gives full paths, catch a nested `run/<id>/…` worktree too.
  for (let i = 1; i < parts.length - 1; i += 1) {
    if (parts[i] === WORKTREE_SEGMENT) return false;
  }
  return true;
}

export interface WatcherHandle {
  stop: () => void;
}

export interface WatcherOptions {
  /** Quiet window before a burst of saves reconciles as one pass. Default 150ms. */
  debounceMs?: number;
  /** Fired after each reconcile — observability + a test barrier. */
  onReconcile?: (result: ReconcileResult) => void;
}

export interface ReconcileScheduler {
  /** Note a change — (re)arm the debounce; the reconcile runs once the quiet window passes. */
  schedule: () => void;
  /** Cancel a pending debounce (does not abort an in-flight reconcile). Idempotent. */
  cancel: () => void;
}

export interface SchedulerOptions {
  reconcile: () => Promise<ReconcileResult>;
  onReconcile?: (result: ReconcileResult) => void;
  /** Quiet window (ms) before a burst of `schedule()` calls collapses to one reconcile. */
  debounceMs: number;
  /** When true, no new reconcile (incl. the trailing re-run) starts. Default: never closed. */
  isClosed?: () => boolean;
}

/**
 * The debounced, single-flight reconcile core of the watcher — separated from `fs.watch` so
 * its two invariants are testable without OS timing:
 *   - DEBOUNCE: a burst of `schedule()` calls within `debounceMs` collapses to ONE reconcile.
 *   - SINGLE-FLIGHT: reconciles never overlap. A real reconcile (recursive scan + N sequential
 *     db round-trips) can outlast the debounce, and two overlapping passes would collide on the
 *     `node.id` INSERT — `syncNodes` is deliberately non-transactional (libsql `:memory:` tx
 *     opens a fresh connection), so the loser throws a UNIQUE violation, its change batch is
 *     dropped, and (phase 1 already deleted the changed rows) the index silently diverges from
 *     disk. So at most one reconcile runs at a time; a `schedule()` that lands mid-pass sets a
 *     `pending` flag and the in-flight pass re-runs exactly once when it finishes (trailing-edge
 *     coalesce). Bursts collapse to serial single-flight passes, never concurrent ones.
 */
export function createReconcileScheduler(opts: SchedulerOptions): ReconcileScheduler {
  const { reconcile, onReconcile, debounceMs } = opts;
  const isClosed = opts.isClosed ?? (() => false);
  let timer: ReturnType<typeof setTimeout> | null = null;
  let running = false; // a reconcile is in flight (single-flight guard)
  let pending = false; // a wake landed mid-pass — re-run once when the pass finishes

  const run = () => {
    running = true;
    reconcile()
      .then((result) => onReconcile?.(result))
      .catch((err) => console.warn(`maestro watcher · reconcile failed: ${(err as Error).message}`))
      .finally(() => {
        // Synchronous: no `await` between clearing `running` and the trailing re-run, so a
        // concurrent `fire()` cannot slip a second reconcile in through the gap.
        running = false;
        if (pending && !isClosed()) {
          pending = false;
          run();
        }
      });
  };

  const fire = () => {
    timer = null;
    if (isClosed()) return;
    if (running) {
      pending = true; // coalesce into a single trailing re-run rather than overlap
      return;
    }
    run();
  };

  return {
    schedule: () => {
      if (isClosed()) return;
      if (timer) clearTimeout(timer);
      timer = setTimeout(fire, debounceMs);
    },
    cancel: () => {
      if (timer) clearTimeout(timer);
      timer = null;
      pending = false;
    },
  };
}

/**
 * Watch `root` for `_work.md` edits and keep the index live. Wires `fs.watch` events to a
 * debounced single-flight `createReconcileScheduler` (which owns the concurrency invariants).
 * Returns a handle whose `stop()` closes the OS watcher and cancels any pending debounce
 * (idempotent). A failing reconcile is logged, never thrown — a bad edit must not kill the
 * watcher.
 */
export function startWatcher(db: IndexDb, root: string, opts: WatcherOptions = {}): WatcherHandle {
  const { debounceMs = 150, onReconcile } = opts;
  let closed = false;
  const scheduler = createReconcileScheduler({
    reconcile: () => reconcileAndEmit(db, root),
    onReconcile,
    debounceMs,
    isClosed: () => closed,
  });

  const watcher = watch(root, { recursive: true }, (_type, filename) => {
    if (closed || !filename) return;
    if (!isWatchedChange(filename.toString())) return;
    scheduler.schedule();
  });

  return {
    stop: () => {
      if (closed) return;
      closed = true;
      scheduler.cancel();
      watcher.close();
    },
  };
}
