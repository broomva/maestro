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

import { Database } from "bun:sqlite";
import { describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { INDEX_TABLES } from "@maestro/protocol";
import { applyEmbeddedMigrations, EMBEDDED_MIGRATIONS } from "./embedded-migrations";
import journal from "./migrations/meta/_journal.json" with { type: "json" };

/** Table names in the db, sorted — the `bun:sqlite` read the assertions share. */
function tableNames(db: Database): Set<string> {
  const rows = db
    .query("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
    .all() as { name: string }[];
  return new Set(rows.map((r) => r.name));
}

/** The stored `PRAGMA user_version` (0 on a fresh db). */
function userVersion(db: Database): number {
  const row = db.query("PRAGMA user_version").get() as { user_version: number } | null;
  return row ? Number(row.user_version) : 0;
}

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
  test("a fresh db gains every index table and a stamped user_version", () => {
    const db = new Database(":memory:");
    try {
      const applied = applyEmbeddedMigrations(db);
      expect(applied).toBe(EMBEDDED_MIGRATIONS.length);

      const names = tableNames(db);
      // The physical `scan_cursor` table is not in the protocol IndexTable enum;
      // assert the seven contract tables all exist.
      for (const t of INDEX_TABLES) expect(names.has(t)).toBe(true);
      expect(names.has("scan_cursor")).toBe(true);

      expect(userVersion(db)).toBe(EMBEDDED_MIGRATIONS.length);
    } finally {
      db.close();
    }
  });

  test("self-heals a crash-interrupted partial apply (some tables, user_version still 0)", () => {
    // Simulate a first apply that created SOME tables then crashed before the version
    // stamp: run only the first statements of migration 0000, leaving user_version 0.
    const db = new Database(":memory:");
    const m0 = EMBEDDED_MIGRATIONS[0];
    if (!m0) throw new Error("expected at least one embedded migration");
    try {
      const firstThree = m0.sql
        .split("--> statement-breakpoint")
        .map((s) => s.trim())
        .filter((s) => s.length > 0)
        .slice(0, 3);
      for (const stmt of firstThree) db.run(stmt);
      expect(userVersion(db)).toBe(0); // partial, unstamped

      // Recovery: applyEmbeddedMigrations must create the MISSING tables (skipping the
      // present ones), not wedge on "table already exists".
      const applied = applyEmbeddedMigrations(db);
      expect(applied).toBe(EMBEDDED_MIGRATIONS.length);

      const names = tableNames(db);
      for (const t of INDEX_TABLES) expect(names.has(t)).toBe(true);
      expect(userVersion(db)).toBe(EMBEDDED_MIGRATIONS.length);
    } finally {
      db.close();
    }
  });

  test("re-applying to an already-migrated file is a no-op (no 'table exists' throw)", () => {
    // A FILE db (not :memory:, which mints a fresh db per connection) proves the
    // user_version stamp persists across a reconnect — the 24/7 restart path.
    const dir = mkdtempSync(join(tmpdir(), "maestro-embed-"));
    const path = join(dir, "index.db");
    try {
      const first = new Database(path);
      expect(applyEmbeddedMigrations(first)).toBe(EMBEDDED_MIGRATIONS.length);
      first.close();

      const second = new Database(path);
      expect(applyEmbeddedMigrations(second)).toBe(0); // already at user_version
      second.close();
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
