// The tick briefing (BRO-1772 slice 2, ORCHESTRATOR §2) — the supervisor assembles a CURATED, bounded,
// fresh view of the index into the tick's context, so the orchestrator (BRO-1784) reasons over a
// briefing, not raw board queries. Seven sections: cause · attention · active runs · queue · bench ·
// ledger · last wake log. Read-only + pure over an injected `now` — a snapshot test replays it over a
// seeded index. The orchestrator SESSION that consumes this briefing (formatting it into the prompt) is
// BRO-1784; slice 2 owns the assembly + its shape.

import {
  type Budget,
  contractRunnableReason,
  type Done,
  type TickCause,
  WK_GROUP_ORDER,
} from "@maestro/protocol";
import { and, eq, inArray, isNull, max, sql } from "drizzle-orm";
import type { IndexDb } from "../db/client";
import { event, node, runBudget, schedule, session } from "../db/schema";
import { deriveDayTotalUsdFromIndex } from "../ledger/day-total";
import { readLastWakeLog, type WakeLog } from "./tick";

/** Concurrency cap (ORCHESTRATOR §3 — "default 3, runtime config"). No config field yet; a constant
 *  until one lands. The ledger surfaces `activeRuns` vs this so a tick knows if it may dispatch. */
export const DEFAULT_CONCURRENCY_CAP = 3;
/** §2 "curated, BOUNDED": cap each list section so a flooded board never buries the orchestrator's small
 *  budget. The most-attention items survive the cap (the sort runs before the slice). */
export const BRIEFING_SECTION_CAP = 50;

/** §2.2 / §2.4 — a work node the human (attention) or the loop (queue) should see, with its age. */
export interface BriefingNode {
  nodeId: string;
  path: string;
  title: string | null;
  state: string;
  /** now − updatedAt (ms) — how long it has sat in this state. */
  ageMs: number;
}

/** §2.4 — a queue node (proposed | triggered) plus its ORCHESTRATOR §3.3 dispatch-runnability, so the
 *  policy reasons over the briefing without re-reading the board. `runnable` gates whether a `proposed`
 *  node may be dispatched (triggered nodes dispatch first regardless); `notRunnableReason` feeds the wake
 *  log's "left it queued, why" line. Computed from the node's frontmatter contract at assembly. */
export interface BriefingQueueNode extends BriefingNode {
  runnable: boolean;
  notRunnableReason: string | null;
}

/** §2.3 — a live run, with the staleness + spend signals a tick reasons over. */
export interface BriefingRun {
  sessionId: string;
  nodeId: string;
  branch: string;
  iterations: number;
  spentUsd: number;
  /** now − the session's LAST event ts (ms) — the staleness signal (§2.3). Falls back to `startedAt`
   *  for a run that has not emitted an event yet. NOT `session.updatedAt` (which only moves on a status
   *  transition, so a busy running session would falsely read as stale — the §3.1 nudge would misfire). */
  lastEventAgeMs: number;
}

/** §2.5 — an enabled schedule on the bench, with its next fire. */
export interface BriefingSchedule {
  scheduleId: string;
  nodeId: string;
  triggerKind: string;
  nextFireAt: number | null;
}

/** §2.6 — day spend across all runs + concurrency in use vs the cap. */
export interface BriefingLedger {
  daySpentUsd: number;
  /** the day budget §3.1's "≥90% spent → dispatch nothing" rule divides by, or null when unconfigured
   *  (no global-day-budget config field exists yet — the operand §2.6 names, surfaced honestly as null). */
  dayBudgetUsd: number | null;
  activeRuns: number;
  concurrencyCap: number;
}

/** The assembled tick briefing (§2). */
export interface Briefing {
  cause: TickCause;
  attention: BriefingNode[];
  activeRuns: BriefingRun[];
  queue: BriefingQueueNode[];
  bench: BriefingSchedule[];
  ledger: BriefingLedger;
  lastWakeLog: WakeLog | null;
}

/** Optional inputs the runtime supplies from config (none have a config field yet — defaults apply). */
export interface BriefingOptions {
  /** a global day budget (USD) for the ledger's §3.1 denominator; default null (unconfigured). */
  dayBudgetUsd?: number | null;
}

/** Attention-axis rank of a state (D-ORDER / WK_GROUP_ORDER) — earlier = more attention. */
const rank = (state: string): number => {
  const i = (WK_GROUP_ORDER as readonly string[]).indexOf(state);
  return i === -1 ? WK_GROUP_ORDER.length : i;
};

/** §2.2/§2.4 — order by attention rank, then oldest-first (largest age = most stale) within a rank. */
function byAttentionThenAge(a: BriefingNode, b: BriefingNode): number {
  return rank(a.state) - rank(b.state) || b.ageMs - a.ageMs;
}

/**
 * Assemble the tick briefing for `cause` at `now`. Reuses the shared attention axis (WK_GROUP_ORDER)
 * and the canonical day-total derivation — it does not re-derive either. Live rows only
 * (`deletedAt IS NULL`), so a tombstoned node/session never reaches the orchestrator. Each list section
 * is capped (BRIEFING_SECTION_CAP) after ordering, so the briefing stays bounded (§2).
 */
