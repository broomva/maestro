/// <reference types="bun" />
// proxy.test.ts — the model proxy + budget-in-path guard (HARNESS §3, F3.1) for BRO-1788
// (`bun test apps/runtime --filter proxy`).
//
// The three done.check guarantees:
//   1. The child env has NO key — the runtime key attaches only at forward time inside the proxy.
//   2. Race: N children racing one budget never overspend — preflight RESERVES a per-call cost
//      CEILING (>= actual) against every cap, so a call that would breach is refused up-front and the
//      caps are exact under concurrency; the day accounting survives the UTC rollover.
//   3. A refusal answers 402 budget_exhausted, emits budget.refused, and parks the run blocked.

import { afterEach, beforeEach, expect, test } from "bun:test";
import { EVENT_TYPES } from "@maestro/protocol";
import { eq } from "drizzle-orm";
import { type IndexHandle, indexUrl, openIndex } from "../db/client";
import { runBudget } from "../db/schema";
import { buildChildEnv } from "../harness/spawn-contract";
import {
  BudgetGuard,
  deriveDayTotal,
  deriveSpentBySession,
  type MeteredRecord,
  type RefusalReason,
  type Reservation,
} from "./budget";
import { MemoryEventSink } from "./events";
import {
  DEFAULT_MODEL_PINS,
  estimateCallCeilingUsd,
  MAX_DOCUMENT_TOKENS,
  MAX_IMAGE_TOKENS,
  MODEL_PRICING,
  resolvePinnedModel,
} from "./models";
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

/** A guard with a pinned clock + an explicit default reserve so the guard-level arithmetic is exact.
 *  (The proxy passes a model-priced ceiling per call; these guard tests use the default reserve.) */
function guardWith(sink: MemoryEventSink, reserveUsd = 0.5, dayTotalUsd = 0): BudgetGuard {
  return new BudgetGuard(handle.db, sink, { now: FIXED_NOW, reserveUsd, dayTotalUsd });
}

/** Preflight and assert it passed, returning the held Reservation to thread into meter/release, which
 *  now REQUIRE it so each call reconciles against exactly what it reserved. */
