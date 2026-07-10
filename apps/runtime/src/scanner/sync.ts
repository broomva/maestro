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

/**
 * Apply a scan to the `node` table. Ordering matters for the partial-unique
 * `node.path` (live rows only): updates + tombstones run BEFORE inserts, so a node
 * that moved to a new path (or vanished) frees its old path before a different node
 * claims it in the same scan. (A pure path *swap* between two retained ids in a
 * single scan — extraordinarily rare — is the one case this order can't resolve; the
 * incremental watcher, BRO-1804, avoids it by processing one FS event at a time.)
 *
 * The passes run sequentially, NOT inside `db.transaction()`: libsql's transaction
 * API opens a separate connection, and a `:memory:` db is per-connection, so a
 * transaction there operates on an empty database (breaks tests + any in-memory
 * runtime). Atomicity is not required for the startup scan anyway — F9 step 4 opens
 * the API only AFTER reconcile completes (no reader sees a partial index), and the
 * sync is idempotent, so a scan interrupted by a crash is fully healed by the next
 * startup scan (fs-index.md "cache with teeth").
 */
export async function syncNodes(
  db: IndexDb,
  scanned: ScannedNode[],
  now: number = Date.now(),
): Promise<SyncSummary> {
  const existing = await db.select().from(node);
  const existingById = new Map(existing.map((r) => [r.id, r]));
  const scannedIds = new Set(scanned.map((s) => s.id));
  const summary: SyncSummary = { inserted: 0, updated: 0, tombstoned: 0, unchanged: 0 };

  // Pass 1 — update changed/resurrected rows (frees + moves paths).
  for (const s of scanned) {
    const ex = existingById.get(s.id);
    if (!ex) continue;
    if (ex.deletedAt === null && sameContent(ex, s)) {
      summary.unchanged++;
      continue;
    }
    await db
      .update(node)
      .set({ ...s, updatedAt: now, deletedAt: null })
      .where(eq(node.id, s.id));
    summary.updated++;
  }
  // Pass 2 — tombstone live rows the scan no longer sees.
  for (const ex of existing) {
    if (ex.deletedAt === null && !scannedIds.has(ex.id)) {
      await db.update(node).set({ deletedAt: now, updatedAt: now }).where(eq(node.id, ex.id));
      summary.tombstoned++;
    }
  }
  // Pass 3 — insert new rows (paths freed by passes 1-2 are now available).
  for (const s of scanned) {
    if (existingById.has(s.id)) continue;
    await db.insert(node).values({ ...s, updatedAt: now, deletedAt: null });
    summary.inserted++;
  }

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
  const { nodes, errors } = await scanWorkspace(root);
  const summary = await syncNodes(db, nodes, now);
  return { summary, errors };
}
