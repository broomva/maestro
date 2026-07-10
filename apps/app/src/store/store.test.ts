// store.test.ts — the BRO-1775 client-store contract suite.
//
// The done.check: "replaying a recorded event stream produces expected store
// state; localStorage isolated to prefs slice". The reducer + projector are pure,
// so the core is exercised with no React and no network; `connectStream` is driven
// through an injected fetch + EventSource.

import { describe, expect, test } from "bun:test";
import type {
  EventEnvelope,
  EventType,
  LiveGate,
  LiveNode,
  LiveSession,
  OrchState,
} from "@maestro/protocol";
import {
  applyEvents,
  connectStream,
  createMaestroStore,
  deriveWorkItem,
  type EventSourceLike,
  emptyServerTruth,
  hydrate,
  selectBoard,
  selectWorkItem,
  selectWorkItems,
} from "./index";

// ── Row + event builders ───────────────────────────────────────────────────────

function node(o: Partial<LiveNode> & { id: string; path: string; state: OrchState }): LiveNode {
  return {
    id: o.id,
    path: o.path,
    parentId: o.parentId ?? null,
    kind: o.kind ?? "task",
    state: o.state,
    owner: o.owner ?? null,
    gate: o.gate ?? "human",
    budgetJson: null,
    doneJson: null,
    title: o.title ?? null,
    createdAt: o.createdAt ?? 1000,
    updatedAt: o.updatedAt ?? 1000,
  };
}

function session(o: {
  id: string;
  nodeId: string;
  startedAt?: number;
  branch?: string;
}): LiveSession {
  return {
    id: o.id,
    nodeId: o.nodeId,
    branch: o.branch ?? `run/${o.id}`,
    status: "running",
    startedAt: o.startedAt ?? 100,
    endedAt: null,
    diffstatJson: null,
    updatedAt: o.startedAt ?? 100,
  };
}

function gate(o: {
  id: string;
  sessionId: string;
  verdict?: "approve" | null;
  openedAt?: number;
}): LiveGate {
  return {
    id: o.id,
    sessionId: o.sessionId,
    kind: "completion",
    proposalJson: null,
    verdict: o.verdict ?? null,
    decidedBy: null,
    openedAt: o.openedAt ?? 200,
    decidedAt: o.verdict ? 300 : null,
    updatedAt: 200,
  };
}

function ev(
  seq: number,
  type: string,
  payload?: unknown,
  sessionId: string | null = null,
  ts = "2026-07-10T00:00:00.000Z",
): EventEnvelope {
  return { seq, type: type as EventType, payload, sessionId, ts, actor: "system" };
}

// ── The reducer + projector, driven by a recorded event stream ──────────────────