async function reserve(
  guard: BudgetGuard,
  c: SessionContext,
  reserveUsd?: number,
): Promise<Reservation> {
  const v = await guard.preflight(c, reserveUsd);
  if (!v.ok) throw new Error(`preflight refused unexpectedly: ${v.reason}`);
  return v.reservation;
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

// ── 2. model pinning + cost ceiling ──────────────────────────────────────────

test("proxy pins a model per role, env override wins, blank override ignored", () => {
  expect(resolvePinnedModel("agent", {})).toBe(DEFAULT_MODEL_PINS.agent);
  expect(resolvePinnedModel("verifier", { MAESTRO_MODEL_VERIFIER: "claude-sonnet-5" })).toBe(
    "claude-sonnet-5",
  );
  expect(resolvePinnedModel("agent", { MAESTRO_MODEL_AGENT: "   " })).toBe(
    DEFAULT_MODEL_PINS.agent,
  );
});

test("cost ceiling scales with max_tokens, exceeds a real Opus call cost, and is >= 0", () => {
  const small = estimateCallCeilingUsd("claude-opus-4-8", { messages: [], max_tokens: 100 }, {});
  const big = estimateCallCeilingUsd("claude-opus-4-8", { messages: [], max_tokens: 64000 }, {});
  expect(big).toBeGreaterThan(small); // scales with the request
  // a 64k-output Opus call ceiling must exceed a typical real call (~$1) so the reserve truly bounds spend
  expect(big).toBeGreaterThan(1);
  // unknown model → conservative fallback price, never 0/negative
  expect(estimateCallCeilingUsd("mystery-model", { max_tokens: 1000 }, {})).toBeGreaterThan(0);
  expect(MODEL_PRICING["claude-opus-4-8"]?.outputPerMtok).toBeGreaterThan(0);
});

test("cost ceiling uses BYTE length, so dense input is not under-counted (P20 round-3 overspend)", () => {
  // Same CHARACTER count, different byte count: '文' is 3 UTF-8 bytes, 'a' is 1. A chars/token estimate
  // (the round-3 under-count) prices these equally and admits a call it can't afford; byte length
  // prices the CJK input ~3x higher. This assertion FAILS against a chars/token ceiling.
  const ascii = estimateCallCeilingUsd(
    "claude-opus-4-8",
    { messages: [{ role: "user", content: "a".repeat(3000) }], max_tokens: 10 },
    {},
  );
  const cjk = estimateCallCeilingUsd(
    "claude-opus-4-8",
    { messages: [{ role: "user", content: "文".repeat(3000) }], max_tokens: 10 },
    {},
  );
  expect(cjk).toBeGreaterThan(ascii * 2); // byte length captures the 3x density; char count would not
});

test("cost ceiling adds a per-image / per-document token bound of the RIGHT magnitude (modality billed by dimensions) — P20 round-5", () => {
  const tinyImg = {
    type: "image",
    source: { type: "base64", media_type: "image/png", data: "AA==" },
  };
  const tinyDoc = {
    type: "document",
    source: { type: "base64", media_type: "application/pdf", data: "AA==" },
  };
  const text = estimateCallCeilingUsd(
    "claude-opus-4-8",
    { messages: [{ role: "user", content: [{ type: "text", text: "x" }] }], max_tokens: 10 },
    {},
  );
  const withImage = estimateCallCeilingUsd(
    "claude-opus-4-8",
    {
      messages: [{ role: "user", content: [{ type: "text", text: "x" }, tinyImg] }],
      max_tokens: 10,
    },
    {},
  );
  const withDoc = estimateCallCeilingUsd(
    "claude-opus-4-8",
    {
      messages: [{ role: "user", content: [{ type: "text", text: "x" }, tinyDoc] }],
      max_tokens: 10,
    },
    {},
  );
  // Opus input is $15/Mtok. Assert each block adds AT LEAST its full token bound over the text-only
  // cost — this PINS the magnitude (finding #6): a full drop of the modality term fails both, and
  // mutating MAX_DOCUMENT_TOKENS (e.g. 160000→2000) fails the document assertion (it no longer clears
  // text + 160k-worth). Using the exported constants keeps the test in lockstep with the source.
  expect(withImage).toBeGreaterThan(text + (MAX_IMAGE_TOKENS * 15) / 1e6);
  expect(withDoc).toBeGreaterThan(text + (MAX_DOCUMENT_TOKENS * 15) / 1e6);
});

test("cost ceiling STRIPS image/document base64 so a big screenshot is not false-refused (P20 round-5 finding #4)", () => {
  // A ~300KB PNG is ~410KB base64. Pricing that blob as ~410k TEXT tokens yields a ~$7 Opus ceiling and
  // false-refuses a routine screenshot (P11 / agent-browser). Stripping the base64 and bounding the
  // image by MAX_IMAGE_TOKENS keeps the ceiling small. This FAILS if the base64 is not stripped (the
  // unstripped ceiling is > $7).
  const bigBase64 = "A".repeat(410_000);
  const ceiling = estimateCallCeilingUsd(
    "claude-opus-4-8",
    {
      messages: [
        {
          role: "user",
          content: [
            { type: "image", source: { type: "base64", media_type: "image/png", data: bigBase64 } },
          ],
        },
      ],
      max_tokens: 1024,
    },
    {},
  );
  // stripped input ≈ MAX_IMAGE_TOKENS + tiny structure → well under $1; unstripped would be ~$7.
  expect(ceiling).toBeLessThan(1);
  // and it is still >= the sound per-image bound (not zero) — the image is priced, just correctly.
  expect(ceiling).toBeGreaterThan((MAX_IMAGE_TOKENS * 15) / 1e6);
});

test("modelPrice: valid env override wins; NaN / negative / missing-slash fall back to the table", () => {
  const base = estimateCallCeilingUsd("claude-opus-4-8", { max_tokens: 1000 }, {});
  expect(
    estimateCallCeilingUsd(
      "claude-opus-4-8",
      { max_tokens: 1000 },
      { MAESTRO_PRICE_CLAUDE_OPUS_4_8: "1/1" },
    ),
  ).toBeLessThan(base); // valid override (cheaper) → smaller ceiling
  for (const bad of ["abc", "5", "-1/-1", "1/x"]) {
    expect(
      estimateCallCeilingUsd(
        "claude-opus-4-8",
        { max_tokens: 1000 },
        { MAESTRO_PRICE_CLAUDE_OPUS_4_8: bad },
      ),
    ).toBeCloseTo(base, 6); // malformed → table price, never a NaN/negative ceiling
  }
});

// ── 3. budget guard: reserve / reconcile / release ───────────────────────────

test("budget preflight RESERVES cost + an iteration under caps", async () => {
  const guard = guardWith(new MemoryEventSink(), 0.5);
  await guard.open("run-1");
  const v = await guard.preflight(ctx("run-1", { per_run_usd: 5, max_iterations: 3 }));
  expect(v.ok).toBe(true);
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.iterations).toBe(1);
  expect(row?.spentUsd).toBeCloseTo(0.5, 6); // reservation held
});

