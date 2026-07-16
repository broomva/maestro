/// <reference types="bun" />
// ledger.test.ts — the BRO-1818 autonomy-ledger suite (done.check `bun test apps/runtime --filter ledger`).
//
// Two halves:
//   1. The pure derivation (`deriveLedger` + formatters + `defaultLedgerWindow`) — the KPI math at every
//      edge the ticket named: overlapping (parallel) runs, a human look mid-window, runtime-down gaps,
//      a run started before the window, a still-active run, run.killed's dual terminal+look role, looks
//      outside the window, and the UTC day boundary.
//   2. The `/api/ledger` endpoint over a real `:memory:` index — proving it SERVES the derivation and that
//      the KPI is DERIVED, never a stored percentage (there is no `ledger` table; the label carries no `%`).

import { describe, expect, test } from "bun:test";
import type { EventType, LedgerResponse } from "@maestro/protocol";
import { createApp } from "../app";
import { DEFAULT_PORT, type RuntimeConfig } from "../config";
import { type IndexHandle, openIndex } from "../db/client";
import * as schema from "../db/schema";
import { event } from "../db/schema";
import {
  defaultLedgerWindow,
  deriveLedger,
  formatLedgerLabel,
  formatUnsupervised,
  isHumanLook,
  LEDGER_DAY_MS,
  type LedgerEvent,
  type LedgerWindow,
} from "./ledger";

const MIN = 60_000;
const H = 3_600_000;

/** A run's lifecycle events: `run.started` + (unless still-active) a terminal. A kill is actor "system"
 *  (the supervisor emits it) but a human look; everything else is actor "agent". */
function run(
  sessionId: string,
  startTs: number,
  endTs: number | null,
  endType: "run.finished" | "run.failed" | "run.killed" | "run.orphaned" = "run.finished",
): LedgerEvent[] {
  const evs: LedgerEvent[] = [{ sessionId, actor: "agent", ts: startTs, type: "run.started" }];
  if (endTs !== null) {
    const actor = endType === "run.killed" ? "system" : "agent";
    evs.push({ sessionId, actor, ts: endTs, type: endType });
  }
  return evs;
}

/** A gate decision/escalation — a human look (actor "user"). */
function look(ts: number, type: "gate.decided" | "gate.escalated" = "gate.decided"): LedgerEvent {
  return { sessionId: "s-gate", actor: "user", ts, type };
}

// A 2-hour window [0, 2H) is the default fixture window for the interval-math cases.
const W: LedgerWindow = { since: 0, until: 2 * H };

describe("ledger derivation — unsupervised hours (union, not sum)", () => {
  test("a single run in the window contributes its full duration", () => {
    const l = deriveLedger(run("s1", 0, H), W);
    expect(l.unsupervisedMs).toBe(H);
    expect(l.activeRuns).toBe(0);
    expect(l.humanLooks).toBe(0);
  });

  test("overlapping (parallel) runs UNION — one unsupervised hour, not the sum", () => {
    // A [0, 1H] and B [30m, 90m] overlap → union [0, 90m] = 1.5H. Summing would wrongly give 2H.
    const l = deriveLedger([...run("a", 0, H), ...run("b", 30 * MIN, 90 * MIN)], W);
    expect(l.unsupervisedMs).toBe(90 * MIN);
  });

  test("fully-nested parallel runs count once (the wider interval)", () => {
    // B [20m, 40m] sits inside A [0, 1H] → union is just A = 1H.
    const l = deriveLedger([...run("a", 0, H), ...run("b", 20 * MIN, 40 * MIN)], W);
    expect(l.unsupervisedMs).toBe(H);
  });

  test("disjoint runs (a runtime-down gap between them) do NOT count the gap", () => {
    // A [0, 30m] then B [1H, 90m]; the [30m, 1H] gap is idle (no run active) → 30m + 30m = 1H.
    const l = deriveLedger([...run("a", 0, 30 * MIN), ...run("b", H, 90 * MIN)], W);
    expect(l.unsupervisedMs).toBe(H);
  });

  test("adjacent intervals (end == next start) merge without double-counting the seam", () => {
    const l = deriveLedger([...run("a", 0, H), ...run("b", H, 2 * H)], W);
    expect(l.unsupervisedMs).toBe(2 * H);
  });
});

