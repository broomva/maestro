/// <reference types="bun" />
// orchestrator.test.ts — the mock-model tick (BRO-1784 slice 2). done.check: "mock-model tick executes
// policy checklist deterministically" (`bun test apps/runtime --filter orchestrator`). computeOrchestratorTick
// assembles the §2 briefing from a SEEDED live index and runs the §3 policy — no model — so the whole
// checklist (safety · surface · dispatch, with runnability computed from the real frontmatter contract) is
// exercised end-to-end and replays identically.

import { describe, expect, test } from "bun:test";
import { type IndexHandle, openIndex } from "../db/client";
import { node, session } from "../db/schema";
import { computeOrchestratorTick } from "./orchestrator";

const DAY = 86_400_000;
const NOW = 100 * DAY + 5_000_000; // comfortably past a day boundary; dayStart excludes nothing seeded
const MIN = 60_000;

async function seedNode(
  h: IndexHandle,
  o: {
    id: string;
    state: string;
    updatedAt?: number;
    doneJson?: string | null;
    budgetJson?: string | null;
    gate?: string;
  },
) {
  await h.db.insert(node).values({
    id: o.id,
    path: `work/${o.id}`,
    parentId: null,
    kind: "task",
    state: o.state as never,
    owner: null,
    gate: (o.gate ?? "human") as never,
    budgetJson: o.budgetJson ?? null,
    doneJson: o.doneJson ?? null,
    title: o.id,
    createdAt: 1,
    updatedAt: o.updatedAt ?? NOW - MIN,
    deletedAt: null,
  });
}

describe("computeOrchestratorTick (mock-model tick, deterministic)", () => {
  test("executes the §3 checklist: surface attention, nudge a stale run, dispatch runnable, defer the rest", async () => {
    const h = await openIndex(":memory:");
    try {
      // §3.2 attention — a node at the human gate.
      await seedNode(h, { id: "rev", state: "review", updatedAt: NOW - 2 * MIN });
      // §3.1 a running session silent 40 min (no events → staleness falls back to startedAt).
      await seedNode(h, { id: "nrun", state: "running" });
      await h.db.insert(session).values({
        id: "s-run",
        nodeId: "nrun",
        branch: "run/s-run",
        status: "running" as never,
        startedAt: NOW - 40 * MIN,
        endedAt: null,
        diffstatJson: null,
        updatedAt: NOW - 40 * MIN,
        deletedAt: null,
      });
      // §3.3 a triggered node (dispatches first) + a proposed node with NO contract (not runnable).
      await seedNode(h, { id: "t1", state: "triggered", updatedAt: NOW - 3 * MIN });
      await seedNode(h, { id: "p-bad", state: "proposed", updatedAt: NOW - 4 * MIN });

      const d = await computeOrchestratorTick(h.db, "interval", NOW, {
        briefing: { dayBudgetUsd: 5 },
      });

      expect(d.budgetHalt).toBeNull(); // day spend 0 < 90% of 5
      expect(d.attention.map((a) => a.nodeId)).toEqual(["rev"]);
      expect(d.nudges.map((n) => n.sessionId)).toEqual(["s-run"]);
      expect(d.needsHuman).toEqual([]);
      // cap 3 − 1 active run = 2 slots; t1 (triggered) dispatches; p-bad is proposed + not runnable.
      expect(d.dispatches.map((x) => x.nodeId)).toEqual(["t1"]);
      expect(d.deferrals).toEqual([{ nodeId: "p-bad", reason: "no done.check or judge rubric" }]);
      // §7 narrative touches each decision, in plain voice.
      expect(d.wakeLog).toContain("needs you");
      expect(d.wakeLog).toContain("Started [t1](#node/t1).");
      expect(d.wakeLog).toContain("Nudged [nrun](#node/nrun)");
      // the decision keeps the precise reason; the wake log softens it to coffee-voice (no system terms).
      expect(d.deferrals).toEqual([{ nodeId: "p-bad", reason: "no done.check or judge rubric" }]);
      expect(d.wakeLog).toContain("[p-bad](#node/p-bad): no way to check when it's done.");
      expect(d.wakeLog).not.toContain("done.check");
    } finally {
      h.client.close();
    }
  });

  test("runnability is computed from the REAL frontmatter contract — a runnable proposed node dispatches", async () => {
    const h = await openIndex(":memory:");
    try {
      // A complete contract (done.check + budget + human gate) → runnable → dispatched (no cap pressure).
      await seedNode(h, {
        id: "p-ok",
        state: "proposed",
        doneJson: JSON.stringify({ check: "bun test" }),
        budgetJson: JSON.stringify({ per_run_usd: 0.5 }),
        gate: "human",
      });
      // A judge-only contract WITHOUT a budget → not runnable ("no budget block").
      await seedNode(h, {
        id: "p-nobudget",
        state: "proposed",
        doneJson: JSON.stringify({ check: "make test", judge: "rubric.md" }),
        budgetJson: null,
        gate: "human",
      });

      const d = await computeOrchestratorTick(h.db, "manual", NOW);
      expect(d.dispatches.map((x) => x.nodeId)).toEqual(["p-ok"]);
      expect(d.deferrals).toEqual([{ nodeId: "p-nobudget", reason: "no budget block" }]);
    } finally {
      h.client.close();
    }
  });

  test("an empty workspace → a nothing tick, still a two-line wake log", async () => {
    const h = await openIndex(":memory:");
    try {
      const d = await computeOrchestratorTick(h.db, "interval", NOW);
      expect(d.dispatches).toEqual([]);
      expect(d.nudges).toEqual([]);
      expect(d.attention).toEqual([]);
      expect(d.wakeLog.split("\n").length).toBeGreaterThanOrEqual(2);
      expect(d.wakeLog).toContain("Did nothing new this tick.");
    } finally {
      h.client.close();
    }
  });
});
