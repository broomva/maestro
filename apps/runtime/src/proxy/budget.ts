// The budget-in-path guard (HARNESS §3, F3.1, AUTONOMY §4). AUTHORITATIVE state is `run_budget`
// (per-session `spent_usd` + `iterations`) plus an in-memory day accounting. "On the invoice is too
// late" — every model call is RESERVED before it forwards and RECONCILED after it returns.
//
// Reserve-then-reconcile: preflight RESERVES a per-call cost CEILING (models.ts estimateCallCeilingUsd
// — output bounded by max_tokens, text input bounded by bytes, image/document input bounded by a
// per-block token floor, so ceiling >= actual) against every cap ATOMICALLY. Because the reservation
// is >= the real cost, a call that would breach a cap is refused UP-FRONT (the safe answer when one
// call could overspend), and meter() only ever reconciles DOWNWARD to the actual — so spend can never
// exceed a cap under any concurrency. A failed/non-billable call releases the reservation. The per-call
// reserve is threaded from preflight → meter/release by the caller (the proxy), so each call reconciles
// against exactly what it reserved.
//
// Concurrency: the per_run + iteration reservation is a single conditional SQL UPDATE libSQL
// serializes as the sole writer; the per_day reservation is a synchronous in-memory check+increment
// (no await between), so JS's cooperative scheduling makes it atomic too.
//
// Day accounting is SPLIT into reserved (in-flight, committed-but-unsettled) + spent (metered today).
// The load-bearing rule that survives the UTC day rollover: OUTSTANDING reservations CARRY FORWARD
// across midnight — only the SETTLED spend resets. A call reserved before midnight settles (meters) on
// the new day and must count against it, so its committed dollars stay in #dayReservedUsd until it
// meters/releases. Zeroing them at rollover was the round-4/round-5 fail-open: it dropped in-flight
// commitments, so new-day calls overfilled the cap before the straddler booked its actual on top
// (per_day $1 settled $1.5 with two new-day calls racing one straddler). Carrying the reservation
// makes the guard see the straddler and refuse the over-cap call — and it aligns the LIVE day total
// with the D5 derivation (both attribute a call to the day it SETTLES, i.e. its meter timestamp).

import type { Budget } from "@maestro/protocol";
import { EVENT_TYPES } from "@maestro/protocol";
import { and, eq, lt, sql } from "drizzle-orm";
import type { IndexDb } from "../db/client";
import { runBudget } from "../db/schema";
import type { BudgetEventSink } from "./events";
import type { SessionContext } from "./tokens";

/** Which cap a refusal hit — the `budget.refused` reason + the child's park classification. */
export type RefusalReason = "per_run" | "per_day" | "iteration_cap";

/**
 * A held reservation — the amount reserved for one in-flight call, threaded from `preflight` to the
 * matching `meter`/`release` so the call reconciles against exactly what it reserved. No day bucket is
 * carried: outstanding reservations follow the runtime across the UTC rollover (they settle on the new
 * day), so a plain amount is sufficient and the round-4 bucket-tag is gone.
 */
export interface Reservation {
  reserveUsd: number;
}

/** The pre-forward verdict. `ok:false` → the proxy answers 402 and the run parks `blocked` (F3.1). */
export type BudgetVerdict =
  | { ok: true; reservation: Reservation }
  | { ok: false; reason: RefusalReason };

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
  /** In-flight reservations (committed-but-unsettled). CARRIES across the UTC rollover — a straddling
   *  call settles on the new day, so its commitment must remain visible to new-day cap checks. */
  #dayReservedUsd = 0;
  /** Metered actual spend for the CURRENT day. Resets at the UTC rollover (yesterday's settled spend
   *  does not count against today's cap). */
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
   *  observability. Reserved carries across the UTC rollover; only settled spend resets, so per_day is
   *  a DAILY cap that still bounds a call straddling midnight. */
  get dayTotalUsd(): number {
    this.#rolloverIfNeeded();
    return this.#dayReservedUsd + this.#daySpentUsd;
  }

  /**
   * Advance the day bucket when the clock crosses into a new UTC day (24/7 runtime, D5). ONLY the
   * settled spend resets; OUTSTANDING reservations carry forward — a call reserved before midnight
   * settles on the new day and must count against it. Dropping them here was the round-4/round-5
   * fail-open seam (a straddler's commitment vanished, letting new-day calls overfill the cap).
   */
  #rolloverIfNeeded(): void {
    const b = dayBucket(this.#now());
    if (b !== this.#dayBucket) {
      this.#dayBucket = b;
      this.#daySpentUsd = 0; // yesterday's settled spend is done; reservations stay (they settle today)
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
        .where(and(...conds))
        .returning({ sessionId: runBudget.sessionId });
      rowsAffected = res.length;
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
    return { ok: true, reservation: { reserveUsd: reserve } };
  }

  /**
   * Reconcile a reservation to the ACTUAL cost of a completed call (HARNESS §3 step 3). Pass the
   * `reservation` the matching preflight returned. Emits `budget.metered` with the real usage. The
   * call SETTLES today: its reservation is released from the in-flight pool and its actual is booked to
   * today's spend — so a call reserved before midnight and metered after correctly counts on the new
   * day (its reservation carried across the rollover, so no slot was ever double-freed).
   */
  async meter(ctx: SessionContext, input: MeterInput, reservation: Reservation): Promise<void> {
    const actual = Number.isFinite(input.usd) && input.usd > 0 ? input.usd : 0;
    // In-memory reconcile FIRST — it can't throw, so a subsequent failure never strands the day
    // reservation (the availability DoS P20 round-3 flagged).
    this.#releaseDayReservation(reservation.reserveUsd); // release the held reservation
    this.#daySpentUsd += actual; // book the actual to today (the settlement day)
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
          spentUsd: sql`max(0, ${runBudget.spentUsd} + ${actual - reservation.reserveUsd})`,
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
   * the `reservation` the matching preflight returned.
   */
  async release(ctx: SessionContext, reservation: Reservation): Promise<void> {
    // In-memory refund FIRST so a DB throw can't strand the day reservation (availability DoS).
    this.#releaseDayReservation(reservation.reserveUsd);
    try {
      await this.#db
        .update(runBudget)
        .set({ spentUsd: sql`max(0, ${runBudget.spentUsd} - ${reservation.reserveUsd})` })
        .where(eq(runBudget.sessionId, ctx.session));
    } catch {
      // cache-only refund; nothing was durably spent (no budget.metered emitted), so nothing to reconcile
    }
  }

  /**
   * Refund an in-flight day reservation — rollover-aware + clamped, the ONE mutation helper so no site
   * drifts. Because outstanding reservations CARRY across the rollover (they are not zeroed), the
   * amount a call reserved is still present when it meters/releases on a later day, so a plain
   * decrement is correct with no bucket bookkeeping.
   */
  #releaseDayReservation(reserveUsd: number): void {
    this.#rolloverIfNeeded();
    this.#dayReservedUsd = Math.max(0, this.#dayReservedUsd - reserveUsd);
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
 * `dayStartMs` (D5). Recomputed at startup and used to seed `BudgetGuard.dayTotalUsd`. A call is
 * attributed to the day it METERED (its event timestamp), which matches the live accounting.
 */
export function deriveDayTotal(metered: readonly MeteredRecord[], dayStartMs: number): number {
  let total = 0;
  for (const m of metered) if (m.ts >= dayStartMs) total += Math.max(0, m.usd);
  return total;
}
