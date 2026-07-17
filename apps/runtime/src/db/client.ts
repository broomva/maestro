// The control-plane index client — opens the embedded SQLite file and applies
// migrations (BRO-1796).
//
// Driver (fs-index.md §7, DATA-MODEL §B; BRO-1841): drizzle-orm/bun-sqlite over
// Bun's built-in `bun:sqlite`. The index is a runtime-local, single-writer,
// rebuildable-from-FS SQLite db — the hot path is a budget read-modify-write
// before each model call, SQLite's exact sweet spot. `bun:sqlite` is compiled
// INTO the Bun runtime, so it embeds cleanly in `bun build --compile`: the
// single-binary self-host (P5's day-one deliverable) opens the index and serves
// reads, which the prior `@libsql/client` native addon could NOT (it is a
// `.node` binding `bun build --compile` cannot embed — the crash BRO-1841 closes).
// The Turso-Cloud-swap story `@libsql/client` bought is deferred to a team-tier
// server deploy (STACK.md); the schema (drizzle-orm/sqlite-core) and the
// `$inferSelect ≡ protocol` seam are driver-agnostic, so that swap stays a swap.
//
// This module OPENS the index; it does not own the four loops or the guards —
// those (scanner BRO-1800, read API BRO-1812, SSE BRO-1816, budget guard
// BRO-1788) hang off the handle this returns.

import { Database } from "bun:sqlite";
import { type BunSQLiteDatabase, drizzle } from "drizzle-orm/bun-sqlite";
import { applyEmbeddedMigrations } from "./embedded-migrations";
import { indexSchema } from "./schema";

/** The drizzle handle, typed with the full index schema for relational queries. */
export type IndexDb = BunSQLiteDatabase<typeof indexSchema>;

/** An open index: the drizzle query handle plus the raw `bun:sqlite` db to close. */
export interface IndexHandle {
  db: IndexDb;
  client: Database;
}

/**
 * Resolve a filesystem index path to the argument `bun:sqlite`'s `new Database()`
 * takes. `bun:sqlite` opens a plain FS path directly (and `:memory:` for the
 * in-memory db tests use), so this is a passthrough — no `file:` URL scheme (that
 * was the `@libsql/client` form). Kept as the single seam every open goes through,
 * so the driver's path contract lives in one place. Resolve the path before
 * calling (the runtime config already resolves `indexPath` against the workspace).
 */
export function indexUrl(indexPath: string): string {
  return indexPath;
}

/** Open a drizzle handle over a `bun:sqlite` file path (or `:memory:`) WITHOUT migrating. */
export function createIndexClient(path: string): IndexHandle {
  const client = new Database(path);
  const db = drizzle(client, { schema: indexSchema });
  return { db, client };
}

/**
 * Bring an open handle up to the current schema via the compiled-safe embedded
 * migrator (embedded-migrations.ts) — the ONE schema path for dev, test, and the
 * `bun build --compile` binary alike. Idempotent: a reopened file skips already-
 * applied migrations. (`bun:sqlite` is synchronous; the async signature is kept so
 * the many `await openIndex(...)` call sites are untouched.)
 */
export async function applyMigrations(handle: IndexHandle): Promise<void> {
  applyEmbeddedMigrations(handle.client);
}

/**
 * Open the index at `path` and bring it up to schema — the standard entry point.
 * Pass `indexUrl(config.indexPath)` for the real file, or `:memory:` for tests.
 */
export async function openIndex(path: string): Promise<IndexHandle> {
  const handle = createIndexClient(path);
  try {
    await applyMigrations(handle);
  } catch (err) {
    // A migration failure is fatal, but close the just-opened db before rethrowing
    // so a failed open never leaks the SQLite file handle.
    handle.client.close();
    throw err;
  }
  return handle;
}
