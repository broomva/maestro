// The F7 scheduler (BRO-1749) — the runtime's trigger engine. It "fires from the index,
// not from ticks" (ORCHESTRATOR §7): a bounded poll finds schedules whose `next_fire_at <= now`,
// fires each EXACTLY ONCE, emits a `schedule.fired` synthetic event (the durable fire signal the
// F6 tick consumes to dispatch, BRO-1772), and advances `next_fire_at`. The dispatch-on-fire +
// routine→Standing settle is the tick's job; this module owns the fire and its exactly-once
// guarantee.
//
// EXACTLY-ONCE — the CAS-advance IS the claim (no separate lease):
//   1. CAS-ADVANCE `next_fire_at` OFF the due value (`WHERE next_fire_at = <due>`, returning the
//      row). This one atomic UPDATE is the idempotency claim, keyed on the fire instant:
//        - concurrency  → SQLite's write-lock elects ONE winner among racing polls (0 rows = lost);
//        - restart      → the advanced value is index-persisted, so a re-poll of the same instant
//                         is no longer due (exactly-once across an index-preserving restart);
//        - crash        → a crash AFTER the advance loses at most THIS one fire and re-arms
//                         FORWARD — never a wedge, because `next_fire_at` has moved on.
//   2. EMIT `schedule.fired` (only the CAS winner emits). A crash in the tiny advance→emit window
//      drops one fire (at-most-once there) — the next interval fires; it can never double.
//
// The SEMANTIC-KEY LEASE of FLOWS §F7 ("nightly-triage-2026-07-07 → drop") is for HOOK triggers —
// event-driven, with NO monotonic `next_fire_at` to CAS on — and lands with the trigger taxonomy
// (BRO-1761). For the `next_fire_at`-driven kinds here (heartbeat / one-shot), a lease would be
// redundant with the CAS and could WEDGE the schedule if a crash stranded it (P20 BRO-1749).
//
// SCOPE NOTE: "exactly-once across restart" means an INDEX-PRESERVING restart. `schedule` is a
// FS-derived rebuildable cache (schema.ts); once a producer writes it (BRO-1761/1772), a
// `--rebuild` that re-derives `next_fire_at` from `_work.md` must re-apply fired history — tracked
// with that producer, not here (today nothing writes the table, so this is latent).

import { EVENT_TYPES } from "@maestro/protocol";
import { and, asc, eq, isNotNull, isNull, lte } from "drizzle-orm";
import type { IndexDb } from "../db/client";
import { event, schedule } from "../db/schema";

/** Default poll cadence for the live loop (ms). */
export const DEFAULT_SCHEDULER_POLL_MS = 1_000;

/** One schedule that fired this pass — returned so the caller (the tick, BRO-1772) can dispatch it. */
export interface FiredSchedule {
  scheduleId: string;
  nodeId: string;
  /** the fire instant (epoch ms) — the `next_fire_at` that came due, not the poll time. */
  firedAt: number;
}

/**
 * The next fire instant after `now`, or null to STOP firing. `heartbeat` recurs every `spec`-ms
 * interval (the 'interval' tick-cause maps to heartbeat, F7 canon) — advancing to the first grid
 * instant STRICTLY AFTER `now`, which SKIPS fires missed while the runtime was down: a routine
 * overdue by a long outage fires ONCE and re-arms in the future, never once-per-missed-interval.
 * Catching up would be the very storm F7 exists to kill. cron / hook / goal are ONE-SHOT here
 * (→ null): cron's next-instant + hook/goal trigger semantics are the trigger-taxonomy ticket's
 * (BRO-1761). A `spec` that is not a plain positive integer stops (→ null), never a tight loop.
 */
export function computeNextFireAt(
  triggerKind: string,
  spec: string,
  dueAt: number,
  now: number,
): number | null {
  if (triggerKind === "heartbeat" && /^\d+$/.test(spec)) {
    const interval = Number(spec);
    if (interval > 0) {
      // steps = how many whole intervals `now` is past `dueAt`, +1 → the first instant after now.
      const steps = Math.floor(Math.max(0, now - dueAt) / interval) + 1;
      return dueAt + steps * interval;
    }
  }
  return null;
}

/**
 * Fire every schedule due at `now`, exactly once, and return the ones that fired (for the tick to
 * dispatch). Pure over an injected `now` — no ambient clock — so the exactly-once and concurrency
 * properties are unit-testable by replay over a `:memory:` / file index. Each schedule fires in its
 * own try/catch (`onError` + continue): a throw on one — the due set is `next_fire_at`-ordered —
 * must never starve the schedules after it.
 */
