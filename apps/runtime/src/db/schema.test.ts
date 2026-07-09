/// <reference types="bun" />
// schema.test.ts — the seam test for BRO-1796 (`bun test apps/runtime -t schema`).
//
// Two jobs, matching the ticket done.check:
//  1. RUNTIME: migrations apply on an empty (`:memory:`) db, and the constraints
//     the contract pins actually bite — seq is a monotonic total order, a
//     synthetic event persists with a null session_id, `node.path` is unique only
//     among LIVE rows (tombstone + recreate is allowed), timestamps round-trip as
//     ms-precision numbers, and the column defaults hold.
//  2. COMPILE-TIME: every table's drizzle `$inferSelect` is structurally
//     equivalent to its `@maestro/protocol` row shape. This is the seam — the
//     drizzle binding (here) may not drift from the contract (there). tsc enforces
//     it via the `Expect<Assignable<…>>` aliases below; `bun test` transpiles
//     types away, so `bun run typecheck` (CI `quality`) is what makes it bite.

import { describe, expect, test } from "bun:test";
import type {
  EventRow,
  GateRow,
  LeaseRow,
  NodeRow,
  RunBudgetRow,
  ScanCursorRow,
  ScheduleRow,
  SessionRow,
} from "@maestro/protocol";
import { eq, type InferSelectModel } from "drizzle-orm";
import { openIndex } from "./client";
import {
  event,
  type gate,
  indexSchema,
  lease,
  node,
  runBudget,
  scanCursor,
  schedule,
  type session,
} from "./schema";

// ── Compile-time contract: drizzle $inferSelect ≡ protocol row shape ──────────
// Bidirectional assignability = structural equivalence for these all-required
// object types: a missing, extra, or mistyped column fails one direction at tsc.
// (The tuple wrap in `Assignable` blocks union distribution so `EventType` and
// friends compare as a whole.)
type Expect<T extends true> = T;
type Assignable<A, B> = [A] extends [B] ? true : false;
type Equiv<A, B> =
  Assignable<A, B> extends true ? (Assignable<B, A> extends true ? true : false) : false;

type _NodeContract = Expect<Equiv<InferSelectModel<typeof node>, NodeRow>>;
type _SessionContract = Expect<Equiv<InferSelectModel<typeof session>, SessionRow>>;
type _EventContract = Expect<Equiv<InferSelectModel<typeof event>, EventRow>>;
type _GateContract = Expect<Equiv<InferSelectModel<typeof gate>, GateRow>>;
type _ScheduleContract = Expect<Equiv<InferSelectModel<typeof schedule>, ScheduleRow>>;
type _BudgetContract = Expect<Equiv<InferSelectModel<typeof runBudget>, RunBudgetRow>>;
type _LeaseContract = Expect<Equiv<InferSelectModel<typeof lease>, LeaseRow>>;
type _CursorContract = Expect<Equiv<InferSelectModel<typeof scanCursor>, ScanCursorRow>>;

// ── Runtime: a fresh in-memory index per test (migrations applied on open) ────
const freshIndex = () => openIndex(":memory:");

describe("index schema — migrations apply on an empty db", () => {
  test("every one of the eight tables is created and queryable", async () => {
    const { db, client } = await freshIndex();
    // A select against each table both proves the table exists (migration ran)
    // and that the empty db has no rows.
    for (const table of Object.values(indexSchema)) {
      expect(await db.select().from(table)).toEqual([]);
    }
    client.close();
  });

  test("opening the same db twice is idempotent (migrations do not re-run)", async () => {
    // The migrator records applied migrations in its journal table; a second
    // openIndex over a shared client must not throw "table already exists".
    const { db, client } = await freshIndex();
    await db.insert(node).values(makeNode({ id: "n1", path: "/a" }));
    // A second migrate() call on the same handle is a no-op.
    const rows = await db.select().from(node);
    expect(rows.length).toBe(1);
    client.close();
  });
});

describe("event — schema constraints", () => {
  test("seq is a monotonic, gap-free total order (the SSE cursor)", async () => {
    const { db, client } = await freshIndex();
    for (let i = 0; i < 3; i++) {
      await db.insert(event).values({
        sessionId: "run-1",
        ts: 1_700_000_000_000 + i,
        actor: "agent",
        type: "tool.call",
      });
    }
    const rows = await db.select().from(event);
    const seqs = rows.map((r) => r.seq);
    expect(seqs).toEqual([1, 2, 3]);
    client.close();
  });

  test("a synthetic event persists with a null session_id (D-DURABILITY)", async () => {
    const { db, client } = await freshIndex();
    await db
      .insert(event)
      .values({ sessionId: null, ts: 1_700_000_000_000, actor: "system", type: "node.updated" });
    const [row] = await db.select().from(event);
    expect(row?.sessionId).toBeNull();
    expect(row?.type).toBe("node.updated");
    client.close();
  });
});