describe("store: server-truth reducer + projector (replay)", () => {
  test("replays node.updated events into work items (event-sourced, no hydrate)", () => {
    const stream = [
      ev(
        1,
        "node.updated",
        node({ id: "root", path: "", state: "proposed", kind: "initiative", title: "Growth" }),
      ),
      ev(
        2,
        "node.updated",
        node({ id: "seo", path: "seo", parentId: "root", state: "running", title: "SEO refresh" }),
      ),
      ev(3, "node.updated", node({ id: "blk", path: "blk", parentId: "root", state: "blocked" })),
    ];
    const s = applyEvents(emptyServerTruth(), stream);
    const items = selectWorkItems(s);
    // Path-sorted (parent before child): "" < "blk" < "seo".
    expect(items.map((i) => i.id)).toEqual(["root", "blk", "seo"]);
    expect(items.map((i) => i.state)).toEqual(["proposed", "blocked", "running"]);
    // Title fallback: `blk` had no heading → last path segment.
    expect(items.find((i) => i.id === "blk")?.title).toBe("blk");
    expect(s.cursor).toBe(3);
  });

  test("board groups replayed nodes in WK_GROUP_ORDER (review first), non-empty only", () => {
    const s = applyEvents(emptyServerTruth(), [
      ev(1, "node.updated", node({ id: "r", path: "r", state: "review", updatedAt: 5000 })),
      ev(2, "node.updated", node({ id: "b", path: "b", state: "blocked", updatedAt: 4000 })),
      ev(3, "node.updated", node({ id: "run1", path: "run1", state: "running", updatedAt: 3000 })),
      ev(4, "node.updated", node({ id: "run2", path: "run2", state: "running", updatedAt: 6000 })),
      ev(5, "node.updated", node({ id: "p", path: "p", state: "proposed", updatedAt: 2000 })),
    ]);
    const board = selectBoard(s);
    expect(board.map((g) => g.state)).toEqual(["review", "blocked", "running", "proposed"]);
    // Within-group recency default (updatedAt desc): run2 before run1.
    expect(board.find((g) => g.state === "running")?.items.map((i) => i.id)).toEqual([
      "run2",
      "run1",
    ]);
  });

  test("schedule.fired events accumulate into the tick log (oldest first)", () => {
    const s = applyEvents(emptyServerTruth(), [
      ev(1, "schedule.fired", {
        scheduleId: "sc1",
        nodeId: "n1",
        firedAt: "2026-07-10T01:00:00.000Z",
      }),
      ev(
        2,
        "schedule.fired",
        { scheduleId: "sc2", nodeId: "n2" },
        null,
        "2026-07-10T02:00:00.000Z",
      ),
    ]);
    expect(s.ticks.map((t) => t.scheduleId)).toEqual(["sc1", "sc2"]);
    // firedAt falls back to the event ts when the payload omits it.
    expect(s.ticks[1]?.firedAt).toBe("2026-07-10T02:00:00.000Z");
  });

  test("gateId surfaces only at state review; a session log event refines the card age", () => {
    // Hydrate a running node + its session + an OPEN gate (the read-API bootstrap).
    let s = hydrate(emptyServerTruth(), {
      nodes: [node({ id: "seo", path: "seo", state: "running" })],
      sessions: [session({ id: "s1", nodeId: "seo", startedAt: 100 })],
      gates: [gate({ id: "g1", sessionId: "s1" })],
    });
    // While running, no gateId (the gate only surfaces at review).
    expect(selectWorkItem(s, "seo")?.gateId).toBeUndefined();
    expect(selectWorkItem(s, "seo")?.sessionId).toBe("s1");
    expect(selectWorkItem(s, "seo")?.run).toBe("run/s1");

    // A tool.call on the session refines lastEventAt; the node moves to review.
    s = applyEvents(s, [
      ev(1, "tool.call", { name: "grep" }, "s1", "2026-07-10T03:00:00.000Z"),
      ev(2, "node.updated", node({ id: "seo", path: "seo", state: "review" })),
    ]);
    const item = selectWorkItem(s, "seo");
    expect(item?.state).toBe("review");
    expect(item?.gateId).toBe("g1"); // now at review → the open gate surfaces
    expect(item?.lastEventAt).toBe("2026-07-10T03:00:00.000Z"); // refined by the session event
  });

  test("projector derives initiative / project ancestor labels from the parentId chain", () => {
    const s = hydrate(emptyServerTruth(), {
      nodes: [
        node({ id: "init", path: "", state: "proposed", kind: "initiative", title: "Growth" }),
        node({
          id: "proj",
          path: "growth",
          parentId: "init",
          state: "proposed",
          kind: "project",
          title: "Q3 launch",
        }),
        node({ id: "task", path: "growth/seo", parentId: "proj", state: "running", title: "SEO" }),
      ],
    });
    const item = selectWorkItem(s, "task");
    expect(item?.initiative).toBe("Growth");
    expect(item?.project).toBe("Q3 launch");
  });

  test("titleOf trims a whitespace heading", () => {
    const item = deriveWorkItem(
      node({ id: "n", path: "n", state: "proposed", title: "  Deploy  " }),
      emptyServerTruth(),
    );
    expect(item.title).toBe("Deploy");
  });

  test("gateId picks the most-recently-opened open gate (not a stale prior one)", () => {
    const s = hydrate(emptyServerTruth(), {
      nodes: [node({ id: "seo", path: "seo", state: "review" })],
      sessions: [
        session({ id: "s1", nodeId: "seo", startedAt: 100 }),
        session({ id: "s2", nodeId: "seo", startedAt: 200 }),
      ],
      gates: [
        gate({ id: "gold", sessionId: "s1", openedAt: 100 }), // a stale open gate
        gate({ id: "gnew", sessionId: "s2", openedAt: 300 }), // the current one
      ],
    });
    expect(selectWorkItem(s, "seo")?.gateId).toBe("gnew");
  });

  test("deriveWorkItem never emits an excluded field (no chat/events/budget/percent)", () => {
    const item = deriveWorkItem(
      node({ id: "n", path: "n", state: "proposed" }),
      emptyServerTruth(),
    );
    for (const banned of ["chat", "events", "budget", "done", "trigger", "progress", "percent"]) {
      expect(item).not.toHaveProperty(banned);
    }
  });
});

