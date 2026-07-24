/// <reference types="bun" />
// nudge.test.ts (BRO-1945 / s2b-ii) — the §3.1 nudge seam + the `nudgedSessionIds` derivation. The two
// halves of the escalation contract policy.ts documents get their own proofs here: (a) a nudge MOVES the
// session's max(event.ts), (b) the "already nudged" set is scoped to the CURRENT stale window.

import { describe, expect, test } from "bun:test";
import { EVENT_TYPES } from "@maestro/protocol";
import { eq, max } from "drizzle-orm";
import { type IndexHandle, openIndex } from "../db/client";
import { event, node } from "../db/schema";
import { MemoryEventSink } from "../proxy/events";
import { createNudger, deriveNudgedSessionIds, renderNudgeText } from "./nudge";

const MIN = 60_000;

async function seedNode(h: IndexHandle, id: string, title: string | null) {
  await h.db.insert(node).values({
    id,
    path: `work/${id}`,
    parentId: null,
    kind: "task",
    state: "running" as never,
    owner: null,
    gate: "human",
    budgetJson: null,
    doneJson: null,
    title,
    createdAt: 1,
    updatedAt: 1,
    deletedAt: null,
  });
}

async function addEvent(h: IndexHandle, sessionId: string, type: string, ts: number) {
  await h.db
    .insert(event)
    .values({ sessionId, ts, actor: "agent", type: type as never, payload: null });
}

describe("renderNudgeText", () => {
  test("restates the GOAL in plain voice, and never the done contract (§6 hard line)", () => {
    const t = renderNudgeText({ title: "Fix the meta tags", path: "work/seo/meta" }, 40 * MIN);
    expect(t).toContain("quiet for 40 minutes");
    expect(t).toContain("The goal is still: Fix the meta tags (work/seo/meta).");
    // §6/CLAUDE.md §Voice — no em dashes, no enum names, no restatement of `done:`.
    expect(t).not.toContain("—");
    expect(t.toLowerCase()).not.toContain("done.check");
    expect(t.toLowerCase()).not.toContain("running");
  });

  test("falls back to the path when a node has no title", () => {
    expect(renderNudgeText({ title: null, path: "work/x" }, MIN)).toContain(
      "The goal is still: work/x (work/x).",
    );
  });
});

describe("createNudger", () => {
  test("routes ONE goal-restating chat into the live run and records run.nudged (contract (a))", async () => {
    const h = await openIndex(":memory:");
    try {
      await seedNode(h, "n1", "Ship the thing");
      await addEvent(h, "s1", EVENT_TYPES.RUN_BEAT, 1000); // the run's last worker word
      const chats: unknown[] = [];
      const sink = new MemoryEventSink();
      const nudge = createNudger({
        db: h.db,
        live: () => ({ chat: async (m) => void chats.push(m), runDir: "/tmp/runs/run-s1" }),
        sink,
      });

      const ok = await nudge({ sessionId: "s1", nodeId: "n1", ageMs: 40 * MIN, at: 50_000 });
      expect(ok).toBe(true);
      // exactly one chat, shaped as the UIMessage envelope the child's `chat` control line folds in.
      expect(chats.length).toBe(1);
      expect(chats[0]).toMatchObject({ role: "user" });
      expect(JSON.stringify(chats[0])).toContain("The goal is still: Ship the thing");

      // (a) the record landed on the SESSION's timeline and MOVED its max(event.ts) — 1000 → 50_000.
      const maxRows = await h.db
        .select({ m: max(event.ts) })
        .from(event)
        .where(eq(event.sessionId, "s1"));
      expect(maxRows[0]?.m).toBe(50_000);
      const rows = await h.db.select().from(event).where(eq(event.type, EVENT_TYPES.RUN_NUDGED));
      expect(rows.length).toBe(1);
      expect(rows[0]?.actor).toBe("system"); // the orchestrator sent it, not the human
      // durable FS journal first (D-DURABILITY), then the index projection.
      expect(sink.ofType(EVENT_TYPES.RUN_NUDGED).length).toBe(1);
      expect(sink.events[0]?.runDir).toBe("/tmp/runs/run-s1");
    } finally {
      h.client.close();
    }
  });

  test("a run that is NOT live is not nudged — no chat, no record, false (never over-claims)", async () => {
    const h = await openIndex(":memory:");
    try {
      await seedNode(h, "n1", "Ship the thing");
      const nudge = createNudger({ db: h.db, live: () => null, sink: new MemoryEventSink() });
      expect(await nudge({ sessionId: "s1", nodeId: "n1", ageMs: 40 * MIN, at: 50_000 })).toBe(
        false,
      );
      expect((await h.db.select().from(event)).length).toBe(0);
    } finally {
      h.client.close();
    }
  });

  test("a record that fails to land returns FALSE (the tick then says 'worth a look', not 'Nudged')", async () => {
    const h = await openIndex(":memory:");
    try {
      await seedNode(h, "n1", null);
      const failing = {
        emit: async () => {
          throw new Error("disk full");
        },
      };
      const nudge = createNudger({
        db: h.db,
        live: () => ({ chat: async () => {}, runDir: "/tmp/runs/run-s1" }),
        sink: failing,
      });
      expect(await nudge({ sessionId: "s1", nodeId: "n1", ageMs: 40 * MIN, at: 50_000 })).toBe(
        false,
      );
      // MUTATION PROOF: a nudge the next tick cannot see must not be claimed — no index record either.
      const rows = await h.db.select().from(event).where(eq(event.type, EVENT_TYPES.RUN_NUDGED));
      expect(rows.length).toBe(0);
    } finally {
      h.client.close();
    }
  });

  test("a tombstoned node still gets a goal line (from its id), not a crash", async () => {
    const h = await openIndex(":memory:");
    try {
      const chats: unknown[] = [];
      const nudge = createNudger({
        db: h.db,
        live: () => ({ chat: async (m) => void chats.push(m), runDir: "/tmp/r" }),
        sink: new MemoryEventSink(),
      });
      expect(await nudge({ sessionId: "s1", nodeId: "gone", ageMs: MIN, at: 5 })).toBe(true);
      expect(JSON.stringify(chats[0])).toContain("The goal is still: gone (gone).");
    } finally {
      h.client.close();
    }
  });
});

