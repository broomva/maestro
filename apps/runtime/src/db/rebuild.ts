// Index rebuild — "cache with teeth" (BRO-1808, ARCHITECTURE §3b, fs-index.md). The libSQL
// index is a DERIVED, rebuildable cache: the FS is truth (fs-index.md §1), so deleting the
// index file and rescanning the workspace must reproduce the same derived `node` rows. This
// module is the rebuild command + the canonical dump the identity invariant test compares.
//
// The identity holds MODULO the index-assigned `updatedAt` clock: two rebuilds at different
// wall-clock times stamp a different `updatedAt` (the team-tier LWW clock), but every FS-derived
// column is identical. `dumpIndex` strips that one clock so the dump is byte-stable across
// rebuilds; pass a fixed `now` to `rebuildIndex` and even `updatedAt` matches.

import { mkdir, rm } from "node:fs/promises";
import { dirname } from "node:path";
import { asc } from "drizzle-orm";
import { scanIntoIndex } from "../scanner";
import { type IndexDb, type IndexHandle, indexUrl, openIndex } from "./client";
import { node } from "./schema";

/**
 * One node row minus the index-assigned `updatedAt` clock — the canonical, rebuild-stable
 * shape. Everything here is FS-derived (frontmatter/path) and so is reproduced identically by
 * a rescan; `deletedAt` is included (a clean rebuild has no tombstones, so it is null for all —
 * a spurious tombstone would surface as a diff).
 */
export interface CanonicalNode {
  id: string;
  path: string;
  parentId: string | null;
  kind: typeof node.$inferSelect.kind;
  state: typeof node.$inferSelect.state;
  owner: string | null;
  gate: typeof node.$inferSelect.gate;
  budgetJson: string | null;
  doneJson: string | null;
  title: string | null;
  createdAt: number;
  deletedAt: number | null;
}

/**
 * A deterministic, timestamp-stripped dump of the index's `node` rows (sorted by id) — the
 * canonical form the rebuild-identity test compares. Byte-stable across rebuilds regardless of
 * the wall clock, because the index-assigned `updatedAt` is the only non-FS-derived column and
 * it is excluded.
 */
export async function dumpIndex(db: IndexDb): Promise<CanonicalNode[]> {
  const rows = await db.select().from(node).orderBy(asc(node.id));
  return rows.map((r) => ({
    id: r.id,
    path: r.path,
    parentId: r.parentId,
    kind: r.kind,
    state: r.state,
    owner: r.owner,
    gate: r.gate,
    budgetJson: r.budgetJson,
    doneJson: r.doneJson,
    title: r.title,
    createdAt: r.createdAt,
    deletedAt: r.deletedAt,
  }));
}

export interface RebuildResult {
  /** The freshly-opened index handle (the caller closes `handle.client`). */
  handle: IndexHandle;
  /** Nodes seen by the rescan. */
  nodeCount: number;
  /** Per-file scan errors, surfaced never dropped. */
  errors: string[];
}

export interface RebuildOptions {
  /** The index clock for the rescan (fixed value → deterministic `updatedAt`). Default now. */
  now?: number;
}

/**
 * Rebuild the index at `indexPath` from the workspace `root`: DELETE the index file (and its
 * WAL/SHM siblings) — the "kill the index" half of the P1 exit — then reopen (recreating an
 * empty schema'd db) and rescan the workspace into it. Returns the open handle; the caller
 * closes it. `indexPath` must be a real file path, not `:memory:` (there is no file to delete).
 */
export async function rebuildIndex(
  indexPath: string,
  root: string,
  opts: RebuildOptions = {},
): Promise<RebuildResult> {
  if (indexPath === ":memory:") {
    throw new Error("rebuildIndex needs a file index path, not :memory:");
  }
  // Remove the index and its libSQL WAL/SHM siblings so the reopen is a clean, empty db.
  await Promise.all([
    rm(indexPath, { force: true }),
    rm(`${indexPath}-wal`, { force: true }),
    rm(`${indexPath}-shm`, { force: true }),
  ]);
  // Ensure the parent dir exists — libSQL creates the FILE, not the dir. A first-ever rebuild
  // on a workspace whose `.maestro/` does not exist yet would otherwise fail with SQLite error
  // 14 ("unable to open database file"). Mirrors the startup path in index.ts.
  await mkdir(dirname(indexPath), { recursive: true });
  const handle = await openIndex(indexUrl(indexPath));
  const { summary, errors } = await scanIntoIndex(handle.db, root, opts.now);
  return {
    handle,
    nodeCount: summary.inserted + summary.updated + summary.unchanged,
    errors: errors.map((e) => e.message),
  };
}
