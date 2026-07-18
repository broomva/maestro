/// <reference types="bun" />
// briefing.test.ts — the tick briefing assembler (BRO-1772 slice 2). The done.check's "briefing
// snapshot test": over a seeded index, assembleBriefing produces the 7 curated sections (ORCHESTRATOR
// §2), live-only, attention-ordered, bounded. Driven over a real `:memory:` index.

import { describe, expect, test } from "bun:test";
import { EVENT_TYPES } from "@maestro/protocol";
import { type IndexHandle, openIndex } from "../db/client";
import { event, node, runBudget, schedule, session } from "../db/schema";
import { assembleBriefing, BRIEFING_SECTION_CAP } from "./briefing";

const NOW = 1_000_000;

async function seedNode(
  h: IndexHandle,
  o: {
    id: string;
    state: string;
    updatedAt: number;
    title?: string;
    deletedAt?: number;
    doneJson?: string | null;
    budgetJson?: string | null;
  },
) {
  await h.db.insert(node).values({
    id: o.id,
    path: o.id,
    parentId: null,
    kind: "task",
    state: o.state as never,
    owner: null,
    gate: "human",
    budgetJson: o.budgetJson ?? null,
    doneJson: o.doneJson ?? null,
    title: o.title ?? o.id,
    createdAt: 1,
    updatedAt: o.updatedAt,
    deletedAt: o.deletedAt ?? null,
  });
}

async function seed(h: IndexHandle) {
  // §2.2 attention (blocked|review) — review ranks before blocked; within review, OLDER sits higher.
  await seedNode(h, { id: "rev1", state: "review", updatedAt: 990_000, title: "Ship" }); // age 10k
  await seedNode(h, { id: "rev2", state: "review", updatedAt: 970_000 }); // age 30k (older → first)
  await seedNode(h, { id: "blk", state: "blocked", updatedAt: 980_000 }); // age 20k
  // §2.4 queue (proposed|triggered) — triggered ranks before proposed.
  await seedNode(h, { id: "trig", state: "triggered", updatedAt: 995_000 });
  await seedNode(h, { id: "prop", state: "proposed", updatedAt: 985_000 });
  // running (drives active-runs) + rows that must NOT surface.
  await seedNode(h, { id: "run", state: "running", updatedAt: 999_000 });
  await seedNode(h, { id: "done1", state: "done", updatedAt: 100 }); // excluded (not attention/queue)
  await seedNode(h, { id: "ghost", state: "review", updatedAt: 999_000, deletedAt: 9999 }); // tombstoned

  // §2.3 active runs — a running session + its budget (iterations/spend). `updatedAt` == `startedAt`
  // for a running session (status-transition-only), so staleness must come from the LAST EVENT below.
  await h.db.insert(session).values({
    id: "s-run",
    nodeId: "run",
    branch: "run/s-run",
    status: "running" as never,
    startedAt: 900_000,
    endedAt: null,
    diffstatJson: null,
    updatedAt: 900_000, // == startedAt (real running sessions never bump this)
    deletedAt: null,
  });
  await h.db
    .insert(runBudget)
    .values({ sessionId: "s-run", spentUsd: 1.25, iterations: 3, lastCallAt: 999_900 });

  // §2.5 bench — one enabled + a due-null enabled (must sort LAST) + one disabled (excluded).
  await h.db.insert(schedule).values([
    {
      id: "sc-soon",
      nodeId: "run",
      triggerKind: "heartbeat" as never,
      spec: "1000",
      nextFireAt: 5000,
      enabled: true,
      updatedAt: 1,
      deletedAt: null,
    },
    {
      id: "sc-null",
      nodeId: "run",
      triggerKind: "cron" as never,
      spec: "x",
      nextFireAt: null,
      enabled: true,
      updatedAt: 1,
      deletedAt: null,
    },
    {
      id: "sc-off",
      nodeId: "run",
      triggerKind: "cron" as never,
      spec: "x",
      nextFireAt: 6000,
      enabled: false,
      updatedAt: 1,
      deletedAt: null,
    },
  ]);

  // §2.3 last-EVENT staleness — a recent event for s-run (ts 999900 → lastEventAgeMs 100). Its max ts
  // beats the older budget.metered below, so staleness reflects real activity, not session age.
  await h.db.insert(event).values({
    sessionId: "s-run",
    ts: 999_900,
    actor: "agent" as never,
    type: EVENT_TYPES.TOOL_CALL,
    payload: JSON.stringify({ name: "shell" }),
  });
  // §2.6 ledger — a metered budget event (day spend) in today's window (dayStart=0 for NOW<1 day).
  await h.db.insert(event).values({
    sessionId: "s-run",
    ts: 500,
    actor: "system" as never,
    type: EVENT_TYPES.BUDGET_METERED,
    payload: JSON.stringify({ session: "s-run", usd: 2.5 }),
  });
  // §2.7 last wake log — a prior tick.fired the briefing surfaces (synthetic, no session).
  await h.db.insert(event).values({
    sessionId: null,
    ts: 900_000,
    actor: "system" as never,
    type: EVENT_TYPES.TICK_FIRED,
    payload: JSON.stringify({ tickId: "t1", cause: "interval", wokeAt: 900_000 }),
  });
}

