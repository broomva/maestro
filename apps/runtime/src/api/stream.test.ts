/// <reference types="bun" />
// stream.test.ts — the BRO-1816 SSE contract suite (`bun test apps/runtime -t sse`).
//
// The done.check: "resume-from-cursor property test; two subscribers see identical
// order". Each stream is driven through the real Hono app (`app.request`) over a
// live `:memory:` index — the response `body` is a real ReadableStream, read frame
// by frame. A per-connection AbortController is threaded into the request `signal`
// so a client "disconnect" (`controller.abort()`) fires the SAME request-abort the
// tail loop watches in production — this is what the resume test exercises.

import { afterEach, describe, expect, test } from "bun:test";
import { type Actor, type EventType, MAESTRO_PROTOCOL_VERSION } from "@maestro/protocol";
import { createApp } from "../app";
import { DEFAULT_PORT, type RuntimeConfig } from "../config";
import { type IndexHandle, openIndex } from "../db/client";
import { event } from "../db/schema";

// ── Config + app fixtures ──────────────────────────────────────────────────────

/** A runtime config with a fast SSE cadence so tail delivery is prompt in tests. */
function cfg(overrides?: Partial<RuntimeConfig>): RuntimeConfig {
  return {
    port: DEFAULT_PORT,
    workspace: "/tmp/ws",
    indexPath: ":memory:",
    lockPath: "/tmp/ws/.maestro/runtime.lock",
    streamPollMs: 5,
    streamHeartbeatMs: 10_000, // effectively off unless a test overrides it
    ...overrides,
  };
}

const handles: IndexHandle[] = [];

/** Open a fresh `:memory:` index + its app, tracked for teardown. */
async function mkApp(overrides?: Partial<RuntimeConfig>) {
  const handle = await openIndex(":memory:");
  handles.push(handle);
  const app = createApp(cfg(overrides), Date.now(), handle.db);
  return { ...handle, app };
}

const settle = (ms = 30) => new Promise((r) => setTimeout(r, ms));

afterEach(async () => {
  // Let any dangling tail loop unwind (its next poll fails cleanly) before close.
  await settle();
  for (const h of handles.splice(0)) h.client.close();
});

// ── Event seeding ──────────────────────────────────────────────────────────────

type EventSeed = {
  sessionId?: string | null;
  type?: string;
  actor?: Actor;
  ts?: number;
  payload?: unknown;
};

/** Insert events in order — `seq` autoincrements 1..N in call order. */
async function seedEvents(h: IndexHandle, specs: EventSeed[]): Promise<void> {
  await h.db.insert(event).values(
    specs.map((s, i) => ({
      sessionId: s.sessionId === undefined ? "s1" : s.sessionId,
      ts: s.ts ?? 1000 + i,
      actor: s.actor ?? ("agent" as Actor),
      type: (s.type ?? "tool.call") as EventType,
      payload: s.payload === undefined ? null : JSON.stringify(s.payload),
    })),
  );
}

// ── SSE frame reading ──────────────────────────────────────────────────────────

type Frame = { id?: string; data?: string; comment: boolean };

const nonComment = (frames: Frame[]) => frames.filter((f) => !f.comment);

/** Parse one raw SSE frame (the text between `\n\n` delimiters). */
function parseFrame(raw: string): Frame {
  let id: string | undefined;
  const dataParts: string[] = [];
  let comment = false;
  for (const line of raw.split("\n")) {
    if (line.startsWith(":")) comment = true;
    else if (line.startsWith("id:")) id = line.slice(3).replace(/^ /, "");
    else if (line.startsWith("data:")) dataParts.push(line.slice(5).replace(/^ /, ""));
  }
  return { id, data: dataParts.length ? dataParts.join("\n") : undefined, comment };
}

const TIMED_OUT = Symbol("timed-out");

/** Race a promise against a timeout, resolving to a sentinel if it wins. */
function withTimeout<T>(p: Promise<T>, ms: number): Promise<T | typeof TIMED_OUT> {
  return Promise.race([
    p,
    new Promise<typeof TIMED_OUT>((r) => setTimeout(() => r(TIMED_OUT), Math.max(0, ms))),
  ]);
}

