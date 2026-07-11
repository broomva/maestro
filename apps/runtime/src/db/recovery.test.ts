/// <reference types="bun" />
// recovery.test.ts — BRO-1814 done.check `bun test apps/runtime --filter recovery`. Crash recovery
// (FLOWS §F9, DECISIONS §D5) + the D4 singleton lock. Fixtures simulate the post-crash state (index
// behind the journal; a still-`running` session; stale budget + lease) and assert the exact reconciled
// tables. Anti-vacuity [[self-hosting-vacuous-pass]]: exact counts / statuses / spend, not "it ran".

import { afterEach, describe, expect, test } from "bun:test";
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { EVENT_TYPES } from "@maestro/protocol";
import { and, eq, sql } from "drizzle-orm";
import { acquireRuntimeLock, DEFAULT_LOCK_STALE_MS, RuntimeLockedError } from "../runtime-lock";
import { type IndexHandle, openIndex } from "./client";
import { recoverOnStartup } from "./recovery";
import { event, lease, runBudget, session } from "./schema";

const handles: IndexHandle[] = [];
const tmps: string[] = [];
afterEach(async () => {
  for (const h of handles.splice(0)) h.client.close();
  for (const d of tmps.splice(0)) await rm(d, { recursive: true, force: true });
});
async function openMem(): Promise<IndexHandle> {
  const h = await openIndex(":memory:");
  handles.push(h);
  return h;
}
async function makeWorkspace(): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), "maestro-recovery-"));
  tmps.push(dir);
  return dir;
}

/** One flattened journal line, byte-shaped exactly like SessionTee.#write ({...payload, ts, actor, type})
 *  AND fsJournalSink ({...payload, ts, actor, type}) — the co-writer. `actor` defaults to the tee's
 *  "agent"; budget co-writer lines pass "system". */
function journalLine(
  type: string,
  payload: Record<string, unknown> = {},
  tsMs = 1_700_000_000_000,
  actor = "agent",
): string {
  return JSON.stringify({ ...payload, ts: new Date(tsMs).toISOString(), actor, type });
}
/** Seed an index `event` row that byte-matches a `journalLine(type,payload,ts,actor)` — the tee's live
 *  insert (payload re-nested as a JSON string, numeric ts). Used to model "already indexed before crash". */
async function seedIndexed(
  h: IndexHandle,
  sessionId: string,
  type: string,
  payload: Record<string, unknown> = {},
  tsMs = 1_700_000_000_000,
  actor = "agent",
): Promise<void> {
  await h.db.insert(event).values({
    sessionId,
    ts: tsMs,
    actor: actor as never,
    type: type as never,
    payload: Object.keys(payload).length > 0 ? JSON.stringify(payload) : null,
  });
}
/** Seed an index `event` row with a VERBATIM payload string — models the tee's live insert, which stores
 *  `JSON.stringify(payload)` (so an empty `{}` payload is indexed as the STRING "{}", NOT null; stdio.ts
 *  line 449). Used to reproduce the eventKey byte-divergence seedIndexed's empty→null collapse would hide. */
async function seedIndexedRaw(
  h: IndexHandle,
  sessionId: string,
  type: string,
  payload: string | null,
  tsMs = 1_700_000_000_000,
  actor = "agent",
): Promise<void> {
  await h.db
    .insert(event)
    .values({ sessionId, ts: tsMs, actor: actor as never, type: type as never, payload });
}
async function typeCount(h: IndexHandle, sessionId: string, type: string): Promise<number> {
  const rows = await h.db
    .select({ t: event.type })
    .from(event)
    .where(and(eq(event.sessionId, sessionId), eq(event.type, type as never)));
  return rows.length;
}
/** Write a run's session.jsonl (creating runs/run-<id>/). */
async function writeRunJournal(
  ws: string,
  id: string,
  lines: string[],
  file = "session.jsonl",
): Promise<void> {
  const dir = join(ws, "runs", `run-${id}`);
  await mkdir(dir, { recursive: true });
  await writeFile(join(dir, file), lines.length ? `${lines.join("\n")}\n` : "", "utf8");
}
async function seedSession(h: IndexHandle, id: string, status: string): Promise<void> {
  await h.db.insert(session).values({
    id,
    nodeId: `n-${id}`,
    branch: `run/${id}`,
    status: status as never,
    startedAt: 1,
    updatedAt: 1,
  });
}
async function eventCount(h: IndexHandle, sessionId: string): Promise<number> {
  const [row] = await h.db
    .select({ n: sql<number>`count(*)` })
    .from(event)
    .where(eq(event.sessionId, sessionId));
  return row?.n ?? 0;
}

