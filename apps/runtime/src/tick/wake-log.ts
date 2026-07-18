// The tick wake log (ORCHESTRATOR §7, §2.7) — a leaf module so both the tick (writes it) and the briefing
// (reads the previous one for continuity) depend on THIS, not on each other. Extracted from tick.ts in
// BRO-1784 s2b so runTick can call computeOrchestratorTick (tick → orchestrator → briefing) without a
// cycle back through tick.ts (briefing now imports the wake log from here, the leaf).

import { EVENT_TYPES, type TickCause } from "@maestro/protocol";
import { desc, eq } from "drizzle-orm";
import type { IndexDb } from "../db/client";
import { event } from "../db/schema";

/** Compact decision/outcome counts a tick records for the board + §2.7 continuity. */
export interface WakeSummary {
  /** work actually started this tick (a real supervisor dispatch, not a plan). */
  dispatched: number;
  /** running sessions nudged this tick (a chat restating the goal). s2b-i: always 0 (nudge = s2b-ii). */
  nudged: number;
  /** running sessions surfaced for the human (stale, and the tick could not or did not revive them). */
  needsHuman: number;
  /** nodes at the human gate the tick surfaced (blocked | review). */
  attention: number;
  /** queue nodes left alone this tick, with a reason. */
  deferred: number;
}

/**
 * A wake-log record — the "why I woke + what I did" the next tick's briefing reads (§2.7). `narrative` and
 * `summary` are OPTIONAL so a slice-1 placeholder `tick.fired` (which recorded only the cause) still parses;
 * a s2b tick records the rendered §7 narrative + the outcome counts.
 */
export interface WakeLog {
  tickId: string;
  cause: TickCause;
  /** epoch ms of the wake. */
  wokeAt: number;
  /** the rendered §7 wake-log narrative (the human-facing text). */
  narrative?: string;
  /** the tick's outcome counts. */
  summary?: WakeSummary;
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
