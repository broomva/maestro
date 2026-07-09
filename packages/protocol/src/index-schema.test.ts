/// <reference types="bun" />
import { describe, expect, test } from "bun:test";
import {
  AUTHORITATIVE_TABLES,
  compareReplay,
  type EventRow,
  type GateRow,
  INDEX_TABLES,
  type LeaseRow,
  type NodeRow,
  REBUILDABLE_TABLES,
  type ReplayKey,
  type RunBudgetRow,
  replayKeyEqual,
  type ScanCursorRow,
  type ScheduleRow,
  type SessionRow,
  TABLE_AUTHORITY,
  TABLE_REBUILD,
} from "./index-schema";

// The done.check for seam-fs-index (BRO-1754) is `bun test packages/protocol
// --filter index-schema`. `--filter` is a no-op in `bun test` (only `-t` filters
// by name), so it reduces to "the contract doc exists AND all protocol tests
// pass" — but every describe below carries "index-schema" so `-t index-schema`
// isolates this seam's suite, honoring the ticket's intent.

describe("index-schema — the seven tables", () => {
  test("INDEX_TABLES lists exactly the DATA-MODEL §B.3 seven", () => {
    expect([...(INDEX_TABLES as readonly string[])].sort()).toEqual(
      ["event", "gate", "lease", "node", "run_budget", "schedule", "session"].sort(),
    );
  });
});

describe("index-schema — the derived-vs-authoritative split (DATA-MODEL §B.1)", () => {
  test("run_budget and lease are the only authoritative tables", () => {
    expect([...(AUTHORITATIVE_TABLES as readonly string[])].sort()).toEqual([
      "lease",
      "run_budget",
    ]);
  });

  test("the other five are FS-derived (a cache with teeth)", () => {
    expect([...(REBUILDABLE_TABLES as readonly string[])].sort()).toEqual(
      ["event", "gate", "node", "schedule", "session"].sort(),
    );
  });

  test("the split partitions every table exactly once", () => {
    for (const t of INDEX_TABLES) {
      const derived = REBUILDABLE_TABLES.includes(t);
      const authoritative = AUTHORITATIVE_TABLES.includes(t);
      expect(derived).toBe(!authoritative); // exactly one side
      expect(TABLE_AUTHORITY[t]).toBe(derived ? "fs-derived" : "authoritative");
    }
  });
});

describe("index-schema — the rebuild sources honor D-DURABILITY", () => {
  test("every table declares at least one rebuild source", () => {
    for (const t of INDEX_TABLES) {
      expect(TABLE_REBUILD[t].length).toBeGreaterThan(0);
    }
  });

  test("gate and run_budget are journal-replay recoverable (rebuild guarantee unqualified)", () => {
    // D-DURABILITY: gate.opened/gate.decided and budget.* are journaled, so
    // decided gates + spend counters survive the index.
    expect(TABLE_REBUILD.gate).toContain("journal-replay");
    expect(TABLE_REBUILD.run_budget).toContain("journal-replay");
  });

  test("lease is the only table that is not FS-recoverable (reconcile on crash)", () => {
    const reconcileOnly = INDEX_TABLES.filter(
      (t) => TABLE_REBUILD[t].length === 1 && TABLE_REBUILD[t][0] === "reconcile",
    );
    expect(reconcileOnly as readonly string[]).toEqual(["lease"]);
  });

  test("node/schedule scan the FS; session also scans git", () => {
    expect(TABLE_REBUILD.node).toEqual(["fs-scan"]);
    expect(TABLE_REBUILD.schedule).toEqual(["fs-scan"]);
    expect(TABLE_REBUILD.session).toContain("git-scan");
  });
});

// ── The rebuild-identity property test skeleton ──────────────────────────────
//
// The identity guarantee (p1-rebuild-invariant) is: kill the index, rebuild it,
// and the dump is byte-identical modulo wall-clock timestamps. Its beating heart
// is that `event.seq` is reassigned in a DETERMINISTIC total order — otherwise two
// rebuilds diverge. So the seam's property test proves `compareReplay` is a strict
// total order; p1-rebuild-invariant then builds the full kill/rebuild/diff test on
// top of it.

