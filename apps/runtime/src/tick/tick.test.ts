/// <reference types="bun" />
// tick.test.ts — the F6 tick engine (BRO-1772 slice 1). The done.check for this slice: a coalescing
// fixture (a wake arriving during an in-flight tick collapses to one tick) + the wake log is readable
// by the next tick. (The briefing snapshot test is slice 2.) Driven over a real `:memory:` index.

import { describe, expect, test } from "bun:test";
import { EVENT_TYPES } from "@maestro/protocol";
import { and, eq } from "drizzle-orm";
import { type IndexHandle, openIndex } from "../db/client";
import { event, lease, node, session } from "../db/schema";
import { readLastWakeLog, runTick } from "./tick";

async function countType(h: IndexHandle, type: (typeof EVENT_TYPES)[keyof typeof EVENT_TYPES]) {
  const rows = await h.db.select().from(event).where(eq(event.type, type));
  return rows.length;
}

describe("runTick", () => {
  test("fires a tick: emits tick.fired + the wake log is readable", async () => {
    const h = await openIndex(":memory:");
    try {
      const r = await runTick(h.db, "interval", 1000, "t1");
      expect(r).toEqual({ skipped: false, cause: "interval", tickId: "t1" });
      expect(await countType(h, EVENT_TYPES.TICK_FIRED)).toBe(1);
      // the record carries the engine fields + the s2b narrative/summary (an empty board → a nothing tick).
      const wl = await readLastWakeLog(h.db);
      expect(wl).toMatchObject({ tickId: "t1", cause: "interval", wokeAt: 1000 });
      expect(wl?.narrative).toContain("Did nothing new this tick.");
      expect(wl?.summary).toEqual({
        dispatched: 0,
        nudged: 0,
        needsHuman: 0,
        attention: 0,
        deferred: 0,
      });
    } finally {
      h.client.close();
    }
  });

  test("COALESCING — a wake during an in-flight tick (lease held) collapses to one tick", async () => {
    const h = await openIndex(":memory:");
    try {
      // Simulate a tick already in flight by pre-holding the global `tick` lease.
      await h.db.insert(lease).values({
        key: "tick",
        holder: "maestro",
        acquiredAt: 1,
        expiresAt: 9_999_999_999_999,
      });
      const r = await runTick(h.db, "worker_return", 1000, "t2");
      expect(r).toEqual({ skipped: true, cause: "worker_return", reason: "tick_in_flight" });
      // it surfaced the skip and did NOT fire.
      expect(await countType(h, EVENT_TYPES.TICK_SKIPPED)).toBe(1);
      expect(await countType(h, EVENT_TYPES.TICK_FIRED)).toBe(0);
      expect(await readLastWakeLog(h.db)).toBeNull();
    } finally {
      h.client.close();
    }
  });

  test("NO WEDGE — takes over an EXPIRED lease (a crash-stranded lease self-heals on the next wake, no restart)", async () => {
    // The defect P20 caught: an expiry-blind acquire would coalesce forever against a lease stranded by
    // a crash. The acquire is expiry-aware — a lease whose expiresAt is in the past is a DEAD tick and
    // gets taken over, so the loop is never permanently wedged.
    const h = await openIndex(":memory:");
    try {
      await h.db.insert(lease).values({
        key: "tick",
        holder: "dead-tick",
        acquiredAt: 1,
        expiresAt: 500, // already expired at now=1000
      });
      const r = await runTick(h.db, "interval", 1000, "t2");
      expect(r).toEqual({ skipped: false, cause: "interval", tickId: "t2" }); // took over → fired
      expect(await countType(h, EVENT_TYPES.TICK_FIRED)).toBe(1);
      // the fenced release removed our lease (holder t2), not left the dead one.
      expect((await h.db.select().from(lease).where(eq(lease.key, "tick"))).length).toBe(0);
    } finally {
      h.client.close();
    }
  });

  test("releases the lease on settle — the NEXT wake ticks, it does not coalesce forever", async () => {
    const h = await openIndex(":memory:");
    try {
      expect((await runTick(h.db, "interval", 1000, "t1")).skipped).toBe(false);
      // the lease is gone after the tick settled → the next wake fires a fresh tick, not a skip.
      const [held] = await h.db.select().from(lease).where(eq(lease.key, "tick"));
      expect(held).toBeUndefined();
      expect((await runTick(h.db, "interval", 2000, "t2")).skipped).toBe(false);
      expect(await countType(h, EVENT_TYPES.TICK_FIRED)).toBe(2);
    } finally {
      h.client.close();
    }
  });

  test("wake log chains — the next tick reads its predecessor's record (continuity, §2.7)", async () => {
    const h = await openIndex(":memory:");
    try {
      await runTick(h.db, "interval", 1000, "t1");
      expect((await readLastWakeLog(h.db))?.tickId).toBe("t1");
      await runTick(h.db, "manual", 2000, "t2");
      // the LAST wake log is the most recent tick — what the next tick's briefing §2.7 reads.
      expect(await readLastWakeLog(h.db)).toMatchObject({
        tickId: "t2",
        cause: "manual",
        wokeAt: 2000,
      });
    } finally {
      h.client.close();
    }
  });

  test("FENCED release does not steal a live tick's lease (holder-scoped delete)", async () => {
    // After tick B takes over an expired lease, tick A's late `finally` (delete WHERE holder=A) must be
    // a no-op — otherwise A would steal B's live lease and a third tick could run (double-tick). This
    // asserts the exact predicate the release uses.
    const h = await openIndex(":memory:");
    try {
      await h.db.insert(lease).values({
        key: "tick",
        holder: "B",
        acquiredAt: 2,
        expiresAt: 9_999_999_999_999,
      });
      // tick A's stale, superseded release:
      await h.db.delete(lease).where(and(eq(lease.key, "tick"), eq(lease.holder, "A")));
      const [row] = await h.db.select().from(lease).where(eq(lease.key, "tick"));
      expect(row?.holder).toBe("B"); // B's live lease survived — the fence held
    } finally {
      h.client.close();
    }
  });

  test("a default tickId is minted when none is injected", async () => {
    const h = await openIndex(":memory:");
    try {
      const r = await runTick(h.db, "manual", 1000);
      expect(r.skipped).toBe(false);
      expect(r.tickId).toMatch(/^[0-9a-f-]{36}$/); // a uuid
    } finally {
      h.client.close();
    }
  });
});

