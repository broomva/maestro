/// <reference types="bun" />
// scheduler.test.ts — the F7 scheduler (BRO-1749). The done.check: a kill-mid-fire restart fixture
// fires exactly once; contention (repeated polls) never doubles. Driven over a real index
// (`:memory:` for the unit paths, a file db for the restart path) — no ambient clock.

import { describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { EVENT_TYPES } from "@maestro/protocol";
import { eq } from "drizzle-orm";
import { type IndexHandle, openIndex } from "../db/client";
import { event, schedule } from "../db/schema";
import { computeNextFireAt, fireDueSchedules } from "./scheduler";

type Trigger = "heartbeat" | "cron" | "hook" | "goal";

interface SchedSeed {
  id: string;
  nodeId?: string;
  triggerKind?: Trigger;
  spec?: string;
  nextFireAt?: number | null;
  enabled?: boolean;
  deletedAt?: number | null;
}

async function seedSchedule(h: IndexHandle, o: SchedSeed): Promise<void> {
  await h.db.insert(schedule).values({
    id: o.id,
    nodeId: o.nodeId ?? "n0",
    triggerKind: o.triggerKind ?? "heartbeat",
    spec: o.spec ?? "1000",
    nextFireAt: o.nextFireAt === undefined ? 100 : o.nextFireAt,
    enabled: o.enabled ?? true,
    updatedAt: 1,
    deletedAt: o.deletedAt ?? null,
  });
}

async function firedCount(h: IndexHandle): Promise<number> {
  const rows = await h.db.select().from(event).where(eq(event.type, EVENT_TYPES.SCHEDULE_FIRED));
  return rows.length;
}

async function schedRow(h: IndexHandle, id: string) {
  const [row] = await h.db.select().from(schedule).where(eq(schedule.id, id));
  return row;
}

describe("computeNextFireAt", () => {
  test("heartbeat advances one interval when on time (now == dueAt)", () => {
    expect(computeNextFireAt("heartbeat", "1000", 5000, 5000)).toBe(6000);
  });
  test("heartbeat SKIPS missed fires — a long-overdue routine re-arms in the FUTURE (one fire, not a backlog storm)", () => {
    // dueAt=100, interval=1000, now=5500 → grid 100,1100,…,5100,6100 → first > now is 6100.
    expect(computeNextFireAt("heartbeat", "1000", 100, 5500)).toBe(6100);
    // massively overdue (epoch 1ms, 60s interval) → ONE future instant, not a catch-up loop (BRO-1749 dogfood).
    expect(computeNextFireAt("heartbeat", "60000", 1, 1_000_000)).toBe(1_020_001);
  });
  test("a malformed / non-positive heartbeat spec stops (null), never a tight loop", () => {
    expect(computeNextFireAt("heartbeat", "nope", 5000, 5000)).toBeNull();
    expect(computeNextFireAt("heartbeat", "0", 5000, 5000)).toBeNull();
    expect(computeNextFireAt("heartbeat", "-5", 5000, 5000)).toBeNull();
    // strict integer parse: `1e3` / `500.5` / `100abc` are NOT positive-int intervals → stop.
    expect(computeNextFireAt("heartbeat", "1e3", 5000, 5000)).toBeNull();
    expect(computeNextFireAt("heartbeat", "500.5", 5000, 5000)).toBeNull();
    expect(computeNextFireAt("heartbeat", "100abc", 5000, 5000)).toBeNull();
  });
  test("cron / hook / goal are one-shot here (null) — the taxonomy is BRO-1761", () => {
    expect(computeNextFireAt("cron", "0 9 * * *", 5000, 5000)).toBeNull();
    expect(computeNextFireAt("hook", "sel", 5000, 5000)).toBeNull();
    expect(computeNextFireAt("goal", "cond", 5000, 5000)).toBeNull();
  });
});

describe("fireDueSchedules", () => {
  test("fires a due heartbeat: emits schedule.fired + advances next_fire_at by the interval", async () => {
    const h = await openIndex(":memory:");
    try {
      await seedSchedule(h, { id: "s1", spec: "1000", nextFireAt: 100 });
      const fired = await fireDueSchedules(h.db, 500);
      expect(fired.map((f) => f.scheduleId)).toEqual(["s1"]);
      expect(fired[0]?.firedAt).toBe(100); // the DUE instant, not the poll time
      expect(await firedCount(h)).toBe(1);
      expect((await schedRow(h, "s1"))?.nextFireAt).toBe(1100); // 100 + 1000
    } finally {
      h.client.close();
    }
  });

  test("does not fire a not-yet-due schedule", async () => {
    const h = await openIndex(":memory:");
    try {
      await seedSchedule(h, { id: "s1", nextFireAt: 10_000 });
      expect(await fireDueSchedules(h.db, 500)).toEqual([]);
      expect(await firedCount(h)).toBe(0);
    } finally {
      h.client.close();
    }
  });

  test("does not fire a disabled or tombstoned schedule", async () => {
    const h = await openIndex(":memory:");
    try {
      await seedSchedule(h, { id: "off", nextFireAt: 100, enabled: false });
      await seedSchedule(h, { id: "gone", nextFireAt: 100, deletedAt: 9999 });
      expect(await fireDueSchedules(h.db, 500)).toEqual([]);
      expect(await firedCount(h)).toBe(0);
    } finally {
      h.client.close();
    }
  });

  test("a one-shot (non-heartbeat) fires once then stops (next_fire_at null)", async () => {
    const h = await openIndex(":memory:");
    try {
      await seedSchedule(h, {
        id: "cron1",
        triggerKind: "cron",
        spec: "0 9 * * *",
        nextFireAt: 100,
      });
      expect((await fireDueSchedules(h.db, 500)).length).toBe(1);
      expect((await schedRow(h, "cron1"))?.nextFireAt).toBeNull();
      expect(await fireDueSchedules(h.db, 500)).toEqual([]); // nothing due on a second pass
      expect(await firedCount(h)).toBe(1);
    } finally {
      h.client.close();
    }
  });

  test("a goal trigger self-disables after its single fire", async () => {
    const h = await openIndex(":memory:");
    try {
      await seedSchedule(h, { id: "g1", triggerKind: "goal", spec: "cond", nextFireAt: 100 });
      expect((await fireDueSchedules(h.db, 500)).length).toBe(1);
      const row = await schedRow(h, "g1");
      expect(row?.enabled).toBe(false); // self-disabled
      expect(row?.nextFireAt).toBeNull();
    } finally {
      h.client.close();
    }
  });

  test("NO WEDGE — a heartbeat keeps firing on each successive due instant (re-arms forward)", async () => {
    // The defect P20 caught in the lease design: a schedule must never permanently stop firing.
    // With the CAS-advance as the sole claim, `next_fire_at` always moves on, so the next due
    // instant fires again — no state can wedge it.
    const h = await openIndex(":memory:");
    try {
      await seedSchedule(h, { id: "hb", spec: "1000", nextFireAt: 100 });
      expect((await fireDueSchedules(h.db, 150)).map((f) => f.scheduleId)).toEqual(["hb"]);
      expect((await schedRow(h, "hb"))?.nextFireAt).toBe(1100); // re-armed forward
      expect((await fireDueSchedules(h.db, 1150)).map((f) => f.scheduleId)).toEqual(["hb"]); // fires AGAIN
      expect((await schedRow(h, "hb"))?.nextFireAt).toBe(2100);
      expect(await firedCount(h)).toBe(2); // two distinct fires — never disabled
    } finally {
      h.client.close();
    }
  });

  test("NO DOUBLE — repeated polls at the same `now` fire a due schedule exactly once (the CAS claim)", async () => {
    const h = await openIndex(":memory:");
    try {
      await seedSchedule(h, { id: "s1", spec: "1000", nextFireAt: 100 });
      await fireDueSchedules(h.db, 500);
      await fireDueSchedules(h.db, 500);
      await fireDueSchedules(h.db, 500);
      expect(await firedCount(h)).toBe(1); // only the first CAS-advance winner emitted
    } finally {
      h.client.close();
    }
  });

  test("CONCURRENCY — two racing polls on the same due row fire it exactly once (the CAS single-winner claim)", async () => {
    // The race harness for the `claimed.length === 0` guard: both polls SELECT the due row, then race
    // the CAS-advance; SQLite's write-lock lets exactly one win (the other's `.returning()` is empty).
    // Removing the guard makes this DOUBLE (a real regression this test locks out).
    const h = await openIndex(":memory:");
    try {
      await seedSchedule(h, { id: "s1", spec: "1000", nextFireAt: 100 });
      const [a, b] = await Promise.all([fireDueSchedules(h.db, 500), fireDueSchedules(h.db, 500)]);
      expect(a.length + b.length).toBe(1); // exactly one poll claimed the fire
      expect(await firedCount(h)).toBe(1);
    } finally {
      h.client.close();
    }
  });

  test("EXACTLY-ONCE across a restart: fire, kill the runtime, reopen the same index, no second fire", async () => {
    // The CAS-advanced next_fire_at is index-persisted; a restart over the same file no longer sees
    // the instant due. (This test now depends solely on the CAS — there is no lease — so neutering
    // the advance makes it re-fire and FAIL.)
    const dir = mkdtempSync(join(tmpdir(), "maestro-sched-"));
    const path = join(dir, "index.db");
    try {
      const h1 = await openIndex(path);
      await seedSchedule(h1, { id: "s1", spec: "1000", nextFireAt: 100 });
      expect((await fireDueSchedules(h1.db, 500)).length).toBe(1); // fired once
      expect(await firedCount(h1)).toBe(1);
      h1.client.close(); // "kill" the runtime

      const h2 = await openIndex(path);
      expect(await fireDueSchedules(h2.db, 500)).toEqual([]); // NO second fire
      expect(await firedCount(h2)).toBe(1); // still exactly one schedule.fired event total
      h2.client.close();
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  test("fires multiple due schedules in one pass, oldest-first", async () => {
    const h = await openIndex(":memory:");
    try {
      await seedSchedule(h, { id: "late", nextFireAt: 300 });
      await seedSchedule(h, { id: "early", nextFireAt: 100 });
      const fired = await fireDueSchedules(h.db, 500);
      expect(fired.map((f) => f.scheduleId)).toEqual(["early", "late"]); // asc next_fire_at
      expect(await firedCount(h)).toBe(2);
    } finally {
      h.client.close();
    }
  });
});
