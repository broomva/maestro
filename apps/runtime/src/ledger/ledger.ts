// The autonomy ledger (BRO-1818, ROADMAP §P3) — the product's own KPI, DERIVED from the event log,
// NEVER stored as a percentage (data-contract §"no stored %"). The scarce resource is unsupervised
// hours: how long agents run before a human must look (AUTONOMY.md). This module is the pure derivation;
// `api/reads.ts` serves it and a rung-1 chrome surface renders it calmly.
//
// Two numbers, both from events (the exit metric is "unsupervised hours trending up at constant human
// looks", ROADMAP §P3 Exit):
//   • unsupervisedMs — WALL-CLOCK the system was working autonomously = the UNION of run-active intervals
//     intersected with the window. Union, NOT sum: three runs in parallel for an hour is ONE unsupervised
//     hour (the human was away one hour), never three. A run is active from its `run.started` to its
//     terminal event; a run still going at the window end is clamped to the window end.
//   • humanLooks — a notch per human look: a gate verdict/escalation (actor "user") or a kill
//     (`run.killed`). See {@link isHumanLook}.

import { type Actor, EVENT_TYPES, type EventType } from "@maestro/protocol";

/** The run-lifecycle terminals that END a run-active interval (supervisor.ts TerminalEvent + orphan). */
export const RUN_TERMINAL_TYPES = [
  EVENT_TYPES.RUN_FINISHED,
  EVENT_TYPES.RUN_FAILED,
  EVENT_TYPES.RUN_KILLED,
  EVENT_TYPES.RUN_ORPHANED,
] as const;

const RUN_TERMINALS: ReadonlySet<string> = new Set(RUN_TERMINAL_TYPES);

/**
 * The run-lifecycle event types the ledger endpoint must FETCH (start + every terminal) to rebuild
 * run-active intervals. Exported so the read query's SQL `IN (…)` filter stays in lockstep with the
 * derivation — adding a terminal here updates both the interval fold and the query. (The human-look
 * `run.killed` is a terminal, so it is already in this set; the query's second arm only needs to add the
 * actor-"user" looks that carry no run interval.)
 */
export const LEDGER_RUN_LIFECYCLE_TYPES: readonly EventType[] = [
  EVENT_TYPES.RUN_STARTED,
  ...RUN_TERMINAL_TYPES,
];

/** The minimal event shape the derivation reads — a subset of the stored `event` row (schema.ts). */
export interface LedgerEvent {
  sessionId: string | null;
  ts: number; // epoch ms
  actor: Actor;
  type: string;
}

/** The half-open window `[since, until)` the ledger is computed over (epoch ms). */
export interface LedgerWindow {
  since: number;
  until: number;
}

/**
 * One unsupervised stretch on the scoreboard bar, in POSITIONAL percent of the window (0–100) — the
 * geometry the `AutonomyScoreboard` chrome renders. This is layout position along the window timeline,
 * NOT a progress percentage (canon forbids progress-% KPIs; a positional timeline is a receipt).
 */
export interface LedgerSegment {
  /** left edge, percent of the window (0–100). */
  start: number;
  /** width, percent of the window. */
  width: number;
  /** the stretch that is still running at the window end (there is at least one active run). */
  live?: boolean;
}

/** The derived scoreboard (never a stored value). `unsupervisedMs` + `humanLooks` are the two KPIs;
 *  `activeRuns` is a receipt (runs still active at `until`); `segments` + `notches` are the bar geometry
 *  (positional %, so the chrome renders the real timeline, not a faked or empty bar). */
export interface Ledger {
  since: number;
  until: number;
  unsupervisedMs: number;
  humanLooks: number;
  activeRuns: number;
  /** merged unsupervised stretches as positional % of the window (for the scoreboard bar). */
  segments: LedgerSegment[];
  /** human-look positions as percent of the window (0–100), the bar's notches. */
  notches: number[];
}

/**
 * Is `e` a HUMAN LOOK — a moment a human engaged with the running system (ROADMAP §P3: gate decision,
 * kill, chat)? Two arms:
 *   • `actor === "user"` — the deliberate human write-surface events. Today that is `gate.decided` +
 *     `gate.escalated` (a verdict / point; intents.ts appends both with actor "user"). It is also the
 *     forward-compatible arm: a user chat turn is currently INJECTED into the running child
 *     (broomva-child §chat) and not journaled as an event, so chat-notches are not counted yet — this
 *     arm counts them automatically once a chat turn is journaled as a user-actor event (follow-up).
 *   • `type === run.killed` — a kill. The supervisor emits the terminal `run.killed` with actor "system"
 *     (supervisor.ts), but a kill is ONLY ever human-initiated (F8 kill intent; crashes are `run.failed`,
 *     budget halts are `budget.exhausted`, reaper cleanups are `run.orphaned`), so it is always a look.
 */
export function isHumanLook(e: Pick<LedgerEvent, "actor" | "type">): boolean {
  return e.actor === "user" || e.type === EVENT_TYPES.RUN_KILLED;
}

/** One run's active interval [start, end] in epoch ms (end = the terminal ts, or the window end if still
 *  running). Internal to {@link deriveLedger}. */
