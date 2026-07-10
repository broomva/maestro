// The runtime read API (BRO-1812) — the seven GET routes of API §1 Reads, hung on
// the Hono app over an open index handle. "Reads are cheap index queries" (API §1):
// each route is one indexed `select` projected to a typed `@maestro/protocol`
// envelope (api.ts), live rows only (`deletedAt IS NULL` — tombstones never cross
// the wire). Writes (intents), the SSE stream, and chat land on the same app later
// (BRO-1816 reuses this same handle + the `event.seq` cursor).
//
// Seam boundaries this module holds (does NOT cross):
//   - The board reuses the SHARED attention axis `WK_GROUP_ORDER` /
//     `compareByAttention` (plain-voice.ts, D-ORDER) — imported, never redefined
//     (owner: seam-gate-queue BRO-1789). The within-group attention-recency key for
//     {review, blocked} is BRO-1789 / the board UI's (BRO-1780); here a plain
//     `updatedAt DESC` default orders each group.
//   - The full derived read projection (`WorkItem`: title fallback, `look`,
//     `worker`, `run`, `gateId`, ancestry) is the projector's (BRO-1775). This
//     surface serves the raw live rows; the client store projects them.

import { readFile } from "node:fs/promises";
import { join } from "node:path";
import {
  type BoardGroup,
  type BoardResponse,
  type BriefResponse,
  DEFAULT_EVENT_PAGE_SIZE,
  type ErrorResponse,
  type EventPage,
  type LiveNode,
  MAESTRO_PROTOCOL_VERSION,
  type NodeDetail,
  type OrchState,
  parseWorkInput,
  type SchedulesResponse,
  type SessionDetail,
  type TreeResponse,
  WK_GROUP_ORDER,
  X_MAESTRO_PROTOCOL,
} from "@maestro/protocol";
import { and, asc, desc, eq, gt, inArray, isNull, sql } from "drizzle-orm";
import type { Context, Hono } from "hono";
import type { IndexDb } from "../db/client";
import { event, gate, node, schedule, session } from "../db/schema";
import { WORK_FILE } from "../scanner";
import { parseSeqCursor, toEnvelope } from "./event-projection";

/** What the read routes need: the open index handle + the workspace root (briefs). */
export interface ReadDeps {
  db: IndexDb;
  /** Workspace root — `/api/node/:id/brief` reads `<workspace>/<path>/_work.md`. */
  workspace: string;
}

/** Drop the internal `deletedAt` from a derived row — the wire carries only live rows. */
function live<T extends { deletedAt: number | null }>(row: T): Omit<T, "deletedAt"> {
  const { deletedAt: _tombstone, ...rest } = row;
  return rest;
}

/** A typed `not_found` refusal (API §4). */
function notFound(c: Context, message: string) {
  const body: ErrorResponse = { error: { code: "not_found", message, retryable: false } };
  return c.json(body, 404);
}

/**
 * Register the API §1 read routes on `app`, backed by `deps.db`. Idempotent per
 * app; called by `createApp` once an index handle exists. Every `/api/*` response
 * carries the `x-maestro-protocol` version header (API §versioning).
 */
