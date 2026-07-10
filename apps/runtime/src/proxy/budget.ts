// The budget-in-path guard (HARNESS §3, F3.1, AUTONOMY §4). AUTHORITATIVE state is `run_budget`
// (per-session `spent_usd` + `iterations`) plus an in-memory day-total accumulator. "On the invoice
// is too late" — every model call is RESERVED before it forwards and RECONCILED after it returns.
//
// Why reserve-then-reconcile (not check-then-meter): metering only after the response is a race — N
// concurrent callers all read the same pre-meter spend, all pass the cap, and overspend (an earlier
// cut did exactly this). Instead, preflight RESERVES a conservative per-call cost against every cap
// ATOMICALLY (per_run + iterations in one conditional SQL UPDATE; per_day in a synchronous in-memory
// step the single-threaded event loop makes atomic). meter() reconciles the reservation to the actual
// cost; a failed/non-billable call releases it. So spent-including-in-flight-reservations never
// exceeds a cap, as long as the reservation is >= the real per-call cost (see #reserveUsd).
//
// Concurrency: the reservation UPDATE is a single statement libSQL serializes as the sole writer, so
// two callers can never both take the last slot; the day reservation is a synchronous check+increment
// (no await between), so JS's cooperative scheduling makes it atomic too.

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

/**
 * The conservative per-call cost reserved at preflight (USD). The no-overspend guarantee is exact
 * when this is >= the largest a single model call can cost; below that, an in-flight call can overshoot
 * a cap by at most (actual - reserve) and the reconcile keeps the books exact afterward. Operators set
 * it to their max per-call cost for a hard guarantee.
 */
export const DEFAULT_RESERVE_USD = 0.5;

const DAY_MS = 86_400_000;
const dayBucket = (ms: number): number => Math.floor(ms / DAY_MS);

export interface BudgetGuardOptions {
  /** Injected clock (epoch ms). Defaults to Date.now; tests pin it. */
  now?: () => number;
  /** Day-total seed — BRO-1814 derives it from `budget.metered` at F9.2 startup (D5) and passes it. */
  dayTotalUsd?: number;
  /** Per-call reservation (see DEFAULT_RESERVE_USD). */
  reserveUsd?: number;
}

export class BudgetGuard {
  readonly #db: IndexDb;
  readonly #sink: BudgetEventSink;
  readonly #now: () => number;
  readonly #reserveUsd: number;
  #dayTotalUsd: number;
  #dayBucket: number;

  constructor(db: IndexDb, sink: BudgetEventSink, opts: BudgetGuardOptions = {}) {
    this.#db = db;
    this.#sink = sink;
    this.#now = opts.now ?? Date.now;
    this.#reserveUsd = opts.reserveUsd ?? DEFAULT_RESERVE_USD;
    this.#dayTotalUsd = opts.dayTotalUsd ?? 0;
    this.#dayBucket = dayBucket(this.#now());
  }

  /** The runtime-day total spend so far (workspace scope) — observability + the per-day cap. Rolls
   *  over at the UTC day boundary so per_day is a DAILY cap, not a lifetime one. */
  get dayTotalUsd(): number {
    this.#rolloverIfNeeded();
    return this.#dayTotalUsd;
  }

  /** Reset the day total when the clock crosses into a new UTC day (24/7 runtime, D5). */
  #rolloverIfNeeded(): void {
    const b = dayBucket(this.#now());
    if (b !== this.#dayBucket) {
      this.#dayBucket = b;
      this.#dayTotalUsd = 0;
    }
  }

  /** Ensure a `run_budget` row exists for a session (called at spawn). Idempotent. */
  async open(session: string): Promise<void> {
    await this.#db.insert(runBudget).values({ sessionId: session }).onConflictDoNothing();
  }