describe("recovery — journal replay (F9.1, no event loss; the CO-WRITER reality)", () => {
  test("inserts the crash-tail AND the journal-only budget events, dedup by content, no dup", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedSession(h, "a", "blocked");
    const T = 1_700_000_000_000;
    // Pre-crash INDEX state: the tee indexed 2 child events. The proxy's budget lines are journal-ONLY
    // (fsJournalSink never inserts them into the index), so the index is a SUBSEQUENCE of the journal.
    await seedIndexed(h, "a", "run.beat", { i: 0 }, T);
    await seedIndexed(h, "a", "agent.said", { text: "hi" }, T);
    // The journal (complete truth): the 2 indexed tee events, INTERLEAVED with 2 journal-only budget
    // lines (actor system), plus a tee crash-tail the index never got.
    await writeRunJournal(ws, "a", [
      journalLine("run.beat", { i: 0 }, T),
      journalLine("budget.metered", { session: "a", usd: 0.3 }, T, "system"),
      journalLine("agent.said", { text: "hi" }, T),
      journalLine("budget.metered", { session: "a", usd: 0.4 }, T, "system"),
      journalLine("run.exiting", { code: 0, reason: "done" }, T),
    ]);
    const r = await recoverOnStartup(h.db, { workspace: ws, now: () => T });
    // The 2 already-indexed tee events are CONSUMED (not re-inserted); the 2 budget + the run.exiting
    // crash-tail are inserted. A count-based watermark would have dropped budget + duplicated a tee event.
    expect(r.replayedEvents).toBe(3);
    expect(await eventCount(h, "a")).toBe(5); // exactly the journal multiset — no loss, no dup
    expect(await typeCount(h, "a", "run.beat")).toBe(1); // NOT duplicated
    expect(await typeCount(h, "a", "budget.metered")).toBe(2); // journal-only events now indexed (D5 can read them)
  });

  test("replays across rotated segments in order (.1 → session.jsonl)", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedSession(h, "b", "blocked");
    const T = 1_700_000_000_000;
    await writeRunJournal(
      ws,
      "b",
      [journalLine("run.beat", { i: 0 }, T), journalLine("run.beat", { i: 1 }, T)],
      "session.jsonl.1",
    );
    await writeRunJournal(ws, "b", [journalLine("run.beat", { i: 2 }, T)], "session.jsonl");
    const r = await recoverOnStartup(h.db, { workspace: ws, now: () => 2 });
    expect(r.replayedEvents).toBe(3); // .1 (2) + live (1) — nothing indexed yet
    expect(await eventCount(h, "b")).toBe(3);
  });

  test("is idempotent — a second recovery over the same co-writer journal inserts nothing", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedSession(h, "c", "blocked");
    const T = 1_700_000_000_000;
    await writeRunJournal(ws, "c", [
      journalLine("run.beat", { i: 0 }, T),
      journalLine("budget.metered", { session: "c", usd: 0.5 }, T, "system"),
    ]);
    expect((await recoverOnStartup(h.db, { workspace: ws, now: () => T })).replayedEvents).toBe(2);
    expect((await recoverOnStartup(h.db, { workspace: ws, now: () => T })).replayedEvents).toBe(0);
    expect(await eventCount(h, "c")).toBe(2);
  });

  test("never DELETES an index-only event — a prior recovery's run.orphaned survives a re-recovery", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedSession(h, "d", "blocked");
    const T = 1_700_000_000_000;
    // The index holds a run.orphaned from a prior recovery (index-only — recovery does not journal it).
    await seedIndexed(h, "d", "run.orphaned", {}, T, "system");
    await writeRunJournal(ws, "d", [journalLine("run.beat", { i: 0 }, T)]); // journal lacks run.orphaned
    await recoverOnStartup(h.db, { workspace: ws, now: () => T });
    // The multiset-diff only INSERTS journal events; it never deletes → run.orphaned is untouched.
    expect(await typeCount(h, "d", "run.orphaned")).toBe(1);
    expect(await typeCount(h, "d", "run.beat")).toBe(1); // the journal event was inserted
  });

  test('an EMPTY-{} payload event indexed as "{}" is NOT duplicated (eventKey canonicalizes {}↔null)', async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedSession(h, "e", "blocked");
    const T = 1_700_000_000_000;
    // The tee indexed an event whose payload was an empty object → stored as the STRING "{}" (stdio.ts
    // 449). The journal line for the same event carries NO payload keys → parseJournalLine reconstructs
    // `null`. Raw string keys diverge ("{}" vs "") — the P20 minor. canonicalPayload collapses both to "".
    await seedIndexedRaw(h, "e", "run.beat", "{}", T);
    await writeRunJournal(ws, "e", [journalLine("run.beat", {}, T)]);
    const r = await recoverOnStartup(h.db, { workspace: ws, now: () => T });
    expect(r.replayedEvents).toBe(0); // recognized as already-indexed — NOT re-inserted
    expect(await eventCount(h, "e")).toBe(1); // no duplicate (was 2 before the fix)
  });

  test("a RESERVED-KEY payload (a key named type) is NOT duplicated (journal drops it; both sides canonicalize)", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedSession(h, "f", "blocked");
    const T = 1_700_000_000_000;
    // The tee indexed the full payload {type:"custom",n:1}. On the wire the journal line is
    // {...payload, ts, actor, type} so the envelope's type OVERWRITES payload.type → the "custom" is LOST;
    // parseJournalLine reconstructs {n:1}. Index "{\"type\":\"custom\",\"n\":1}" vs journal "{\"n\":1}"
    // — canonicalPayload drops reserved keys on BOTH sides → both "{\"n\":1}" → matched, no dup.
    await seedIndexedRaw(h, "f", "run.beat", JSON.stringify({ type: "custom", n: 1 }), T);
    await writeRunJournal(ws, "f", [journalLine("run.beat", { type: "custom", n: 1 }, T)]);
    const r = await recoverOnStartup(h.db, { workspace: ws, now: () => T });
    expect(r.replayedEvents).toBe(0); // recognized as already-indexed — NOT re-inserted
    expect(await eventCount(h, "f")).toBe(1); // no duplicate (was 2 before the fix)
  });
});