describe("ledger derivation — window clamping", () => {
  test("a run started BEFORE the window is clamped to the window start", () => {
    // window [1H, 2H); run [30m, 90m] → clamped [1H, 90m] = 30m.
    const l = deriveLedger(run("s1", 30 * MIN, 90 * MIN), { since: H, until: 2 * H });
    expect(l.unsupervisedMs).toBe(30 * MIN);
  });

  test("a still-active run (no terminal) clamps to the window end and counts as active", () => {
    const l = deriveLedger(run("s1", 0, null), W);
    expect(l.unsupervisedMs).toBe(2 * H);
    expect(l.activeRuns).toBe(1);
  });

  test("a run whose terminal lands AFTER the window end reads as active-at-until", () => {
    // run [0, 3H], window [0, 2H) → clamped [0, 2H] = 2H; terminal >= until ⇒ activeRuns 1.
    const l = deriveLedger(run("s1", 0, 3 * H), W);
    expect(l.unsupervisedMs).toBe(2 * H);
    expect(l.activeRuns).toBe(1);
  });

  test("a run entirely before the window contributes nothing and is not active", () => {
    const l = deriveLedger(run("s1", -2 * H, -H), W);
    expect(l.unsupervisedMs).toBe(0);
    expect(l.activeRuns).toBe(0);
  });

  test("earliest run.started wins on a respawn (multiple run.started, one terminal)", () => {
    // A fresh_context respawn re-emits run.started; the run is live from the FIRST start to the terminal.
    const evs: LedgerEvent[] = [
      { sessionId: "s1", actor: "agent", ts: 10 * MIN, type: "run.started" },
      { sessionId: "s1", actor: "agent", ts: 20 * MIN, type: "run.started" },
      { sessionId: "s1", actor: "agent", ts: 50 * MIN, type: "run.finished" },
    ];
    expect(deriveLedger(evs, W).unsupervisedMs).toBe(40 * MIN);
  });
});

describe("ledger derivation — human looks (a notch per look)", () => {
  test("a gate decision mid-window is one look and does NOT reduce unsupervised hours", () => {
    // The two KPIs are orthogonal: a look is a notch, not a pause. The run keeps running through it.
    const l = deriveLedger([...run("s1", 0, 2 * H), look(H)], W);
    expect(l.humanLooks).toBe(1);
    expect(l.unsupervisedMs).toBe(2 * H);
  });

  test("gate.escalated counts as a look (actor user)", () => {
    expect(deriveLedger([look(20 * MIN, "gate.escalated")], W).humanLooks).toBe(1);
  });

  test("run.killed is BOTH a terminal (ends the interval) and a look (a human kill)", () => {
    // run.started@0, run.killed@40m → 40m unsupervised (interval ends at the kill) + 1 look.
    const l = deriveLedger(run("s1", 0, 40 * MIN, "run.killed"), W);
    expect(l.unsupervisedMs).toBe(40 * MIN);
    expect(l.humanLooks).toBe(1);
    expect(l.activeRuns).toBe(0);
  });

  test("looks OUTSIDE the half-open window are excluded; an in-window look is counted", () => {
    const l = deriveLedger(
      [
        look(-10 * MIN), // before since → out
        look(2 * H), // == until → out (half-open [since, until))
        look(H), // in
      ],
      W,
    );
    expect(l.humanLooks).toBe(1);
  });

  test("a synthetic (null-sessionId) user look is a notch but creates no run interval", () => {
    const evs: LedgerEvent[] = [{ sessionId: null, actor: "user", ts: H, type: "gate.decided" }];
    const l = deriveLedger(evs, W);
    expect(l.humanLooks).toBe(1);
    expect(l.unsupervisedMs).toBe(0);
  });

  test("agent/tool/system non-kill events are never looks", () => {
    expect(isHumanLook({ actor: "agent", type: "run.beat" })).toBe(false);
    expect(isHumanLook({ actor: "tool", type: "tool.call" })).toBe(false);
    expect(isHumanLook({ actor: "system", type: "run.finished" })).toBe(false);
    expect(isHumanLook({ actor: "user", type: "gate.decided" })).toBe(true);
    expect(isHumanLook({ actor: "system", type: "run.killed" })).toBe(true);
  });

  test("empty history is a clean zero ledger", () => {
    expect(deriveLedger([], W)).toEqual({
      since: 0,
      until: 2 * H,
      unsupervisedMs: 0,
      humanLooks: 0,
      activeRuns: 0,
      segments: [],
      notches: [],
    });
  });

  test("an inverted/empty window derives to zero (no crash)", () => {
    const l = deriveLedger([...run("s1", 0, H), look(30 * MIN)], { since: 2 * H, until: 0 });
    expect(l.unsupervisedMs).toBe(0);
    expect(l.humanLooks).toBe(0);
  });
});