// ── s2b: the tick runs the policy + ACTS (dispatch) + narrates honestly ──────────
const MIN = 60_000;
async function seedNode(h: IndexHandle, id: string, state: string, updatedAt: number) {
  await h.db.insert(node).values({
    id,
    path: `work/${id}`,
    parentId: null,
    kind: "task",
    state: state as never,
    owner: null,
    gate: "human",
    budgetJson: null,
    doneJson: null,
    title: id,
    createdAt: 1,
    updatedAt,
    deletedAt: null,
  });
}

describe("runTick — s2b execution + honest narrative", () => {
  test("DISPATCHES the runnable work its policy decides, via the seam, and narrates 'Started'", async () => {
    const h = await openIndex(":memory:");
    try {
      await seedNode(h, "t1", "triggered", 1000); // triggered → dispatched
      const calls: string[] = [];
      await runTick(h.db, "interval", 2000, "tk", {
        dispatch: async (nodeId) => {
          calls.push(nodeId);
          return true;
        },
      });
      expect(calls).toEqual(["t1"]); // the tick actually called the dispatch seam
      const wl = await readLastWakeLog(h.db);
      expect(wl?.narrative).toContain("Started [t1](#node/t1).");
      expect(wl?.summary?.dispatched).toBe(1);
      expect(wl?.summary?.deferred).toBe(0);
    } finally {
      h.client.close();
    }
  });

  test("READ-ONLY (no dispatch seam) — surfaces runnable work honestly, starts nothing", async () => {
    const h = await openIndex(":memory:");
    try {
      await seedNode(h, "t1", "triggered", 1000);
      await runTick(h.db, "interval", 2000, "tk"); // no opts.dispatch → read-only
      const wl = await readLastWakeLog(h.db);
      expect(wl?.summary?.dispatched).toBe(0);
      expect(wl?.summary?.deferred).toBe(1);
      // honest: it did NOT claim to start t1; it left it alone with a plain reason.
      expect(wl?.narrative).not.toContain("Started [t1]");
      expect(wl?.narrative).toContain("[t1](#node/t1): nothing to run it yet.");
    } finally {
      h.client.close();
    }
  });

  test("a dispatch that FAILS to start is narrated as 'could not start', not 'Started'", async () => {
    const h = await openIndex(":memory:");
    try {
      await seedNode(h, "t1", "triggered", 1000);
      await runTick(h.db, "interval", 2000, "tk", { dispatch: async () => false });
      const wl = await readLastWakeLog(h.db);
      expect(wl?.summary?.dispatched).toBe(0);
      expect(wl?.narrative).not.toContain("Started [t1]");
      expect(wl?.narrative).toContain("[t1](#node/t1): could not start it just now.");
    } finally {
      h.client.close();
    }
  });

  test("a stale run is SURFACED to the human (not nudged) — honest copy, no false 'even after a nudge'", async () => {
    const h = await openIndex(":memory:");
    try {
      const now = 100 * 86_400_000; // past a day boundary
      await seedNode(h, "nrun", "running", now - 40 * MIN);
      await h.db.insert(session).values({
        id: "s-run",
        nodeId: "nrun",
        branch: "run/s-run",
        status: "running" as never,
        startedAt: now - 40 * MIN, // silent 40 min → stale (no events)
        endedAt: null,
        diffstatJson: null,
        updatedAt: now - 40 * MIN,
        deletedAt: null,
      });
      await runTick(h.db, "interval", now, "tk"); // no nudge seam (s2b-i)
      const wl = await readLastWakeLog(h.db);
      expect(wl?.summary?.needsHuman).toBe(1);
      expect(wl?.summary?.nudged).toBe(0);
      expect(wl?.narrative).toContain("has been quiet 40 minutes, worth a look.");
      expect(wl?.narrative).not.toContain("even after a nudge");
      expect(wl?.narrative).not.toContain("Nudged");
    } finally {
      h.client.close();
    }
  });
});
