/// <reference types="bun" />
// proxy.test.ts — the model proxy + budget-in-path guard (HARNESS §3, F3.1) for BRO-1788
// (`bun test apps/runtime --filter proxy`).
//
// The three done.check guarantees:
//   1. The child env has NO key — the runtime key attaches only at forward time inside the proxy.
//   2. Race: N children racing one budget never overspend — the DOLLAR caps (per_run + per_day) and
//      the iteration cap are all EXACT under concurrency, because preflight RESERVES against each.
//   3. A refusal answers 402 budget_exhausted, emits budget.refused, and parks the run blocked.

import { afterEach, beforeEach, expect, test } from "bun:test";
import { EVENT_TYPES } from "@maestro/protocol";
import { eq } from "drizzle-orm";
import { type IndexHandle, indexUrl, openIndex } from "../db/client";
import { runBudget } from "../db/schema";
import { buildChildEnv } from "../harness/spawn-contract";
import { BudgetGuard, deriveDayTotal, deriveSpentBySession, type MeteredRecord } from "./budget";
import { MemoryEventSink } from "./events";
import { DEFAULT_MODEL_PINS, resolvePinnedModel } from "./models";
import { createModelProxy, type ModelUpstream, PARK_STATE, serveProxy } from "./proxy";
import { type SessionContext, SessionTokenRegistry } from "./tokens";

let handle: IndexHandle;
beforeEach(async () => {
  handle = await openIndex(indexUrl(":memory:"));
});
afterEach(() => {
  handle.client.close();
});

const FIXED_NOW = () => 1_700_000_000_000;

function ctx(session: string, budget: SessionContext["budget"]): SessionContext {
  return { session, runDir: `/runs/${session}`, role: "agent", budget };
}

/** A guard with a pinned clock + an explicit reserve so the arithmetic in tests is exact. */
function guardWith(sink: MemoryEventSink, reserveUsd = 0.5, dayTotalUsd = 0): BudgetGuard {
  return new BudgetGuard(handle.db, sink, { now: FIXED_NOW, reserveUsd, dayTotalUsd });
}

// ── 1. tokens: mint / resolve / revoke ───────────────────────────────────────

test("proxy tokens: mint resolves, revoke invalidates, re-mint drops the old token", () => {
  let n = 0;
  const reg = new SessionTokenRegistry(() => `tok-${++n}`);
  const c = ctx("run-1", { per_run_usd: 5 });
  const t1 = reg.mint(c);
  expect(reg.resolve(t1)).toEqual(c);
  const t2 = reg.mint(c); // re-mint (fresh-context restart) revokes the prior token
  expect(reg.resolve(t1)).toBeNull();
  expect(reg.resolve(t2)).toEqual(c);
  reg.revoke("run-1"); // revoke on kill
  expect(reg.resolve(t2)).toBeNull();
  expect(reg.size).toBe(0);
});

// ── 2. model pinning ─────────────────────────────────────────────────────────

test("proxy pins a model per role, env override wins, blank override ignored", () => {
  expect(resolvePinnedModel("agent", {})).toBe(DEFAULT_MODEL_PINS.agent);
  expect(resolvePinnedModel("verifier", { MAESTRO_MODEL_VERIFIER: "claude-sonnet-5" })).toBe(
    "claude-sonnet-5",
  );
  expect(resolvePinnedModel("agent", { MAESTRO_MODEL_AGENT: "   " })).toBe(
    DEFAULT_MODEL_PINS.agent,
  );
});

// ── 3. budget guard: reserve / reconcile / release ───────────────────────────

test("budget preflight RESERVES cost + an iteration under caps", async () => {
  const guard = guardWith(new MemoryEventSink(), 0.5);
  await guard.open("run-1");
  const v = await guard.preflight(ctx("run-1", { per_run_usd: 5, max_iterations: 3 }));
  expect(v.ok).toBe(true);
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.iterations).toBe(1); // reserved
  expect(row?.spentUsd).toBeCloseTo(0.5, 6); // reservation held
});

test("budget meter reconciles a reservation to the actual cost, journals budget.metered", async () => {
  const sink = new MemoryEventSink();
  const guard = guardWith(sink, 0.5);
  await guard.open("run-1");
  const c = ctx("run-1", { per_run_usd: 5 });
  await guard.preflight(c); // reserves 0.5
  await guard.meter(c, { usd: 0.3, tokens: 1200 }); // reconcile 0.5 → 0.3
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.spentUsd).toBeCloseTo(0.3, 6);
  expect(guard.dayTotalUsd).toBeCloseTo(0.3, 6);
  const metered = sink.ofType(EVENT_TYPES.BUDGET_METERED);
  expect(metered.length).toBe(1);
  expect(metered[0]?.payload.usd).toBe(0.3);
});

