/// <reference types="bun" />
// rebuild.test.ts — the index-identity invariant for BRO-1808 (`bun test apps/runtime --filter rebuild`).
//
// The index is a DERIVED cache (fs-index.md "cache with teeth"): the FS is truth, so KILLING
// the index and rescanning must reproduce byte-identical `node` rows. This is the P1-exit
// half "killed index rebuilds identical". It composes the scanner's own determinism guarantee
// (scanner.test.ts: "same workspace → identical set") one level up, at the index.
//
// Uses a FILE-backed index (NOT `:memory:`) on purpose: rebuildIndex deletes the index file,
// which only means something when there is a file on disk to delete.

import { afterAll, expect, test } from "bun:test";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { asc } from "drizzle-orm";
import { indexUrl, openIndex } from "./client";
import { dumpIndex, rebuildIndex } from "./rebuild";
import { node } from "./schema";

// ── fixtures (mirrors scanner.test.ts) ──────────────────────────────────────────
const tmps: string[] = [];
afterAll(() => {
  for (const t of tmps) rmSync(t, { recursive: true, force: true });
});

function makeFixture(files: Record<string, string>): string {
  const root = mkdtempSync(join(tmpdir(), "maestro-rebuild-"));
  tmps.push(root);
  for (const [rel, content] of Object.entries(files)) {
    const abs = join(root, rel);
    mkdirSync(dirname(abs), { recursive: true });
    writeFileSync(abs, content);
  }
  return root;
}

function wm(o: {
  id: string;
  kind?: string;
  state?: string;
  owner?: string;
  gate?: string;
  extraYaml?: string;
  created?: string;
  brief?: string;
}): string {
  const lines = [
    "---",
    `id: ${o.id}`,
    `kind: ${o.kind ?? "task"}`,
    `state: ${o.state ?? "proposed"}`,
  ];
  if (o.owner) lines.push(`owner: "${o.owner}"`);
  if (o.gate) lines.push(`gate: ${o.gate}`);
  if (o.extraYaml) lines.push(o.extraYaml);
  lines.push(
    `created: ${o.created ?? "2026-06-25"}`,
    `updated: ${o.created ?? "2026-06-25"}`,
    "---",
  );
  if (o.brief) lines.push("", o.brief);
  return `${lines.join("\n")}\n`;
}

/** A non-trivial tree: root initiative, a child, a depth-2 grandchild, and a node
 *  exercising the budget/done JSON columns — so identity covers parentId, nesting, JSON. */
function workspaceFixture(): string {
  return makeFixture({
    "_work.md": wm({ id: "root", kind: "initiative", brief: "# The whole thing" }),
    "alpha/_work.md": wm({ id: "alpha", kind: "project", owner: "@bree", brief: "# Alpha" }),
    "alpha/beta/_work.md": wm({
      id: "beta",
      state: "running",
      gate: "auto",
      extraYaml: "done:\n  check: bun test apps/runtime",
      brief: "# Beta deep",
    }),
    "gamma/_work.md": wm({
      id: "gamma",
      state: "review",
      extraYaml: "budget:\n  usd: 5\ndone:\n  check: bun test",
      brief: "# Gamma with a budget",
    }),
  });
}

/** Open the file index, run one deterministic scan, return the raw rows sorted by id. */
async function buildAndRows(indexPath: string, root: string, now: number) {
  const handle = await openIndex(indexUrl(indexPath));
  const scanner = await import("../scanner");
  await scanner.scanIntoIndex(handle.db, root, now);
  const rows = await handle.db.select().from(node).orderBy(asc(node.id));
  const dump = await dumpIndex(handle.db);
  handle.client.close();
  return { rows, dump };
}

