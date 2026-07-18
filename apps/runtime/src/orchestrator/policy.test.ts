/// <reference types="bun" />
// policy.test.ts — the orchestrator decision policy (ORCHESTRATOR §3), BRO-1784 slice 2. Pure over a
// hand-built Briefing (no db, no model): each checklist rule fires deterministically, and the §7 wake log
// renders in plain voice. Part of the done.check `bun test apps/runtime --filter orchestrator`.

import { describe, expect, test } from "bun:test";
import type { Briefing, BriefingQueueNode, BriefingRun } from "../tick/briefing";
import {
  DEFERRAL_REASONS,
  decidePolicy,
  renderWakeLog,
  STALE_RUN_MS,
  softenReason,
} from "./policy";

function briefing(p: Partial<Briefing> = {}): Briefing {
  return {
    cause: "interval",
    attention: [],
    activeRuns: [],
    queue: [],
    bench: [],
    ledger: { daySpentUsd: 0, dayBudgetUsd: null, activeRuns: 0, concurrencyCap: 3 },
    lastWakeLog: null,
    ...p,
  };
}
function qnode(
  o: Partial<BriefingQueueNode> & { nodeId: string; state: string },
): BriefingQueueNode {
  return {
    path: `work/${o.nodeId}`,
    title: o.nodeId,
    ageMs: 1000,
    runnable: true,
    notRunnableReason: null,
    ...o,
  };
}
function run(o: Partial<BriefingRun> & { sessionId: string }): BriefingRun {
  return {
    nodeId: `n-${o.sessionId}`,
    branch: `run/${o.sessionId}`,
    iterations: 1,
    spentUsd: 0,
    lastEventAgeMs: 0,
    ...o,
  };
}

describe("§3.1 safety — day budget", () => {
  test("≥90% day budget spent → dispatch nothing; queue all deferred with the budget reason", () => {
    const d = decidePolicy(
      briefing({
        ledger: { daySpentUsd: 4.5, dayBudgetUsd: 5, activeRuns: 0, concurrencyCap: 3 },
        queue: [qnode({ nodeId: "t1", state: "triggered" })],
      }),
    );
    expect(d.budgetHalt).not.toBeNull();
    expect(d.budgetHalt?.reason).toContain("90%");
    expect(d.dispatches).toEqual([]);
    expect(d.deferrals).toEqual([{ nodeId: "t1", reason: "day budget hold" }]);
  });

  test("below the ratio → no halt, dispatch proceeds (positive control)", () => {
    const d = decidePolicy(
      briefing({
        ledger: { daySpentUsd: 4.0, dayBudgetUsd: 5, activeRuns: 0, concurrencyCap: 3 },
        queue: [qnode({ nodeId: "t1", state: "triggered" })],
      }),
    );
    expect(d.budgetHalt).toBeNull();
    expect(d.dispatches).toEqual([{ nodeId: "t1", state: "triggered" }]);
  });

  test("unconfigured day budget (null) never halts", () => {
    const d = decidePolicy(
      briefing({
        ledger: { daySpentUsd: 999, dayBudgetUsd: null, activeRuns: 0, concurrencyCap: 3 },
      }),
    );
    expect(d.budgetHalt).toBeNull();
  });
});

describe("§3.1 safety — stale runs", () => {
  const stale = STALE_RUN_MS + 60_000; // 31 min silent

  test("a run silent > 30 min with no prior nudge → nudge it", () => {
    const d = decidePolicy(
      briefing({ activeRuns: [run({ sessionId: "s1", lastEventAgeMs: stale })] }),
    );
    expect(d.nudges.map((n) => n.sessionId)).toEqual(["s1"]);
    expect(d.needsHuman).toEqual([]);
  });

  test("a run still silent AFTER a prior nudge → recommend the human look, not a re-nudge", () => {
    const d = decidePolicy(
      briefing({ activeRuns: [run({ sessionId: "s1", nodeId: "n1", lastEventAgeMs: stale })] }),
      { nudgedSessionIds: new Set(["s1"]) },
    );
    expect(d.needsHuman.map((n) => n.sessionId)).toEqual(["s1"]);
    expect(d.needsHuman[0]?.afterNudge).toBe(true);
    expect(d.nudges).toEqual([]);
    // the afterNudge:true branch renders the "even after a nudge" copy (the nudge did not revive it).
    expect(d.wakeLog).toContain("[n1](#node/n1) has been quiet");
    expect(d.wakeLog).toContain("even after a nudge, worth a look.");
  });

  test("a run within the staleness window → neither nudged nor escalated (positive control)", () => {
    const d = decidePolicy(
      briefing({ activeRuns: [run({ sessionId: "s1", lastEventAgeMs: 10 * 60_000 })] }),
    );
    expect(d.nudges).toEqual([]);
    expect(d.needsHuman).toEqual([]);
  });
});

describe("§3.2 surface — attention list", () => {
  test("attention nodes are surfaced verbatim, never dispatched or cleared", () => {
    const d = decidePolicy(
      briefing({
        attention: [
          { nodeId: "rev", path: "work/rev", title: "Ship", state: "review", ageMs: 20_000 },
          { nodeId: "blk", path: "work/blk", title: null, state: "blocked", ageMs: 40_000 },
        ],
      }),
    );
    expect(d.attention).toEqual([
      { nodeId: "rev", state: "review", ageMs: 20_000 },
      { nodeId: "blk", state: "blocked", ageMs: 40_000 },
    ]);
    // surfacing is not dispatching — the orchestrator never acts on the human's gates.
    expect(d.dispatches).toEqual([]);
  });
});