interface Interval {
  start: number;
  end: number;
}

/**
 * Derive the ledger from `events` over `window`. `events` should include every run-lifecycle event with
 * `ts < until` (so a run that STARTED before the window but is active WITHIN it is measured — its
 * `run.started` is needed to know the run was live) plus the human-look events in the window; extra events
 * are ignored. Pure + order-independent (it sorts internally).
 */
export function deriveLedger(events: readonly LedgerEvent[], window: LedgerWindow): Ledger {
  const { since, until } = window;

  // 1. Fold run-lifecycle events into per-session intervals. Key by sessionId (the session IS the run,
  //    ARCHITECTURE §"the session is the verb"). start = the FIRST run.started; end = the FIRST terminal
  //    at/after that start, or `until` if the run never terminated (still active at the window end). A
  //    session with a terminal but no observed start is skipped (we cannot bound its interval).
  const starts = new Map<string, number>(); // sessionId → earliest run.started ts
  const ends = new Map<string, number>(); // sessionId → earliest terminal ts
  for (const e of events) {
    if (e.sessionId === null) continue; // synthetics (gate.*, node.updated) carry no run interval
    if (e.type === EVENT_TYPES.RUN_STARTED) {
      const cur = starts.get(e.sessionId);
      if (cur === undefined || e.ts < cur) starts.set(e.sessionId, e.ts);
    } else if (RUN_TERMINALS.has(e.type)) {
      const cur = ends.get(e.sessionId);
      if (cur === undefined || e.ts < cur) ends.set(e.sessionId, e.ts);
    }
  }

  let activeRuns = 0;
  const intervals: Interval[] = [];
  for (const [sessionId, start] of starts) {
    const terminal = ends.get(sessionId);
    // Still active at the window end iff no terminal, or the terminal is at/after `until`.
    if (terminal === undefined || terminal >= until) activeRuns++;
    const end = terminal ?? until;
    // Clamp to the window; drop a non-overlapping or zero-length interval.
    const clampedStart = Math.max(start, since);
    const clampedEnd = Math.min(end, until);
    if (clampedEnd > clampedStart) intervals.push({ start: clampedStart, end: clampedEnd });
  }

  // 2. UNION the intervals (wall-clock, not run-hours) into disjoint merged stretches, and sum the
  //    covered time. `merged` also feeds the scoreboard segments (step 4).
  intervals.sort((a, b) => a.start - b.start);
  const merged: Interval[] = [];
  for (const iv of intervals) {
    const last = merged[merged.length - 1];
    if (last && iv.start <= last.end) {
      // Overlapping/adjacent → extend the current merged stretch.
      if (iv.end > last.end) last.end = iv.end;
    } else {
      merged.push({ start: iv.start, end: iv.end });
    }
  }
  let unsupervisedMs = 0;
  for (const m of merged) unsupervisedMs += m.end - m.start;

  // 3. Count human looks within the window (half-open [since, until)), collecting their positions for
  //    the bar's notches.
  let humanLooks = 0;
  const lookTimes: number[] = [];
  for (const e of events) {
    if (e.ts >= since && e.ts < until && isHumanLook(e)) {
      humanLooks++;
      lookTimes.push(e.ts);
    }
  }

  // 4. Project to the scoreboard bar geometry — POSITIONAL percent of the window (not a progress %). A
  //    merged stretch touching `until` while a run is still active is the `live` segment. A degenerate
  //    window (span <= 0) yields no geometry (the aggregate numbers are still valid/zero).
  const span = until - since;
  const segments: LedgerSegment[] =
    span > 0
      ? merged.map((m) => {
          const seg: LedgerSegment = {
            start: ((m.start - since) / span) * 100,
            width: ((m.end - m.start) / span) * 100,
          };
          if (m.end >= until && activeRuns > 0) seg.live = true;
          return seg;
        })
      : [];
  const notches: number[] = span > 0 ? lookTimes.map((t) => ((t - since) / span) * 100) : [];

  return { since, until, unsupervisedMs, humanLooks, activeRuns, segments, notches };
}

// The plain-voice formatters live in @maestro/protocol (shared with the app chrome so both sides render
// identical strings); re-exported here so the endpoint + tests import them alongside the derivation.
export { formatLedgerLabel, formatUnsupervised } from "@maestro/protocol";

/** A UTC day in ms — the ledger's default window is the current UTC day (matches dispatch.ts / budget.ts,
 *  which floor to the same boundary; no local-timezone drift). */
export const LEDGER_DAY_MS = 86_400_000;

/**
 * The default ledger window given `now` (epoch ms): from UTC day-start (`floor(now / DAY) * DAY`) to
 * `now` — "today so far". The endpoint overrides either bound via `?since` / `?until`. UTC (not local)
 * so the boundary is deterministic across hosts and matches the budget/dispatch day math (no DST seam).
 */
export function defaultLedgerWindow(now: number): LedgerWindow {
  return { since: Math.floor(now / LEDGER_DAY_MS) * LEDGER_DAY_MS, until: now };
}