describe("recovery — budget reconcile (D5 derive-and-max, through the journal→index path)", () => {
  test("derives from budget.metered that recovery REPLAYS out of the journal (not pre-seeded)", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    // A: budget.metered live only in the JOURNAL (journal-only co-writer) → replay indexes them → D5
    // derives 0.3+0.4=0.7 > stored 0.5 → corrected. This is the path the first cut's test bypassed.
    await seedSession(h, "A", "blocked");
    await h.db.insert(runBudget).values({ sessionId: "A", spentUsd: 0.5, iterations: 1 });
    await writeRunJournal(ws, "A", [
      journalLine("budget.metered", { session: "A", usd: 0.3 }, 1, "system"),
      journalLine("budget.metered", { session: "A", usd: 0.4 }, 2, "system"),
      journalLine("budget.refused", { session: "A", reason: "per_run" }, 3, "system"),
    ]);
    // B: derived 1.0 < stored 2.0 → unchanged (a crash-window under-count never lowers spend).
    await seedSession(h, "B", "blocked");
    await h.db.insert(runBudget).values({ sessionId: "B", spentUsd: 2.0, iterations: 3 });
    await writeRunJournal(ws, "B", [
      journalLine("budget.metered", { session: "B", usd: 1.0 }, 1, "system"),
    ]);

    const r = await recoverOnStartup(h.db, { workspace: ws, now: () => 10 });
    expect(r.budgetReconciled).toBe(1); // only A corrected
    const [a] = await h.db.select().from(runBudget).where(eq(runBudget.sessionId, "A"));
    const [b] = await h.db.select().from(runBudget).where(eq(runBudget.sessionId, "B"));
    expect(a?.spentUsd).toBeCloseTo(0.7, 6); // derived (budget.refused excluded)
    expect(b?.spentUsd).toBeCloseTo(2.0, 6); // stored wins
  });
});