export async function fireDueSchedules(
  db: IndexDb,
  now: number,
  onError?: (err: unknown, scheduleId: string) => void,
): Promise<FiredSchedule[]> {
  const due = await db
    .select()
    .from(schedule)
    .where(
      and(
        eq(schedule.enabled, true),
        isNull(schedule.deletedAt),
        isNotNull(schedule.nextFireAt),
        lte(schedule.nextFireAt, now),
      ),
    )
    .orderBy(asc(schedule.nextFireAt));

  const fired: FiredSchedule[] = [];
  for (const sched of due) {
    const dueAt = sched.nextFireAt;
    if (dueAt === null) continue; // the WHERE guarantees non-null; this narrows the type
    try {
      // 1. CAS-advance off the due value — the atomic exactly-once claim (see header). A `goal`
      //    trigger self-disables after its single fire (scope); heartbeat/one-shot keep enabled.
      const next = computeNextFireAt(sched.triggerKind, sched.spec, dueAt, now);
      // Write `enabled` ONLY to self-disable a goal — never echo it back for other kinds, so this
      // CAS (whose WHERE keys on next_fire_at, not enabled) can't clobber a concurrent external
      // disable once a producer writes the schedule table (BRO-1761/1772).
      const advance: { nextFireAt: number | null; updatedAt: number; enabled?: boolean } = {
        nextFireAt: next,
        updatedAt: now,
      };
      if (sched.triggerKind === "goal") advance.enabled = false;
      const claimed = await db
        .update(schedule)
        .set(advance)
        .where(and(eq(schedule.id, sched.id), eq(schedule.nextFireAt, dueAt)))
        .returning({ id: schedule.id });
      if (claimed.length === 0) continue; // a concurrent poll won this fire → don't double

      // 2. Emit the durable fire signal (synthetic — no session; the F6 tick consumes it).
      await db.insert(event).values({
        sessionId: null,
        ts: now,
        actor: "system",
        type: EVENT_TYPES.SCHEDULE_FIRED,
        payload: JSON.stringify({ scheduleId: sched.id, nodeId: sched.nodeId }),
      });
      fired.push({ scheduleId: sched.id, nodeId: sched.nodeId, firedAt: dueAt });
    } catch (err) {
      onError?.(err, sched.id); // isolate this schedule; the rest of the due set still fires
    }
  }
  return fired;
}

/** A live scheduler loop handle. */
export interface SchedulerHandle {
  /** Run one poll pass now — the test hook + a manual kick. Single-flighted with the loop. */
  poll: () => Promise<FiredSchedule[]>;
  /** Stop the loop. */
  stop: () => void;
}

export interface SchedulerOptions {
  /** Poll cadence (ms). Default DEFAULT_SCHEDULER_POLL_MS. */
  pollMs?: number;
  /** Injectable clock (default Date.now) — makes the loop deterministic in tests. */
  now?: () => number;
  /** Called with the schedules that fired each pass — the dispatch hook (the F6 tick, BRO-1772). */
  onFire?: (fired: FiredSchedule[]) => void;
  /** Notified on a poll/fire error; the loop swallows it and continues (a transient DB hiccup must not stop the scheduler). */
  onError?: (err: unknown) => void;
}

/**
 * Start the live scheduler: a single-flight poll loop that fires due schedules every `pollMs`.
 * Mirrors the watcher's single-flight discipline — a slow pass never overlaps the next. `stop()`
 * ends it; `poll()` runs one pass on demand (tests + a manual kick).
 */
export function startScheduler(db: IndexDb, opts: SchedulerOptions = {}): SchedulerHandle {
  const pollMs = opts.pollMs ?? DEFAULT_SCHEDULER_POLL_MS;
  const now = opts.now ?? Date.now;
  let closed = false;
  let running = false; // single-flight guard — a slow pass never overlaps the next

  const poll = async (): Promise<FiredSchedule[]> => {
    if (running || closed) return [];
    running = true;
    try {
      const fired = await fireDueSchedules(db, now(), opts.onError);
      if (fired.length > 0) opts.onFire?.(fired);
      return fired;
    } catch (err) {
      opts.onError?.(err);
      return [];
    } finally {
      running = false;
    }
  };

  const timer = setInterval(() => {
    void poll();
  }, pollMs);
  // Don't keep the process alive on this timer alone (mirrors the runtime's other loops).
  (timer as { unref?: () => void }).unref?.();

  return {
    poll,
    stop() {
      closed = true;
      clearInterval(timer);
    },
  };
}
