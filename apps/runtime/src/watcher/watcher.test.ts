/// <reference types="bun" />
// watcher.test.ts — the seam test for BRO-1804 (`bun test apps/runtime -t watcher`).
//
// Three halves:
//   1. PURE (isWatchedChange): wake on any change outside skip-dirs / `run/<id>` worktrees —
//      Bun's recursive fs.watch truncates filenames, so the idempotent reconcile (not the
//      filename) is what decides whether a node actually changed.
//   2. reconcileAndEmit: a scan reconciles into the index and appends exactly one
//      `node.updated` (carrying the live-node payload) per inserted/changed LIVE node —
//      an unchanged re-scan emits nothing, and a tombstoned node never emits (tombstones
//      do not cross the wire).
//   3. END-TO-END (startWatcher): a `_work.md` written on disk drives a reconcile + a
//      `node.updated` event within the debounce window.

import { afterAll, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { EVENT_TYPES } from "@maestro/protocol";
import { eq } from "drizzle-orm";
import { openIndex } from "../db/client";
import { event } from "../db/schema";
import { scanIntoIndex } from "../scanner";
import {
  createReconcileScheduler,
  isWatchedChange,
  type ReconcileResult,
  reconcileAndEmit,
  startWatcher,
} from "./index";

// ── fixtures ──────────────────────────────────────────────────────────────────
const roots: string[] = [];
afterAll(() => {
  for (const r of roots) rmSync(r, { recursive: true, force: true });
});

function makeFixture(files: Record<string, string>): string {
  const root = mkdtempSync(join(tmpdir(), "maestro-watch-"));
  roots.push(root);
  for (const [rel, content] of Object.entries(files)) {
    const abs = join(root, rel);
    mkdirSync(dirname(abs), { recursive: true });
    writeFileSync(abs, content);
  }
  return root;
}

/** Write/overwrite a `_work.md` under `root` at `rel`. */
function write(root: string, rel: string, content: string): void {
  const abs = join(root, rel);
  mkdirSync(dirname(abs), { recursive: true });
  writeFileSync(abs, content);
}

/** Minimal `_work.md` renderer (mirrors the scanner fixture helper). */
function wm(o: { id: string; state?: string; brief?: string }): string {
  return [
    "---",
    `id: ${o.id}`,
    "kind: task",
    `state: ${o.state ?? "proposed"}`,
    "created: 2026-06-25",
    "updated: 2026-06-25",
    "---",
    "",
    o.brief ?? `# ${o.id}`,
    "",
  ].join("\n");
}

const fresh = () => openIndex(":memory:");
const nodeUpdated = (db: Awaited<ReturnType<typeof fresh>>["db"]) =>
  db.select().from(event).where(eq(event.type, EVENT_TYPES.NODE_UPDATED));

const delay = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));
const NOOP_RESULT: ReconcileResult = {
  summary: { inserted: 0, updated: 0, tombstoned: 0, unchanged: 0, changedIds: [] },
  emitted: 0,
};

// ── 1. PURE — isWatchedChange ───────────────────────────────────────────────────
describe("isWatchedChange — the wake filter", () => {
  test("wakes on any change outside skip-dirs / worktrees (the reconcile decides what changed)", () => {
    expect(isWatchedChange("_work.md")).toBe(true);
    expect(isWatchedChange("x/_work.md")).toBe(true);
    expect(isWatchedChange("a/b/c/_work.md")).toBe(true);
    expect(isWatchedChange("x\\child")).toBe(true); // windows separator; Bun-truncated segment
    // Bun's recursive watch reports only the top segment, so we wake on it and let the
    // idempotent re-scan sort out whether a node actually changed — even a non-node file.
    expect(isWatchedChange("child")).toBe(true);
    expect(isWatchedChange("x/notes.md")).toBe(true);
  });

  test("never wakes on an empty path", () => {
    expect(isWatchedChange("")).toBe(false);
  });

  test("ignores skip-dirs (.git / node_modules / .maestro / dist) at any depth", () => {
    expect(isWatchedChange(".git")).toBe(false);
    expect(isWatchedChange("node_modules/pkg/_work.md")).toBe(false);
    expect(isWatchedChange(".maestro/index.db")).toBe(false);
    expect(isWatchedChange("dist")).toBe(false);
  });

  test("ignores a `run/<id>` worktree copy (its churn must not re-index)", () => {
    expect(isWatchedChange("run")).toBe(false); // Bun-truncated top segment
    expect(isWatchedChange("run/abc123/_work.md")).toBe(false);
    expect(isWatchedChange("x/run/abc123/y/_work.md")).toBe(false);
    // A top-level `run` is treated as worktree churn even for a bare `run/_work.md` — the
    // startup full scan still indexes a real `run` node; only live edits to it are skipped.
    expect(isWatchedChange("run/_work.md")).toBe(false);
  });

  test("ignores `runs/run-<id>/` session-receipt churn (PLURAL runs — the real high-freq source)", () => {
    // The P20 r3 catch: the receipt dir is `runs/run-<id>/` (DATA-MODEL §A.1), NOT `run/`.
    // It churns hard (session.jsonl per event, progress.md per iteration) and holds no _work.md.
    expect(isWatchedChange("runs")).toBe(false); // Bun-truncated top segment
    expect(isWatchedChange("runs/run-7f3a/session.jsonl")).toBe(false);
    expect(isWatchedChange("runs/run-7f3a/progress.md")).toBe(false);
    expect(isWatchedChange("runs/run-7f3a/checks/build.log")).toBe(false);
    // Nested under a work folder (full-path platforms) — still suppressed by the `runs` segment.
    expect(isWatchedChange("growth/seo/runs/run-7f3a/child.stderr.log")).toBe(false);
    // Sanity: the SINGULAR `run` worktree suppression did NOT accidentally start matching a
    // legit work folder whose name merely starts with "run".
    expect(isWatchedChange("running-tasks/_work.md")).toBe(true);
  });
});