describe("deriveNudgedSessionIds — contract (b): scoped to the CURRENT stale window", () => {
  test("a nudge that is still the run's LAST word → nudged", async () => {
    const h = await openIndex(":memory:");
    try {
      await addEvent(h, "s1", EVENT_TYPES.RUN_BEAT, 1000);
      await addEvent(h, "s1", EVENT_TYPES.RUN_NUDGED, 2000);
      expect([...(await deriveNudgedSessionIds(h.db, ["s1"]))]).toEqual(["s1"]);
    } finally {
      h.client.close();
    }
  });

  test("REVIVED then re-stale → NOT nudged (it earns a fresh first nudge, not an escalation)", async () => {
    const h = await openIndex(":memory:");
    try {
      await addEvent(h, "s1", EVENT_TYPES.RUN_NUDGED, 2000);
      await addEvent(h, "s1", EVENT_TYPES.AGENT_SAID, 3000); // the nudge worked; the run spoke
      // ...and then went quiet again. "Ever nudged" would wrongly escalate here.
      expect([...(await deriveNudgedSessionIds(h.db, ["s1"]))]).toEqual([]);
    } finally {
      h.client.close();
    }
  });

  test("never nudged → not in the set; scoping is per session", async () => {
    const h = await openIndex(":memory:");
    try {
      await addEvent(h, "s1", EVENT_TYPES.RUN_NUDGED, 2000);
      await addEvent(h, "s2", EVENT_TYPES.RUN_BEAT, 2000);
      const got = await deriveNudgedSessionIds(h.db, ["s1", "s2"]);
      expect(got.has("s1")).toBe(true);
      expect(got.has("s2")).toBe(false);
    } finally {
      h.client.close();
    }
  });

  test("a nudge with NO other activity at all still counts (the run never spoke)", async () => {
    const h = await openIndex(":memory:");
    try {
      await addEvent(h, "s1", EVENT_TYPES.RUN_NUDGED, 2000);
      expect([...(await deriveNudgedSessionIds(h.db, ["s1"]))]).toEqual(["s1"]);
    } finally {
      h.client.close();
    }
  });

  test("empty input → empty set (no query)", async () => {
    const h = await openIndex(":memory:");
    try {
      expect((await deriveNudgedSessionIds(h.db, [])).size).toBe(0);
    } finally {
      h.client.close();
    }
  });
});