test("budget meter reconciles a reservation to the actual cost, journals budget.metered", async () => {
  const sink = new MemoryEventSink();
  const guard = guardWith(sink, 0.5);
  await guard.open("run-1");
  const c = ctx("run-1", { per_run_usd: 5 });
  const r = await reserve(guard, c); // reserves 0.5
  await guard.meter(c, { usd: 0.3, tokens: 1200 }, r); // reconcile 0.5 → 0.3
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
  const r = await reserve(guard, c);
  await guard.release(c, r); // failed/non-billable call
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.spentUsd).toBeCloseTo(0, 6);
  expect(row?.iterations).toBe(1); // attempt kept — a flaky upstream drains iterations, fail-closed
  expect(guard.dayTotalUsd).toBeCloseTo(0, 6);
});

test("budget preflight REFUSES per_run and journals budget.refused (parks blocked)", async () => {
  const sink = new MemoryEventSink();
  const guard = guardWith(sink, 0.5);
  await guard.open("run-1");
  const c = ctx("run-1", { per_run_usd: 1, max_iterations: 100 });
  await guard.meter(c, { usd: 0.5 }, await reserve(guard, c)); // spent 0.5
  await guard.meter(c, { usd: 0.5 }, await reserve(guard, c)); // spent 1.0 (full)
  const v = await guard.preflight(c); // 1.0 + 0.5 > 1 → refuse
  expect(v).toEqual({ ok: false, reason: "per_run" });
  expect(sink.ofType(EVENT_TYPES.BUDGET_REFUSED)[0]?.payload).toEqual({
    session: "run-1",
    reason: "per_run",
  });
  expect(PARK_STATE).toBe("blocked");
});

test("budget REFUSES a call whose reserve exceeds the remaining budget (no overspend, actual>reserve)", async () => {
  // The round-1 disqualifier: a call costing more than the default reserve overspent. With a ceiling
  // reserve >= actual, such a call is refused UP-FRONT — two concurrent $2-reserve calls on a $1 cap
  // both refuse and nothing is spent (vs the old code settling $4).
  const guard = guardWith(new MemoryEventSink(), 0.5);
  await guard.open("run-1");
  const c = ctx("run-1", { per_run_usd: 1 });
  const verdicts = await Promise.all([guard.preflight(c, 2), guard.preflight(c, 2)]);
  expect(verdicts.every((v) => !v.ok)).toBe(true);
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.spentUsd).toBeCloseTo(0, 6); // nothing spent — the calls never forwarded
});

test("budget preflight REFUSES per_day from the in-memory accumulator", async () => {
  const sink = new MemoryEventSink();
  const guard = guardWith(sink, 0.5, 20);
  await guard.open("run-1");
  const v = await guard.preflight(ctx("run-1", { per_day_usd: 20, per_run_usd: 100 }));
  expect(v).toEqual({ ok: false, reason: "per_day" });
  expect(sink.ofType(EVENT_TYPES.BUDGET_REFUSED)[0]?.payload.reason).toBe("per_day");
});

test("budget preflight REFUSES at the iteration cap (fails closed with no row too)", async () => {
  const guard = guardWith(new MemoryEventSink(), 0.5);
  const v = await guard.preflight(ctx("ghost", { max_iterations: 5 })); // no open() → no row
  expect(v).toEqual({ ok: false, reason: "iteration_cap" });
  expect(guard.dayTotalUsd).toBeCloseTo(0, 6); // day reservation rolled back on the failure
});