test("budget release refunds the reservation but KEEPS the consumed iteration", async () => {
  const guard = guardWith(new MemoryEventSink(), 0.5);
  await guard.open("run-1");
  const c = ctx("run-1", { per_run_usd: 5, max_iterations: 10 });
  await guard.preflight(c); // spent 0.5, iter 1
  await guard.release(c); // failed/non-billable call
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.spentUsd).toBeCloseTo(0, 6); // refunded
  expect(row?.iterations).toBe(1); // attempt kept — a flaky upstream drains iterations, fail-closed
});

test("budget preflight REFUSES per_run and journals budget.refused (parks blocked)", async () => {
  const sink = new MemoryEventSink();
  const guard = guardWith(sink, 0.5);
  await guard.open("run-1");
  const c = ctx("run-1", { per_run_usd: 1, max_iterations: 100 });
  await guard.preflight(c); // spent 0.5
  await guard.meter(c, { usd: 0.5 }); // spent 0.5
  await guard.preflight(c); // spent 1.0
  await guard.meter(c, { usd: 0.5 }); // spent 1.0 (full)
  const v = await guard.preflight(c); // 1.0 + 0.5 > 1 → refuse
  expect(v).toEqual({ ok: false, reason: "per_run" });
  const refused = sink.ofType(EVENT_TYPES.BUDGET_REFUSED);
  expect(refused.length).toBe(1);
  expect(refused[0]?.payload).toEqual({ session: "run-1", reason: "per_run" });
  expect(PARK_STATE).toBe("blocked");
});

test("budget preflight REFUSES per_day from the in-memory accumulator", async () => {
  const sink = new MemoryEventSink();
  const guard = guardWith(sink, 0.5, 20); // dayTotal seeded at 20
  await guard.open("run-1");
  const v = await guard.preflight(ctx("run-1", { per_day_usd: 20, per_run_usd: 100 }));
  expect(v).toEqual({ ok: false, reason: "per_day" });
  expect(sink.ofType(EVENT_TYPES.BUDGET_REFUSED)[0]?.payload.reason).toBe("per_day");
});

test("budget preflight REFUSES at the iteration cap (fails closed with no row too)", async () => {
  const guard = guardWith(new MemoryEventSink(), 0.5);
  // never called open() → no row → fail closed as iteration_cap
  const v = await guard.preflight(ctx("ghost", { max_iterations: 5 }));
  expect(v).toEqual({ ok: false, reason: "iteration_cap" });
});

test("budget per_day ROLLS OVER at the day boundary (not a lifetime cap)", async () => {
  let clock = 1_700_000_000_000;
  const guard = new BudgetGuard(handle.db, new MemoryEventSink(), {
    now: () => clock,
    reserveUsd: 0.5,
    dayTotalUsd: 0,
  });
  await guard.open("run-1");
  const c = ctx("run-1", { per_day_usd: 0.5 });
  await guard.preflight(c);
  await guard.meter(c, { usd: 0.5 }); // day total now 0.5 (full)
  expect((await guard.preflight(c)).ok).toBe(false); // same day → refused
  clock += 86_400_000; // cross into the next UTC day
  expect((await guard.preflight(c)).ok).toBe(true); // rolled over → fresh day budget
});

// ── 4. the race: N children racing one budget, no overspend ───────────────────

test("RACE: the per_run DOLLAR cap is EXACT under concurrency (reservation, unbounded iters)", async () => {
  // The old bug: preflight checked but did not RESERVE dollars, so all N passed and overspent. With
  // reservation, only floor(cap/reserve) can hold the budget at once — even with iterations unbounded.
  const guard = guardWith(new MemoryEventSink(), 0.25);
  await guard.open("run-1");
  const c = ctx("run-1", { per_run_usd: 1 }); // no max_iterations on purpose
  const verdicts = await Promise.all(Array.from({ length: 40 }, () => guard.preflight(c)));
  expect(verdicts.filter((v) => v.ok).length).toBe(4); // 4 × 0.25 = 1.0, no more
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.spentUsd).toBeCloseTo(1.0, 6); // reserved spend never exceeds per_run
});

test("RACE: the per_day DOLLAR cap is EXACT under concurrency", async () => {
  const guard = guardWith(new MemoryEventSink(), 0.25);
  await guard.open("run-1");
  const c = ctx("run-1", { per_day_usd: 1, per_run_usd: 100 }); // per_day binds
  const verdicts = await Promise.all(Array.from({ length: 40 }, () => guard.preflight(c)));
  expect(verdicts.filter((v) => v.ok).length).toBe(4);
  expect(guard.dayTotalUsd).toBeCloseTo(1.0, 6);
});