describe("§3.3 dispatch — cap + runnability", () => {
  test("triggered first, then proposed; stops at the concurrency cap", () => {
    const d = decidePolicy(
      briefing({
        ledger: { daySpentUsd: 0, dayBudgetUsd: null, activeRuns: 1, concurrencyCap: 3 },
        // assembleBriefing delivers the queue triggered-first; we mirror that order here.
        queue: [
          qnode({ nodeId: "t1", state: "triggered" }),
          qnode({ nodeId: "t2", state: "triggered" }),
          qnode({ nodeId: "p1", state: "proposed" }),
        ],
      }),
    );
    // slots = cap(3) − active(1) = 2 → t1, t2 dispatched; p1 deferred at cap.
    expect(d.dispatches.map((x) => x.nodeId)).toEqual(["t1", "t2"]);
    expect(d.deferrals).toEqual([{ nodeId: "p1", reason: "at concurrency cap" }]);
  });

  test("a non-runnable PROPOSED node is left queued with its reason; runnable ones dispatch", () => {
    const d = decidePolicy(
      briefing({
        queue: [
          qnode({ nodeId: "ok", state: "proposed" }),
          qnode({
            nodeId: "bad",
            state: "proposed",
            runnable: false,
            notRunnableReason: "no budget block",
          }),
        ],
      }),
    );
    expect(d.dispatches.map((x) => x.nodeId)).toEqual(["ok"]);
    expect(d.deferrals).toEqual([{ nodeId: "bad", reason: "no budget block" }]);
  });

  test("runnability gates PROPOSED only — a triggered node dispatches even when marked not runnable", () => {
    const d = decidePolicy(
      briefing({
        queue: [
          qnode({ nodeId: "t", state: "triggered", runnable: false, notRunnableReason: "x" }),
        ],
      }),
    );
    expect(d.dispatches.map((x) => x.nodeId)).toEqual(["t"]);
    expect(d.deferrals).toEqual([]);
  });

  test("at the cap already (0 slots) → nothing dispatched", () => {
    const d = decidePolicy(
      briefing({
        ledger: { daySpentUsd: 0, dayBudgetUsd: null, activeRuns: 3, concurrencyCap: 3 },
        queue: [qnode({ nodeId: "t1", state: "triggered" })],
      }),
    );
    expect(d.dispatches).toEqual([]);
    expect(d.deferrals).toEqual([{ nodeId: "t1", reason: "at concurrency cap" }]);
  });

  test("a non-runnable proposed node over cap reports its REAL reason, not 'at concurrency cap'", () => {
    // runnability is checked before the slot check so the actionable blocker wins (forward-honesty):
    // a broken contract will never dispatch, so "no budget block" beats the misleading "waiting for a slot".
    const d = decidePolicy(
      briefing({
        ledger: { daySpentUsd: 0, dayBudgetUsd: null, activeRuns: 3, concurrencyCap: 3 },
        queue: [
          qnode({
            nodeId: "bad",
            state: "proposed",
            runnable: false,
            notRunnableReason: "no budget block",
          }),
        ],
      }),
    );
    expect(d.dispatches).toEqual([]);
    expect(d.deferrals).toEqual([{ nodeId: "bad", reason: "no budget block" }]);
  });
});

describe("§7 wake log", () => {
  test("a nothing tick still writes two lines (silence reads as breakage)", () => {
    const d = decidePolicy(briefing({ cause: "interval" }));
    const lines = d.wakeLog.split("\n");
    expect(lines.length).toBeGreaterThanOrEqual(2);
    expect(lines[0]).toBe("Woke on a scheduled check.");
    expect(d.wakeLog).toContain("Did nothing new this tick.");
  });

  test("leads with what needs you, in plain voice (no enum names), then did, then left alone", () => {
    const d = decidePolicy(
      briefing({
        cause: "worker_return",
        attention: [
          { nodeId: "rev", path: "work/rev", title: null, state: "review", ageMs: 120_000 },
        ],
        queue: [
          qnode({ nodeId: "ok", state: "triggered" }),
          qnode({
            nodeId: "bad",
            state: "proposed",
            runnable: false,
            notRunnableReason: "no budget block",
          }),
        ],
      }),
    );
    const log = d.wakeLog;
    // plain voice: "review" surfaces as "needs you", not the enum; the deferral reason has no system terms.
    expect(log).toContain("needs you");
    expect(log).not.toContain("review");
    expect(log).not.toContain("done.check");
    expect(log).not.toContain("concurrency");
    // ordering: needs-you block precedes the did block precedes the left-alone block.
    expect(log.indexOf("Needs you:")).toBeGreaterThanOrEqual(0);
    expect(log.indexOf("Needs you:")).toBeLessThan(log.indexOf("Did:"));
    expect(log.indexOf("Did:")).toBeLessThan(log.indexOf("Left alone:"));
    expect(log).toContain("Started [ok](#node/ok).");
    // the technical reason is softened for the human ("no budget block" → "no spending limit set"),
    // while the decision object keeps the precise reason.
    expect(log).toContain("[bad](#node/bad): no spending limit set.");
    expect(d.deferrals).toEqual([{ nodeId: "bad", reason: "no budget block" }]);
  });

  test("renderWakeLog is pure over the decision (same decision → same log)", () => {
    const d = decidePolicy(
      briefing({ activeRuns: [run({ sessionId: "s1", lastEventAgeMs: STALE_RUN_MS + 1 })] }),
    );
    expect(renderWakeLog(d)).toBe(d.wakeLog);
  });

  test("every deferral reason the policy can emit is softened for the human (no verbatim leak)", () => {
    // Guards the PLAIN_REASON sync point (P20 NIT): a new reason without a mapping would reach the wake
    // log untranslated. Each enumerated reason must translate to something OTHER than itself.
    for (const reason of DEFERRAL_REASONS) {
      expect(softenReason(reason)).not.toBe(reason);
    }
  });
});