test("budget a refused per_run does NOT drive the day accumulator negative", async () => {
  const guard = guardWith(new MemoryEventSink(), 0.5);
  await guard.open("run-1");
  // per_run 0.1 < reserve 0.5 → the SQL reservation fails AFTER the day reserve was taken; the rollback
  // must be rollover-aware + clamped, never negative.
  const v = await guard.preflight(ctx("run-1", { per_day_usd: 100, per_run_usd: 0.1 }), 0.5);
  expect(v).toEqual({ ok: false, reason: "per_run" });
  expect(guard.dayTotalUsd).toBeGreaterThanOrEqual(0);
  expect(guard.dayTotalUsd).toBeCloseTo(0, 6);
});

// ── 4. day rollover (per_day is a DAILY cap, and survives a cross-midnight call) ──

test("budget per_day ROLLS OVER at the day boundary (not a lifetime cap)", async () => {
  let clock = 1_700_000_000_000;
  const guard = new BudgetGuard(handle.db, new MemoryEventSink(), {
    now: () => clock,
    reserveUsd: 0.5,
  });
  await guard.open("run-1");
  const c = ctx("run-1", { per_day_usd: 0.5 });
  await guard.meter(c, { usd: 0.5 }, await reserve(guard, c)); // day total 0.5 (full)
  expect((await guard.preflight(c)).ok).toBe(false); // same day → refused
  clock += 86_400_000; // cross into the next UTC day
  expect((await guard.preflight(c)).ok).toBe(true); // rolled over → fresh day budget
});

test("budget a call reserved before midnight and metered after books FULL actual to the new day", async () => {
  // The round-2 fail-open: meter applied a relative delta to a bucket rollover had zeroed, dropping the
  // crossing call — per_day then admitted more. Booking full actual to the new day closes it.
  let clock = 1_700_000_000_000;
  const guard = new BudgetGuard(handle.db, new MemoryEventSink(), {
    now: () => clock,
    reserveUsd: 0.5,
  });
  await guard.open("run-1");
  const c = ctx("run-1", { per_day_usd: 1 });
  const r = await reserve(guard, c, 0.5); // reserved on day D (bucket D)
  clock += 86_400_000; // cross midnight before the response comes back
  await guard.meter(c, { usd: 0.5 }, r); // metered on day D+1 — books full actual, no stale delta
  expect(guard.dayTotalUsd).toBeCloseTo(0.5, 6); // the crossing call is counted on D+1, not lost
});

test("budget MULTI-CALL cross-midnight straddle does NOT overspend per_day (reservations carry across rollover)", async () => {
  // The P20 round-5 CRITICAL, closed. A single scalar #dayReservedUsd ZEROED at rollover dropped a
  // straddler's in-flight commitment: A reserves on day D, the clock crosses UTC midnight, then B AND C
  // both reserve on D+1 *before* A meters — with A's commitment invisible both were admitted, then A's
  // actual booked on top settled $1.5 on a $1 cap. Carrying outstanding reservations across the
  // rollover keeps A visible, so the SECOND new-day call (C) is refused.
  //
  // Anti-vacuity (mutation-proven): make #rolloverIfNeeded also zero #dayReservedUsd (drop the carry)
  // and C is ADMITTED — expect(vC.ok).toBe(false) fails. The vC REFUSAL is the discriminator; the
  // dayTotalUsd assertion alone can't catch it, because the buggy guard UNDER-reports its own total
  // (finding #7). per_day is workspace-scope, so A/B/C share the day total.
  let clock = 1_700_000_000_000;
  const guard = new BudgetGuard(handle.db, new MemoryEventSink(), {
    now: () => clock,
    reserveUsd: 0.5,
  });
  const A = ctx("run-A", { per_day_usd: 1, per_run_usd: 100 });
  const B = ctx("run-B", { per_day_usd: 1, per_run_usd: 100 });
  const C = ctx("run-C", { per_day_usd: 1, per_run_usd: 100 });
  await guard.open("run-A");
  await guard.open("run-B");
  await guard.open("run-C");

  const rA = await reserve(guard, A, 0.5); // day D: A reserves, in flight
  clock += 86_400_000; // cross into day D+1 before A settles
  const vB = await guard.preflight(B, 0.5); // D+1: B reserves; A(carried) + B == cap
  expect(vB.ok).toBe(true);
  const vC = await guard.preflight(C, 0.5); // D+1: A(carried) + B fill the cap → C must refuse
  expect(vC.ok).toBe(false); // pre-fix (zeroing rollover) ADMITS C here → $1.5 on a $1 cap
  expect((vC as { reason: RefusalReason }).reason).toBe("per_day");
  await guard.meter(A, { usd: 0.5 }, rA); // A settles on D+1 (its carried reservation releases)
  expect(guard.dayTotalUsd).toBeLessThanOrEqual(1 + 1e-9); // secondary sanity: never over the cap
});