// ── Resume cursor (the Last-Event-ID resume the store tracks) ───────────────────

describe("store: resume cursor", () => {
  test("the cursor advances to the last applied seq", () => {
    const s = applyEvents(emptyServerTruth(), [
      ev(1, "node.updated", node({ id: "a", path: "a", state: "proposed" })),
      ev(2, "node.updated", node({ id: "b", path: "b", state: "proposed" })),
    ]);
    expect(s.cursor).toBe(2);
  });

  test("re-applying an event at or behind the cursor is a no-op (idempotent resume)", () => {
    const s1 = applyEvents(emptyServerTruth(), [
      ev(1, "node.updated", node({ id: "a", path: "a", state: "proposed" })),
      ev(2, "node.updated", node({ id: "b", path: "b", state: "running" })),
    ]);
    // A reconnect replays the boundary event (seq 2) — must not double-apply.
    const s2 = applyEvents(s1, [
      ev(2, "node.updated", node({ id: "b", path: "b", state: "done" })),
    ]);
    expect(s2).toBe(s1); // same reference — nothing changed
    expect(selectWorkItem(s2, "b")?.state).toBe("running"); // the stale replay was ignored
  });
});

// ── Persistence isolation (the load-bearing invariant of contract §5) ───────────

function inspectableStorage() {
  const map = new Map<string, string>();
  return {
    map,
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => {
      map.set(k, v);
    },
    removeItem: (k: string) => {
      map.delete(k);
    },
  };
}

function countingStorage() {
  const map = new Map<string, string>();
  let writes = 0;
  return {
    writes: () => writes,
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => {
      writes++;
      map.set(k, v);
    },
    removeItem: (k: string) => {
      map.delete(k);
    },
  };
}

describe("store: persistence isolation", () => {
  test("persist writes only on a real pref change, not on every server-truth event", () => {
    const storage = countingStorage();
    const store = createMaestroStore({ storage, name: "wc" });
    store.getState().setView("board"); // a real pref change → a write
    const afterPref = storage.writes();
    expect(afterPref).toBeGreaterThan(0);
    // A storm of server-truth events must NOT re-write the unchanged prefs blob.
    for (let i = 1; i <= 5; i++) {
      store
        .getState()
        .applyEvent(ev(i, "node.updated", node({ id: `n${i}`, path: `n${i}`, state: "running" })));
    }
    expect(storage.writes()).toBe(afterPref); // no additional writes on the hot path
  });

  test("only the prefs slice is written to storage — never server truth", () => {
    const storage = inspectableStorage();
    const store = createMaestroStore({ storage, name: "test-prefs" });
    // Mutate BOTH slices.
    store.getState().setView("board");
    store.getState().setNavOpen(false);
    store.getState().setCol("nav", 240);
    store
      .getState()
      .applyEvent(ev(1, "node.updated", node({ id: "secret", path: "secret", state: "running" })));

    const raw = storage.map.get("test-prefs");
    expect(raw).toBeDefined();
    const parsed = JSON.parse(raw as string) as { state: Record<string, unknown> };
    // Exactly the three prefs keys — no server truth leaks into storage.
    expect(Object.keys(parsed.state).sort()).toEqual(["cols", "navOpen", "view"]);
    expect(raw).not.toContain("secret");
    expect(raw).not.toContain("server");
  });

  test("a rehydrated store restores the persisted prefs (and only prefs)", () => {
    const storage = inspectableStorage();
    const a = createMaestroStore({ storage, name: "test-prefs2" });
    a.getState().setView("list");
    a.getState().setCol("nav", 180);
    a.getState().applyEvent(ev(1, "node.updated", node({ id: "x", path: "x", state: "running" })));

    // A fresh store over the same storage rehydrates prefs, NOT server truth.
    const b = createMaestroStore({ storage, name: "test-prefs2" });
    expect(b.getState().view).toBe("list");
    expect(b.getState().cols.nav).toBe(180);
    expect(Object.keys(b.getState().server.nodes)).toEqual([]); // server truth is not persisted
  });
});