describe("ledger scoreboard geometry — positional %, not a progress %", () => {
  test("a single run projects to one segment at the right window position + width", () => {
    // run [30m, 90m] over [0, 2H) → start 30m/120m = 25%, width 60m/120m = 50%.
    const l = deriveLedger(run("s1", 30 * MIN, 90 * MIN), W);
    expect(l.segments).toEqual([{ start: 25, width: 50 }]);
    expect(l.notches).toEqual([]);
  });

  test("overlapping runs project to ONE merged segment (union geometry)", () => {
    // A [0, 1H] + B [30m, 90m] → merged [0, 90m] → start 0%, width 75%.
    const l = deriveLedger([...run("a", 0, H), ...run("b", 30 * MIN, 90 * MIN)], W);
    expect(l.segments).toEqual([{ start: 0, width: 75 }]);
  });

  test("a still-active run yields a `live` segment reaching the window end", () => {
    const l = deriveLedger(run("s1", H, null), W); // [1H, 2H) still running
    expect(l.segments).toEqual([{ start: 50, width: 50, live: true }]);
  });

  test("a finished run does NOT mark its segment live", () => {
    const l = deriveLedger(run("s1", 0, H), W);
    expect(l.segments[0]?.live).toBeUndefined();
  });

  test("looks project to notch positions in percent of the window", () => {
    const l = deriveLedger([look(30 * MIN), look(H)], W); // 30m → 25%, 1H → 50%
    expect(l.notches).toEqual([25, 50]);
  });

  test("a degenerate (empty) window yields no geometry but valid zero aggregates", () => {
    const l = deriveLedger([...run("s1", 0, H), look(30 * MIN)], { since: H, until: H });
    expect(l.segments).toEqual([]);
    expect(l.notches).toEqual([]);
    expect(l.unsupervisedMs).toBe(0);
  });
});

describe("ledger formatters + default window (plain voice, no %)", () => {
  test("formatUnsupervised: whole hours + minutes, sub-minute floors to 0m, never a %", () => {
    expect(formatUnsupervised(0)).toBe("0m");
    expect(formatUnsupervised(59_999)).toBe("0m");
    expect(formatUnsupervised(MIN)).toBe("1m");
    expect(formatUnsupervised(H)).toBe("1h 0m");
    expect(formatUnsupervised(2 * H + 14 * MIN)).toBe("2h 14m");
    expect(formatUnsupervised(-5)).toBe("0m"); // negative guarded
  });

  test("formatLedgerLabel reads as a receipt; looks pluralize; carries no percent sign", () => {
    expect(formatLedgerLabel({ unsupervisedMs: 2 * H + 14 * MIN, humanLooks: 3 })).toBe(
      "2h 14m unsupervised · 3 looks",
    );
    expect(formatLedgerLabel({ unsupervisedMs: H, humanLooks: 1 })).toBe(
      "1h 0m unsupervised · 1 look",
    );
    expect(formatLedgerLabel({ unsupervisedMs: 0, humanLooks: 0 })).toBe(
      "0m unsupervised · 0 looks",
    );
    expect(formatLedgerLabel({ unsupervisedMs: 5 * H, humanLooks: 9 })).not.toContain("%");
  });

  test("defaultLedgerWindow: UTC day-start → now, aligned to the day boundary", () => {
    const now = 3 * LEDGER_DAY_MS + 5 * H; // 3 whole UTC days + 5h
    const win = defaultLedgerWindow(now);
    expect(win.since).toBe(3 * LEDGER_DAY_MS);
    expect(win.until).toBe(now);
    expect(win.since % LEDGER_DAY_MS).toBe(0); // day-aligned
    expect(win.until - win.since).toBe(5 * H); // "5h so far today"
    expect(win.until - win.since).toBeLessThan(LEDGER_DAY_MS);
  });
});

// ── /api/ledger — the endpoint SERVES the derivation; the KPI is derived, not stored ──