/**
 * Open an SSE stream on `path` and read frames until `until(frames)` holds (or the
 * timeout elapses), then abort the connection — the AbortController fires the
 * request signal, so the server tail loop ends promptly, exactly as a real client
 * disconnect would. Returns the response (for header assertions) + the frames read.
 */
async function collect(
  app: Awaited<ReturnType<typeof mkApp>>["app"],
  path: string,
  opts: {
    headers?: Record<string, string>;
    until: (frames: Frame[]) => boolean;
    timeoutMs?: number;
  },
): Promise<{ res: Response; frames: Frame[] }> {
  const controller = new AbortController();
  const res = await app.request(path, { headers: opts.headers, signal: controller.signal });
  const frames: Frame[] = [];
  if (!res.body) return { res, frames };
  const reader = res.body.getReader();
  const dec = new TextDecoder();
  const timeoutMs = opts.timeoutMs ?? 1500;
  const deadline = Date.now() + timeoutMs;
  let buf = "";
  try {
    while (!opts.until(frames) && Date.now() < deadline) {
      const read = await withTimeout(reader.read(), deadline - Date.now());
      if (read === TIMED_OUT || read.done) break;
      buf += dec.decode(read.value, { stream: true });
      let idx = buf.indexOf("\n\n");
      while (idx >= 0) {
        const raw = buf.slice(0, idx);
        buf = buf.slice(idx + 2);
        if (raw.length > 0) frames.push(parseFrame(raw));
        if (opts.until(frames)) break;
        idx = buf.indexOf("\n\n");
      }
    }
  } finally {
    controller.abort();
    await reader.cancel().catch(() => {});
  }
  return { res, frames };
}

/** The envelope carried in a data frame. */
const env = (f: Frame) => JSON.parse(f.data ?? "null");
const seqsOf = (frames: Frame[]) => nonComment(frames).map((f) => Number(f.id));

// ── The suite ──────────────────────────────────────────────────────────────────