export function registerReadRoutes(app: Hono, deps: ReadDeps): void {
  const { db, workspace } = deps;

  // Stamp the protocol version on every read response (API §versioning, D-NAME).
  app.use("/api/*", async (c, next) => {
    await next();
    c.header(X_MAESTRO_PROTOCOL, String(MAESTRO_PROTOCOL_VERSION));
  });

  // GET /api/tree — the work tree: live nodes, path-sorted (parent before child).
  app.get("/api/tree", async (c) => {
    const rows = await db.select().from(node).where(isNull(node.deletedAt)).orderBy(asc(node.path));
    const body: TreeResponse = { nodes: rows.map(live) };
    return c.json(body);
  });

  // GET /api/board — live nodes grouped by state, groups in D-ORDER (review first).
  app.get("/api/board", async (c) => {
    const rows = await db.select().from(node).where(isNull(node.deletedAt));
    const byState = new Map<OrchState, LiveNode[]>();
    for (const row of rows) {
      const list = byState.get(row.state);
      if (list) list.push(live(row));
      else byState.set(row.state, [live(row)]);
    }
    const groups: BoardGroup[] = [];
    for (const state of WK_GROUP_ORDER) {
      const nodes = byState.get(state);
      if (!nodes || nodes.length === 0) continue;
      // Within-group default recency (the authoritative attention key is BRO-1789's).
      nodes.sort((a, b) => b.updatedAt - a.updatedAt);
      groups.push({ state, nodes });
    }
    const body: BoardResponse = { groups };
    return c.json(body);
  });

  // GET /api/schedules — the orchestrator's bench: enabled routines, soonest first.
  // NULLS LAST explicitly: a hook/goal routine has no `nextFireAt`, and SQLite sorts
  // NULLs first on a plain ASC — which would float a no-scheduled-fire routine above an
  // imminent cron fire. Push nulls to the end so "soonest first" holds.
  app.get("/api/schedules", async (c) => {
    const rows = await db
      .select()
      .from(schedule)
      .where(and(eq(schedule.enabled, true), isNull(schedule.deletedAt)))
      .orderBy(sql`${schedule.nextFireAt} is null`, asc(schedule.nextFireAt));
    const body: SchedulesResponse = { schedules: rows.map(live) };
    return c.json(body);
  });

  // GET /api/node/:id/brief — the `_work.md` body (the look's source). Registered
  // before /api/node/:id is irrelevant (distinct trie paths), but kept adjacent.
  app.get("/api/node/:id/brief", async (c) => {
    const id = c.req.param("id");
    const [row] = await db
      .select()
      .from(node)
      .where(and(eq(node.id, id), isNull(node.deletedAt)))
      .limit(1);
    if (!row) return notFound(c, `node ${id} not found`);
    const absFile = join(workspace, row.path === "" ? WORK_FILE : join(row.path, WORK_FILE));
    let brief: string;
    try {
      brief = parseWorkInput(await readFile(absFile, "utf8")).brief;
    } catch {
      // The index says the node exists but its `_work.md` is unreadable (a vanished
      // file, a mid-scan race) — the brief is not available.
      return notFound(c, `brief for node ${id} is unavailable`);
    }
    const body: BriefResponse = { path: row.path, brief };
    return c.json(body);
  });

  // GET /api/node/:id — one node: its row + sessions (newest first) + their gates.
  app.get("/api/node/:id", async (c) => {
    const id = c.req.param("id");
    const [row] = await db
      .select()
      .from(node)
      .where(and(eq(node.id, id), isNull(node.deletedAt)))
      .limit(1);
    if (!row) return notFound(c, `node ${id} not found`);
    const sessions = await db
      .select()
      .from(session)
      .where(and(eq(session.nodeId, id), isNull(session.deletedAt)))
      .orderBy(desc(session.startedAt));
    const sessionIds = sessions.map((s) => s.id);
    // Gates join through the session (a node has no direct gate column).
    const gates = sessionIds.length
      ? await db
          .select()
          .from(gate)
          .where(and(inArray(gate.sessionId, sessionIds), isNull(gate.deletedAt)))
          .orderBy(desc(gate.openedAt))
      : [];
    const body: NodeDetail = {
      node: live(row),
      sessions: sessions.map(live),
      gates: gates.map(live),
    };
    return c.json(body);
  });

  // GET /api/sessions/:id — session row + diffstat receipt (carried on the row).
  app.get("/api/sessions/:id", async (c) => {
    const id = c.req.param("id");
    const [row] = await db
      .select()
      .from(session)
      .where(and(eq(session.id, id), isNull(session.deletedAt)))
      .limit(1);
    if (!row) return notFound(c, `session ${id} not found`);
    const body: SessionDetail = { session: live(row) };
    return c.json(body);
  });

  // GET /api/sessions/:id/events?after=<seq> — a page of the session timeline.
  // Per-session events only (`event where session_id = ?`); synthetics (null
  // sessionId) belong to the global stream, not a session timeline (DATA-MODEL §B.5).
  app.get("/api/sessions/:id/events", async (c) => {
    const id = c.req.param("id");
    const after = parseSeqCursor(c.req.query("after"));
    const rows = await db
      .select()
      .from(event)
      .where(and(eq(event.sessionId, id), gt(event.seq, after)))
      .orderBy(asc(event.seq))
      .limit(DEFAULT_EVENT_PAGE_SIZE + 1);
    const page = rows.slice(0, DEFAULT_EVENT_PAGE_SIZE);
    const hasMore = rows.length > DEFAULT_EVENT_PAGE_SIZE;
    // nextAfter is the last returned seq only when a further page exists; null at the
    // tail so the client switches to the SSE stream at that seq (BRO-1816).
    const last = page[page.length - 1];
    const nextAfter = hasMore && last ? last.seq : null;
    const body: EventPage = { events: page.map(toEnvelope), nextAfter };
    return c.json(body);
  });
}