// ── 2. reconcileAndEmit ─────────────────────────────────────────────────────────
describe("reconcileAndEmit — scan → index → node.updated", () => {
  test("emits one node.updated (a live-node payload) per inserted node", async () => {
    const root = makeFixture({ "_work.md": wm({ id: "root", brief: "# Root" }) });
    const { db, client } = await fresh();

    const r = await reconcileAndEmit(db, root, 1000);
    expect(r.summary.inserted).toBe(1);
    expect(r.emitted).toBe(1);

    const events = await nodeUpdated(db);
    expect(events).toHaveLength(1);
    const [e] = events;
    expect(e?.sessionId).toBeNull(); // synthetic — no session (D-DURABILITY)
    expect(e?.actor).toBe("system");
    const payload = JSON.parse(e?.payload ?? "null");
    expect(payload.id).toBe("root");
    expect("deletedAt" in payload).toBe(false); // tombstone column stripped (LiveNode shape)
    client.close();
  });

  test("an unchanged re-scan emits nothing (the change feed does not wake on a no-op)", async () => {
    const root = makeFixture({ "_work.md": wm({ id: "root" }) });
    const { db, client } = await fresh();
    await reconcileAndEmit(db, root, 1000);

    const r2 = await reconcileAndEmit(db, root, 2000);
    expect(r2.summary.unchanged).toBe(1);
    expect(r2.emitted).toBe(0);
    expect(await nodeUpdated(db)).toHaveLength(1); // no second event
    client.close();
  });

  test("a content change emits one node.updated", async () => {
    const root = makeFixture({ "_work.md": wm({ id: "root", state: "proposed" }) });
    const { db, client } = await fresh();
    await reconcileAndEmit(db, root, 1000);

    write(root, "_work.md", wm({ id: "root", state: "running" }));
    const r = await reconcileAndEmit(db, root, 2000);
    expect(r.summary.updated).toBe(1);
    expect(r.emitted).toBe(1);
    expect(await nodeUpdated(db)).toHaveLength(2);
    client.close();
  });

  test("a tombstoned (vanished) node emits nothing — tombstones never cross the wire", async () => {
    const root = makeFixture({
      "a/_work.md": wm({ id: "a" }),
      "b/_work.md": wm({ id: "b" }),
    });
    const { db, client } = await fresh();
    const first = await reconcileAndEmit(db, root, 1000);
    expect(first.emitted).toBe(2); // a + b inserted

    // `b` vanishes from disk.
    rmSync(join(root, "b"), { recursive: true, force: true });
    const r = await reconcileAndEmit(db, root, 2000);
    expect(r.summary.tombstoned).toBe(1);
    expect(r.emitted).toBe(0); // the vanished node is NOT projected
    expect(await nodeUpdated(db)).toHaveLength(2); // still just the two inserts
    client.close();
  });
});