// ── the invariant ────────────────────────────────────────────────────────────
test("rebuild reproduces byte-identical nodes modulo the index clock", async () => {
  const root = workspaceFixture();
  const dir = mkdtempSync(join(tmpdir(), "maestro-idx-"));
  tmps.push(dir);
  const indexPath = join(dir, "index.db");

  // Build #1 at clock 1000.
  const first = await buildAndRows(indexPath, root, 1000);
  expect(first.dump.length).toBe(4); // root, alpha, beta, gamma
  expect(existsSync(indexPath)).toBe(true);

  // KILL the index + rebuild at a DIFFERENT clock (2000). The file is deleted then rescanned.
  const rebuilt = await rebuildIndex(indexPath, root, { now: 2000 });
  expect(rebuilt.errors).toEqual([]);
  expect(rebuilt.nodeCount).toBe(4);
  const afterDump = await dumpIndex(rebuilt.handle.db);
  const afterRows = await rebuilt.handle.db.select().from(node).orderBy(asc(node.id));
  rebuilt.handle.client.close();

  // Identity MODULO the clock: every FS-derived column matches across the different-clock rebuild.
  expect(afterDump).toEqual(first.dump);
  // ...and the clock genuinely moved — proving the dump's timestamp-strip is load-bearing,
  // not vacuously equal because nothing changed.
  expect(afterRows.every((r) => r.updatedAt === 2000)).toBe(true);
  expect(first.rows.every((r) => r.updatedAt === 1000)).toBe(true);
});

test("rebuild at the SAME clock reproduces the full rows, updatedAt included", async () => {
  const root = workspaceFixture();
  const dir = mkdtempSync(join(tmpdir(), "maestro-idx-"));
  tmps.push(dir);
  const indexPath = join(dir, "index.db");

  const first = await buildAndRows(indexPath, root, 7777);
  const rebuilt = await rebuildIndex(indexPath, root, { now: 7777 });
  const afterRows = await rebuilt.handle.db.select().from(node).orderBy(asc(node.id));
  rebuilt.handle.client.close();

  // Fixed clock ⇒ even the LWW `updatedAt` is deterministic, so the raw rows are identical.
  expect(afterRows).toEqual(first.rows);
});

test("rebuild deletes the prior index file, not merely reopening it", async () => {
  const root = workspaceFixture();
  const dir = mkdtempSync(join(tmpdir(), "maestro-idx-"));
  tmps.push(dir);
  const indexPath = join(dir, "index.db");

  await buildAndRows(indexPath, root, 1000);
  // A stray row that the FS does NOT justify — if rebuild merely reopened (didn't delete), this
  // ghost would survive. A clean rebuild drops the whole file, so it must be gone afterward.
  const ghost = await openIndex(indexUrl(indexPath));
  await ghost.db.insert(node).values({
    id: "ghost",
    path: "ghost",
    parentId: null,
    kind: "task",
    state: "proposed",
    owner: null,
    gate: "human",
    budgetJson: null,
    doneJson: null,
    title: null,
    createdAt: 1,
    updatedAt: 1,
    deletedAt: null,
  });
  ghost.client.close();

  const rebuilt = await rebuildIndex(indexPath, root, { now: 3000 });
  const ids = (await dumpIndex(rebuilt.handle.db)).map((r) => r.id);
  rebuilt.handle.client.close();
  expect(ids).not.toContain("ghost");
  expect(ids).toEqual(["alpha", "beta", "gamma", "root"]);
});

test("rebuild creates the index's parent dir when it does not exist yet", async () => {
  // The first-ever rebuild on a fresh workspace: `.maestro/` does not exist. libSQL creates the
  // FILE but not the DIR, so without an mkdir this fails with SQLite error 14 (found by dogfood).
  const root = workspaceFixture();
  const dir = mkdtempSync(join(tmpdir(), "maestro-idx-"));
  tmps.push(dir);
  const indexPath = join(dir, ".maestro", "index.db"); // parent does NOT exist
  expect(existsSync(dirname(indexPath))).toBe(false);

  const rebuilt = await rebuildIndex(indexPath, root, { now: 1000 });
  const count = (await dumpIndex(rebuilt.handle.db)).length;
  rebuilt.handle.client.close();
  expect(existsSync(indexPath)).toBe(true);
  expect(count).toBe(4);
});

test("rebuildIndex refuses :memory: (there is no file to kill)", async () => {
  const root = workspaceFixture();
  await expect(rebuildIndex(":memory:", root)).rejects.toThrow(/:memory:/);
});