describe("index-schema — compareReplay is a strict total order (rebuild-identity core)", () => {
  // A curated sample exercising every discriminator: ts ties broken by path,
  // path ties broken by line, and fully-distinct keys.
  const KEYS: ReplayKey[] = [
    { ts: 100, sourcePath: "runs/run-a/session.jsonl", line: 0 },
    { ts: 100, sourcePath: "runs/run-a/session.jsonl", line: 1 },
    { ts: 100, sourcePath: "runs/run-a/session.jsonl", line: 2 },
    { ts: 100, sourcePath: "runs/run-b/session.jsonl", line: 0 }, // ts tie, later path
    { ts: 100, sourcePath: ".maestro/journal.jsonl", line: 0 }, // ts tie, earlier path
    { ts: 99, sourcePath: "runs/run-z/session.jsonl", line: 9 }, // earliest ts, latest path
    { ts: 101, sourcePath: ".maestro/journal.jsonl", line: 0 }, // latest ts, earliest path
    { ts: 100, sourcePath: "runs/run-b/session.jsonl", line: 5 },
  ];

  const sign = (n: number) => (n < 0 ? -1 : n > 0 ? 1 : 0);

  test("irreflexive: every key compares equal to itself", () => {
    for (const k of KEYS) {
      expect(compareReplay(k, k)).toBe(0);
      expect(replayKeyEqual(k, { ...k })).toBe(true);
    }
  });

  test("antisymmetric: compare(a,b) === -compare(b,a) in sign", () => {
    // `===` treats +0 and -0 as equal (unlike toBe's Object.is), so the diagonal
    // (a vs itself → 0 vs -0) holds.
    for (const a of KEYS) {
      for (const b of KEYS) {
        expect(sign(compareReplay(a, b)) === -sign(compareReplay(b, a))).toBe(true);
      }
    }
  });

  test("transitive: a<b and b<c ⟹ a<c across all triples", () => {
    for (const a of KEYS) {
      for (const b of KEYS) {
        for (const c of KEYS) {
          if (compareReplay(a, b) < 0 && compareReplay(b, c) < 0) {
            expect(compareReplay(a, c)).toBeLessThan(0);
          }
        }
      }
    }
  });

  test("total: distinct journal lines never tie (ts ties break on path, path ties on line)", () => {
    for (const a of KEYS) {
      for (const b of KEYS) {
        const distinct = a.ts !== b.ts || a.sourcePath !== b.sourcePath || a.line !== b.line;
        if (distinct) expect(compareReplay(a, b)).not.toBe(0);
      }
    }
  });

  test("sorting is stable and deterministic — two shuffles land in the same order", () => {
    const forward = [...KEYS].sort(compareReplay);
    const reversed = [...KEYS].reverse().sort(compareReplay);
    expect(reversed).toEqual(forward);
    // ts is the primary key; a sorted run is non-decreasing in ts.
    let prevTs = Number.NEGATIVE_INFINITY;
    for (const k of forward) {
      expect(k.ts).toBeGreaterThanOrEqual(prevTs);
      prevTs = k.ts;
    }
  });

  test("a non-finite ts degrades gracefully — sorts by (path, line), never returns NaN", () => {
    // ts should be finite (parser precondition), but the comparator must stay a
    // defined total order rather than poison seq assignment with NaN.
    const bad: ReplayKey = { ts: Number.NaN, sourcePath: "runs/run-a/session.jsonl", line: 0 };
    const good: ReplayKey = { ts: 100, sourcePath: "runs/run-a/session.jsonl", line: 1 };
    expect(Number.isNaN(compareReplay(bad, good))).toBe(false);
    expect(sign(compareReplay(bad, good)) === -sign(compareReplay(good, bad))).toBe(true);
  });

  // The full "cache with teeth" test lands in p1-rebuild-invariant: it asserts two
  // REBUILDS are byte-identical (canonical (ts,path,line) order) and reproduce every
  // §B.5 query answer — on a MULTI-FILE journal with a cross-file out-of-ts-order
  // line (the case that separates ingest order from replay order). Named here as the
  // seam skeleton (the ticket: "the property test is named here").
  test.skip("two rebuilds are byte-identical + reproduce every query answer (p1-rebuild-invariant)", () => {});
});

