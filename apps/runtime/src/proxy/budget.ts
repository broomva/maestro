// The budget-in-path guard (HARNESS §3, F3.1, AUTONOMY §4). AUTHORITATIVE state is `run_budget`
// (per-session `spent_usd` + `iterations`) plus an in-memory day accounting. "On the invoice is too
// late" — every model call is RESERVED before it forwards and RECONCILED after it returns.
//
// Reserve-then-reconcile: preflight RESERVES a per-call cost CEILING (models.ts estimateCallCeilingUsd
// — output bounded by max_tokens, input over-estimated, so ceiling >= actual) against every cap
// ATOMICALLY. Because the reservation is >= the real cost, a call that would breach a cap is refused
// UP-FRONT (the safe answer when one call could overspend), and meter() only ever reconciles DOWNWARD
// to the actual — so spend can never exceed a cap under any concurrency. A failed/non-billable call
// releases the reservation. The per-call reserve is threaded from preflight → meter/release by the
// caller (the proxy), so each call reconciles against exactly what it reserved.
//
// Concurrency: the per_run + iteration reservation is a single conditional SQL UPDATE libSQL
// serializes as the sole writer; the per_day reservation is a synchronous in-memory check+increment
// (no await between), so JS's cooperative scheduling makes it atomic too.
//
// Day accounting is SPLIT into reserved (in-flight) + spent (metered) so it survives the UTC day
// rollover: a call reserved before midnight and metered after books its FULL actual to the new day
// (never a stale relative delta against a freshly-zeroed bucket — a fail-open seam otherwise).

import type { Budget } from "@maestro/protocol";
import { EVENT_TYPES } from "@maestro/protocol";
import { and, eq, lt, sql } from "drizzle-orm";
import type { IndexDb } from "../db/client";
import { runBudget } from "../db/schema";
import type { BudgetEventSink } from "./events";
import type { SessionContext } from "./tokens";

/** Which cap a refusal hit — the `budget.refused` reason + the child's park classification. */
export type RefusalReason = "per_run" | "per_day" | "iteration_cap";

/** The pre-forward verdict. `ok:false` → the proxy answers 402 and the run parks `blocked` (F3.1). */
export type BudgetVerdict = { ok: true } | { ok: false; reason: RefusalReason };

/** Actual usage metered from a completed model call (HARNESS §3 step 3). */
export interface MeterInput {
  usd: number;
  tokens?: number;
}

/** Fallback per-call reservation when the caller passes none (direct guard use / tests). The proxy
 *  always passes a model-priced ceiling (models.ts estimateCallCeilingUsd), which is the sound value. */
export const DEFAULT_RESERVE_USD = 0.5;

const DAY_MS = 86_400_000;
const dayBucket = (ms: number): number => Math.floor(ms / DAY_MS);

export interface BudgetGuardOptions {
  /** Injected clock (epoch ms). Defaults to Date.now; tests pin it. */
  now?: () => number;
  /** Day metered-total seed — BRO-1814 derives it from `budget.metered` at F9.2 startup (D5). */
  dayTotalUsd?: number;
  /** Fallback per-call reserve (see DEFAULT_RESERVE_USD). */
  reserveUsd?: number;
}

export class BudgetGuard {
  readonly #db: IndexDb;
  readonly #sink: BudgetEventSink;
  readonly #now: () => number;
  readonly #defaultReserve: number;
  /** In-flight reservations for the current day (released on meter/reconcile). */
  #dayReservedUsd = 0;
  /** Metered actual spend for the current day. */
  #daySpentUsd: number;
  #dayBucket: number;

  constructor(db: IndexDb, sink: BudgetEventSink, opts: BudgetGuardOptions = {}) {
    this.#db = db;
    this.#sink = sink;
    this.#now = opts.now ?? Date.now;
    this.#defaultReserve = opts.reserveUsd ?? DEFAULT_RESERVE_USD;
    this.#daySpentUsd = opts.dayTotalUsd ?? 0;
    this.#dayBucket = dayBucket(this.#now());
  }

  /** The runtime-day total (reserved in-flight + metered), workspace scope — the per_day cap check +
   *  observability. Rolls over at the UTC day boundary so per_day is a DAILY cap, not a lifetime one. */
  get dayTotalUsd(): number {
    this.#rolloverIfNeeded();
    return this.#dayReservedUsd + this.#daySpentUsd;
  }

