// The F6 tick (BRO-1772) — the orchestrator's wake. This is SLICE 1: the tick ENGINE (lease +
// coalescing, ORCHESTRATOR §8 / F6.2) + the durable WAKE LOG (§7) — the record of "why I woke" that
// the next tick reads for continuity. It "fires from the index, not from the tick" the same way the
// scheduler does: a wake cause (a schedule firing = `interval`, a worker returning, a user message)
// runs a tick, and the tick narrates it.
//
// COALESCING (§8): a global `tick` lease means ONE tick at a time. A wake that arrives while a tick is
// in flight coalesces — it emits `tick.skipped{cause,reason}` and drops (a hook storm produces one
// tick; Loop 4 watches for chronic skips = interval too tight). The lease is released when the tick
// settles, so the next wake ticks; the TTL is only a crash safety net.
//
// DEFERRED to slice 2 / BRO-1784 (the orchestrator agent): assembling the 7-section BRIEFING (§2) and
// SPAWNING the orchestrator session that fills the rich wake-log narrative + mirrors it to
// routines/maestro/runs/run-<tick-id>/ (FS-durable). Slice 1 records the cause as a synthetic
// `tick.fired` — durable in the index, streamed to the wake-log UI, and readable by the next tick.

import { randomUUID } from "node:crypto";
import { EVENT_TYPES, type TickCause } from "@maestro/protocol";
import { and, desc, eq, lt } from "drizzle-orm";
import type { IndexDb } from "../db/client";
import { event, lease } from "../db/schema";

/** The global single-tick lease key (F6.2 — one tick at a time). */
const TICK_LEASE_KEY = "tick";
/** Tick lease TTL — the recovery window for a lease STRANDED by a crash (a tick that dies holding it).
 *  A live tick releases on settle; a stranded one is taken over by the next wake after this TTL (no
 *  restart needed — the acquire is expiry-aware, below). Must exceed the max tick duration so a slow
 *  live tick is never stolen mid-flight; slice-1 ticks are sub-ms, so 5m is ample. Slice 2's long
 *  orchestrator ticks will renew the lease (heartbeat) rather than widen this — tracked with BRO-1784. */
const TICK_LEASE_TTL_MS = 5 * 60 * 1000;

/** The outcome of a tick attempt. */
export interface TickResult {
  /** true when the wake coalesced into an in-flight tick (lease held) and this tick did not run. */
  skipped: boolean;
  cause: TickCause;
  /** present when the tick ran (not skipped) — the wake-log record's id. */
  tickId?: string;
  /** present when skipped — why (e.g. `tick_in_flight`). */
  reason?: string;
}

/** A wake-log record — the "why I woke" the next tick's briefing reads (§2.7). */
export interface WakeLog {
  tickId: string;
  cause: TickCause;
  /** epoch ms of the wake. */
  wokeAt: number;
}

/**
 * Run one tick for `cause`, coalescing concurrent wakes into a single tick (F6.2). Returns whether it
 * ran or coalesced. Pure over an injected `now` (+ an injectable `tickId` for deterministic tests) — no
 * ambient clock — so the coalescing + wake-log properties are unit-testable by replay over the index.
 */
export async function runTick(
  db: IndexDb,
  cause: TickCause,
  now: number,
  tickId: string = randomUUID(),
): Promise<TickResult> {
  // F6.2 — acquire the global tick lease, taking over an EXPIRED one. A lease stranded by a crash
  // self-heals on the next wake after the TTL (no restart needed) — the expiry-blind onConflictDoNothing
  // this replaces would WEDGE the tick, the exact sibling failure the scheduler's lease was removed for
  // (P20 BRO-1749). `tickId` is the fencing token: the release (finally, below) deletes only OUR lease,
  // so a superseded tick's late release can never steal a live one. `.returning()` yields a row iff we
  // INSERTED (no lease) or UPDATED (took over an expired one); a held+unexpired lease → 0 rows → coalesce.
  const won = await db
    .insert(lease)
    .values({
      key: TICK_LEASE_KEY,
      holder: tickId,
      acquiredAt: now,
      expiresAt: now + TICK_LEASE_TTL_MS,
    })
    .onConflictDoUpdate({
      target: lease.key,
      set: { holder: tickId, acquiredAt: now, expiresAt: now + TICK_LEASE_TTL_MS },
      setWhere: lt(lease.expiresAt, now), // steal ONLY a dead (expired) lease, never a live tick's
    })
    .returning({ holder: lease.holder });
  if (won.length === 0) {
    // Coalesced — surface it (Loop 4 watches for chronic skips, §8).
    await db.insert(event).values({
      sessionId: null,
      ts: now,
      actor: "system",
      type: EVENT_TYPES.TICK_SKIPPED,
      payload: JSON.stringify({ cause, reason: "tick_in_flight" }),
    });
    return { skipped: true, cause, reason: "tick_in_flight" };
  }
  try {
    // §7 — the durable wake log: a synthetic `tick.fired` the next tick reads (readLastWakeLog). The
    // briefing (§2) + the orchestrator session that fills the rich narrative are slice 2 / BRO-1784.
    await db.insert(event).values({
      sessionId: null,
      ts: now,
      actor: "system",
      type: EVENT_TYPES.TICK_FIRED,
      payload: JSON.stringify({ tickId, cause, wokeAt: now } satisfies WakeLog),
    });
    return { skipped: false, cause, tickId };
  } finally {
    // Fenced release: delete ONLY the lease WE still hold (key + our tickId). If a later tick already
    // took over an expired lease, its holder differs → this is a no-op, so we never steal a live tick's
    // lease. Runs even if the body threw — a live tick never strands the lease (only a crash can).
    await db.delete(lease).where(and(eq(lease.key, TICK_LEASE_KEY), eq(lease.holder, tickId)));
  }
}

/** The most-recent wake-log record (the last `tick.fired`), or null — §2.7 of the next tick's briefing. */
export async function readLastWakeLog(db: IndexDb): Promise<WakeLog | null> {
  const [row] = await db
    .select()
    .from(event)
    .where(eq(event.type, EVENT_TYPES.TICK_FIRED))
    .orderBy(desc(event.seq))
    .limit(1);
  if (!row?.payload) return null;
  return JSON.parse(row.payload) as WakeLog;
}