test("RACE: the iteration cap is EXACT under concurrency (no over-reservation)", async () => {
  const guard = guardWith(new MemoryEventSink(), 0.01);
  await guard.open("run-1");
  const CAP = 8;
  const c = ctx("run-1", { max_iterations: CAP, per_run_usd: 1000 });
  const verdicts = await Promise.all(Array.from({ length: 40 }, () => guard.preflight(c)));
  expect(verdicts.filter((v) => v.ok).length).toBe(CAP);
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.iterations).toBe(CAP);
});

test("RACE: concurrent reserve+reconcile cycles keep exact books (no lost update)", async () => {
  const guard = guardWith(new MemoryEventSink(), 0.1);
  await guard.open("run-1");
  const c = ctx("run-1", { per_run_usd: 1000 });
  const N = 50;
  await Promise.all(
    Array.from({ length: N }, async () => {
      const v = await guard.preflight(c);
      if (v.ok) await guard.meter(c, { usd: 0.1 }); // actual == reserve
    }),
  );
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.spentUsd).toBeCloseTo(N * 0.1, 6); // 5.0 exactly
});

// ── 5. the proxy handler: key-free child + forward + meter + refusals ─────────

function keyCapturingUpstream(usage: { usd: number; tokens?: number } | undefined = { usd: 0.3 }): {
  upstream: ModelUpstream;
  seenKey: () => string | null;
} {
  let seen: string | null = null;
  return {
    seenKey: () => seen,
    upstream: {
      async forward(req) {
        seen = req.apiKey;
        return { status: 200, body: { ok: true, model: req.model }, usage };
      },
    },
  };
}

test("proxy: the child env is key-free, and the runtime key attaches only at forward time", async () => {
  const childEnv = buildChildEnv(
    { ANTHROPIC_API_KEY: "sk-ant-RUNTIME", PATH: "/bin" },
    {
      session: "run-1",
      runDir: "/runs/run-1",
      contractPath: "/runs/run-1/contract.json",
      modelProxyUrl: "http://127.0.0.1:0",
      modelToken: "bearer-1",
    },
  );
  expect(childEnv.ANTHROPIC_API_KEY).toBeUndefined();
  expect(Object.values(childEnv)).not.toContain("sk-ant-RUNTIME");

  const guard = guardWith(new MemoryEventSink(), 0.5);
  const tokens = new SessionTokenRegistry(() => "bearer-1");
  const { upstream, seenKey } = keyCapturingUpstream({ usd: 0.3, tokens: 900 });
  await guard.open("run-1");
  const token = tokens.mint(ctx("run-1", { per_run_usd: 5, max_iterations: 10 }));
  const app = createModelProxy({
    guard,
    tokens,
    upstream,
    apiKey: () => "sk-ant-RUNTIME",
    env: {},
  });

  const res = await app.request("/v1/messages", {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: JSON.stringify({ messages: [] }),
  });
  expect(res.status).toBe(200);
  expect(seenKey()).toBe("sk-ant-RUNTIME"); // attached at forward time, from the supervisor
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.spentUsd).toBeCloseTo(0.3, 6); // reserve 0.5 reconciled to actual 0.3
});

test("proxy: an unknown/absent bearer is 401 and never forwards", async () => {
  const guard = guardWith(new MemoryEventSink());
  const { upstream, seenKey } = keyCapturingUpstream();
  const app = createModelProxy({
    guard,
    tokens: new SessionTokenRegistry(),
    upstream,
    apiKey: () => "sk",
    env: {},
  });
  expect((await app.request("/v1/messages", { method: "POST" })).status).toBe(401);
  const badAuth = await app.request("/v1/messages", {
    method: "POST",
    headers: { authorization: "Bearer nope" },
  });
  expect(badAuth.status).toBe(401);
  expect(seenKey()).toBeNull();
});

test("proxy: a revoked bearer is 401 and never forwards", async () => {
  const guard = guardWith(new MemoryEventSink());
  const tokens = new SessionTokenRegistry(() => "bearer-1");
  const { upstream, seenKey } = keyCapturingUpstream();
  await guard.open("run-1");
  const token = tokens.mint(ctx("run-1", { per_run_usd: 5 }));
  tokens.revoke("run-1"); // killed
  const app = createModelProxy({ guard, tokens, upstream, apiKey: () => "sk", env: {} });
  const res = await app.request("/v1/messages", {
    method: "POST",
    headers: { authorization: `Bearer ${token}` },
  });
  expect(res.status).toBe(401);
  expect(seenKey()).toBeNull();
});