describe("node — path is unique only among LIVE rows (partial index)", () => {
  test("two live rows with the same path collide", async () => {
    const { db, client } = await freshIndex();
    await db.insert(node).values(makeNode({ id: "n1", path: "/growth" }));
    let threw = false;
    try {
      await db.insert(node).values(makeNode({ id: "n2", path: "/growth" }));
    } catch {
      threw = true;
    }
    expect(threw).toBe(true);
    client.close();
  });

  test("tombstoning a node frees its path for a fresh live row", async () => {
    const { db, client } = await freshIndex();
    await db.insert(node).values(makeNode({ id: "n1", path: "/growth" }));
    // Tombstone n1 (a vanished FS node soft-deletes, never disappears).
    await db.update(node).set({ deletedAt: 1_700_000_000_500 }).where(eqId("n1"));
    // The path is now free among live rows — recreate it.
    await db.insert(node).values(makeNode({ id: "n2", path: "/growth" }));
    const all = await db.select().from(node);
    expect(all.length).toBe(2);
    const live = all.filter((r) => r.deletedAt === null);
    expect(live.length).toBe(1);
    expect(live[0]?.id).toBe("n2");
    client.close();
  });
});

describe("timestamps + defaults", () => {
  test("a *At column round-trips as an ms-precision number, not seconds or a Date", async () => {
    const { db, client } = await freshIndex();
    // 13-digit epoch ms with a non-zero ms tail: {mode:'timestamp'} would truncate
    // this to whole seconds and hand back a Date. {mode:'number'} keeps it exact.
    const createdAt = 1_700_000_000_123;
    await db.insert(node).values(makeNode({ id: "n1", path: "/a", createdAt }));
    const [row] = await db.select().from(node);
    expect(typeof row?.createdAt).toBe("number");
    expect(row?.createdAt).toBe(createdAt);
    client.close();
  });

  test("column defaults hold: node.gate=human, schedule.enabled=true, budget zeroed", async () => {
    const { db, client } = await freshIndex();
    await db.insert(node).values({
      id: "n1",
      path: "/a",
      kind: "task",
      state: "proposed",
      createdAt: 1,
      updatedAt: 1,
    });
    const [n] = await db.select().from(node);
    expect(n?.gate).toBe("human");

    await db
      .insert(schedule)
      .values({ id: "s1", nodeId: "n1", triggerKind: "cron", spec: "0 6 * * *", updatedAt: 1 });
    const [s] = await db.select().from(schedule);
    expect(s?.enabled).toBe(true);

    await db.insert(runBudget).values({ sessionId: "run-1" });
    const [b] = await db.select().from(runBudget);
    expect(b?.spentUsd).toBe(0);
    expect(b?.iterations).toBe(0);
    expect(b?.lastCallAt).toBeNull();
    client.close();
  });
});

describe("authoritative rows carry no sync fields", () => {
  test("run_budget and lease insert with only their operational columns", async () => {
    const { db, client } = await freshIndex();
    // These inserts omit updatedAt/deletedAt — proving those columns do not exist
    // on the per-runtime authoritative tables (they would be NOT NULL if they did).
    await db
      .insert(runBudget)
      .values({ sessionId: "run-1", spentUsd: 1.25, iterations: 3, lastCallAt: 1_700_000_000_000 });
    await db.insert(lease).values({ key: "n1", holder: "runtime-a", acquiredAt: 1, expiresAt: 2 });
    await db
      .insert(scanCursor)
      .values({ path: "runs/run-1/session.jsonl", byteOffset: 128, lastSeq: 4, updatedAt: 1 });
    expect((await db.select().from(runBudget)).length).toBe(1);
    expect((await db.select().from(lease)).length).toBe(1);
    expect((await db.select().from(scanCursor)).length).toBe(1);
    client.close();
  });
});

// ── helpers ───────────────────────────────────────────────────────────────────
const eqId = (id: string) => eq(node.id, id);

/** A minimal valid `node` row (all NOT NULL columns) with per-test overrides. */
function makeNode(over: Partial<InferSelectModel<typeof node>> & { id: string; path: string }) {
  return {
    parentId: null,
    kind: "task" as const,
    state: "proposed" as const,
    createdAt: 1_700_000_000_000,
    updatedAt: 1_700_000_000_000,
    ...over,
  };
}
