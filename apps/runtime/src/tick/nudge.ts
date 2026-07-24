// The tick's NUDGE (ORCHESTRATOR §3.1 + §4, BRO-1945 / s2b-ii) — the task-drift defense. A running
// session silent past the staleness threshold gets ONE chat message restating its goal, routed into the
// live child's stdin (the F10 control channel the chat endpoint uses). Still silent after that nudge →
// the tick recommends the human look; it cannot kill without a grant (§5).
//
// This module owns BOTH halves of the §3.1 escalation contract that BRO-1784 s2b flagged in policy.ts:
//
//  (a) A nudge WRITES an event (`run.nudged`, actor `system`) on the session's own timeline, so the
//      session's `max(event.ts)` moves. The briefing's `lastEventAgeMs` therefore drops below the
//      threshold and a nudged run leaves the stale set NATURALLY — it re-enters only after a fresh
//      silent window, which is exactly when a second look is warranted.
//  (b) `deriveNudgedSessionIds` scopes the "already nudged" set to the CURRENT stale window: a session
//      counts as nudged only while the nudge is still its LAST word. Any worker activity after the
//      nudge means the nudge worked and the run revived; if it later goes quiet again it earns a fresh
//      FIRST nudge, not an escalation. "Ever nudged" would escalate a run the nudge demonstrably
//      revived — the wrong signal, and the reason the derivation is not a boolean flag on the session.
//
// It is deliberately split from tick.ts: the derivation is a pure index read (unit-testable over a
// seeded `:memory:` index), and the nudger takes a NARROW `live()` port rather than the supervisor, so
// the tick's import graph never drags the supervisor/proxy/sandbox tree in (the leaf-split discipline
// wake-log.ts already follows).

import { EVENT_TYPES } from "@maestro/protocol";
import { and, eq, inArray, max, ne } from "drizzle-orm";
import type { IndexDb } from "../db/client";
import { event, node } from "../db/schema";
import { type BudgetEventSink, fsJournalSink } from "../proxy/events";

/** A stale run the tick decided to nudge (§3.1) — the seam's input. */
export interface NudgeTarget {
  sessionId: string;
  nodeId: string;
  /** how long the run has been silent (ms) — the honest "you have been quiet N minutes" framing. */
  ageMs: number;
  /** the TICK's clock (epoch ms). Passed in so the nudge record carries the tick's `now`, never
   *  ambient time — the same discipline the briefing and the wake log follow. */
  at: number;
}

/** The tick's nudge seam: route one goal-restating chat into a stale run. Resolves true IFF the nudge
 *  actually went out (the run was live and the record landed) — the tick narrates "Nudged" only then. */
export type Nudge = (target: NudgeTarget) => Promise<boolean>;

/** The live run's control channel + receipts dir — the narrow slice of a supervisor `RunEntry` a nudge
 *  needs. `chat` is the F10 stdin control verb; `runDir` is `runs/run-<id>/`, where session.jsonl lives. */
export interface NudgeChannel {
  chat(message: unknown): Promise<void>;
  runDir: string;
}

export interface NudgerDeps {
  db: IndexDb;
  /** The live run's channel, or null when the run is not live (already reaped, or never in this
   *  process). A run that is not live CANNOT be nudged: there is no child to speak to, so the tick
   *  surfaces it to the human instead of claiming a nudge that never happened. */
  live(sessionId: string): NudgeChannel | null;
  /** Durable journal sink for the `run.nudged` record; defaults to the FS `session.jsonl` sink
   *  (D-DURABILITY: the FS journal is canonical, the `event` table is its projection). */
  sink?: BudgetEventSink;
}

/**
 * The nudge text (§3.1 "one chat message restating its goal"). Plain voice, no em dashes, no enum names
 * (§6/CLAUDE.md §Voice) — a person may read this turn in the session's chat.
 *
 * It restates the GOAL and nothing else. ORCHESTRATOR §6 hard line: the orchestrator never restates or
 * reinterprets a node's `done:` contract, so the done.check is deliberately absent from this copy.
 */
export function renderNudgeText(
  work: { title: string | null; path: string },
  ageMs: number,
): string {
  const m = Math.round(ageMs / 60000);
  const quiet = m <= 0 ? "under a minute" : m === 1 ? "1 minute" : `${m} minutes`;
  const goal = work.title?.trim() ? work.title.trim() : work.path;
  return [
    `Checking in: this run has been quiet for ${quiet}.`,
    `The goal is still: ${goal} (${work.path}).`,
    "Keep going if you are still working, or say what is in the way.",
  ].join("\n");
}

/**
 * Build the tick's nudge seam. One nudge = (1) a goal-restating chat into the live child's stdin, then
 * (2) a durable `run.nudged` record on the session's timeline (FS journal first, then the index row —
 * the same order the harness event writer uses, so the projection never leads its source).
 *
 * Ordering is deliberate: the chat goes FIRST. If the record write then fails we return false, so the
 * wake log says "worth a look" rather than "Nudged" — a nudge whose record did not land is one the next
 * tick cannot reason about, and over-claiming it would be the forward-honesty violation. The cost of
 * that conservatism is a possible duplicate nudge next tick, which is harmless (one more chat line).
 */
