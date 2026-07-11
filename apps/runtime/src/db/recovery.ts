// Crash recovery (FLOWS §F9, DECISIONS §D5) — the runtime's startup reconcile, run BEFORE the API opens
// (F9.4). The libSQL index is a rebuildable cache; the FS journals + `_work.md` are truth. On a crash
// the FS-FIRST discipline (stdio.ts SessionTee) means each `session.jsonl*` is AHEAD of the index — the
// journal has the tail of events whose index insert did not complete. Recovery makes the index whole:
//
//   1. REPLAY — for each run's journal (its rotated segments in order, BRO-1811), re-insert the tail the
//      index is missing (per-session high-water mark = the count already indexed; the tee inserts in
//      journal order under a single writer, so index[0..K) == journal[0..K) and journal[K..N) is the gap).
//   2. RECONCILE BUDGET (D5 "derive-and-max") — set run_budget.spent_usd = max(stored, derived) from the
//      durable `budget.metered` events; overcounting a crash-window call is a cent lost, undercounting
//      leaks the guard. `budget.refused` is excluded (nothing spent).
//   3. EXPIRE LEASES — drop `lease` rows past their TTL (a runtime crash left them; no live holder).
//   4. PARK ORPHANS — a session still `running` has no live process (this is a fresh runtime); park it
//      `blocked` + emit `run.orphaned` (plain-voice "Stuck"). NEVER silently respawn — the human decides.
//
// Session/node ROWS themselves survive in the persisted index.db (they were index writes, not journaled)
// and are reconciled from `_work.md` by the scanner (F9.1); recovery here reconciles the EVENT tail +
// the authoritative budget/lease/session-status tables the journal + a fresh process invalidate.

import { mkdir, readdir } from "node:fs/promises";
import { join } from "node:path";
import type { Actor, EventType } from "@maestro/protocol";
import { EVENT_TYPES } from "@maestro/protocol";
import { and, eq, lt, sql } from "drizzle-orm";
import { readSegmentsInOrder } from "../harness/stdio";
import { deriveSpentBySession, type MeteredRecord } from "../proxy/budget";
import type { IndexDb } from "./client";
import { event, lease, runBudget, session } from "./schema";

/** What a startup recovery reconciled — surfaced in the startup log (observability, AUTONOMY §4). */
export interface RecoverySummary {
  /** Events re-inserted from journal tails the index was missing (the crash-window gap). */
  replayedEvents: number;
  /** run_budget rows whose spent_usd the D5 derive-and-max corrected upward. */
  budgetReconciled: number;
  /** Expired `lease` rows dropped. */
  leasesExpired: number;
  /** Sessions parked `blocked` + `run.orphaned` (were `running` with no live process). */
  orphansParked: number;
}

/** One parsed journal line (the flattened A.3 shape the tee writes: `{...payload, ts, actor, type}`). */
interface JournalEvent {
  ts: number;
  actor: Actor;
  type: EventType;
  /** JSON-string payload (the flattened fields minus ts/actor/type), or null. */
  payload: string | null;
}

/** Parse one flattened journal line back to an insertable event, or null if malformed (skip it). */
export function parseJournalLine(line: string): JournalEvent | null {
  let o: Record<string, unknown>;
  try {
    o = JSON.parse(line) as Record<string, unknown>;
  } catch {
    return null;
  }
  const { ts, actor, type, ...rest } = o;
  if (typeof type !== "string" || typeof actor !== "string") return null;
  // `ts` is ISO-8601 on the wire (tee writes new Date(ms).toISOString()); the index row stores epoch-ms.
  const epoch = typeof ts === "string" ? Date.parse(ts) : typeof ts === "number" ? ts : Number.NaN;
  if (!Number.isFinite(epoch)) return null;
  const payload = Object.keys(rest).length > 0 ? JSON.stringify(rest) : null;
  return { ts: epoch, actor: actor as Actor, type: type as EventType, payload };
}

/** All journal events for a run, its rotated segments read in append order (BRO-1811 gapless layout). */
async function readRunJournal(runDir: string): Promise<JournalEvent[]> {
  const out: JournalEvent[] = [];
  for (const buf of await readSegmentsInOrder(runDir)) {
    for (const line of buf.split("\n")) {
      if (line === "") continue;
      const ev = parseJournalLine(line);
      if (ev !== null) out.push(ev);
    }
  }
  return out;
}

