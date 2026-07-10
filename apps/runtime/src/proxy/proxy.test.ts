/// <reference types="bun" />
// proxy.test.ts — the model proxy + budget-in-path guard (HARNESS §3, F3.1) for BRO-1788
// (`bun test apps/runtime --filter proxy`).
//
// The three done.check guarantees:
//   1. The child env has NO key — the runtime key attaches only at forward time inside the proxy.
//   2. Race: N children racing one budget never overspend (the iteration cap is EXACT under
//      concurrency; concurrent meters never lose an update).
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
import { createModelProxy, type ModelUpstream, parkForRefusal } from "./proxy";
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

// ── 1. tokens: mint / resolve / revoke ───────────────────────────────────────

test("proxy tokens: mint resolves, revoke invalidates, re-mint drops the old token", () => {
  let n = 0;
  const reg = new SessionTokenRegistry(() => `tok-${++n}`);
  const c = ctx("run-1", { per_run_usd: 5 });
  const t1 = reg.mint(c);
  expect(reg.resolve(t1)).toEqual(c);
  // re-mint (fresh-context restart reuses the session id) revokes the prior token
  const t2 = reg.mint(c);
  expect(reg.resolve(t1)).toBeNull();
  expect(reg.resolve(t2)).toEqual(c);
  // revoke on kill
  reg.revoke("run-1");
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

// ── 3. budget guard: preflight + meter ───────────────────────────────────────

test("budget preflight allows under caps and reserves an iteration", async () => {
  const sink = new MemoryEventSink();
  const guard = new BudgetGuard(handle.db, sink, { now: FIXED_NOW });
  await guard.open("run-1");
  const v = await guard.preflight(ctx("run-1", { per_run_usd: 5, max_iterations: 3 }));
  expect(v.ok).toBe(true);
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.iterations).toBe(1); // reserved
});

test("budget meter accumulates spend and day total, and journals budget.metered", async () => {
  const sink = new MemoryEventSink();
  const guard = new BudgetGuard(handle.db, sink, { now: FIXED_NOW });
  await guard.open("run-1");
  const c = ctx("run-1", { per_run_usd: 5 });
  await guard.meter(c, { usd: 0.5, tokens: 1200 });
  await guard.meter(c, { usd: 0.25 });
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.spentUsd).toBeCloseTo(0.75, 6);
  expect(guard.dayTotalUsd).toBeCloseTo(0.75, 6);
  const metered = sink.ofType(EVENT_TYPES.BUDGET_METERED);
  expect(metered.length).toBe(2);
  expect(metered[0]?.runDir).toBe("/runs/run-1");
  expect(metered[0]?.payload.usd).toBe(0.5);
});

test("budget preflight REFUSES per_run and journals budget.refused (parks blocked)", async () => {
  const sink = new MemoryEventSink();
  const guard = new BudgetGuard(handle.db, sink, { now: FIXED_NOW });
  await guard.open("run-1");
  await guard.meter(ctx("run-1", { per_run_usd: 5 }), { usd: 5 }); // exhaust
  const v = await guard.preflight(ctx("run-1", { per_run_usd: 5, max_iterations: 100 }));
  expect(v).toEqual({ ok: false, reason: "per_run" });
  const refused = sink.ofType(EVENT_TYPES.BUDGET_REFUSED);
  expect(refused.length).toBe(1);
  expect(refused[0]?.payload).toEqual({ session: "run-1", reason: "per_run" });
  expect(parkForRefusal()).toBe("blocked");
});

test("budget preflight REFUSES per_day from the in-memory accumulator", async () => {
  const sink = new MemoryEventSink();
  const guard = new BudgetGuard(handle.db, sink, { now: FIXED_NOW, dayTotalUsd: 20 });
  await guard.open("run-1");
  const v = await guard.preflight(ctx("run-1", { per_day_usd: 20, per_run_usd: 100 }));
  expect(v).toEqual({ ok: false, reason: "per_day" });
  expect(sink.ofType(EVENT_TYPES.BUDGET_REFUSED)[0]?.payload.reason).toBe("per_day");
});

test("budget preflight REFUSES at the iteration cap (fails closed with no row too)", async () => {
  const sink = new MemoryEventSink();
  const guard = new BudgetGuard(handle.db, sink, { now: FIXED_NOW });
  // never called open() → no row → fail closed as iteration_cap
  const v = await guard.preflight(ctx("ghost", { max_iterations: 5 }));
  expect(v).toEqual({ ok: false, reason: "iteration_cap" });
});