export function createNudger(deps: NudgerDeps): Nudge {
  const sink = deps.sink ?? fsJournalSink();
  return async (target: NudgeTarget): Promise<boolean> => {
    const channel = deps.live(target.sessionId);
    if (!channel) return false; // not live → nothing to speak to; the tick surfaces it to the human

    const [work] = await deps.db
      .select({ title: node.title, path: node.path })
      .from(node)
      .where(eq(node.id, target.nodeId))
      .limit(1);
    // A run whose node row vanished (tombstoned mid-flight) still gets a goal line, from the id.
    const text = renderNudgeText(work ?? { title: null, path: target.nodeId }, target.ageMs);

    // (1) Route it in. `control.chat` is best-effort by contract (writing to a dead child's stdin is
    // swallowed, never thrown — the reap path owns lifecycle), so liveness in the registry is the
    // strongest delivery evidence available; that is what `live()` returning a channel means.
    // `role: "user"` is the WIRE shape, not a claim of authorship: the child's `chat` control line
    // folds a UIMessage into the conversation as a user turn, and that is the only turn kind it
    // accepts. Who actually sent it is recorded honestly below (`run.nudged`, actor `system`), and
    // the copy never speaks as the human.
    await channel.chat({
      id: `nudge-${target.sessionId}-${target.at}`,
      role: "user",
      parts: [{ type: "text", text }],
    });

    // (2) Record it — contract (a). Without this the session's max(event.ts) never moves, the run stays
    // stale forever, and the next tick would nudge it again every tick.
    try {
      const payload = { reason: "stale", ageMs: target.ageMs, text };
      await sink.emit(channel.runDir, {
        ts: new Date(target.at).toISOString(),
        actor: "system",
        type: EVENT_TYPES.RUN_NUDGED,
        payload,
      });
      await deps.db.insert(event).values({
        sessionId: target.sessionId,
        ts: target.at,
        actor: "system",
        type: EVENT_TYPES.RUN_NUDGED,
        payload: JSON.stringify(payload),
      });
    } catch (err) {
      console.warn(
        `maestro tick · nudge sent but not recorded (${target.sessionId}): ${(err as Error).message}`,
      );
      return false;
    }
    return true;
  };
}

/**
 * The §3.1 "already nudged in the CURRENT stale window" set — contract (b).
 *
 * A session is in the set iff its last `run.nudged` is still its LAST event: the nudge spoke, and
 * nothing on that run has spoken since. Any other event after the nudge (a beat, a tool call, an
 * utterance) means the run revived, so it drops out and earns a fresh first nudge if it goes quiet
 * again. A tie (activity at the same millisecond as the nudge) reads as REVIVED — the tick will not
 * claim "even after a nudge" unless the nudge is demonstrably the last word.
 *
 * WHY "any non-nudge event" is a sound proxy for "the worker spoke": the caller only ever asks about
 * sessions the briefing reports as still `running` (§2.3). The `system`-actor events that are NOT
 * worker activity — `run.hung`, `run.failed`, `run.killed`, `gate.*` — all accompany the session
 * leaving `running` (the supervisor parks or reaps it as it writes them), so they are never in this
 * population; and the synthetics that could be (`node.updated`, `schedule.fired`, `tick.*`) carry a
 * NULL sessionId, which `inArray` excludes. What remains on a live run is the child's own stream plus
 * its budget accounting: genuine activity. If a future event type breaks that property, this filter
 * is where it must be narrowed.
 *
 * Pure read; returns an empty set for an empty input (no query issued).
 */
export async function deriveNudgedSessionIds(
  db: IndexDb,
  sessionIds: readonly string[],
): Promise<Set<string>> {
  const ids = [...new Set(sessionIds)];
  if (ids.length === 0) return new Set();
  const [nudges, activity] = await Promise.all([
    db
      .select({ sid: event.sessionId, lastTs: max(event.ts) })
      .from(event)
      .where(and(inArray(event.sessionId, ids), eq(event.type, EVENT_TYPES.RUN_NUDGED)))
      .groupBy(event.sessionId),
    db
      .select({ sid: event.sessionId, lastTs: max(event.ts) })
      .from(event)
      .where(and(inArray(event.sessionId, ids), ne(event.type, EVENT_TYPES.RUN_NUDGED)))
      .groupBy(event.sessionId),
  ]);
  const lastActivity = new Map(activity.map((r) => [r.sid, r.lastTs]));
  const out = new Set<string>();
  for (const n of nudges) {
    if (n.sid == null || n.lastTs == null) continue;
    const worker = lastActivity.get(n.sid);
    if (worker == null || n.lastTs > worker) out.add(n.sid);
  }
  return out;
}