// ── 5. the race: N children racing one budget, no overspend ───────────────────

test("RACE: the per_run DOLLAR cap is EXACT under concurrency (reservation, unbounded iters)", async () => {
  const guard = guardWith(new MemoryEventSink(), 0.25);
  await guard.open("run-1");
  const c = ctx("run-1", { per_run_usd: 1 }); // no max_iterations on purpose
  const verdicts = await Promise.all(Array.from({ length: 40 }, () => guard.preflight(c)));
  expect(verdicts.filter((v) => v.ok).length).toBe(4); // 4 × 0.25 = 1.0, no more
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.spentUsd).toBeCloseTo(1.0, 6);
});

test("RACE: the per_day DOLLAR cap is EXACT under concurrency", async () => {
  const guard = guardWith(new MemoryEventSink(), 0.25);
  await guard.open("run-1");
  const c = ctx("run-1", { per_day_usd: 1, per_run_usd: 100 });
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
      if (v.ok) await guard.meter(c, { usd: 0.1 }, v.reservation); // actual == reserve
    }),
  );
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.spentUsd).toBeCloseTo(N * 0.1, 6); // 5.0 exactly
});

// ── 6. the proxy handler: key-free child + forward + meter + refusals ─────────

function keyCapturingUpstream(usage: { usd: number; tokens?: number }): {
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
    body: JSON.stringify({ messages: [], max_tokens: 100 }),
  });
  expect(res.status).toBe(200);
  expect(seenKey()).toBe("sk-ant-RUNTIME"); // attached at forward time, from the supervisor
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.spentUsd).toBeCloseTo(0.3, 6); // reserve (ceiling) reconciled to actual 0.3
});

test("proxy: an unknown/absent bearer is 401 and never forwards", async () => {
  const { upstream, seenKey } = keyCapturingUpstream({ usd: 0.3 });
  const app = createModelProxy({
    guard: guardWith(new MemoryEventSink()),
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
  const { upstream, seenKey } = keyCapturingUpstream({ usd: 0.3 });
  await guard.open("run-1");
  const token = tokens.mint(ctx("run-1", { per_run_usd: 5 }));
  tokens.revoke("run-1");
  const app = createModelProxy({ guard, tokens, upstream, apiKey: () => "sk", env: {} });
  const res = await app.request("/v1/messages", {
    method: "POST",
    headers: { authorization: `Bearer ${token}` },
  });
  expect(res.status).toBe(401);
  expect(seenKey()).toBeNull();
});

test("proxy: an over-budget call answers 402 budget_exhausted and never forwards", async () => {
  const sink = new MemoryEventSink();
  const guard = guardWith(sink, 0.5);
  const tokens = new SessionTokenRegistry(() => "bearer-1");
  const { upstream, seenKey } = keyCapturingUpstream({ usd: 0.3 });
  await guard.open("run-1");
  // per_run of a fraction of a cent — a real call's ceiling exceeds it → refused up-front.
  const token = tokens.mint(ctx("run-1", { per_run_usd: 0.0001, max_iterations: 100 }));
  const app = createModelProxy({ guard, tokens, upstream, apiKey: () => "sk", env: {} });

  const res = await app.request("/v1/messages", {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: JSON.stringify({ messages: [], max_tokens: 1000 }),
  });
  expect(res.status).toBe(402);
  const body = (await res.json()) as { error: { code: string } };
  expect(body.error.code).toBe("budget_exhausted");
  expect(seenKey()).toBeNull();
  expect(sink.ofType(EVENT_TYPES.BUDGET_REFUSED).length).toBe(1);
});

test("proxy: a big-max_tokens call is refused when its CEILING exceeds the budget (anti-vacuity)", async () => {
  // per_run = $1 sits BETWEEN the old flat reserve ($0.5) and this call's ceiling (~$5.5 for a 64k
  // Opus call). The ceiling reserve → 402. A flat $0.5 reserve would ADMIT it (200) — so reverting
  // proxy.ts to the round-2 flat reserve makes THIS test fail. It is what proves the ceiling is live.
  const sink = new MemoryEventSink();
  const guard = guardWith(sink, 0.5);
  const tokens = new SessionTokenRegistry(() => "bearer-1");
  const { upstream, seenKey } = keyCapturingUpstream({ usd: 2 });
  await guard.open("run-1");
  const token = tokens.mint(ctx("run-1", { per_run_usd: 1, max_iterations: 100 }));
  const app = createModelProxy({ guard, tokens, upstream, apiKey: () => "sk", env: {} });
  const res = await app.request("/v1/messages", {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: JSON.stringify({ messages: [], max_tokens: 64000 }),
  });
  expect(res.status).toBe(402); // ceiling > $1 → refused; a flat $0.5 reserve would 200 here
  expect(seenKey()).toBeNull();
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.spentUsd).toBeCloseTo(0, 6);
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
    body: JSON.stringify({ messages: [], max_tokens: 100 }),
  });
  expect(res.status).toBe(502);
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.spentUsd).toBeCloseTo(0, 6); // reservation refunded
  expect(row?.iterations).toBe(1); // attempt kept
});