const cfg = (workspace: string): RuntimeConfig => ({
  port: DEFAULT_PORT,
  workspace,
  indexPath: ":memory:",
  lockPath: `${workspace}/.maestro/runtime.lock`,
});

async function mkApp(workspace = "/tmp/ws-ledger") {
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(workspace), Date.now(), handle.db);
  return { ...handle, app };
}

/** Insert raw event rows (the ledger reads the append-only `event` log directly). A no-op on an empty
 *  list — drizzle rejects `.values([])`, and an empty log is a legitimate ledger fixture. */
async function seed(h: IndexHandle, rows: LedgerEvent[]): Promise<void> {
  if (rows.length === 0) return;
  await h.db.insert(event).values(
    rows.map((r) => ({
      sessionId: r.sessionId,
      ts: r.ts,
      actor: r.actor,
      type: r.type as EventType,
      payload: null,
    })),
  );
}

describe("GET /api/ledger — serves the derivation over a seeded index", () => {
  test("one finished run + one gate decision → derived hours, looks, and plain-voice label", async () => {
    const h = await mkApp();
    const base = 10 * LEDGER_DAY_MS; // a clean UTC day start
    await seed(h, [
      ...run("s1", base, base + H), // 1h of autonomous work
      look(base + 30 * MIN), // one human look inside the window
    ]);
    const res = await h.app.request(`/api/ledger?since=${base}&until=${base + 2 * H}`);
    expect(res.status).toBe(200);
    const body = (await res.json()) as LedgerResponse;
    expect(body.unsupervisedMs).toBe(H);
    expect(body.humanLooks).toBe(1);
    expect(body.activeRuns).toBe(0);
    expect(body.since).toBe(base);
    expect(body.until).toBe(base + 2 * H);
    expect(body.label).toBe("1h 0m unsupervised · 1 look");
    // The endpoint serves the bar geometry: the 1h run is [0%, 50%); the look sits at 25%.
    expect(body.segments).toEqual([{ start: 0, width: 50 }]);
    expect(body.notches).toEqual([25]);
    h.client.close();
  });

  test("a run that started BEFORE the window and is still active is measured from the window start", async () => {
    const h = await mkApp();
    const base = 20 * LEDGER_DAY_MS;
    // run.started 30m before the window, no terminal → active; window [base, base+2H).
    await seed(h, run("s-live", base - 30 * MIN, null));
    const res = await h.app.request(`/api/ledger?since=${base}&until=${base + 2 * H}`);
    const body = (await res.json()) as LedgerResponse;
    expect(body.unsupervisedMs).toBe(2 * H); // clamped to [base, base+2H]
    expect(body.activeRuns).toBe(1);
    h.client.close();
  });

  test("the KPI is DERIVED, not stored: no `ledger` table, no percentage field, no `%` in the label", async () => {
    const h = await mkApp();
    const base = 30 * LEDGER_DAY_MS;
    await seed(h, run("s1", base, base + H));
    const res = await h.app.request(`/api/ledger?since=${base}&until=${base + 2 * H}`);
    const body = (await res.json()) as LedgerResponse;
    // No stored KPI table (the canon forbids a stored %): schema exports node/session/event/gate/schedule.
    expect(Object.keys(schema)).not.toContain("ledger");
    // No percentage on the wire — receipts, not percentages.
    expect(Object.keys(body).some((k) => /percent|pct|ratio/i.test(k))).toBe(false);
    expect(String(body.label)).not.toContain("%");
    expect(res.headers.get("x-maestro-protocol")).toBeTruthy();
    h.client.close();
  });

  test("garbage / absent query params fall back to the default UTC-day window (200, not 400)", async () => {
    const h = await mkApp();
    await seed(h, []); // empty log
    const res = await h.app.request("/api/ledger?since=notanumber");
    expect(res.status).toBe(200);
    const body = (await res.json()) as LedgerResponse;
    // Default window: since is the current UTC day start (day-aligned), until ~ now.
    expect(body.since % LEDGER_DAY_MS).toBe(0);
    expect(body.until).toBeGreaterThanOrEqual(body.since);
    expect(body.unsupervisedMs).toBe(0);
    h.client.close();
  });

  test("without an index, /api/ledger 404s (degradation contract, like the other reads)", async () => {
    const app = createApp(cfg("/tmp/ws-ledger"), Date.now());
    expect((await app.request("/api/ledger")).status).toBe(404);
  });
});
