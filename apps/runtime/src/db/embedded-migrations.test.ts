/// <reference types="bun" />
// embedded-migrations.test.ts — the compiled-safe migrator (BRO-1812).
//
// Two jobs:
//  1. SEAM: the statically-embedded migration list stays in lockstep with drizzle's
//     `meta/_journal.json`. A `bun run db:generate` that adds `0001_*.sql` without
//     registering it in EMBEDDED_MIGRATIONS would ship a binary missing the new
//     schema — this test fails first, in CI `quality`, not in production.
//  2. RUNTIME: applying to a fresh db creates every table and stamps
//     `user_version`; a re-apply is a no-op (restart idempotency without a folder).

import { describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createClient } from "@libsql/client";
import { INDEX_TABLES } from "@maestro/protocol";
import { applyEmbeddedMigrations, EMBEDDED_MIGRATIONS } from "./embedded-migrations";
import journal from "./migrations/meta/_journal.json" with { type: "json" };

describe("embedded migrations — the journal seam", () => {
  test("the embedded list matches the drizzle journal tag-for-tag, in order", () => {
    const journalTags = journal.entries
      .slice()
      .sort((a, b) => a.idx - b.idx)
      .map((e) => e.tag);
    expect(EMBEDDED_MIGRATIONS.map((m) => m.tag)).toEqual(journalTags);
  });

  test("every embedded migration carries non-empty DDL", () => {
    for (const m of EMBEDDED_MIGRATIONS) {
      expect(m.sql.length).toBeGreaterThan(0);
    }
  });
});

describe("applyEmbeddedMigrations — apply + idempotency", () => {
  test("a fresh db gains every index table and a stamped user_version", async () => {
    const client = createClient({ url: ":memory:" });
    try {
      const applied = await applyEmbeddedMigrations(client);
      expect(applied).toBe(EMBEDDED_MIGRATIONS.length);

      const tables = await client.execute(
        "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
      );
      const names = new Set(tables.rows.map((r) => String(r.name)));
      // The physical `scan_cursor` table is not in the protocol IndexTable enum;
      // assert the seven contract tables all exist.
      for (const t of INDEX_TABLES) expect(names.has(t)).toBe(true);
      expect(names.has("scan_cursor")).toBe(true);

      const uv = await client.execute("PRAGMA user_version");
      expect(Number(uv.rows[0]?.user_version)).toBe(EMBEDDED_MIGRATIONS.length);
    } finally {
      client.close();
    }
  });

  test("re-applying to an already-migrated file is a no-op (no 'table exists' throw)", async () => {
    // A FILE db (not :memory:, which mints a fresh db per connection) proves the
    // user_version stamp persists across a reconnect — the 24/7 restart path.
    const dir = mkdtempSync(join(tmpdir(), "maestro-embed-"));
    const url = `file:${join(dir, "index.db")}`;
    try {
      const first = createClient({ url });
      expect(await applyEmbeddedMigrations(first)).toBe(EMBEDDED_MIGRATIONS.length);
      first.close();

      const second = createClient({ url });
      expect(await applyEmbeddedMigrations(second)).toBe(0); // already at user_version
      second.close();
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
