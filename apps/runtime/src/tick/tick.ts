// The F6 tick (BRO-1772 + BRO-1784) — the orchestrator's wake. The tick ENGINE (lease + coalescing,
// ORCHESTRATOR §8 / F6.2) runs the §3 decision policy over the live index and NARRATES + ACTS on it:
//
//  - SLICE 1 (BRO-1772): the lease/coalescing engine + the durable `tick.fired` wake-log record.
//  - SLICE 2 (BRO-1784): the deterministic decision policy (computeOrchestratorTick — brain, read-only).
//  - SLICE 2b (this file, BRO-1784): the tick EXECUTES what the policy decides — it DISPATCHES runnable
//    work through the same supervisor the chat path uses, and records the honest §7 narrative of what it
//    ACTUALLY did (never a plan it didn't run). The BRO-1944 boundary is precise: this tick's BRAIN is the
//    deterministic computeOrchestratorTick (no LLM decides), and its dispatch is an in-process supervisor
//    call (the runtime's own code, an allowed agent verb) — so no UNCONFINED orchestrator model decides or
//    issues intents. (Dispatch does spawn a WORKER child, but that is the pre-existing, confined worker
//    path, not the orchestrator; a live orchestrator SESSION issuing gate verbs stays gated behind BRO-1944.)
//
// DEFERRED to s2b-ii: NUDGE execution (a chat into a stale session) + the `nudgedSessionIds` derivation;
// this slice SURFACES a stale run to the human instead. Also deferred: the FS mirror of the wake log to
// routines/maestro/runs/run-<tick-id>/ (the index `tick.fired` event is already the durable record).
//
// COALESCING (§8): a global `tick` lease means ONE tick at a time. A wake that arrives while a tick is
// in flight coalesces — it emits `tick.skipped{cause,reason}` and drops (a hook storm produces one
// tick; Loop 4 watches for chronic skips = interval too tight). The lease is released when the tick
// settles, so the next wake ticks; the TTL is only a crash safety net.

import { randomUUID } from "node:crypto";
import { EVENT_TYPES, type TickCause } from "@maestro/protocol";
import { and, eq, lt } from "drizzle-orm";
import type { IndexDb } from "../db/client";
import { event, lease } from "../db/schema";
import {
  computeOrchestratorTick,
  type OrchestratorTickOptions,
} from "../orchestrator/orchestrator";
import {
  type DeferDecision,
  type DispatchDecision,
  type OrchestratorDecision,
  renderWakeLog,
} from "../orchestrator/policy";
import type { WakeLog, WakeSummary } from "./wake-log";

// Re-exported so existing importers (`./tick`) keep resolving after the leaf split (BRO-1784 s2b).
export { readLastWakeLog, type WakeLog, type WakeSummary } from "./wake-log";

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

/** The seams the tick uses to ACT on its decisions (BRO-1784 s2b). All optional — a read-only runtime
 *  (no dispatch mounted) narrates without starting work, honestly ("nothing to run it yet"). */
export interface RunTickOptions {
  /** start a node's run — the same supervisor dispatch the chat path uses; resolves true iff it started.
   *  Absent → the runtime is read-only (no model loop), so the tick surfaces runnable work but starts none. */
  dispatch?: (nodeId: string) => Promise<boolean>;
  /** passed through to computeOrchestratorTick (day budget, staleness threshold, nudgedSessionIds). */
  orchestrator?: OrchestratorTickOptions;
}

/**
 * Run one tick for `cause`, coalescing concurrent wakes into a single tick (F6.2). Acquires the tick lease
 * FIRST (cheap), then — only as the winner — assembles the briefing, runs the §3 policy, DISPATCHES the
 * runnable work it decided (via `opts.dispatch`), and records the honest §7 narrative of what it actually
 * did. Pure over an injected `now` + `tickId` so the engine properties stay unit-testable by replay.
 */
