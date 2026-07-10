// Compiled-safe schema application (BRO-1812).
//
// `bun build --compile` produces a single binary that carries NO on-disk
// migrations/ folder, so drizzle-orm/libsql's folder migrator
// (`migrate(db, { migrationsFolder })`) cannot run inside the shipped supervisor —
// it would throw "migrations folder not found" the moment the runtime opened the
// index. Instead we STATICALLY text-import each generated migration `.sql` — Bun
// embeds a `with { type: "text" }` import into the compiled binary (verified) — and
// apply it exactly once, guarded by `PRAGMA user_version`.
//
// This is the runtime's ONLY schema-application path — dev (`bun run`), test
// (`:memory:`), and the compiled binary all take it, so the three behave
// identically (the same guarantee the rebuild-identity tests lean on). The index
// is a derived, rebuildable-from-FS cache (fs-index.md §1), so "ensure the schema
// exists" is the right semantics, not multi-environment migration history —
// `user_version` is exactly that, embedded in the SQLite file itself.

import type { Client } from "@libsql/client";
import migration0000 from "./migrations/0000_shallow_cassandra_nova.sql" with { type: "text" };

/** One embedded migration — its journal tag plus the raw DDL text. */
export interface EmbeddedMigration {
  tag: string;
  sql: string;
}

/**
 * Every migration, in journal order. This list MUST stay in lockstep with
 * `migrations/meta/_journal.json`: a fresh `bun run db:generate` output is not
 * applied by the compiled binary until it is registered here (a static import is
 * the only form `bun build --compile` can embed). The seam is machine-checked —
 * `embedded-migrations.test.ts` asserts this list's tags + order equal the
 * journal's entries, so a forgotten registration fails `bun test`, not production.
 */
export const EMBEDDED_MIGRATIONS: readonly EmbeddedMigration[] = [
  { tag: "0000_shallow_cassandra_nova", sql: migration0000 },
];

/**
 * Bring a libSQL database up to the embedded schema, idempotently. Reads
 * `PRAGMA user_version` (0 on a fresh db), applies every migration at or beyond the
 * stored version, then stamps `user_version` to the migration count. A reopen finds
 * `user_version` already at the count and applies nothing — so a restarted 24/7
 * supervisor never re-runs `CREATE TABLE` and never throws "table already exists".
 *
 * Each migration's DDL is a `;`-separated script (drizzle's `--> statement-
 * breakpoint` markers are `--` line comments), so `executeMultiple` runs the whole
 * script in one call. Not wrapped in a transaction: libSQL auto-commits DDL, and a
 * half-applied fresh derived index is simply re-derived on the next open (the
 * "cache with teeth" contract). Returns how many migrations it applied.
 */
export async function applyEmbeddedMigrations(client: Client): Promise<number> {
  const res = await client.execute("PRAGMA user_version");
  const row = res.rows[0];
  const current = row ? Number(row.user_version) : 0;
  let applied = 0;
  for (const migration of EMBEDDED_MIGRATIONS.slice(current)) {
    await client.executeMultiple(migration.sql);
    applied++;
  }
  if (applied > 0) {
    // PRAGMA cannot bind a parameter; the stamp is an integer literal we control
    // (the migration count), never user input — no injection surface.
    await client.execute(`PRAGMA user_version = ${EMBEDDED_MIGRATIONS.length}`);
  }
  return applied;
}