/** Replay each run's journal tail into the index (F9.1). Returns how many events were re-inserted. */
async function replayJournals(db: IndexDb, workspace: string): Promise<number> {
  const runsRoot = join(workspace, "runs");
  let dirs: string[];
  try {
    dirs = await readdir(runsRoot);
  } catch {
    return 0; // no runs/ dir yet — nothing to replay
  }
  let replayed = 0;
  for (const name of dirs) {
    const m = /^run-(.+)$/.exec(name);
    if (!m) continue;
    const sessionId = m[1] as string;
    const journal = await readRunJournal(join(runsRoot, name));
    if (journal.length === 0) continue;
    // High-water mark: the count already indexed for this session (the tee inserts in journal order
    // under a single writer, so the first K index rows == the first K journal lines; the gap is [K..N)).
    const [{ n: indexed } = { n: 0 }] = await db
      .select({ n: sql<number>`count(*)` })
      .from(event)
      .where(eq(event.sessionId, sessionId));
    for (let i = indexed; i < journal.length; i++) {
      const ev = journal[i] as JournalEvent;
      await db.insert(event).values({
        sessionId,
        ts: ev.ts,
        actor: ev.actor,
        type: ev.type,
        payload: ev.payload,
      });
      replayed++;
    }
  }
  return replayed;
}

/** D5 "derive-and-max": correct run_budget.spent_usd upward from the durable budget.metered events. */
async function reconcileBudget(db: IndexDb): Promise<number> {
  const rows = await db.select().from(event).where(eq(event.type, EVENT_TYPES.BUDGET_METERED));
  const records: MeteredRecord[] = [];
  for (const r of rows) {
    if (r.sessionId === null) continue;
    let usd = 0;
    try {
      const p = JSON.parse(r.payload ?? "{}") as { usd?: unknown };
      if (typeof p.usd === "number" && Number.isFinite(p.usd)) usd = p.usd;
    } catch {
      // a malformed budget.metered payload contributes 0 — never crash recovery on one bad row
    }
    records.push({ session: r.sessionId, usd, ts: r.ts });
  }
  const derived = deriveSpentBySession(records);
  let corrected = 0;
  for (const [sessionId, derivedUsd] of derived) {
    const [row] = await db
      .select({ spentUsd: runBudget.spentUsd })
      .from(runBudget)
      .where(eq(runBudget.sessionId, sessionId));
    if (row === undefined) continue; // no budget row (a session that never spent) — nothing to reconcile
    if (derivedUsd > row.spentUsd) {
      await db
        .update(runBudget)
        .set({ spentUsd: derivedUsd })
        .where(eq(runBudget.sessionId, sessionId));
      corrected++;
    }
  }
  return corrected;
}

/** Drop `lease` rows past their TTL — a crashed runtime left them with no live holder (F9.2). */
async function expireLeases(db: IndexDb, now: number): Promise<number> {
  const res = await db.delete(lease).where(lt(lease.expiresAt, now));
  return res.rowsAffected;
}

/** Park every still-`running` session `blocked` + emit `run.orphaned` — no live process owns them after
 *  a restart, and F9.3 NEVER silently respawns (the human decides resume vs discard). */
async function parkOrphans(db: IndexDb, now: number): Promise<number> {
  const running = await db
    .select({ id: session.id })
    .from(session)
    .where(eq(session.status, "running"));
  for (const { id } of running) {
    await db.insert(event).values({
      sessionId: id,
      ts: now,
      actor: "system" satisfies Actor,
      type: EVENT_TYPES.RUN_ORPHANED,
      payload: null,
    });
    await db
      .update(session)
      .set({ status: "blocked", updatedAt: now, endedAt: now })
      .where(and(eq(session.id, id), eq(session.status, "running")));
  }
  return running.length;
}

/**
 * Run the full F9 crash recovery over the persisted index, BEFORE the API opens. Order matters: replay
 * FIRST (so budget reconcile sees the full journal-tail of `budget.metered` events), then reconcile
 * budget, expire leases, and park orphans last. Idempotent — a re-run with no new journal tail replays
 * nothing, re-derives the same budgets, finds no fresh leases, and parks no already-blocked session.
 */
export async function recoverOnStartup(
  db: IndexDb,
  opts: { workspace: string; now?: () => number },
): Promise<RecoverySummary> {
  const now = (opts.now ?? Date.now)();
  const replayedEvents = await replayJournals(db, opts.workspace);
  const budgetReconciled = await reconcileBudget(db);
  const leasesExpired = await expireLeases(db, now);
  const orphansParked = await parkOrphans(db, now);
  return { replayedEvents, budgetReconciled, leasesExpired, orphansParked };
}

// Ensure the workspace runs/ root exists (a fresh workspace has none) — callers that enumerate it get a
// clean empty result rather than an ENOENT they must special-case. Exported for the startup wiring.
export async function ensureRunsRoot(workspace: string): Promise<void> {
  await mkdir(join(workspace, "runs"), { recursive: true });
}
