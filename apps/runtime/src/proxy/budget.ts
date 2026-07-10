// The budget-in-path guard (HARNESS §3, F3.1, AUTONOMY §4). AUTHORITATIVE state is `run_budget`
// (per-session `spent_usd` + `iterations`, atomic single-statement RMW) plus an in-memory day-total
// accumulator (the runtime is single-writer, so an in-process counter is safe and avoids a per-call
// aggregate query on the hot path). "On the invoice is too late" — every model call is gated BEFORE
// it forwards, and metered AFTER it returns.
//
// Concurrency: both writes are single-statement (a conditional UPDATE reserving an iteration; an
// atomic `spent += usd`), which libSQL serializes as the single writer — so N children racing one
// budget never lose an update and never reserve past a cap.

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

export interface BudgetGuardOptions {
  /** Injected clock (epoch ms). Defaults to Date.now; tests pin it. */
  now?: () => number;
  /** Day-total seed — BRO-1814 derives it from `budget.metered` at F9.2 startup (D5) and passes it. */
  dayTotalUsd?: number;
}

export class BudgetGuard {
  readonly #db: IndexDb;
  readonly #sink: BudgetEventSink;
  readonly #now: () => number;
  #dayTotalUsd: number;

  constructor(db: IndexDb, sink: BudgetEventSink, opts: BudgetGuardOptions = {}) {
    this.#db = db;
    this.#sink = sink;
    this.#now = opts.now ?? Date.now;
    this.#dayTotalUsd = opts.dayTotalUsd ?? 0;
  }

  /** The runtime-day total spend so far (workspace scope) — observability + the per-day cap check. */
  get dayTotalUsd(): number {
    return this.#dayTotalUsd;
  }

  /** Ensure a `run_budget` row exists for a session (called at spawn). Idempotent. */
  async open(session: string): Promise<void> {
    await this.#db.insert(runBudget).values({ sessionId: session }).onConflictDoNothing();
  }

  /**
   * Pre-forward guard (HARNESS §3 step 1). Refuses if any cap is hit; otherwise RESERVES one
   * iteration atomically and allows the call. Reserving up-front (rather than counting on response)
   * is what makes the iteration cap hold under concurrency — two racing calls can't both slip past
   * the last slot.
   */
  async preflight(ctx: SessionContext): Promise<BudgetVerdict> {
    const { budget } = ctx;

    // (a) per-day cap — the in-memory accumulator is authoritative for the workspace-day.
    if (budget.per_day_usd !== undefined && this.#dayTotalUsd >= budget.per_day_usd) {
      await this.#refuse(ctx, "per_day");
      return { ok: false, reason: "per_day" };
    }

    // (b) per-run spend + iteration cap — one conditional UPDATE. rowsAffected 0 means a cap blocked
    // the reservation (or the row is missing); read back to name which, so the refusal is precise.
    const conds = [eq(runBudget.sessionId, ctx.session)];
    if (budget.per_run_usd !== undefined) conds.push(lt(runBudget.spentUsd, budget.per_run_usd));
    if (budget.max_iterations !== undefined) {
      conds.push(lt(runBudget.iterations, budget.max_iterations));
    }
    const res = await this.#db
      .update(runBudget)
      .set({ iterations: sql`${runBudget.iterations} + 1`, lastCallAt: this.#now() })
      .where(and(...conds));

    if (res.rowsAffected === 0) {
      const reason = await this.#classifyRunRefusal(ctx.session, budget);
      await this.#refuse(ctx, reason);
      return { ok: false, reason };
    }
    return { ok: true };
  }

  /**
   * Post-response metering (HARNESS §3 step 3). Atomic `spent += usd` so concurrent meters never lose
   * an update; also advances the in-memory day total. Emits `budget.metered`.
   */
  async meter(ctx: SessionContext, input: MeterInput): Promise<void> {
    const usd = Number.isFinite(input.usd) && input.usd > 0 ? input.usd : 0;
    await this.#db
      .update(runBudget)
      .set({ spentUsd: sql`${runBudget.spentUsd} + ${usd}`, lastCallAt: this.#now() })
      .where(eq(runBudget.sessionId, ctx.session));
    this.#dayTotalUsd += usd;
    await this.#sink.emit(ctx.runDir, {
      ts: new Date(this.#now()).toISOString(),
      actor: "system",
      type: EVENT_TYPES.BUDGET_METERED,
      payload: { session: ctx.session, usd, tokens: input.tokens ?? null },
    });
  }

  /** Name the run-scoped cap that blocked a reservation (per_run before iteration_cap). */
  async #classifyRunRefusal(session: string, budget: Budget): Promise<RefusalReason> {
    const [row] = await this.#db
      .select({ spentUsd: runBudget.spentUsd, iterations: runBudget.iterations })
      .from(runBudget)
      .where(eq(runBudget.sessionId, session));
    // No row (spawn skipped `open`) is treated as an iteration refusal — the guard fails closed.
    if (row === undefined) return "iteration_cap";
    if (budget.per_run_usd !== undefined && row.spentUsd >= budget.per_run_usd) return "per_run";
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