describe("recovery — lease expiry (F9.2)", () => {
  test("drops leases past their TTL, keeps fresh ones", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await h.db
      .insert(lease)
      .values({ key: "dead", holder: "old-runtime", acquiredAt: 1, expiresAt: 50 });
    await h.db.insert(lease).values({ key: "live", holder: "me", acquiredAt: 1, expiresAt: 5000 });
    const r = await recoverOnStartup(h.db, { workspace: ws, now: () => 100 });
    expect(r.leasesExpired).toBe(1);
    const rows = await h.db.select({ key: lease.key }).from(lease);
    expect(rows.map((x) => x.key)).toEqual(["live"]);
  });
});

describe("recovery — orphan parking (F9.3)", () => {
  test("parks a still-running session blocked + run.orphaned; never touches a settled session", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedSession(h, "orphan", "running");
    await seedSession(h, "done", "done");
    const r = await recoverOnStartup(h.db, { workspace: ws, now: () => 99 });
    expect(r.orphansParked).toBe(1);
    const [o] = await h.db.select().from(session).where(eq(session.id, "orphan"));
    expect(o?.status).toBe("blocked");
    expect(o?.endedAt).toBe(99);
    const orphaned = await h.db
      .select()
      .from(event)
      .where(and(eq(event.sessionId, "orphan"), eq(event.type, EVENT_TYPES.RUN_ORPHANED)));
    expect(orphaned).toHaveLength(1);
    const [d] = await h.db.select().from(session).where(eq(session.id, "done"));
    expect(d?.status).toBe("done"); // untouched — NEVER silently changed
  });
});

