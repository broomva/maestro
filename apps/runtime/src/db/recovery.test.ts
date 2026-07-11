/// <reference types="bun" />
// recovery.test.ts — BRO-1814 done.check `bun test apps/runtime --filter recovery`. Crash recovery
// (FLOWS §F9, DECISIONS §D5) + the D4 singleton lock. Fixtures simulate the post-crash state (index
// behind the journal; a still-`running` session; stale budget + lease) and assert the exact reconciled
// tables. Anti-vacuity [[self-hosting-vacuous-pass]]: exact counts / statuses / spend, not "it ran".

import { afterEach, describe, expect, test } from "bun:test";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
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

/** One flattened journal line, byte-shaped exactly like SessionTee.#write ({...payload, ts, actor, type}). */
function journalLine(
  type: string,
  payload: Record<string, unknown> = {},
  tsMs = 1_700_000_000_000,
): string {
  return JSON.stringify({ ...payload, ts: new Date(tsMs).toISOString(), actor: "agent", type });
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

describe("recovery — journal replay (F9.1, no event loss)", () => {
  test("replays only the tail the index was missing (per-session high-water mark)", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedSession(h, "a", "blocked");
    // The index already has 2 of the session's events (the pre-crash committed prefix).
    for (let i = 0; i < 2; i++) {
      await h.db
        .insert(event)
        .values({ sessionId: "a", ts: 1, actor: "agent", type: "run.beat", payload: null });
    }
    // The journal (FS-first → ahead) has 5 events; recovery must re-insert the missing 3.
    await writeRunJournal(ws, "a", [
      journalLine("run.beat", { i: 0 }),
      journalLine("run.beat", { i: 1 }),
      journalLine("run.beat", { i: 2 }),
      journalLine("agent.said", { text: "hi" }),
      journalLine("run.exiting", { code: 0, reason: "done" }),
    ]);
    const r = await recoverOnStartup(h.db, { workspace: ws, now: () => 2 });
    expect(r.replayedEvents).toBe(3);
    expect(await eventCount(h, "a")).toBe(5); // no loss: index now matches the journal length
  });

  test("replays across rotated segments in order (.1 → session.jsonl)", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedSession(h, "b", "blocked");
    await writeRunJournal(
      ws,
      "b",
      [journalLine("run.beat", { i: 0 }), journalLine("run.beat", { i: 1 })],
      "session.jsonl.1",
    );
    await writeRunJournal(ws, "b", [journalLine("run.beat", { i: 2 })], "session.jsonl");
    const r = await recoverOnStartup(h.db, { workspace: ws, now: () => 2 });
    expect(r.replayedEvents).toBe(3); // .1 (2) + live (1)
    expect(await eventCount(h, "b")).toBe(3);
  });

  test("is idempotent — a second recovery replays nothing", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedSession(h, "c", "blocked");
    await writeRunJournal(ws, "c", [journalLine("run.beat"), journalLine("run.beat")]);
    expect((await recoverOnStartup(h.db, { workspace: ws, now: () => 2 })).replayedEvents).toBe(2);
    expect((await recoverOnStartup(h.db, { workspace: ws, now: () => 2 })).replayedEvents).toBe(0);
    expect(await eventCount(h, "c")).toBe(2);
  });
});

describe("recovery — budget reconcile (D5 derive-and-max)", () => {
  test("corrects stale spend UP to the derived total; leaves higher stored spend alone; ignores refused", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    // Session A: derived 0.3+0.4 = 0.7 > stored 0.5 → corrected to 0.7.
    await h.db.insert(runBudget).values({ sessionId: "A", spentUsd: 0.5, iterations: 1 });
    await h.db.insert(event).values({
      sessionId: "A",
      ts: 1,
      actor: "system",
      type: EVENT_TYPES.BUDGET_METERED,
      payload: JSON.stringify({ session: "A", usd: 0.3 }),
    });
    await h.db.insert(event).values({
      sessionId: "A",
      ts: 2,
      actor: "system",
      type: EVENT_TYPES.BUDGET_METERED,
      payload: JSON.stringify({ session: "A", usd: 0.4 }),
    });
    await h.db.insert(event).values({
      sessionId: "A",
      ts: 3,
      actor: "system",
      type: EVENT_TYPES.BUDGET_REFUSED,
      payload: JSON.stringify({ session: "A", reason: "per_run" }),
    });
    // Session B: derived 1.0 < stored 2.0 → unchanged (a crash-window under-count never lowers spend).
    await h.db.insert(runBudget).values({ sessionId: "B", spentUsd: 2.0, iterations: 3 });
    await h.db.insert(event).values({
      sessionId: "B",
      ts: 1,
      actor: "system",
      type: EVENT_TYPES.BUDGET_METERED,
      payload: JSON.stringify({ session: "B", usd: 1.0 }),
    });

    const r = await recoverOnStartup(h.db, { workspace: ws, now: () => 10 });
    expect(r.budgetReconciled).toBe(1); // only A corrected
    const [a] = await h.db.select().from(runBudget).where(eq(runBudget.sessionId, "A"));
    const [b] = await h.db.select().from(runBudget).where(eq(runBudget.sessionId, "B"));
    expect(a?.spentUsd).toBeCloseTo(0.7, 6); // derived (refused excluded)
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