// ── Selection model (contract §6) ──────────────────────────────────────────────

describe("store: selection model", () => {
  test("open / focus / close manages open sessions and the active one", () => {
    const store = createMaestroStore({ storage: inspectableStorage(), name: "sel1" });
    store.getState().openSession("s1");
    store.getState().openSession("s2");
    expect(store.getState().server.openSessionIds).toEqual(["s1", "s2"]);
    expect(store.getState().server.activeSessionId).toBe("s2");
    store.getState().focusSession("s1");
    expect(store.getState().server.activeSessionId).toBe("s1");
    store.getState().closeSession("s1");
    expect(store.getState().server.openSessionIds).toEqual(["s2"]);
    expect(store.getState().server.activeSessionId).toBe("s2"); // refocused to the survivor
    // Re-opening an already-open session does not duplicate it.
    store.getState().openSession("s2");
    expect(store.getState().server.openSessionIds).toEqual(["s2"]);
  });

  test("open / close manages the FS pane files without duplicates", () => {
    const store = createMaestroStore({ storage: inspectableStorage(), name: "sel2" });
    store.getState().openFile("a.md");
    store.getState().openFile("a.md");
    store.getState().openFile("b.md");
    expect(store.getState().server.openFilePaths).toEqual(["a.md", "b.md"]);
    store.getState().closeFile("a.md");
    expect(store.getState().server.openFilePaths).toEqual(["b.md"]);
  });
});

// ── connectStream — hydrate off the read API, then live off the stream ──────────

describe("store: connectStream", () => {
  class FakeES implements EventSourceLike {
    onmessage: ((ev: { data: string }) => void) | null = null;
    onerror: ((ev: unknown) => void) | null = null;
    closed = false;
    constructor(public url: string) {}
    close() {
      this.closed = true;
    }
    emit(e: EventEnvelope) {
      this.onmessage?.({ data: JSON.stringify(e) });
    }
  }

  test("hydrates /api/tree then applies streamed events; resume cursor rides the URL", async () => {
    const store = createMaestroStore({ storage: inspectableStorage(), name: "cs1" });
    const tree = { nodes: [node({ id: "seed", path: "seed", state: "proposed" })] };
    const fetchImpl = (async () => ({
      ok: true,
      json: async () => tree,
    })) as unknown as typeof fetch;
    let es: FakeES | undefined;
    const handle = connectStream(store, {
      fetchImpl,
      eventSourceFactory: (url) => {
        es = new FakeES(url);
        return es;
      },
    });
    // Let the hydrate promise chain settle so `subscribe()` (in .finally) runs.
    await new Promise((r) => setTimeout(r, 0));

    // Hydrated node is present; the stream opened at the resume cursor (0 after hydrate).
    expect(selectWorkItem(store.getState().server, "seed")).toBeDefined();
    expect(es?.url).toBe("/api/stream?after=0");

    // A live event flows through onmessage into server truth + advances the cursor.
    es?.emit(ev(1, "node.updated", node({ id: "live", path: "live", state: "running" })));
    expect(selectWorkItem(store.getState().server, "live")?.state).toBe("running");
    expect(store.getState().server.cursor).toBe(1);

    handle.close();
    expect(es?.closed).toBe(true);
  });

  test("a non-ok /api/tree reports onError and skips hydration but still subscribes", async () => {
    const store = createMaestroStore({ storage: inspectableStorage(), name: "cs2" });
    const fetchImpl = (async () => ({
      ok: false,
      status: 500,
      json: async () => ({ nodes: [] }),
    })) as unknown as typeof fetch;
    let err: unknown;
    let es: FakeES | undefined;
    connectStream(store, {
      fetchImpl,
      onError: (e) => {
        err = e;
      },
      eventSourceFactory: (url) => {
        es = new FakeES(url);
        return es;
      },
    });
    await new Promise((r) => setTimeout(r, 0));
    expect(err).toBeInstanceOf(Error); // the 500 surfaced, not swallowed as an empty tree
    expect(es).toBeDefined(); // subscription still opened (live tail can recover)
    expect(Object.keys(store.getState().server.nodes)).toEqual([]); // no partial hydrate
  });
});