  /** Reset the day accounting when the clock crosses into a new UTC day (24/7 runtime, D5). */
  #rolloverIfNeeded(): void {
    const b = dayBucket(this.#now());
    if (b !== this.#dayBucket) {
      this.#dayBucket = b;
      this.#dayReservedUsd = 0;
      this.#daySpentUsd = 0;
    }
  }

  /** Ensure a `run_budget` row exists for a session (called at spawn). Idempotent. */
  async open(session: string): Promise<void> {
    await this.#db.insert(runBudget).values({ sessionId: session }).onConflictDoNothing();
  }

  /**
   * Pre-forward guard (HARNESS §3 step 1). RESERVES `reserveUsd` (the per-call cost ceiling) + one
   * iteration against every cap atomically; refuses if any cap would be exceeded. Because the reserve
   * is >= the real cost, a call that would breach is refused here, before it forwards.
   */
  async preflight(ctx: SessionContext, reserveUsd = this.#defaultReserve): Promise<BudgetVerdict> {
    const { budget } = ctx;
    const reserve = reserveUsd;

    // (a) per-day cap — synchronous check+reserve on the in-memory accumulator (no await between, so
    // racing preflights can't both take the last day slot). Day total is WORKSPACE-scope: every
    // session's reservation counts toward it (a session without its own per_day cap still contributes
    // to the day other sessions' caps check against), so always reserve; enforce only when set.
    this.#rolloverIfNeeded();
    if (budget.per_day_usd !== undefined && this.dayTotalUsd + reserve > budget.per_day_usd) {
      await this.#refuse(ctx, "per_day");
      return { ok: false, reason: "per_day" };
    }
    this.#dayReservedUsd += reserve;

    // (b) per-run spend + iteration cap — one conditional UPDATE reserving both. A throw or a
    // rowsAffected 0 must roll back the day reservation we just took (fail-closed availability).
    const conds = [eq(runBudget.sessionId, ctx.session)];
    if (budget.per_run_usd !== undefined) {
      conds.push(sql`${runBudget.spentUsd} + ${reserve} <= ${budget.per_run_usd}`);
    }
    if (budget.max_iterations !== undefined) {
      conds.push(lt(runBudget.iterations, budget.max_iterations));
    }
    let rowsAffected: number;
    try {
      const res = await this.#db
        .update(runBudget)
        .set({
          spentUsd: sql`${runBudget.spentUsd} + ${reserve}`,
          iterations: sql`${runBudget.iterations} + 1`,
          lastCallAt: this.#now(),
        })
        .where(and(...conds));
      rowsAffected = res.rowsAffected;
    } catch (err) {
      this.#releaseDayReservation(reserve); // never strand a day reservation on a DB error
      throw err;
    }

    if (rowsAffected === 0) {
      this.#releaseDayReservation(reserve);
      const reason = await this.#classifyRunRefusal(ctx.session, budget, reserve);
      await this.#refuse(ctx, reason);
      return { ok: false, reason };
    }
    return { ok: true };
  }