describe("recovery — D4 singleton lock", () => {
  test("a second runtime seeing a FRESH lock refuses; the first holds it", async () => {
    const ws = await makeWorkspace();
    const lockPath = join(ws, ".maestro", "runtime.lock");
    const first = await acquireRuntimeLock(lockPath, { id: "rt-1", now: () => 1000 });
    expect(first.id).toBe("rt-1");
    // A second runtime, lock still fresh (same clock) → refuse.
    await expect(
      acquireRuntimeLock(lockPath, { id: "rt-2", now: () => 1000 }),
    ).rejects.toBeInstanceOf(RuntimeLockedError);
  });

  test("the acquired lock file is always FULLY WRITTEN (atomic link — no empty-file window to double-acquire)", async () => {
    const ws = await makeWorkspace();
    const lockPath = join(ws, ".maestro", "runtime.lock");
    await acquireRuntimeLock(lockPath, { id: "rt-1", now: () => 1000 });
    // link()-of-a-pre-written-temp means lockPath, once present, carries the FULL record — never the
    // empty file writeFile(flag:"wx")'s create-then-write window exposed to a racing reader (→ double-acquire).
    expect(JSON.parse(await readFile(lockPath, "utf8"))).toEqual({ id: "rt-1", heartbeat: 1000 });
    // No leftover .tmp (the link's second name was unlinked).
    expect((await readdir(join(ws, ".maestro"))).filter((n) => n.endsWith(".tmp"))).toEqual([]);
  });

  test("a STALE lock (dead prior runtime) is stealable", async () => {
    const ws = await makeWorkspace();
    const lockPath = join(ws, ".maestro", "runtime.lock");
    await acquireRuntimeLock(lockPath, { id: "rt-1", now: () => 1000 });
    // Later than STALE past the first's heartbeat → the prior runtime is presumed dead; steal it.
    const second = await acquireRuntimeLock(lockPath, {
      id: "rt-2",
      now: () => 1000 + DEFAULT_LOCK_STALE_MS + 1,
    });
    expect(second.id).toBe("rt-2");
  });

  test("a heartbeat after being stale-stolen does NOT clobber the stealer + signals ownership loss", async () => {
    const ws = await makeWorkspace();
    const lockPath = join(ws, ".maestro", "runtime.lock");
    const lostTo: string[] = [];
    // rt-1 holds the lock, with an ownership-loss callback wired in.
    const first = await acquireRuntimeLock(lockPath, {
      id: "rt-1",
      now: () => 1000,
      onOwnershipLost: (h) => lostTo.push(h),
    });
    // rt-1 pauses > staleMs; rt-2 presumes it dead and steals the lock.
    const stealT = 1000 + DEFAULT_LOCK_STALE_MS + 1;
    const second = await acquireRuntimeLock(lockPath, { id: "rt-2", now: () => stealT });
    expect(second.id).toBe("rt-2");
    // rt-1 resumes and heartbeats — it MUST re-read, see rt-2 owns the lock, and NOT overwrite it (the
    // split-brain clobber). It signals ownership loss so the caller stands down.
    await first.heartbeat();
    expect((JSON.parse(await readFile(lockPath, "utf8")) as { id: string }).id).toBe("rt-2"); // intact
    expect(lostTo).toEqual(["rt-2"]); // rt-1 told it lost ownership
    // A subsequent heartbeat is a no-op (already stood down) — still rt-2, callback not re-fired.
    await first.heartbeat();
    expect((JSON.parse(await readFile(lockPath, "utf8")) as { id: string }).id).toBe("rt-2");
    expect(lostTo).toEqual(["rt-2"]);
  });

  test("two runtimes racing the SAME stale lock: exactly ONE steals it (atomic rename + verify, no double-acquire)", async () => {
    // The steal captures the stale inode via an ATOMIC `rename` (not rm-then-link) THEN re-validates the
    // captured record's freshness — closing both the loser's-rm-deletes-winner TOCTOU AND the capture-a-
    // just-refreshed-lock TOCTOU. Run the concurrent race many times: correct code yields exactly one
    // holder EVERY time (a naive rename-without-reverify reproduced a double-acquire at ~2% in the harness).
    const ROUNDS = 25;
    const t = () => 1000 + DEFAULT_LOCK_STALE_MS + 1;
    for (let round = 0; round < ROUNDS; round++) {
      const ws = await makeWorkspace();
      const lockPath = join(ws, ".maestro", "runtime.lock");
      await acquireRuntimeLock(lockPath, { id: "rt-1", now: () => 1000 });
      const settled = await Promise.allSettled([
        acquireRuntimeLock(lockPath, { id: `rt-2-${round}`, now: t }),
        acquireRuntimeLock(lockPath, { id: `rt-3-${round}`, now: t }),
      ]);
      const won = settled.filter((s) => s.status === "fulfilled");
      expect(won).toHaveLength(1); // exactly one holder — never two, never zero
      // The on-disk lock belongs to the single winner (no torn/overwritten record).
      const onDisk = JSON.parse(await readFile(lockPath, "utf8")) as { id: string };
      const winnerId = (won[0] as PromiseFulfilledResult<{ id: string }>).value.id;
      expect(onDisk.id).toBe(winnerId);
      // No leftover .tmp or .dead capture files (cleaned on both the win and the loss paths).
      const leftovers = (await readdir(join(ws, ".maestro"))).filter(
        (n) => n.endsWith(".tmp") || n.includes(".dead."),
      );
      expect(leftovers).toEqual([]);
    }
  });

  test("release frees the lock for re-acquire; a stealer's lock is never deleted by the old holder", async () => {
    const ws = await makeWorkspace();
    const lockPath = join(ws, ".maestro", "runtime.lock");
    const first = await acquireRuntimeLock(lockPath, { id: "rt-1", now: () => 1000 });
    await first.release();
    // Freed → a new runtime acquires cleanly (no refusal).
    const second = await acquireRuntimeLock(lockPath, { id: "rt-2", now: () => 2000 });
    expect(second.id).toBe("rt-2");
    // The old holder's release must NOT delete the stealer's lock.
    await first.release();
    const third = acquireRuntimeLock(lockPath, { id: "rt-3", now: () => 2000 });
    await expect(third).rejects.toBeInstanceOf(RuntimeLockedError); // rt-2 still holds it
  });
});