test("proxy: an exhausted budget answers 402 budget_exhausted and never forwards", async () => {
  const sink = new MemoryEventSink();
  const guard = guardWith(sink, 0.5);
  const tokens = new SessionTokenRegistry(() => "bearer-1");
  const { upstream, seenKey } = keyCapturingUpstream();
  await guard.open("run-1");
  await guard.preflight(ctx("run-1", { per_run_usd: 0.5 })); // reserve fills the $0.5 cap
  const token = tokens.mint(ctx("run-1", { per_run_usd: 0.5, max_iterations: 100 }));
  const app = createModelProxy({ guard, tokens, upstream, apiKey: () => "sk", env: {} });

  const res = await app.request("/v1/messages", {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: JSON.stringify({ messages: [] }),
  });
  expect(res.status).toBe(402);
  const body = (await res.json()) as { error: { code: string } };
  expect(body.error.code).toBe("budget_exhausted");
  expect(seenKey()).toBeNull();
  expect(sink.ofType(EVENT_TYPES.BUDGET_REFUSED).length).toBe(1);
});

test("proxy: an upstream throw RELEASES the reservation and returns a retryable 502", async () => {
  const guard = guardWith(new MemoryEventSink(), 0.5);
  const tokens = new SessionTokenRegistry(() => "bearer-1");
  await guard.open("run-1");
  const token = tokens.mint(ctx("run-1", { per_run_usd: 5, max_iterations: 10 }));
  const upstream: ModelUpstream = {
    async forward() {
      throw new Error("ECONNREFUSED");
    },
  };
  const app = createModelProxy({ guard, tokens, upstream, apiKey: () => "sk", env: {} });
  const res = await app.request("/v1/messages", {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: JSON.stringify({ messages: [] }),
  });
  expect(res.status).toBe(502);
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.spentUsd).toBeCloseTo(0, 6); // reservation refunded
  expect(row?.iterations).toBe(1); // attempt kept
});

test("proxy: a no-usage response releases the reservation", async () => {
  const guard = guardWith(new MemoryEventSink(), 0.5);
  const tokens = new SessionTokenRegistry(() => "bearer-1");
  // A 200 with no usage (a default param can't carry `undefined`, so build it inline).
  const upstream: ModelUpstream = {
    async forward(req) {
      return { status: 200, body: { ok: true, model: req.model } };
    },
  };
  await guard.open("run-1");
  const token = tokens.mint(ctx("run-1", { per_run_usd: 5, max_iterations: 10 }));
  const app = createModelProxy({ guard, tokens, upstream, apiKey: () => "sk", env: {} });
  await app.request("/v1/messages", {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: JSON.stringify({ messages: [] }),
  });
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.spentUsd).toBeCloseTo(0, 6);
});

// ── 6. the listener refuses a non-loopback bind (key confinement AS CODE) ──────

test("serveProxy REFUSES a non-loopback hostname", () => {
  const app = createModelProxy({
    guard: guardWith(new MemoryEventSink()),
    tokens: new SessionTokenRegistry(),
    upstream: keyCapturingUpstream().upstream,
    apiKey: () => "sk",
    env: {},
  });
  expect(() => serveProxy(app, { hostname: "0.0.0.0" })).toThrow(/non-loopback/);
});

test("serveProxy binds loopback and exposes a fetchable url", async () => {
  const app = createModelProxy({
    guard: guardWith(new MemoryEventSink()),
    tokens: new SessionTokenRegistry(),
    upstream: keyCapturingUpstream().upstream,
    apiKey: () => "sk",
    env: {},
  });
  const server = serveProxy(app, { port: 0 });
  try {
    expect(server.url).toMatch(/^http:\/\/127\.0\.0\.1:\d+$/);
    const res = await fetch(`${server.url}/v1/messages`, { method: "POST" });
    expect(res.status).toBe(401); // reachable, and unauthenticated calls are refused
  } finally {
    server.stop();
  }
});

// ── 7. D5 derivation helpers (BRO-1814 wires these at startup) ─────────────────

test("proxy budget derivation: per-session spend and day total from budget.metered", () => {
  const metered: MeteredRecord[] = [
    { session: "a", usd: 1.0, ts: 100 },
    { session: "a", usd: 0.5, ts: 200 },
    { session: "b", usd: 2.0, ts: 50 }, // before the day cutoff
  ];
  expect(deriveSpentBySession(metered).get("a")).toBeCloseTo(1.5, 6);
  expect(deriveSpentBySession(metered).get("b")).toBeCloseTo(2.0, 6);
  expect(deriveDayTotal(metered, 100)).toBeCloseTo(1.5, 6); // only ts>=100 counts
});
