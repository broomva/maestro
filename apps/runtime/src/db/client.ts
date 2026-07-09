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

import { fileURLToPath } from "node:url";
import { type Client, createClient } from "@libsql/client";
import { drizzle, type LibSQLDatabase } from "drizzle-orm/libsql";
import { migrate } from "drizzle-orm/libsql/migrator";
import { indexSchema } from "./schema";

/** The drizzle handle, typed with the full index schema for relational queries. */
export type IndexDb = LibSQLDatabase<typeof indexSchema>;

/** An open index: the drizzle query handle plus the raw libSQL client to close. */
export interface IndexHandle {
  db: IndexDb;
  client: Client;
}

/**
 * The generated migrations folder, resolved relative to this module so it works
 * from `src/` and from `bun run`. NOTE (downstream): a `bun build --compile`
 * binary does not carry this folder on disk — the ticket that first opens the
 * index from the compiled supervisor must embed the migration SQL (import as
 * text, or ship the folder alongside the binary). This ticket only provides the
 * schema + migrations + the dev/test open path; the runtime still reports the
 * index as a stub (config.ts) and no compiled path imports this module yet.
 */
// fileURLToPath (not `.pathname`) so a checkout path with a space or unicode is
// percent-decoded and Windows drive letters resolve correctly.
export const MIGRATIONS_DIR = fileURLToPath(new URL("./migrations", import.meta.url));

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

/** Apply every pending migration from {@link MIGRATIONS_DIR} to an open handle. */
export async function applyMigrations(db: IndexDb): Promise<void> {
  await migrate(db, { migrationsFolder: MIGRATIONS_DIR });
}

/**
 * Open the index at `url` and bring it up to schema — the standard entry point.
 * Pass `indexUrl(config.indexPath)` for the real file, or `:memory:` for tests.
 */
export async function openIndex(url: string): Promise<IndexHandle> {
  const handle = createIndexClient(url);
  try {
    await applyMigrations(handle.db);
  } catch (err) {
    // A migration failure is fatal, but close the just-opened handle before
    // rethrowing so a failed open never leaks the libSQL file handle.
    handle.client.close();
    throw err;
  }
  return handle;
}