  /**
   * Pre-forward guard (HARNESS §3 step 1). RESERVES a conservative cost + one iteration against every
   * cap atomically; refuses if any cap would be exceeded. Reserving up-front (not counting on the
   * response) is what makes the caps hold under concurrency.
   */
  async preflight(ctx: SessionContext): Promise<BudgetVerdict> {
    const { budget } = ctx;
    const reserve = this.#reserveUsd;

    // (a) per-day cap — a synchronous check+reserve on the in-memory accumulator (no await between
    // them, so two racing preflights can't both take the last day slot). The day total is
    // WORKSPACE-scope: EVERY session's reservation counts toward it (a session without its own per_day
    // cap still contributes to the day that other sessions' caps check against), so we always reserve
    // — the cap is only ENFORCED when this node carries a per_day_usd.
    this.#rolloverIfNeeded();
    if (budget.per_day_usd !== undefined && this.#dayTotalUsd + reserve > budget.per_day_usd) {
      await this.#refuse(ctx, "per_day");
      return { ok: false, reason: "per_day" };
    }
    this.#dayTotalUsd += reserve;

    // (b) per-run spend + iteration cap — one conditional UPDATE reserving both. rowsAffected 0 means
    // a cap blocked the reservation (or the row is missing).
    const conds = [eq(runBudget.sessionId, ctx.session)];
    if (budget.per_run_usd !== undefined) {
      conds.push(sql`${runBudget.spentUsd} + ${reserve} <= ${budget.per_run_usd}`);
    }
    if (budget.max_iterations !== undefined) {
      conds.push(lt(runBudget.iterations, budget.max_iterations));
    }
    const res = await this.#db
      .update(runBudget)
      .set({
        spentUsd: sql`${runBudget.spentUsd} + ${reserve}`,
        iterations: sql`${runBudget.iterations} + 1`,
        lastCallAt: this.#now(),
      })
      .where(and(...conds));

    if (res.rowsAffected === 0) {
      this.#dayTotalUsd -= reserve; // roll back the day reservation (synchronous)
      const reason = await this.#classifyRunRefusal(ctx.session, budget, reserve);
      await this.#refuse(ctx, reason);
      return { ok: false, reason };
    }
    return { ok: true };
  }

  /**
   * Reconcile a reservation to the ACTUAL cost of a completed call (HARNESS §3 step 3). The delta
   * (actual - reserved) is applied atomically; emits `budget.metered` with the real usage. Must follow
   * a `preflight` for this call.
   */
  async meter(ctx: SessionContext, input: MeterInput): Promise<void> {
    const actual = Number.isFinite(input.usd) && input.usd > 0 ? input.usd : 0;
    const delta = actual - this.#reserveUsd;
    await this.#db
      .update(runBudget)
      .set({ spentUsd: sql`max(0, ${runBudget.spentUsd} + ${delta})`, lastCallAt: this.#now() })
      .where(eq(runBudget.sessionId, ctx.session));
    this.#rolloverIfNeeded();
    this.#dayTotalUsd = Math.max(0, this.#dayTotalUsd + delta);
    await this.#sink.emit(ctx.runDir, {
      ts: new Date(this.#now()).toISOString(),
      actor: "system",
      type: EVENT_TYPES.BUDGET_METERED,
      payload: { session: ctx.session, usd: actual, tokens: input.tokens ?? null },
    });
  }

  /**
   * Release a reservation for a call that never billed (upstream error / no usage): refund the
   * reserved dollars but KEEP the consumed iteration — a flaky upstream then drains `max_iterations`
   * and parks the run (fail-closed) rather than retrying free forever. Emits nothing (no spend).
   */
  async release(ctx: SessionContext): Promise<void> {
    await this.#db
      .update(runBudget)
      .set({ spentUsd: sql`max(0, ${runBudget.spentUsd} - ${this.#reserveUsd})` })
      .where(eq(runBudget.sessionId, ctx.session));
    this.#rolloverIfNeeded();
    this.#dayTotalUsd = Math.max(0, this.#dayTotalUsd - this.#reserveUsd);
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