describe("sse stream", () => {
  test("global stream replays events after Last-Event-ID in seq order", async () => {
    const h = await mkApp();
    await seedEvents(
      h,
      Array.from({ length: 8 }, (_, i) => ({ payload: { i } })),
    ); // seq 1..8
    const { frames } = await collect(h.app, "/api/stream", {
      headers: { "Last-Event-ID": "3" },
      until: (f) => nonComment(f).length >= 5,
    });
    expect(seqsOf(frames)).toEqual([4, 5, 6, 7, 8]);
    // Envelope carries the same seq + a rehydrated payload + an ISO ts.
    const data = nonComment(frames).map(env);
    expect(data.map((e) => e.seq)).toEqual([4, 5, 6, 7, 8]);
    expect(data[0].payload).toEqual({ i: 3 });
    expect(typeof data[0].ts).toBe("string");
  });

  test("global stream honors the ?after= cursor", async () => {
    const h = await mkApp();
    await seedEvents(
      h,
      Array.from({ length: 5 }, () => ({})),
    ); // seq 1..5
    const { frames } = await collect(h.app, "/api/stream?after=2", {
      until: (f) => nonComment(f).length >= 3,
    });
    expect(seqsOf(frames)).toEqual([3, 4, 5]);
  });

  test("Last-Event-ID overrides ?after= on reconnect (no double-delivery of the gap)", async () => {
    const h = await mkApp();
    await seedEvents(
      h,
      Array.from({ length: 5 }, () => ({})),
    ); // seq 1..5
    // A reconnect reuses the opening URL (?after=1) but carries the header — the
    // header must win, or the gap 2..3 replays twice.
    const { frames } = await collect(h.app, "/api/stream?after=1", {
      headers: { "Last-Event-ID": "3" },
      until: (f) => nonComment(f).length >= 2,
    });
    expect(seqsOf(frames)).toEqual([4, 5]);
  });

  test("per-session stream scopes to the session and excludes synthetics", async () => {
    const h = await mkApp();
    await seedEvents(h, [
      { sessionId: "s1", type: "run.started" }, // seq 1
      { sessionId: "s2", type: "tool.call" }, // seq 2
      { sessionId: null, type: "node.updated", actor: "system" }, // seq 3 — synthetic
      { sessionId: "s1", type: "tool.call" }, // seq 4
      { sessionId: "s2", type: "tool.call" }, // seq 5
      { sessionId: "s1", type: "check.result" }, // seq 6
    ]);
    const { frames } = await collect(h.app, "/api/sessions/s1/stream", {
      until: (f) => nonComment(f).length >= 3,
    });
    expect(seqsOf(frames)).toEqual([1, 4, 6]);
    const data = nonComment(frames).map(env);
    expect(data.every((e) => e.sessionId === "s1")).toBe(true);
    expect(data.some((e) => e.type === "node.updated")).toBe(false);
  });

  test("two subscribers see identical order", async () => {
    const h = await mkApp();
    await seedEvents(
      h,
      Array.from({ length: 6 }, () => ({})),
    ); // seq 1..6
    const [a, b] = await Promise.all([
      collect(h.app, "/api/stream", { until: (f) => nonComment(f).length >= 6 }),
      collect(h.app, "/api/stream", { until: (f) => nonComment(f).length >= 6 }),
    ]);
    expect(seqsOf(a.frames)).toEqual([1, 2, 3, 4, 5, 6]);
    expect(seqsOf(b.frames)).toEqual(seqsOf(a.frames));
  });

  test("live tail delivers an event inserted after the client is caught up", async () => {
    const h = await mkApp();
    await seedEvents(
      h,
      Array.from({ length: 3 }, () => ({})),
    ); // seq 1..3
    // Insert a fresh event ~10ms after opening the (already caught-up) stream.
    const late = (async () => {
      await settle(10);
      await seedEvents(h, [{ payload: { late: true } }]); // seq 4
    })();
    const { frames } = await collect(h.app, "/api/stream", {
      headers: { "Last-Event-ID": "3" },
      until: (f) => nonComment(f).length >= 1,
      timeoutMs: 2000,
    });
    await late;
    expect(seqsOf(frames)).toEqual([4]);
    const [delivered] = nonComment(frames);
    expect(delivered).toBeDefined();
    expect(env(delivered as Frame).payload).toEqual({ late: true });
  });

  test("resume from cursor has no gaps and no dupes across a reconnect", async () => {
    const h = await mkApp();
    await seedEvents(
      h,
      Array.from({ length: 8 }, () => ({})),
    ); // seq 1..8
    // First connection: read 4 frames, then disconnect at an arbitrary seq.
    const first = await collect(h.app, "/api/stream", { until: (f) => nonComment(f).length >= 4 });
    const seen1 = seqsOf(first.frames);
    expect(seen1).toEqual([1, 2, 3, 4]);
    // Reconnect from the last id seen (the browser sets Last-Event-ID automatically).
    const last = seen1[seen1.length - 1];
    const second = await collect(h.app, "/api/stream", {
      headers: { "Last-Event-ID": String(last) },
      until: (f) => nonComment(f).length >= 4,
    });
    const seen2 = seqsOf(second.frames);
    expect(seen2).toEqual([5, 6, 7, 8]);
    // The union is exactly 1..8 — no gap, no dupe.
    expect([...seen1, ...seen2]).toEqual([1, 2, 3, 4, 5, 6, 7, 8]);
  });

  test("an idle stream emits a heartbeat comment to keep proxies alive", async () => {
    const h = await mkApp({ streamHeartbeatMs: 10 });
    // No events → the stream is immediately idle and should heartbeat.
    const { frames } = await collect(h.app, "/api/stream", {
      until: (f) => f.some((x) => x.comment),
      timeoutMs: 1000,
    });
    expect(frames.some((f) => f.comment)).toBe(true);
  });

  test("the stream response carries the protocol + no-buffer headers", async () => {
    const h = await mkApp();
    await seedEvents(h, [{}]); // seq 1
    const { res } = await collect(h.app, "/api/stream", {
      until: (f) => nonComment(f).length >= 1,
    });
    expect(res.headers.get("x-maestro-protocol")).toBe(String(MAESTRO_PROTOCOL_VERSION));
    expect(res.headers.get("content-type")).toContain("text/event-stream");
    expect(res.headers.get("x-accel-buffering")).toBe("no");
  });

  test("without an index handle the stream routes 404 (compiled-binary degradation)", async () => {
    // Mirrors index.ts's catch path: createApp with no index → stream never mounted.
    const app = createApp(cfg(), Date.now());
    expect((await app.request("/api/stream")).status).toBe(404);
    expect((await app.request("/api/sessions/x/stream")).status).toBe(404);
  });
});
