/// <reference types="bun" />
// reads.test.ts — the BRO-1812 route contract suite (`bun test apps/runtime/src/api`).
//
// The done.check: "route contract tests against fixture index; board order asserted
// review-first". Each route is exercised through the real Hono app (`app.request`),
// backed by a hand-seeded `:memory:` index — no socket, no network. The board's
// D-ORDER (review first) and the live-only projection (tombstones off the wire) are
// the load-bearing assertions.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import {
  type BoardResponse,
  type BriefResponse,
  DEFAULT_EVENT_PAGE_SIZE,
  type ErrorResponse,
  type EventPage,
  MAESTRO_PROTOCOL_VERSION,
  type NodeDetail,
  type OrchState,
  type SchedulesResponse,
  type SessionDetail,
  type TreeResponse,
} from "@maestro/protocol";
import { createApp } from "../app";
import { DEFAULT_PORT, type RuntimeConfig } from "../config";
import { type IndexHandle, openIndex } from "../db/client";
import { event, gate, node, schedule, session } from "../db/schema";
import { scanIntoIndex } from "../scanner";

const cfg = (workspace: string): RuntimeConfig => ({
  port: DEFAULT_PORT,
  workspace,
  indexPath: ":memory:",
  lockPath: join(workspace, ".maestro/runtime.lock"),
});

/** Open a fresh `:memory:` index + its app, workspace-rooted at `workspace`. */
async function mkApp(workspace = "/tmp/ws") {
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(workspace), Date.now(), handle.db);
  return { ...handle, app };
}

type NodeSeed = {
  id: string;
  path: string;
  state: OrchState;
  parentId?: string | null;
  kind?: "question" | "task" | "project" | "initiative" | "routine";
  title?: string | null;
  updatedAt?: number;
  deletedAt?: number | null;
};

async function seedNode(h: IndexHandle, o: NodeSeed): Promise<void> {
  await h.db.insert(node).values({
    id: o.id,
    path: o.path,
    parentId: o.parentId ?? null,
    kind: o.kind ?? "task",
    state: o.state,
    owner: null,
    gate: "human",
    budgetJson: null,
    doneJson: null,
    title: o.title ?? null,
    createdAt: 1000,
    updatedAt: o.updatedAt ?? 1000,
    deletedAt: o.deletedAt ?? null,
  });
}

// ── Seeded index — the seven routes ───────────────────────────────────────────