// ── 3. createReconcileScheduler — the debounce + single-flight core (P20 major) ──
// Driven directly (no fs.watch), so both invariants are PROVEN deterministically rather
// than left to OS event timing. This is the coverage the P20 gate flagged as missing.
describe("createReconcileScheduler — debounce + single-flight", () => {
  test("debounce coalesces a burst of schedule() calls into ONE reconcile", async () => {
    let calls = 0;
    const sched = createReconcileScheduler({
      reconcile: async () => {
        calls += 1;
        return NOOP_RESULT;
      },
      debounceMs: 20,
    });
    // A burst inside the quiet window — must collapse to a single pass.
    sched.schedule();
    sched.schedule();
    sched.schedule();
    await delay(60);
    expect(calls).toBe(1);
  });

  test("single-flight: reconciles never overlap; a mid-pass wake triggers exactly one trailing run", async () => {
    let active = 0;
    let maxActive = 0;
    let calls = 0;
    const sched = createReconcileScheduler({
      reconcile: async () => {
        active += 1;
        maxActive = Math.max(maxActive, active);
        calls += 1;
        await delay(50); // a reconcile that OUTLASTS the 5ms debounce
        active -= 1;
        return NOOP_RESULT;
      },
      debounceMs: 5,
    });
    sched.schedule(); // fires ~t=5 → reconcile #1 runs t≈5..55
    await delay(20); // t=20: #1 in flight
    sched.schedule(); // fires ~t=25 while #1 runs → sets pending, does NOT overlap
    await delay(15); // t=35: still #1
    sched.schedule(); // fires ~t=40 → pending already set
    await delay(120); // #1 ends ~55 → ONE trailing run ~55..105 → drain
    expect(maxActive).toBe(1); // the invariant: never two reconciles at once
    expect(calls).toBe(2); // #1 + exactly one coalesced trailing run
  });

  test("a closed scheduler runs neither a new nor a trailing reconcile", async () => {
    let calls = 0;
    let closed = false;
    const sched = createReconcileScheduler({
      reconcile: async () => {
        calls += 1;
        await delay(40);
        return NOOP_RESULT;
      },
      debounceMs: 5,
      isClosed: () => closed,
    });
    sched.schedule(); // → run #1 at ~t=5, ends ~45
    await delay(20); // #1 in flight
    sched.schedule(); // → pending
    closed = true; // close mid-flight
    sched.cancel();
    await delay(80); // #1 finishes; the trailing run must NOT fire (closed + cancelled)
    sched.schedule(); // a post-close schedule() is a no-op
    await delay(20);
    expect(calls).toBe(1);
  });
});

// ── 4. END-TO-END — startWatcher ─────────────────────────────────────────────────
describe("startWatcher — a disk edit drives a reconcile", () => {
  test("a new `_work.md` reconciles + emits a node.updated within the debounce window", async () => {
    const root = makeFixture({ "_work.md": wm({ id: "root", brief: "# Root" }) });
    const { db, client } = await fresh();
    await scanIntoIndex(db, root, 1000); // seed the root node without emitting

    let resolveReconcile: (r: ReconcileResult) => void = () => {};
    const reconciled = new Promise<ReconcileResult>((res) => {
      resolveReconcile = res;
    });
    const handle = startWatcher(db, root, {
      debounceMs: 30,
      onReconcile: (r) => {
        if (r.emitted > 0) resolveReconcile(r);
      },
    });

    // A NEW node appears on disk — the watcher should pick it up.
    write(root, "child/_work.md", wm({ id: "child", brief: "# Child" }));

    let timeout: ReturnType<typeof setTimeout> | undefined;
    const result = await Promise.race([
      reconciled,
      new Promise<never>((_, rej) => {
        timeout = setTimeout(() => rej(new Error("watcher did not reconcile in time")), 4000);
      }),
    ]);
    if (timeout) clearTimeout(timeout);

    expect(result.emitted).toBeGreaterThanOrEqual(1);
    const payloads = (await nodeUpdated(db)).map((e) => JSON.parse(e.payload ?? "null"));
    expect(payloads.some((p) => p.id === "child")).toBe(true);
    expect(payloads.every((p) => !("deletedAt" in p))).toBe(true);

    handle.stop();
    handle.stop(); // idempotent
    client.close();
  });

  test("recursion is in effect — a node DEEP under a pre-existing dir wakes the watcher", async () => {
    // The regression guard the P20 review demanded: the test above writes `child/_work.md`, a
    // DIRECT child of root, which fires even under a non-recursive watch. Here the new node is
    // two levels down, inside a pre-existing `projects/` dir — a non-recursive watch of root
    // never sees events under `projects/`, so `recursive:true → false` would fail THIS test.
    const root = makeFixture({
      "_work.md": wm({ id: "root", brief: "# Root" }),
      "projects/_work.md": wm({ id: "projects", brief: "# Projects" }),
    });
    const { db, client } = await fresh();
    await scanIntoIndex(db, root, 1000); // seed root + projects without emitting

    let resolveReconcile: (r: ReconcileResult) => void = () => {};
    const reconciled = new Promise<ReconcileResult>((res) => {
      resolveReconcile = res;
    });
    const handle = startWatcher(db, root, {
      debounceMs: 30,
      onReconcile: (r) => {
        if (r.summary.changedIds.includes("deep")) resolveReconcile(r);
      },
    });

    // A NEW node two levels down, inside the pre-existing `projects/` subtree.
    write(root, "projects/deep/_work.md", wm({ id: "deep", brief: "# Deep" }));

    let timeout: ReturnType<typeof setTimeout> | undefined;
    const result = await Promise.race([
      reconciled,
      new Promise<never>((_, rej) => {
        timeout = setTimeout(
          () => rej(new Error("recursive watcher did not see the deep edit")),
          4000,
        );
      }),
    ]);
    if (timeout) clearTimeout(timeout);

    expect(result.summary.changedIds).toContain("deep");
    const payloads = (await nodeUpdated(db)).map((e) => JSON.parse(e.payload ?? "null"));
    expect(payloads.some((p) => p.id === "deep")).toBe(true);

    handle.stop();
    client.close();
  });
});