export async function assembleBriefing(
  db: IndexDb,
  cause: TickCause,
  now: number,
  opts: BriefingOptions = {},
): Promise<Briefing> {
  const toNode = (n: typeof node.$inferSelect): BriefingNode => ({
    nodeId: n.id,
    path: n.path,
    title: n.title,
    state: n.state,
    ageMs: now - n.updatedAt,
  });
  // Malformed contract JSON in the index shouldn't happen (the scanner writes it), but a parse failure
  // must not crash the tick — treat an unreadable contract as absent (→ not runnable, surfaced honestly).
  const safeParse = <T>(json: string | null): T | undefined => {
    if (!json) return undefined;
    try {
      return JSON.parse(json) as T;
    } catch {
      return undefined;
    }
  };
  const toQueueNode = (n: typeof node.$inferSelect): BriefingQueueNode => {
    const reason = contractRunnableReason(
      safeParse<Done>(n.doneJson),
      safeParse<Budget>(n.budgetJson),
      n.gate,
    );
    return { ...toNode(n), runnable: reason === null, notRunnableReason: reason };
  };

  // §2.2 attention — every node the human should see (blocked | review), attention-ordered, capped.
  const attentionRows = await db
    .select()
    .from(node)
    .where(and(isNull(node.deletedAt), inArray(node.state, ["review", "blocked"])));
  const attention = attentionRows
    .map(toNode)
    .sort(byAttentionThenAge)
    .slice(0, BRIEFING_SECTION_CAP);

  // §2.4 queue — proposed | triggered work the loop may dispatch, attention-ordered (triggered first).
  const queueRows = await db
    .select()
    .from(node)
    .where(and(isNull(node.deletedAt), inArray(node.state, ["triggered", "proposed"])));
  const queue = queueRows.map(toQueueNode).sort(byAttentionThenAge).slice(0, BRIEFING_SECTION_CAP);

  // §2.3 active runs — running sessions with iteration/spend (run_budget) + LAST-EVENT staleness.
  const runningRows = await db
    .select()
    .from(session)
    .where(and(isNull(session.deletedAt), eq(session.status, "running")));
  const runningIds = runningRows.map((s) => s.id);
  const [budgets, lastEvents] = runningIds.length
    ? await Promise.all([
        db.select().from(runBudget).where(inArray(runBudget.sessionId, runningIds)),
        db
          .select({ sid: event.sessionId, lastTs: max(event.ts) })
          .from(event)
          .where(inArray(event.sessionId, runningIds))
          .groupBy(event.sessionId),
      ])
    : [[], []];
  const budgetBySession = new Map(budgets.map((b) => [b.sessionId, b]));
  const lastTsBySession = new Map(lastEvents.map((r) => [r.sid, r.lastTs]));
  const activeRuns = runningRows.map((s): BriefingRun => {
    const b = budgetBySession.get(s.id);
    // last EVENT ts (any type), else the run's start — never `updatedAt` (status-transition-only).
    const lastTs = lastTsBySession.get(s.id) ?? s.startedAt;
    return {
      sessionId: s.id,
      nodeId: s.nodeId,
      branch: s.branch,
      iterations: b?.iterations ?? 0,
      spentUsd: b?.spentUsd ?? 0,
      lastEventAgeMs: now - lastTs,
    };
  });

  // §2.5 bench — enabled schedules + next fires, SOONEST first. `next_fire_at IS NULL` sorts LAST (a
  // fired one-shot/cron sits enabled with a null next fire — SQLite's default NULLS FIRST would wrongly
  // rank it ahead of a due heartbeat).
  const scheduleRows = await db
    .select()
    .from(schedule)
    .where(and(isNull(schedule.deletedAt), eq(schedule.enabled, true)))
    .orderBy(sql`${schedule.nextFireAt} is null, ${schedule.nextFireAt} asc`)
    .limit(BRIEFING_SECTION_CAP);
  const bench = scheduleRows.map(
    (s): BriefingSchedule => ({
      scheduleId: s.id,
      nodeId: s.nodeId,
      triggerKind: s.triggerKind,
      nextFireAt: s.nextFireAt,
    }),
  );

  // §2.6 ledger — day spend (canonical derivation) vs the day budget + concurrency in use vs the cap.
  const daySpentUsd = await deriveDayTotalUsdFromIndex(db, now);
  const ledger: BriefingLedger = {
    daySpentUsd,
    dayBudgetUsd: opts.dayBudgetUsd ?? null,
    activeRuns: activeRuns.length,
    concurrencyCap: DEFAULT_CONCURRENCY_CAP,
  };

  // §2.7 last wake log — the tick's previous narrative (continuity; slice 1's readLastWakeLog).
  const lastWakeLog = await readLastWakeLog(db);

  return { cause, attention, activeRuns, queue, bench, ledger, lastWakeLog };
}