describe("read routes — seeded index", () => {
  let h: Awaited<ReturnType<typeof mkApp>>;

  beforeEach(async () => {
    h = await mkApp();
    // A tree: root initiative + one node per state (to exercise board D-ORDER).
    await seedNode(h, {
      id: "root",
      path: "",
      state: "proposed",
      kind: "initiative",
      title: "Root",
    });
    await seedNode(h, {
      id: "rev",
      path: "rev",
      state: "review",
      parentId: "root",
      title: "Needs you",
      updatedAt: 5000,
    });
    await seedNode(h, {
      id: "blk",
      path: "blk",
      state: "blocked",
      parentId: "root",
      updatedAt: 4000,
    });
    await seedNode(h, {
      id: "run",
      path: "run",
      state: "running",
      parentId: "root",
      updatedAt: 3000,
    });
    await seedNode(h, {
      id: "prop",
      path: "prop",
      state: "proposed",
      parentId: "root",
      updatedAt: 2000,
    });
    await seedNode(h, {
      id: "done1",
      path: "done1",
      state: "done",
      parentId: "root",
      updatedAt: 1500,
    });
    // A tombstoned node — must never surface (live-only projection).
    await seedNode(h, {
      id: "ghost",
      path: "ghost",
      state: "running",
      parentId: "root",
      deletedAt: 9999,
    });

    // A session on `rev` + a gate on that session (node/:id joins gates via session).
    await h.db.insert(session).values({
      id: "s-rev",
      nodeId: "rev",
      branch: "run/s-rev",
      status: "review",
      startedAt: 100,
      endedAt: null,
      diffstatJson: JSON.stringify({ files: 2, plus: 10, minus: 3 }),
      updatedAt: 100,
      deletedAt: null,
    });
    await h.db.insert(gate).values({
      id: "g1",
      sessionId: "s-rev",
      kind: "completion",
      proposalJson: null,
      verdict: null,
      decidedBy: null,
      openedAt: 200,
      decidedAt: null,
      updatedAt: 200,
      deletedAt: null,
    });

    // A SECOND node's two sessions + a gate — proves node/:id scoping does not leak
    // across nodes and that sessions come back newest-first (desc startedAt).
    await h.db.insert(session).values([
      {
        id: "s-run",
        nodeId: "run",
        branch: "run/s-run",
        status: "done",
        startedAt: 300,
        endedAt: 350,
        diffstatJson: null,
        updatedAt: 300,
        deletedAt: null,
      },
      {
        id: "s-run2",
        nodeId: "run",
        branch: "run/s-run2",
        status: "running",
        startedAt: 400,
        endedAt: null,
        diffstatJson: null,
        updatedAt: 400,
        deletedAt: null,
      },
    ]);
    await h.db.insert(gate).values({
      id: "g2",
      sessionId: "s-run",
      kind: "completion",
      proposalJson: null,
      verdict: null,
      decidedBy: null,
      openedAt: 320,
      decidedAt: null,
      updatedAt: 320,
      deletedAt: null,
    });

    // Schedules — one enabled routine (the bench), one disabled (excluded).
    await h.db.insert(schedule).values([
      {
        id: "sc-on",
        nodeId: "root",
        triggerKind: "cron",
        spec: "0 9 * * *",
        nextFireAt: 7000,
        enabled: true,
        updatedAt: 1,
        deletedAt: null,
      },
      {
        id: "sc-off",
        nodeId: "root",
        triggerKind: "cron",
        spec: "0 0 * * *",
        nextFireAt: 8000,
        enabled: false,
        updatedAt: 1,
        deletedAt: null,
      },
    ]);
  });

  afterEach(() => h.client.close());

  test("GET /api/tree returns live nodes, path-sorted, with no deletedAt on the wire", async () => {
    const res = await h.app.request("/api/tree");
    expect(res.status).toBe(200);
    const body = (await res.json()) as TreeResponse;
    // 6 live nodes; the tombstoned `ghost` is absent.
    expect(body.nodes.map((n) => n.id).sort()).toEqual([
      "blk",
      "done1",
      "prop",
      "rev",
      "root",
      "run",
    ]);
    // Path-sorted: "" (root) first, then lexicographic.
    expect(body.nodes.map((n) => n.path)).toEqual(["", "blk", "done1", "prop", "rev", "run"]);
    expect(body.nodes[0]).not.toHaveProperty("deletedAt");
  });

  test("every /api/* response carries the protocol version header", async () => {
    const res = await h.app.request("/api/tree");
    expect(res.headers.get("x-maestro-protocol")).toBe(String(MAESTRO_PROTOCOL_VERSION));
  });

  test("GET /api/board groups by state in D-ORDER — review first", async () => {
    const res = await h.app.request("/api/board");
    expect(res.status).toBe(200);
    const body = (await res.json()) as BoardResponse;
    // Only non-empty groups, in WK_GROUP_ORDER: review, blocked, running, proposed, done.
    expect(body.groups.map((g) => g.state)).toEqual([
      "review",
      "blocked",
      "running",
      "proposed",
      "done",
    ]);
    expect(body.groups[0]?.state).toBe("review");
    // `prop` group has the root (updatedAt 2000) + prop (2000) — both proposed.
    const proposed = body.groups.find((g) => g.state === "proposed");
    expect(proposed?.nodes.map((n) => n.id).sort()).toEqual(["prop", "root"]);
    // Tombstoned node never appears on the board.
    expect(body.groups.flatMap((g) => g.nodes).some((n) => n.id === "ghost")).toBe(false);
  });

  test("board within-group order is updatedAt descending (the recency default)", async () => {
    await seedNode(h, { id: "run2", path: "run2", state: "running", updatedAt: 6000 });
    const res = await h.app.request("/api/board");
    const body = (await res.json()) as BoardResponse;
    const running = body.groups.find((g) => g.state === "running");
    // run2 (6000) before run (3000).
    expect(running?.nodes.map((n) => n.id)).toEqual(["run2", "run"]);
  });

  test("GET /api/node/:id returns the node with ITS sessions and gates (scoped, no leak)", async () => {
    const res = await h.app.request("/api/node/rev");
    expect(res.status).toBe(200);
    const body = (await res.json()) as NodeDetail;
    expect(body.node.id).toBe("rev");
    expect(body.node).not.toHaveProperty("deletedAt");
    // Only rev's session/gate — `run`'s s-run/s-run2/g2 must NOT leak in.
    expect(body.sessions.map((s) => s.id)).toEqual(["s-rev"]);
    expect(body.gates.map((g) => g.id)).toEqual(["g1"]);
  });

  test("GET /api/node/:id returns sessions newest-first and only the node's own gate", async () => {
    const body = (await (await h.app.request("/api/node/run")).json()) as NodeDetail;
    // desc(startedAt): s-run2 (400) before s-run (300).
    expect(body.sessions.map((s) => s.id)).toEqual(["s-run2", "s-run"]);
    // Only run's gate (g2), never rev's g1.
    expect(body.gates.map((g) => g.id)).toEqual(["g2"]);
  });

  test("GET /api/node/:id returns empty sessions + gates for a never-dispatched node", async () => {
    const body = (await (await h.app.request("/api/node/blk")).json()) as NodeDetail;
    expect(body.sessions).toEqual([]);
    expect(body.gates).toEqual([]);
  });

  test("GET /api/node/:id 404s a missing node with the typed error shape", async () => {
    const res = await h.app.request("/api/node/nope");
    expect(res.status).toBe(404);
    const body = (await res.json()) as ErrorResponse;
    expect(body.error.code).toBe("not_found");
    expect(body.error.retryable).toBe(false);
  });

  test("GET /api/node/:id 404s a tombstoned node (never a ghost card)", async () => {
    expect((await h.app.request("/api/node/ghost")).status).toBe(404);
  });

  test("GET /api/sessions/:id returns the session row + diffstat receipt", async () => {
    const res = await h.app.request("/api/sessions/s-rev");
    expect(res.status).toBe(200);
    const body = (await res.json()) as SessionDetail;
    expect(body.session.id).toBe("s-rev");
    expect(body.session.branch).toBe("run/s-rev");
    expect(JSON.parse(body.session.diffstatJson ?? "{}")).toEqual({ files: 2, plus: 10, minus: 3 });
  });

  test("GET /api/sessions/:id 404s a missing session", async () => {
    expect((await h.app.request("/api/sessions/nope")).status).toBe(404);
  });

  test("GET /api/schedules returns only enabled routines (the bench)", async () => {
    const res = await h.app.request("/api/schedules");
    expect(res.status).toBe(200);
    const body = (await res.json()) as SchedulesResponse;
    expect(body.schedules.map((s) => s.id)).toEqual(["sc-on"]);
    expect(body.schedules[0]).not.toHaveProperty("deletedAt");
  });
});

