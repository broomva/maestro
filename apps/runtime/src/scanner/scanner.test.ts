/// <reference types="bun" />
// scanner.test.ts — the seam test for BRO-1800 (`bun test apps/runtime -t scanner`).
//
// Two halves, matching the done.check:
//  1. PURE (scanWorkspace): a fixture workspace derives the expected node set —
//     parent-defaults resolved (F1 step 2), the tree keyed on nesting, titles from
//     the brief, skip-dirs pruned, malformed files surfaced as errors not crashes,
//     and the whole derivation deterministic (same workspace → identical set).
//  2. STATEFUL (syncNodes): reconciling a scan into the index is idempotent (a
//     re-scan of an unchanged workspace writes nothing — no updatedAt churn),
//     tombstones vanished nodes, resurrects returning ones, and bumps updatedAt
//     only on real content change.

import { afterAll, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { openIndex } from "../db/client";
import { node } from "../db/schema";
import { createdToEpochMs, firstHeading, scanIntoIndex, scanWorkspace, syncNodes } from "./index";

// ── fixtures ──────────────────────────────────────────────────────────────────
const roots: string[] = [];
afterAll(() => {
  for (const r of roots) rmSync(r, { recursive: true, force: true });
});

/** Build a temp workspace from a { relativePath: fileContent } map. */
function makeFixture(files: Record<string, string>): string {
  const root = mkdtempSync(join(tmpdir(), "maestro-scan-"));
  roots.push(root);
  for (const [rel, content] of Object.entries(files)) {
    const abs = join(root, rel);
    mkdirSync(dirname(abs), { recursive: true });
    writeFileSync(abs, content);
  }
  return root;
}

/** Render a `_work.md` from fields (extraYaml carries raw blocks like budget/done). */
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

const byId = <T extends { id: string }>(rows: T[]) => new Map(rows.map((r) => [r.id, r]));

// ── PURE: scanWorkspace ─────────────────────────────────────────────────────
describe("scanWorkspace — derivation", () => {
  test("derives the expected node set with the tree keyed on folder nesting", async () => {
    const root = makeFixture({
      "_work.md": wm({ id: "root", kind: "initiative", brief: "# Growth" }),
      "seo/_work.md": wm({ id: "seo", kind: "project", brief: "# SEO refresh" }),
      "seo/fix-meta/_work.md": wm({ id: "fix", kind: "task", brief: "# Fix meta tags" }),
      // a plain folder with no _work.md between seo and deep-task is skipped in the tree
      "seo/notes/deep/_work.md": wm({ id: "deep", kind: "task", brief: "# Deep" }),
    });
    const { nodes, errors } = await scanWorkspace(root);
    expect(errors).toEqual([]);
    const m = byId(nodes);
    expect(m.get("root")?.path).toBe("");
    expect(m.get("root")?.parentId).toBeNull();
    expect(m.get("seo")?.path).toBe("seo");
    expect(m.get("seo")?.parentId).toBe("root");
    expect(m.get("fix")?.path).toBe("seo/fix-meta");
    expect(m.get("fix")?.parentId).toBe("seo");
    // "seo/notes/" has no _work.md, so deep's nearest work ancestor is seo, not notes.
    expect(m.get("deep")?.path).toBe("seo/notes/deep");
    expect(m.get("deep")?.parentId).toBe("seo");
    expect(m.get("root")?.title).toBe("Growth");
    expect(m.get("fix")?.title).toBe("Fix meta tags");
  });

  test("is deterministic — same workspace yields an identical node set", async () => {
    const root = makeFixture({
      "a/_work.md": wm({ id: "a", brief: "# A" }),
      "b/_work.md": wm({ id: "b", brief: "# B" }),
      "a/c/_work.md": wm({ id: "c", brief: "# C" }),
    });
    const first = await scanWorkspace(root);
    const second = await scanWorkspace(root);
    expect(second).toEqual(first);
    expect(first.nodes.map((n) => n.path)).toEqual(["a", "a/c", "b"]); // sorted by path
  });

  test("resolves parent-defaults transitively (F1 step 2), children override", async () => {
    const root = makeFixture({
      "_work.md": wm({
        id: "root",
        owner: "@alex",
        gate: "human",
        extraYaml: "budget:\n  per_run_usd: 5",
        brief: "# Root",
      }),
      // inherits owner + budget from root; sets its own state
      "child/_work.md": wm({ id: "child", state: "running", brief: "# Child" }),
      // overrides owner; still inherits budget from root through the chain
      "child/grand/_work.md": wm({ id: "grand", owner: "@bea", brief: "# Grand" }),
    });
    const { nodes } = await scanWorkspace(root);
    const m = byId(nodes);
    expect(m.get("child")?.owner).toBe("@alex"); // inherited
    expect(m.get("child")?.budgetJson).toBe(JSON.stringify({ per_run_usd: 5 }));
    expect(m.get("grand")?.owner).toBe("@bea"); // overridden
    expect(m.get("grand")?.budgetJson).toBe(JSON.stringify({ per_run_usd: 5 })); // inherited through chain
  });

  test("skips .git / node_modules / .maestro / dist", async () => {
    const root = makeFixture({
      "real/_work.md": wm({ id: "real", brief: "# Real" }),
      ".git/_work.md": wm({ id: "git", brief: "# nope" }),
      "node_modules/pkg/_work.md": wm({ id: "nm", brief: "# nope" }),
      ".maestro/_work.md": wm({ id: "idx", brief: "# nope" }),
    });
    const { nodes } = await scanWorkspace(root);
    expect(nodes.map((n) => n.id)).toEqual(["real"]);
  });

  test("records a malformed _work.md as an error and keeps scanning", async () => {
    const root = makeFixture({
      "good/_work.md": wm({ id: "good", brief: "# Good" }),
      "bad/_work.md": "---\nkind: task\n---\n# missing id, state, dates\n",
    });
    const { nodes, errors } = await scanWorkspace(root);
    expect(nodes.map((n) => n.id)).toEqual(["good"]);
    expect(errors.length).toBe(1);
    expect(errors[0]?.path).toBe("bad");
    expect(errors[0]?.code).toBe("missing_field");
  });

  test("records a duplicate frontmatter id, keeping the first by path order", async () => {
    const root = makeFixture({
      "a/_work.md": wm({ id: "dup", brief: "# A" }),
      "b/_work.md": wm({ id: "dup", brief: "# B" }),
    });
    const { nodes, errors } = await scanWorkspace(root);
    expect(nodes.length).toBe(1);
    expect(nodes[0]?.path).toBe("a");
    expect(errors[0]?.code).toBe("duplicate_id");
    expect(errors[0]?.path).toBe("b");
  });

  test("firstHeading + createdToEpochMs helpers", () => {
    expect(firstHeading("# Title here\n\nbody")).toBe("Title here");
    expect(firstHeading("no heading at all")).toBeNull();
    // date-only strings parse as UTC midnight — deterministic across machines
    expect(createdToEpochMs("2026-06-25")).toBe(Date.UTC(2026, 5, 25));
    expect(() => createdToEpochMs("not-a-date")).toThrow();
  });
});

// ── STATEFUL: syncNodes ──────────────────────────────────────────────────────
describe("syncNodes — reconciliation", () => {
  const fresh = () => openIndex(":memory:");

  test("first sync inserts; an identical re-sync writes nothing (no updatedAt churn)", async () => {
    const root = makeFixture({
      "_work.md": wm({ id: "root", brief: "# Root" }),
      "x/_work.md": wm({ id: "x", brief: "# X" }),
    });
    const { db, client } = await fresh();
    const scan = await scanWorkspace(root);

    const s1 = await syncNodes(db, scan.nodes, 1000);
    expect(s1).toEqual({ inserted: 2, updated: 0, tombstoned: 0, unchanged: 0 });
    const after1 = await db.select().from(node);
    expect(after1.every((r) => r.updatedAt === 1000)).toBe(true);

    // Re-scan the unchanged workspace at a LATER clock — nothing should be rewritten.
    const s2 = await syncNodes(db, (await scanWorkspace(root)).nodes, 2000);
    expect(s2).toEqual({ inserted: 0, updated: 0, tombstoned: 0, unchanged: 2 });
    const after2 = await db.select().from(node);
    expect(after2.every((r) => r.updatedAt === 1000)).toBe(true); // clock NOT churned
    client.close();
  });

  test("tombstones a vanished node, then resurrects it when it returns", async () => {
    const withBoth = makeFixture({
      "a/_work.md": wm({ id: "a", brief: "# A" }),
      "b/_work.md": wm({ id: "b", brief: "# B" }),
    });
    const { db, client } = await fresh();
    await syncNodes(db, (await scanWorkspace(withBoth)).nodes, 1000);

    // A workspace with only `a` — `b` vanished.
    const onlyA = makeFixture({ "a/_work.md": wm({ id: "a", brief: "# A" }) });
    const s = await syncNodes(db, (await scanWorkspace(onlyA)).nodes, 2000);
    expect(s.tombstoned).toBe(1);
    const bRow = byId(await db.select().from(node)).get("b");
    expect(bRow?.deletedAt).toBe(2000);
    expect(await liveCount(db)).toBe(1);

    // `b` returns — resurrected (deletedAt back to null), counted as an update.
    const s2 = await syncNodes(db, (await scanWorkspace(withBoth)).nodes, 3000);
    expect(s2.updated).toBe(1);
    const bRow2 = byId(await db.select().from(node)).get("b");
    expect(bRow2?.deletedAt).toBeNull();
    expect(bRow2?.updatedAt).toBe(3000);
    client.close();
  });

  test("a content change bumps updatedAt; unrelated rows are untouched", async () => {
    const v1 = makeFixture({
      "p/_work.md": wm({ id: "p", state: "proposed", brief: "# P" }),
      "q/_work.md": wm({ id: "q", brief: "# Q" }),
    });
    const { db, client } = await fresh();
    await syncNodes(db, (await scanWorkspace(v1)).nodes, 1000);

    const v2 = makeFixture({
      "p/_work.md": wm({ id: "p", state: "running", brief: "# P" }), // state changed
      "q/_work.md": wm({ id: "q", brief: "# Q" }), // unchanged
    });
    const s = await syncNodes(db, (await scanWorkspace(v2)).nodes, 2000);
    expect(s).toEqual({ inserted: 0, updated: 1, tombstoned: 0, unchanged: 1 });
    const m = byId(await db.select().from(node));
    expect(m.get("p")?.state).toBe("running");
    expect(m.get("p")?.updatedAt).toBe(2000);
    expect(m.get("q")?.updatedAt).toBe(1000); // untouched
    client.close();
  });

  test("a node that moved to a new path frees the old path for a different node", async () => {
    // id `a` at path old/, id `b` at path new/. Next scan: `a` moves to path b-ish?
    // Simpler collision: `a` leaves path `p`, `b` arrives at path `p` in one scan.
    const v1 = makeFixture({ "p/_work.md": wm({ id: "a", brief: "# A" }) });
    const { db, client } = await fresh();
    await syncNodes(db, (await scanWorkspace(v1)).nodes, 1000);

    // `a` now lives at path `moved`, and a NEW node `b` takes path `p`.
    const v2 = makeFixture({
      "moved/_work.md": wm({ id: "a", brief: "# A" }),
      "p/_work.md": wm({ id: "b", brief: "# B" }),
    });
    const s = await syncNodes(db, (await scanWorkspace(v2)).nodes, 2000);
    expect(s.inserted).toBe(1); // b
    expect(s.updated).toBe(1); // a moved
    const m = byId(await db.select().from(node));
    expect(m.get("a")?.path).toBe("moved");
    expect(m.get("b")?.path).toBe("p");
    client.close();
  });

  test("scanIntoIndex reconciles end-to-end and returns errors", async () => {
    const root = makeFixture({
      "ok/_work.md": wm({ id: "ok", brief: "# OK" }),
      "bad/_work.md": "---\nkind: task\n---\n",
    });
    const { db, client } = await fresh();
    const { summary, errors } = await scanIntoIndex(db, root, 1000);
    expect(summary.inserted).toBe(1);
    expect(errors.length).toBe(1);
    expect((await db.select().from(node)).length).toBe(1);
    client.close();
  });
});

// ── helpers ───────────────────────────────────────────────────────────────────
async function liveCount(db: Awaited<ReturnType<typeof openIndex>>["db"]): Promise<number> {
  return (await db.select().from(node)).filter((r) => r.deletedAt === null).length;
}