// ── Row-shape compile + sync-ready invariants ────────────────────────────────

describe("index-schema — row shapes compile and carry the sync-ready fields", () => {
  const node: NodeRow = {
    id: "7f3a9c",
    path: "growth/seo-refresh/fix-meta-tags",
    parentId: "growth/seo-refresh",
    kind: "task",
    state: "running",
    owner: "@alex",
    gate: "human",
    budgetJson: null,
    doneJson: null,
    title: "Fix meta tags",
    createdAt: 1_700_000_000_000,
    updatedAt: 1_700_000_000_000,
    deletedAt: null,
  };
  const session: SessionRow = {
    id: "7f3a",
    nodeId: "7f3a9c",
    branch: "run/7f3a",
    status: "review",
    startedAt: 1_700_000_000_000,
    endedAt: null,
    diffstatJson: null,
    updatedAt: 1_700_000_000_000,
    deletedAt: null,
  };
  const event: EventRow = {
    seq: 1,
    sessionId: null, // synthetic — nullable (D-DURABILITY)
    ts: 1_700_000_000_001, // epoch ms — the storage row; the wire projects this to ISO
    actor: "system",
    type: "node.updated",
  };
  const gate: GateRow = {
    id: "g1",
    sessionId: "7f3a",
    kind: "completion",
    proposalJson: null,
    verdict: null, // pending
    decidedBy: null,
    openedAt: 1_700_000_000_000,
    decidedAt: null,
    updatedAt: 1_700_000_000_000,
    deletedAt: null,
  };
  const schedule: ScheduleRow = {
    id: "s1",
    nodeId: "routines/nightly-triage",
    triggerKind: "cron",
    spec: "0 6 * * *",
    nextFireAt: null,
    enabled: true,
    updatedAt: 1_700_000_000_000,
    deletedAt: null,
  };
  const runBudget: RunBudgetRow = {
    sessionId: "7f3a",
    spentUsd: 0,
    iterations: 0,
    lastCallAt: null,
  };
  const lease: LeaseRow = {
    key: "7f3a9c",
    holder: "runtime-1",
    acquiredAt: 1_700_000_000_000,
    expiresAt: 1_700_000_000_060,
  };
  const cursor: ScanCursorRow = {
    path: "runs/run-7f3a/session.jsonl",
    byteOffset: 0,
    lastSeq: 0,
    updatedAt: 1_700_000_000_000,
  };

  test("FS-derived syncable rows carry updatedAt + a soft-delete tombstone", () => {
    for (const row of [node, session, gate, schedule]) {
      expect(typeof row.updatedAt).toBe("number");
      expect(row).toHaveProperty("deletedAt");
      expect(row.deletedAt).toBeNull();
    }
  });

  test("the append-only event storage row has a numeric ts and a nullable sessionId", () => {
    expect(event).not.toHaveProperty("deletedAt"); // immutable — no soft delete
    expect(event.sessionId).toBeNull();
    expect(typeof event.seq).toBe("number");
    expect(typeof event.ts).toBe("number"); // epoch ms in the row (§B.3), not the ISO wire string
  });

  test("authoritative operational rows are per-runtime (no sync fields)", () => {
    expect(runBudget).not.toHaveProperty("updatedAt");
    expect(lease).not.toHaveProperty("deletedAt");
  });

  test("the scan cursor carries both high-water coordinates", () => {
    expect(typeof cursor.byteOffset).toBe("number"); // per-file offset
    expect(typeof cursor.lastSeq).toBe("number"); // global seq high-water
  });
});
