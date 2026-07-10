// Workspace scanner — the stateful half of BRO-1800 (FLOWS §F9 step 1).
//
// Reconcile a pure scan (../scanner.ts `ScannedNode[]`) into the `node` table:
// upsert every scanned node, and soft-delete (tombstone) every live node that the
// scan no longer sees. The reconciliation is idempotent — re-scanning an unchanged
// workspace writes NOTHING, so `updatedAt` (the last-writer-wins clock for the team
// tier, fs-index.md §4) is not churned; a spurious bump would signal a change to a
// peer runtime that never happened.
//
// Authority stays one-way (fs-index.md §1): the FS is truth, this only projects it.

import { eq } from "drizzle-orm";
import type { IndexDb } from "../db/client";
import { node } from "../db/schema";
import { type ScanError, type ScannedNode, scanWorkspace } from "./scanner";

export interface SyncSummary {
  /** New nodes inserted. */
  inserted: number;
  /** Existing nodes whose content changed, incl. a tombstoned node resurrected. */
  updated: number;
  /** Live nodes the scan no longer sees — soft-deleted. */
  tombstoned: number;
  /** Nodes present and byte-identical (modulo the index clock) — not rewritten. */
  unchanged: number;
}

/** The FS-derived columns — everything except the index-assigned `updatedAt`/`deletedAt`. */
const CONTENT_KEYS = [
  "path",
  "parentId",
  "kind",
  "state",
  "owner",
  "gate",
  "budgetJson",
  "doneJson",
  "title",
  "createdAt",
] as const;

/** True when an existing row's FS-derived content equals a freshly scanned node. */
function sameContent(existing: typeof node.$inferSelect, scanned: ScannedNode): boolean {
  for (const k of CONTENT_KEYS) {
    if (existing[k] !== scanned[k]) return false;
  }
  return true;
}

export interface SyncOptions {
  /**
   * Soft-delete live nodes the scan no longer sees. Default true. Pass false for an
   * INCOMPLETE scan (a dir was unreadable) so a transient read failure cannot
   * mass-tombstone a subtree it merely failed to see — `scanIntoIndex` wires this to
   * `ScanResult.complete`.
   */
  tombstone?: boolean;
}

/**
 * Apply a scan to the `node` table in two phases — FREE, then CLAIM — so the
 * partial-unique `node.path` (live rows only) never transiently collides:
 *
 *   Phase 1 (free)   delete every changed row (re-inserted below, so its old path is
 *                    vacated) and tombstone every vanished row (deletedAt leaves it out
 *                    of the live index).
 *   Phase 2 (claim)  insert the new + changed rows.
 *
 * After phase 1 every path a phase-2 insert claims is free, and scanned paths are
 * distinct per folder, so an ARBITRARY offline reorg — a rename onto a vanished or
 * moved sibling's path, a move-chain, even a pure two-node swap — reconciles with no
 * UNIQUE violation and no dependence on statement order. (An earlier update-then-
 * tombstone ordering wedged on exactly these reorgs.)
 *
 * The reconciliation is idempotent — an unchanged re-scan writes NOTHING, so
 * `updatedAt` (the team-tier LWW clock, fs-index.md §4) is never churned.
 *
 * NOT wrapped in `db.transaction()`: libsql's transaction API opens a separate
 * connection, and a `:memory:` db is per-connection, so a tx there hits an empty
 * database. Atomicity is not required — F9 step 4 opens the API only AFTER reconcile
 * completes (no reader sees a partial index), and because phase 2 never collides the
 * reconcile cannot wedge; a crash mid-reconcile leaves a subset written that the next
 * idempotent scan completes (fs-index.md "cache with teeth").
 */
export async function syncNodes(
  db: IndexDb,
  scanned: ScannedNode[],
  now: number = Date.now(),
  opts: SyncOptions = {},
): Promise<SyncSummary> {
  const { tombstone = true } = opts;
  const existing = await db.select().from(node);
  const existingById = new Map(existing.map((r) => [r.id, r]));
  const scannedIds = new Set(scanned.map((s) => s.id));
  const summary: SyncSummary = { inserted: 0, updated: 0, tombstoned: 0, unchanged: 0 };

  const toClaim: ScannedNode[] = []; // new + changed → inserted in phase 2
  const toFree: string[] = []; // ids of changed rows to delete in phase 1 (frees old path)
  for (const s of scanned) {
    const ex = existingById.get(s.id);
    if (!ex) {
      toClaim.push(s);
      summary.inserted++;
    } else if (ex.deletedAt === null && sameContent(ex, s)) {
      summary.unchanged++;
    } else {
      // changed content, or a resurrected tombstone — delete the stale row, re-insert.
      toFree.push(s.id);
      toClaim.push(s);
      summary.updated++;
    }
  }
  const vanished = tombstone
    ? existing.filter((ex) => ex.deletedAt === null && !scannedIds.has(ex.id))
    : [];
  summary.tombstoned = vanished.length;

  // Phase 1 — FREE: delete changed rows (paths vacated) + tombstone vanished rows.
  for (const id of toFree) await db.delete(node).where(eq(node.id, id));
  for (const ex of vanished) {
    await db.update(node).set({ deletedAt: now, updatedAt: now }).where(eq(node.id, ex.id));
  }
  // Phase 2 — CLAIM: every conflicting old occupant was freed above, so no collision.
  for (const s of toClaim) await db.insert(node).values({ ...s, updatedAt: now, deletedAt: null });

  return summary;
}

export interface ScanIntoIndexResult {
  summary: SyncSummary;
  errors: ScanError[];
}

/**
 * Scan a workspace and reconcile it into the index in one call — the startup
 * entry point (FLOWS §F9 step 1). Returns the sync summary plus any per-file scan
 * errors (surfaced, never silently dropped).
 */
export async function scanIntoIndex(
  db: IndexDb,
  root: string,
  now: number = Date.now(),
): Promise<ScanIntoIndexResult> {
  const { nodes, errors, complete } = await scanWorkspace(root);
  // An incomplete scan (a dir was unreadable) must not tombstone — the missing nodes
  // may simply not have been seen, not deleted.
  const summary = await syncNodes(db, nodes, now, { tombstone: complete });
  return { summary, errors };
}