export async function runTick(
  db: IndexDb,
  cause: TickCause,
  now: number,
  tickId: string = randomUUID(),
  opts: RunTickOptions = {},
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
    const { narrative, summary } = await narrateAndAct(db, cause, now, opts);
    // §7 — the durable wake log: a synthetic `tick.fired` the next tick reads (readLastWakeLog), streamed
    // to the wake-log UI, and the §2.7 continuity for the next tick.
    await db.insert(event).values({
      sessionId: null,
      ts: now,
      actor: "system",
      type: EVENT_TYPES.TICK_FIRED,
      payload: JSON.stringify({ tickId, cause, wokeAt: now, narrative, summary } satisfies WakeLog),
    });
    return { skipped: false, cause, tickId };
  } finally {
    // Fenced release: delete ONLY the lease WE still hold (key + our tickId). If a later tick already
    // took over an expired lease, its holder differs → this is a no-op, so we never steal a live tick's
    // lease. Runs even if the body threw — a live tick never strands the lease (only a crash can).
    await db.delete(lease).where(and(eq(lease.key, TICK_LEASE_KEY), eq(lease.holder, tickId)));
  }
}

/** All-zero counts — the empty tick + the §8 degraded ("couldn't narrate") record. */
const ZERO_SUMMARY: WakeSummary = {
  dispatched: 0,
  nudged: 0,
  needsHuman: 0,
  attention: 0,
  deferred: 0,
};

/**
 * Run the §3 policy, ACT on it (dispatch runnable work via `opts.dispatch`), and render the honest §7
 * narrative of what actually happened. If the briefing/policy throws, degrade per §8 ("runs still fire,
 * nobody narrates"): still record the wake with a plain note rather than silence (§7: silence reads as
 * breakage) — the run loop is untouched, only this tick's narration is lost.
 */
async function narrateAndAct(
  db: IndexDb,
  cause: TickCause,
  now: number,
  opts: RunTickOptions,
): Promise<{ narrative: string; summary: WakeSummary }> {
  let decision: OrchestratorDecision;
  try {
    decision = await computeOrchestratorTick(db, cause, now, opts.orchestrator);
  } catch (err) {
    // §8: the run loop is untouched; only this tick's narration is lost. LOG the cause — a SYSTEMATIC
    // policy/briefing bug would otherwise degrade every tick to this line forever with no diagnostic
    // (consistent with index.ts's other degradations, which all console.warn). Still record the wake
    // (§7: silence reads as breakage) rather than nothing.
    console.warn(`maestro tick · could not narrate (${cause}): ${(err as Error).message}`);
    return {
      narrative:
        "Woke up, but could not read the board this tick. Work still runs; I could not narrate it.",
      summary: ZERO_SUMMARY,
    };
  }

  // EXECUTE the dispatch decisions (s2b-i). Start runnable work through the SAME supervisor the chat path
  // uses — an in-process supervisor call (the runtime's own code, an allowed agent verb), so it is safe
  // without the BRO-1944 confinement (that gates a live orchestrator MODEL, not this deterministic tick).
  // Without a dispatch seam the runtime is read-only: it narrates but starts none. NOTE (accepted, minor):
  // a slot the policy reserved for a dispatch that then FAILS is not reclaimed within this tick — a node
  // deferred "at concurrency cap" may see a freed slot only on the next tick. Never a starvation or an
  // honesty bug (the deferred node dispatches next tick); just a conservative deferral reason.
  const started: DispatchDecision[] = [];
  const notStarted: DeferDecision[] = [];
  for (const wanted of decision.dispatches) {
    if (!opts.dispatch) {
      notStarted.push({ nodeId: wanted.nodeId, reason: "no runner available" });
      continue;
    }
    const ok = await opts.dispatch(wanted.nodeId).catch(() => false);
    if (ok) started.push(wanted);
    else notStarted.push({ nodeId: wanted.nodeId, reason: "could not start" });
  }

  // Reconcile the PLAN into what ACTUALLY happened, so the narrative never claims an unrun action
  // (forward-honesty): dispatched = only what started; the rest join the deferrals with why; every stale
  // run the tick did NOT nudge (nudge = s2b-ii) surfaces to the human (afterNudge: false — no nudge claimed).
  const effective: OrchestratorDecision = {
    ...decision,
    dispatches: started,
    deferrals: [...decision.deferrals, ...notStarted],
    needsHuman: [
      ...decision.needsHuman,
      ...decision.nudges.map((n) => ({ ...n, afterNudge: false })),
    ],
    nudges: [],
  };
  return {
    narrative: renderWakeLog(effective),
    summary: {
      dispatched: effective.dispatches.length,
      nudged: effective.nudges.length,
      needsHuman: effective.needsHuman.length,
      attention: effective.attention.length,
      deferred: effective.deferrals.length,
    },
  };
}