// ── 4. the race: N children racing one budget, no overspend ───────────────────

test("RACE: concurrent meters never lose an update (exact sum)", async () => {
  const guard = new BudgetGuard(handle.db, new MemoryEventSink(), { now: FIXED_NOW });
  await guard.open("run-1");
  const c = ctx("run-1", { per_run_usd: 1000 });
  const N = 50;
  await Promise.all(Array.from({ length: N }, () => guard.meter(c, { usd: 0.1 })));
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.spentUsd).toBeCloseTo(N * 0.1, 6); // 5.0 exactly — no lost +=
});

test("RACE: the iteration cap is EXACT under concurrency (no over-reservation)", async () => {
  const guard = new BudgetGuard(handle.db, new MemoryEventSink(), { now: FIXED_NOW });
  await guard.open("run-1");
  const CAP = 8;
  const N = 40; // 40 callers race for 8 slots
  const c = ctx("run-1", { max_iterations: CAP, per_run_usd: 1000 });
  const verdicts = await Promise.all(Array.from({ length: N }, () => guard.preflight(c)));
  const allowed = verdicts.filter((v) => v.ok).length;
  expect(allowed).toBe(CAP); // exactly CAP win; the rest refuse — the reservation is atomic
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.iterations).toBe(CAP); // never over-reserved past the cap
});

// ── 5. the proxy handler: key-free child + forward + meter + 402 ──────────────

function keyCapturingUpstream(): { upstream: ModelUpstream; seenKey: () => string | null } {
  let seen: string | null = null;
  return {
    seenKey: () => seen,
    upstream: {
      async forward(req) {
        seen = req.apiKey;
        return {
          status: 200,
          body: { ok: true, model: req.model },
          usage: { usd: 0.3, tokens: 900 },
        };
      },
    },
  };
}

test("proxy: the child env is key-free, and the runtime key attaches only at forward time", async () => {
  // The child env (BRO-1756) carries the proxy URL + bearer, never the key…
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

  // …and the proxy forwards WITH that runtime key — proof the key lives only on the supervisor side.
  const sink = new MemoryEventSink();
  const guard = new BudgetGuard(handle.db, sink, { now: FIXED_NOW });
  const tokens = new SessionTokenRegistry(() => "bearer-1");
  const { upstream, seenKey } = keyCapturingUpstream();
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
  expect(row?.spentUsd).toBeCloseTo(0.3, 6); // metered from the response
});

test("proxy: an unknown/absent bearer is 401 and never forwards", async () => {
  const guard = new BudgetGuard(handle.db, new MemoryEventSink(), { now: FIXED_NOW });
  const { upstream, seenKey } = keyCapturingUpstream();
  const app = createModelProxy({
    guard,
    tokens: new SessionTokenRegistry(),
    upstream,
    apiKey: () => "sk",
    env: {},
  });
  const noAuth = await app.request("/v1/messages", { method: "POST" });
  expect(noAuth.status).toBe(401);
  const badAuth = await app.request("/v1/messages", {
    method: "POST",
    headers: { authorization: "Bearer nope" },
  });
  expect(badAuth.status).toBe(401);
  expect(seenKey()).toBeNull(); // never reached upstream
});

test("proxy: an exhausted budget answers 402 budget_exhausted and never forwards", async () => {
  const sink = new MemoryEventSink();
  const guard = new BudgetGuard(handle.db, sink, { now: FIXED_NOW });
  const tokens = new SessionTokenRegistry(() => "bearer-1");
  const { upstream, seenKey } = keyCapturingUpstream();
  await guard.open("run-1");
  await guard.meter(ctx("run-1", { per_run_usd: 2 }), { usd: 2 }); // exhaust
  const token = tokens.mint(ctx("run-1", { per_run_usd: 2, max_iterations: 100 }));
  const app = createModelProxy({ guard, tokens, upstream, apiKey: () => "sk", env: {} });

  const res = await app.request("/v1/messages", {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: JSON.stringify({ messages: [] }),
  });
  expect(res.status).toBe(402);
  const body = (await res.json()) as { error: { code: string } };
  expect(body.error.code).toBe("budget_exhausted");
  expect(seenKey()).toBeNull(); // request never reached Anthropic
  expect(sink.ofType(EVENT_TYPES.BUDGET_REFUSED).length).toBe(1);
});

// ── 6. D5 derivation helpers (BRO-1814 wires these at startup) ─────────────────

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