// ── Event timeline paging ──────────────────────────────────────────────────────

describe("read routes — session events paging", () => {
  let h: Awaited<ReturnType<typeof mkApp>>;

  beforeEach(async () => {
    h = await mkApp();
    // One session's timeline + a synthetic (null sessionId) that must NOT appear.
    const rows: (typeof event.$inferInsert)[] = [];
    const total = DEFAULT_EVENT_PAGE_SIZE + 5;
    for (let i = 0; i < total; i++) {
      rows.push({
        sessionId: "s1",
        ts: 1_000 + i,
        actor: "agent",
        type: "tool.call",
        payload: JSON.stringify({ i }),
      });
    }
    await h.db.insert(event).values(rows);
    // A synthetic node.updated event (null sessionId) — belongs to the global stream.
    await h.db
      .insert(event)
      .values({ sessionId: null, ts: 5_000, actor: "system", type: "node.updated", payload: null });
    // A corrupt row on its own session: a `ts` beyond JS Date range (|ts| > 8.64e15).
    await h.db.insert(event).values({
      sessionId: "sbad",
      ts: 8_640_000_000_000_001,
      actor: "agent",
      type: "tool.call",
      payload: null,
    });
  });

  afterEach(() => h.client.close());

  test("first page returns exactly one page + a nextAfter cursor; second page drains the tail", async () => {
    const res1 = await h.app.request("/api/sessions/s1/events");
    const page1 = (await res1.json()) as EventPage;
    expect(page1.events.length).toBe(DEFAULT_EVENT_PAGE_SIZE);
    expect(page1.nextAfter).toBe(DEFAULT_EVENT_PAGE_SIZE); // seq is 1-based autoincrement

    const res2 = await h.app.request(`/api/sessions/s1/events?after=${page1.nextAfter}`);
    const page2 = (await res2.json()) as EventPage;
    expect(page2.events.length).toBe(5);
    expect(page2.nextAfter).toBeNull();
  });

  test("events project to the wire envelope (ISO ts, rehydrated payload)", async () => {
    const res = await h.app.request("/api/sessions/s1/events?after=0");
    const page = (await res.json()) as EventPage;
    const first = page.events[0];
    expect(typeof first?.ts).toBe("string");
    expect(first?.ts).toBe(new Date(1_000).toISOString());
    expect(first?.payload).toEqual({ i: 0 });
  });

  test("a session timeline excludes synthetics (null sessionId)", async () => {
    const res = await h.app.request("/api/sessions/s1/events?after=0");
    const page = (await res.json()) as EventPage;
    expect(page.events.every((e) => e.type === "tool.call")).toBe(true);
    expect(page.events.some((e) => e.type === "node.updated")).toBe(false);
  });

  test("an unknown session yields an empty page, not a 404", async () => {
    const res = await h.app.request("/api/sessions/nope/events");
    expect(res.status).toBe(200);
    const page = (await res.json()) as EventPage;
    expect(page.events).toEqual([]);
    expect(page.nextAfter).toBeNull();
  });

  test("a corrupt out-of-range ts yields a sentinel ISO, never a 500", async () => {
    const res = await h.app.request("/api/sessions/sbad/events");
    expect(res.status).toBe(200); // not a RangeError-500
    const page = (await res.json()) as EventPage;
    expect(page.events).toHaveLength(1);
    expect(page.events[0]?.ts).toBe(new Date(0).toISOString());
  });
});