describe("assembleBriefing", () => {
  test("assembles the 7 curated sections, live-only, attention-ordered, bounded", async () => {
    const h = await openIndex(":memory:");
    try {
      await seed(h);
      const b = await assembleBriefing(h.db, "worker_return", NOW);

      // §2.1 cause
      expect(b.cause).toBe("worker_return");

      // §2.2 attention — review before blocked; within review, OLDER (rev2, age 30k) before rev1 (10k).
      expect(b.attention.map((n) => n.nodeId)).toEqual(["rev2", "rev1", "blk"]);
      expect(b.attention.map((n) => n.ageMs)).toEqual([30_000, 10_000, 20_000]);

      // §2.4 queue — triggered before proposed.
      expect(b.queue.map((n) => n.nodeId)).toEqual(["trig", "prop"]);

      // §2.3 active runs — budget-joined; staleness from the LAST EVENT (100), not session age (100k).
      expect(b.activeRuns).toEqual([
        {
          sessionId: "s-run",
          nodeId: "run",
          branch: "run/s-run",
          iterations: 3,
          spentUsd: 1.25,
          lastEventAgeMs: 100,
        },
      ]);

      // §2.5 bench — enabled only, soonest first, NULL next-fire LAST.
      expect(b.bench.map((s) => s.scheduleId)).toEqual(["sc-soon", "sc-null"]);

      // §2.6 ledger — day spend + concurrency vs cap; dayBudgetUsd null (unconfigured).
      expect(b.ledger).toEqual({
        daySpentUsd: 2.5,
        dayBudgetUsd: null,
        activeRuns: 1,
        concurrencyCap: 3,
      });

      // §2.7 last wake log — the prior tick.
      expect(b.lastWakeLog).toEqual({ tickId: "t1", cause: "interval", wokeAt: 900_000 });
    } finally {
      h.client.close();
    }
  });

  test("a running session with NO events yet → staleness falls back to time-since-start", async () => {
    const h = await openIndex(":memory:");
    try {
      await h.db.insert(session).values({
        id: "fresh",
        nodeId: "n",
        branch: "run/fresh",
        status: "running" as never,
        startedAt: 999_000,
        endedAt: null,
        diffstatJson: null,
        updatedAt: 999_000,
        deletedAt: null,
      });
      const b = await assembleBriefing(h.db, "interval", NOW);
      expect(b.activeRuns[0]?.lastEventAgeMs).toBe(1000); // now − startedAt, no events
    } finally {
      h.client.close();
    }
  });

  test("dayBudgetUsd flows from options (the §3.1 denominator)", async () => {
    const h = await openIndex(":memory:");
    try {
      const b = await assembleBriefing(h.db, "interval", NOW, { dayBudgetUsd: 5 });
      expect(b.ledger.dayBudgetUsd).toBe(5);
    } finally {
      h.client.close();
    }
  });

  test("bounded — a list section is capped at BRIEFING_SECTION_CAP, keeping the most-attention items", async () => {
    const h = await openIndex(":memory:");
    try {
      // CAP+1 review nodes with distinct ages → age r_i = (i+1)*1000; oldest = highest attention.
      for (let i = 0; i <= BRIEFING_SECTION_CAP; i++) {
        await seedNode(h, { id: `r${i}`, state: "review", updatedAt: NOW - (i + 1) * 1000 });
      }
      const b = await assembleBriefing(h.db, "interval", NOW);
      expect(b.attention).toHaveLength(BRIEFING_SECTION_CAP); // capped
      expect(b.attention[0]?.nodeId).toBe(`r${BRIEFING_SECTION_CAP}`); // oldest survives at the head
      expect(b.attention.some((n) => n.nodeId === "r0")).toBe(false); // youngest dropped by the cap
    } finally {
      h.client.close();
    }
  });

  test("empty index → all sections empty, ledger zeroed, no wake log", async () => {
    const h = await openIndex(":memory:");
    try {
      const b = await assembleBriefing(h.db, "interval", NOW);
      expect(b.attention).toEqual([]);
      expect(b.queue).toEqual([]);
      expect(b.activeRuns).toEqual([]);
      expect(b.bench).toEqual([]);
      expect(b.ledger).toEqual({
        daySpentUsd: 0,
        dayBudgetUsd: null,
        activeRuns: 0,
        concurrencyCap: 3,
      });
      expect(b.lastWakeLog).toBeNull();
    } finally {
      h.client.close();
    }
  });

  test("§2.4 queue nodes carry §3.3 runnability derived from the frontmatter contract", async () => {
    const h = await openIndex(":memory:");
    try {
      // runnable: a done.check + a budget block.
      await seedNode(h, {
        id: "ok",
        state: "proposed",
        updatedAt: 900_000,
        doneJson: JSON.stringify({ check: "bun test" }),
        budgetJson: JSON.stringify({ per_run_usd: 0.5 }),
      });
      // not runnable: has a check but NO budget block.
      await seedNode(h, {
        id: "nobudget",
        state: "triggered",
        updatedAt: 950_000,
        doneJson: JSON.stringify({ check: "bun test" }),
        budgetJson: null,
      });
      // not runnable: no contract at all.
      await seedNode(h, { id: "bare", state: "proposed", updatedAt: 800_000 });

      const b = await assembleBriefing(h.db, "interval", NOW);
      const byId = new Map(b.queue.map((n) => [n.nodeId, n]));
      expect(byId.get("ok")).toMatchObject({ runnable: true, notRunnableReason: null });
      expect(byId.get("nobudget")).toMatchObject({
        runnable: false,
        notRunnableReason: "no budget block",
      });
      expect(byId.get("bare")).toMatchObject({
        runnable: false,
        notRunnableReason: "no done.check or judge rubric",
      });
    } finally {
      h.client.close();
    }
  });
});