  /**
   * Reconcile a reservation to the ACTUAL cost of a completed call (HARNESS §3 step 3). Pass the SAME
   * `reserveUsd` the matching preflight used. Emits `budget.metered` with the real usage. The day
   * accounting books the FULL actual to the CURRENT day (rollover-safe), not a relative delta.
   */
  async meter(
    ctx: SessionContext,
    input: MeterInput,
    reserveUsd = this.#defaultReserve,
  ): Promise<void> {
    const actual = Number.isFinite(input.usd) && input.usd > 0 ? input.usd : 0;
    // In-memory reconcile FIRST — it can't throw, so a subsequent failure never strands the day
    // reservation (the availability DoS P20 round-3 flagged). Book the FULL actual to the current day
    // so a call reserved before midnight and metered after is counted on the new day (rollover-safe).
    this.#rolloverIfNeeded();
    this.#dayReservedUsd = Math.max(0, this.#dayReservedUsd - reserveUsd);
    this.#daySpentUsd += actual;
    // Durable event (D-DURABILITY: the event is truth; run_budget is a rebuildable cache).
    await this.#sink.emit(ctx.runDir, {
      ts: new Date(this.#now()).toISOString(),
      actor: "system",
      type: EVENT_TYPES.BUDGET_METERED,
      payload: { session: ctx.session, usd: actual, tokens: input.tokens ?? null },
    });
    // Reconcile the run_budget CACHE. A libSQL throw is swallowed — the metered event already
    // recorded the spend, and D5 (BRO-1814) rebuilds spent_usd from it at startup.
    try {
      await this.#db
        .update(runBudget)
        .set({
          spentUsd: sql`max(0, ${runBudget.spentUsd} + ${actual - reserveUsd})`,
          lastCallAt: this.#now(),
        })
        .where(eq(runBudget.sessionId, ctx.session));
    } catch {
      // cache stale; the durable budget.metered event is the source of truth (D-DURABILITY, D5)
    }
  }

  /**
   * Release a reservation for a call that never billed (upstream error / no usage): refund the
   * reserved dollars but KEEP the consumed iteration — a flaky upstream then drains `max_iterations`
   * and parks the run (fail-closed) rather than retrying free forever. Emits nothing (no spend). Pass
   * the SAME `reserveUsd` the matching preflight used.
   */
  async release(ctx: SessionContext, reserveUsd = this.#defaultReserve): Promise<void> {
    // In-memory refund FIRST so a DB throw can't strand the day reservation (availability DoS).
    this.#releaseDayReservation(reserveUsd);
    try {
      await this.#db
        .update(runBudget)
        .set({ spentUsd: sql`max(0, ${runBudget.spentUsd} - ${reserveUsd})` })
        .where(eq(runBudget.sessionId, ctx.session));
    } catch {
      // cache-only refund; nothing was durably spent (no budget.metered emitted), so nothing to reconcile
    }
  }

  /** Roll back an in-memory day reservation — rollover-aware + clamped, the ONE mutation helper so no
   *  site drifts (a missing rollover/clamp here drove the accumulator negative in P20 round-2). */
  #releaseDayReservation(reserve: number): void {
    this.#rolloverIfNeeded();
    this.#dayReservedUsd = Math.max(0, this.#dayReservedUsd - reserve);
  }

  /** Name the run-scoped cap that blocked a reservation (per_run before iteration_cap). */
  async #classifyRunRefusal(
    session: string,
    budget: Budget,
    reserve: number,
  ): Promise<RefusalReason> {
    const [row] = await this.#db
      .select({ spentUsd: runBudget.spentUsd, iterations: runBudget.iterations })
      .from(runBudget)
      .where(eq(runBudget.sessionId, session));
    // No row (spawn skipped `open`) is treated as an iteration refusal — the guard fails closed.
    if (row === undefined) return "iteration_cap";
    if (budget.per_run_usd !== undefined && row.spentUsd + reserve > budget.per_run_usd) {
      return "per_run";
    }
    return "iteration_cap";
  }

  /** Emit `budget.refused` on the run's durable journal (D-DURABILITY). */
  async #refuse(ctx: SessionContext, reason: RefusalReason): Promise<void> {
    await this.#sink.emit(ctx.runDir, {
      ts: new Date(this.#now()).toISOString(),
      actor: "system",
      type: EVENT_TYPES.BUDGET_REFUSED,
      payload: { session: ctx.session, reason },
    });
  }
}

// ── D5 derivation helpers (crash reconciliation; BRO-1814 wires these at F9.2 startup) ───────────

/** A journaled `budget.metered` event, as read back for derivation. */
export interface MeteredRecord {
  session: string;
  usd: number;
  ts: number;
}

/**
 * Per-session spend derived from `budget.metered` events (D5 "derive-and-max"). BRO-1814 sets
 * `run_budget.spent_usd = max(stored, derived)` so a crash-window call is over- not under-counted
 * (overcounting is a cent lost; undercounting is the guard leaking). `budget.refused` is excluded —
 * nothing was spent.
 */
export function deriveSpentBySession(metered: readonly MeteredRecord[]): Map<string, number> {
  const out = new Map<string, number>();
  for (const m of metered) out.set(m.session, (out.get(m.session) ?? 0) + Math.max(0, m.usd));
  return out;
}

/**
 * The runtime-day total spend (workspace scope) — the sum of `budget.metered.usd` at or after
 * `dayStartMs` (D5). Recomputed at startup and used to seed `BudgetGuard.dayTotalUsd`.
 */
export function deriveDayTotal(metered: readonly MeteredRecord[], dayStartMs: number): number {
  let total = 0;
  for (const m of metered) if (m.ts >= dayStartMs) total += Math.max(0, m.usd);
  return total;
}
