// The control-plane index client — opens the embedded libSQL file and applies
// migrations (BRO-1796).
//
// Driver split (fs-index.md §7, DATA-MODEL §B): drizzle-orm/libsql over
// @libsql/client. libSQL is a runtime-local, single-writer, rebuildable-from-FS
// SQLite fork — the hot path is a transactional budget read-modify-write before
// each model call, SQLite's exact sweet spot. Using the libsql driver from day
// one makes adopting Turso Cloud later a swap, not a migration.
//
// This module OPENS the index; it does not own the four loops or the guards —
// those (scanner BRO-1800, read API BRO-1812, SSE BRO-1816, budget guard
// BRO-1788) hang off the handle this returns.

import { type Client, createClient } from "@libsql/client";
import { drizzle, type LibSQLDatabase } from "drizzle-orm/libsql";
import { applyEmbeddedMigrations } from "./embedded-migrations";
import { indexSchema } from "./schema";

/** The drizzle handle, typed with the full index schema for relational queries. */
export type IndexDb = LibSQLDatabase<typeof indexSchema>;

/** An open index: the drizzle query handle plus the raw libSQL client to close. */
export interface IndexHandle {
  db: IndexDb;
  client: Client;
}

/**
 * Build a libSQL url for a filesystem index path. `:memory:` is passed through
 * unchanged (the in-memory db used by tests); every other path becomes a
 * `file:` url. The path is used verbatim — resolve it before calling (the
 * runtime config already resolves `indexPath` against the workspace root).
 */
export function indexUrl(indexPath: string): string {
  return indexPath === ":memory:" ? indexPath : `file:${indexPath}`;
}

/** Open a drizzle handle over a libSQL url WITHOUT applying migrations. */
export function createIndexClient(url: string): IndexHandle {
  const client = createClient({ url });
  const db = drizzle(client, { schema: indexSchema });
  return { db, client };
}

/**
 * Bring an open handle up to the current schema via the compiled-safe embedded
 * migrator (embedded-migrations.ts) — the ONE schema path for dev, test, and the
 * `bun build --compile` binary alike. Idempotent: a reopened file skips already-
 * applied migrations.
 */
export async function applyMigrations(handle: IndexHandle): Promise<void> {
  await applyEmbeddedMigrations(handle.client);
}

/**
 * Open the index at `url` and bring it up to schema — the standard entry point.
 * Pass `indexUrl(config.indexPath)` for the real file, or `:memory:` for tests.
 */
export async function openIndex(url: string): Promise<IndexHandle> {
  const handle = createIndexClient(url);
  try {
    await applyMigrations(handle);
  } catch (err) {
    // A migration failure is fatal, but close the just-opened handle before
    // rethrowing so a failed open never leaks the libSQL file handle.
    handle.client.close();
    throw err;
  }
  return handle;
}