// ── Brief route — the full scan → index → read integration ─────────────────────

describe("GET /api/node/:id/brief — scanned workspace", () => {
  const roots: string[] = [];
  let h: Awaited<ReturnType<typeof mkApp>>;

  function makeWorkspace(files: Record<string, string>): string {
    const root = mkdtempSync(join(tmpdir(), "maestro-read-"));
    roots.push(root);
    for (const [rel, content] of Object.entries(files)) {
      const abs = join(root, rel);
      mkdirSync(dirname(abs), { recursive: true });
      writeFileSync(abs, content);
    }
    return root;
  }

  const workMd = (id: string, brief: string) =>
    `---\nid: ${id}\nkind: task\nstate: proposed\ncreated: 2026-06-25\nupdated: 2026-06-25\n---\n\n${brief}\n`;

  afterEach(() => {
    h.client.close();
    for (const r of roots.splice(0)) rmSync(r, { recursive: true, force: true });
  });

  test("returns the _work.md body (frontmatter stripped) for a scanned node", async () => {
    const root = makeWorkspace({
      "_work.md": workMd("root", "# Growth\nthe root brief"),
      "seo/_work.md": workMd("seo", "# SEO refresh\ndo the thing"),
    });
    h = await mkApp(root);
    await scanIntoIndex(h.db, root);

    const res = await h.app.request("/api/node/seo/brief");
    expect(res.status).toBe(200);
    const body = (await res.json()) as BriefResponse;
    expect(body.path).toBe("seo");
    expect(body.brief).toContain("# SEO refresh");
    expect(body.brief).toContain("do the thing");
    expect(body.brief).not.toContain("id: seo"); // frontmatter stripped
  });

  test('returns the ROOT node\'s brief (path "")', async () => {
    const root = makeWorkspace({ "_work.md": workMd("root", "# Growth\nroot brief") });
    h = await mkApp(root);
    await scanIntoIndex(h.db, root);
    const res = await h.app.request("/api/node/root/brief");
    expect(res.status).toBe(200);
    const body = (await res.json()) as BriefResponse;
    expect(body.path).toBe("");
    expect(body.brief).toContain("# Growth");
  });

  test("404s the brief when the node exists in the index but its _work.md vanished", async () => {
    const root = makeWorkspace({
      "_work.md": workMd("root", "# Root"),
      "seo/_work.md": workMd("seo", "# SEO"),
    });
    h = await mkApp(root);
    await scanIntoIndex(h.db, root);
    // The index still has the `seo` node; delete the file so the read hits the catch.
    rmSync(join(root, "seo/_work.md"));
    expect((await h.app.request("/api/node/seo/brief")).status).toBe(404);
  });

  test("404s the brief of an unknown node", async () => {
    const root = makeWorkspace({ "_work.md": workMd("root", "# Root") });
    h = await mkApp(root);
    await scanIntoIndex(h.db, root);
    expect((await h.app.request("/api/node/nope/brief")).status).toBe(404);
  });
});

// ── No-index degradation contract (the compiled-binary path) ───────────────────

describe("read routes — no index (compiled-binary degradation)", () => {
  test("without an index handle, /health is a 200 stub and every read 404s", async () => {
    // Mirrors index.ts's catch path: createApp with no index → reads never mounted.
    const app = createApp(cfg("/tmp/ws"), Date.now());
    const health = await app.request("/health");
    expect(health.status).toBe(200);
    const hb = (await health.json()) as { ok: boolean; index: { status: string } };
    expect(hb.ok).toBe(true);
    expect(hb.index.status).toBe("stub");
    // The `if (index)` mount gate — reads are absent, not erroring.
    expect((await app.request("/api/tree")).status).toBe(404);
    expect((await app.request("/api/board")).status).toBe(404);
    expect((await app.request("/api/schedules")).status).toBe(404);
  });
});
