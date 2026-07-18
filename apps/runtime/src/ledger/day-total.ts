// day-total.ts — today's metered spend, derived from the DURABLE budget events (BRO-1784 slice 2: split
// out of the heavy dispatch.ts so the tick graph — briefing.ts (§2.6 ledger) → this — does NOT drag the
// supervisor / proxy / sandbox dependency graph in. The logic is unchanged; only its home moved. Both the
// dispatch mount (per-day cap seed) and the tick briefing read the same derivation, so it lives on a leaf.

import { EVENT_TYPES } from "@maestro/protocol";
import { eq } from "drizzle-orm";
import { parsePayload } from "../api/event-projection";
import type { IndexDb } from "../db/client";
import { event } from "../db/schema";
import { deriveDayTotal } from "../proxy/budget";

/** UTC day length in ms — the BudgetGuard's day bucket (proxy/budget.ts `dayBucket`). */
export const DAY_MS = 86_400_000;

/**
 * Seed today's metered spend from the DURABLE budget events so the per-day cap is NOT reset to zero on
 * every runtime restart (BRO-1822 latent gap; `deriveDayTotal` was documented as this seed but never
 * wired). By mount time, F9 recovery (index.ts, before the mount) has replayed every journal-only
 * `budget.metered` into the index, so reading them here is sound (they are absent from the index before
 * recovery — the BRO-1814 replay is what makes this correct). Best-effort: a read failure just starts the
 * day total at 0 (a fresh, never-blocking cap), never throws into the mount / tick.
 */
export async function deriveDayTotalUsdFromIndex(db: IndexDb, nowMs: number): Promise<number> {
  const dayStartMs = Math.floor(nowMs / DAY_MS) * DAY_MS;
  try {
    const rows = await db.select().from(event).where(eq(event.type, EVENT_TYPES.BUDGET_METERED));
    const metered = rows.map((r) => {
      const p = parsePayload(r.payload) as { session?: unknown; usd?: unknown } | undefined;
      return {
        session: typeof p?.session === "string" ? p.session : "",
        usd: typeof p?.usd === "number" ? p.usd : 0,
        ts: r.ts,
      };
    });
    return deriveDayTotal(metered, dayStartMs);
  } catch {
    return 0;
  }
}