test("proxy: a no-usage response releases the reservation", async () => {
  const guard = guardWith(new MemoryEventSink(), 0.5);
  const tokens = new SessionTokenRegistry(() => "bearer-1");
  const upstream: ModelUpstream = {
    async forward(req) {
      return { status: 200, body: { ok: true, model: req.model } }; // no usage
    },
  };
  await guard.open("run-1");
  const token = tokens.mint(ctx("run-1", { per_run_usd: 5, max_iterations: 10 }));
  const app = createModelProxy({ guard, tokens, upstream, apiKey: () => "sk", env: {} });
  await app.request("/v1/messages", {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: JSON.stringify({ messages: [], max_tokens: 100 }),
  });
  const [row] = await handle.db.select().from(runBudget).where(eq(runBudget.sessionId, "run-1"));
  expect(row?.spentUsd).toBeCloseTo(0, 6);
});

// ── 7. the listener refuses a non-loopback bind (key confinement AS CODE) ──────

test("serveProxy REFUSES a non-loopback hostname", () => {
  const app = createModelProxy({
    guard: guardWith(new MemoryEventSink()),
    tokens: new SessionTokenRegistry(),
    upstream: keyCapturingUpstream({ usd: 0.3 }).upstream,
    apiKey: () => "sk",
    env: {},
  });
  expect(() => serveProxy(app, { hostname: "0.0.0.0" })).toThrow(/non-loopback/);
});

test("serveProxy binds loopback and exposes a fetchable url", async () => {
  const app = createModelProxy({
    guard: guardWith(new MemoryEventSink()),
    tokens: new SessionTokenRegistry(),
    upstream: keyCapturingUpstream({ usd: 0.3 }).upstream,
    apiKey: () => "sk",
    env: {},
  });
  const server = serveProxy(app, { port: 0 });
  try {
    expect(server.url).toMatch(/^http:\/\/127\.0\.0\.1:\d+$/);
    const res = await fetch(`${server.url}/v1/messages`, { method: "POST" });
    expect(res.status).toBe(401);
  } finally {
    server.stop();
  }
});

// ── 8. D5 derivation helpers (BRO-1814 wires these at startup) ─────────────────

test("proxy budget derivation: per-session spend and day total from budget.metered", () => {
  const metered: MeteredRecord[] = [
    { session: "a", usd: 1.0, ts: 100 },
    { session: "a", usd: 0.5, ts: 200 },
    { session: "b", usd: 2.0, ts: 50 }, // before the day cutoff
  ];
  expect(deriveSpentBySession(metered).get("a")).toBeCloseTo(1.5, 6);
  expect(deriveSpentBySession(metered).get("b")).toBeCloseTo(2.0, 6);
  expect(deriveDayTotal(metered, 100)).toBeCloseTo(1.5, 6);
});
