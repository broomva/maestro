// The runtime event stream (BRO-1816) — the two SSE routes of API §"The event
// stream", hung on the same Hono app + open index handle as the read API. Clients
// hydrate the backlog once off the read API (`/api/sessions/:id/events`, paging on
// `EventPage.nextAfter`), then switch to a stream at that same `seq` and live off
// it — "clients hydrate once, then live off the stream" (API §1).
//
//   GET /api/stream              global — every event (all sessions + synthetics)
//   GET /api/sessions/:id/stream one session's timeline
//
// The wire frame is one `EventEnvelope` per SSE message (projected by the shared
// `toEnvelope`, byte-identical to the read API), with the row's `seq` as the SSE
// `id:`. Resume is the browser-native SSE contract: on reconnect the client sends
// `Last-Event-ID: <seq>` and the runtime replays `event where seq > that` in order.
// `seq` is the index autoincrement — a total order with no gaps (DATA-MODEL §B.5),
// so replay is exact: no gaps, no dupes, no client-side sorting.
//
// ── Why poll-the-tail, not an in-process event bus ────────────────────────────
// Each connection independently tails the index: `select event where seq > cursor
// order by seq`, advancing `cursor` past every row it sends, sleeping `pollMs`
// between empty polls. This is deliberate for P1:
//   * Correctness by construction — a single monotonic cursor over one totally-
//     ordered table means "two subscribers see identical order" and "resume has no
//     gaps/dupes" fall out for free, with no replay-vs-live boundary to de-dupe.
//   * No cross-cutting bus contract — P1 has no event *writers* yet (they land in
//     P2: the session loops, gate transitions, and the scanner's synthetic
//     journal). A push bus would force every future writer to also publish; the
//     tailer instead picks up anything that lands in `event`, matching "the FS/
//     index is the system of record" (ARCHITECTURE §4).
//   * Backpressure-safe per connection — `await stream.writeSSE(...)` suspends on a
//     slow consumer; one stuck client slows only its own loop, never the others
//     (there is no shared queue to overflow).
//
// WATERMARK SAFETY (the one load-bearing invariant): a `seq > cursor` tailer is
// gapless ONLY if commits happen in `seq` order — i.e. a lower `seq` is never
// committed *after* the tailer has already advanced past it. The index is
// single-writer (DATA-MODEL §B.5: "runtime-local, single-writer") and SQLite
// serializes writes on the one runtime connection, so `seq` assignment order IS
// commit order. Every P2 event writer MUST keep this: insert events only through
// the runtime's single serialized index handle. If a future design admits
// concurrent writers, this tailer needs a committed-watermark (min in-flight seq),
// not a raw MAX — flagged here so the invariant is not lost.
//
// KNOWN LIMITATION — rebuild-scoped seq (BRO-1844 follow-up): `seq` values are
// rebuild-scoped and renumber on an index rebuild (schema.ts). A client that
// reconnects with a `Last-Event-ID` from BEFORE a rebuild carries a cursor above
// the new max seq, so its tail silently delivers nothing (only heartbeats) until
// seq climbs past the stale cursor. It is indistinguishable here from the normal
// "caught up, waiting at the tip" case without a stream generation/epoch marker —
// out of P1 scope (there are no event writers or rebuilds in the live path yet). A
// full client reload recovers (a fresh EventSource carries no Last-Event-ID).
//
// A shared single-poller hub that fans one query out to N subscribers is the
// obvious scale optimization (fewer DB polls), but it is not needed for P1
// correctness and it reintroduces the boundary-dedup this design avoids — deferred
// until subscriber counts justify it.

import { MAESTRO_PROTOCOL_VERSION, X_MAESTRO_PROTOCOL } from "@maestro/protocol";
import { and, asc, eq, gt, type SQL } from "drizzle-orm";
import type { Context, Hono } from "hono";
import type { SSEStreamingApi } from "hono/streaming";
import { streamSSE } from "hono/streaming";
import type { IndexDb } from "../db/client";
import { event } from "../db/schema";
import { parseSeqCursor, toEnvelope } from "./event-projection";

/** How long a caught-up connection waits between tail polls (dev/test override it). */
export const DEFAULT_STREAM_POLL_MS = 250;
/** How long a connection stays silent before a heartbeat comment keeps proxies alive. */
export const DEFAULT_STREAM_HEARTBEAT_MS = 15_000;
/** Rows drained per tail query — the backlog is replayed in batches this size. */
const DRAIN_BATCH = 500;
/** An SSE comment frame (line starting `:`) — a no-op keepalive the client ignores. */
const HEARTBEAT_FRAME = ": hb\n\n";

/** What the stream routes need: the open index handle + tuned poll/heartbeat cadence. */
export interface StreamDeps {
  db: IndexDb;
  /** Tail poll interval in ms (default {@link DEFAULT_STREAM_POLL_MS}). */
  pollMs?: number;
  /** Idle heartbeat interval in ms (default {@link DEFAULT_STREAM_HEARTBEAT_MS}). */
  heartbeatMs?: number;
}

/**
 * A `setTimeout` that also resolves the instant `signal` aborts — so a client
 * disconnect ends the tail promptly instead of after a full `pollMs`. On current
 * Bun, `streamSSE` does NOT wire the request-abort signal into `stream.aborted`
 * (only on old Bun), so the tail loop watches the raw request signal directly.
 *
 * The abort listener is removed on BOTH exit paths — `{ once: true }` only auto-
 * removes it *after* it fires, so the common timer-completes path must remove it
 * explicitly. Without this, a 24/7 connection accretes one dead listener per poll
 * on the request-lifetime signal (MaxListeners warning + O(n) eventual abort).
 */
export function abortableSleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise<void>((resolve) => {
    if (signal.aborted) return resolve();
    let timer: ReturnType<typeof setTimeout>;
    const onAbort = () => {
      clearTimeout(timer);
      resolve();
    };
    timer = setTimeout(() => {
      signal.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    signal.addEventListener("abort", onAbort, { once: true });
  });
}

/** The `where` predicate for a tail cursor — global (all rows) or one session. */
function tailWhere(sessionId: string | undefined, cursor: number): SQL | undefined {
  return sessionId === undefined
    ? gt(event.seq, cursor)
    : and(eq(event.sessionId, sessionId), gt(event.seq, cursor));
}

/**
 * Tail the `event` table from `startCursor` onward, writing each row as an SSE
 * frame until the client disconnects. Replays the backlog (`seq > startCursor`) in
 * `DRAIN_BATCH` chunks with no sleep, then settles into a `pollMs` live tail;
 * emits a heartbeat comment after `heartbeatMs` of silence. Every projected write
 * is guarded — a mid-tail index error or a closed stream ends the loop cleanly
 * (never a thrown, logged error on a client that simply went away).
 */
async function tailEvents(
  stream: SSEStreamingApi,
  signal: AbortSignal,
  opts: {
    db: IndexDb;
    sessionId: string | undefined;
    startCursor: number;
    pollMs: number;
    heartbeatMs: number;
  },
): Promise<void> {
  const { db, sessionId, startCursor, pollMs, heartbeatMs } = opts;
  let cursor = startCursor;
  let lastWrite = Date.now();

  while (!signal.aborted && !stream.aborted) {
    let rows: (typeof event.$inferSelect)[];
    try {
      rows = await db
        .select()
        .from(event)
        .where(tailWhere(sessionId, cursor))
        .orderBy(asc(event.seq))
        .limit(DRAIN_BATCH);
    } catch {
      // The index became unreadable mid-tail — close the stream rather than spin.
      return;
    }

    try {
      if (rows.length > 0) {
        for (const row of rows) {
          await stream.writeSSE({ id: String(row.seq), data: JSON.stringify(toEnvelope(row)) });
          cursor = row.seq;
        }
        lastWrite = Date.now();
        // A full batch means the backlog is still draining — re-query immediately.
        if (rows.length === DRAIN_BATCH) continue;
      } else if (Date.now() - lastWrite >= heartbeatMs) {
        await stream.write(HEARTBEAT_FRAME);
        lastWrite = Date.now();
      }
    } catch {
      // The client went away between polls — the write failed; end the tail.
      return;
    }

    await abortableSleep(pollMs, signal);
  }
}

/** Open an SSE tail on `c`, stamping the protocol + no-buffer headers first. */
function openStream(c: Context, deps: StreamDeps, sessionId: string | undefined) {
  const { db } = deps;
  // A non-positive cadence (or one omitted) falls back to the default rather than
  // busy-looping: `pollMs: 0` on the programmatic path would `setTimeout(0)`-spin
  // (the env path is already guarded in loadConfig).
  const pollMs = deps.pollMs && deps.pollMs > 0 ? deps.pollMs : DEFAULT_STREAM_POLL_MS;
  const heartbeatMs =
    deps.heartbeatMs && deps.heartbeatMs > 0 ? deps.heartbeatMs : DEFAULT_STREAM_HEARTBEAT_MS;
  // Last-Event-ID (set automatically by EventSource on reconnect) WINS over an
  // explicit `?after=` — otherwise a reconnect that reuses the opening URL would
  // replay from the stale initial cursor and double-deliver the gap. An EMPTY
  // header (a non-native client or a proxy injecting `Last-Event-ID:`) is treated
  // as ABSENT — otherwise `parseSeqCursor("")` → 0 would re-deliver the whole
  // backlog, the exact double-delivery this precedence rule prevents.
  const resume = c.req.header("Last-Event-ID");
  const startCursor =
    resume != null && resume !== "" ? parseSeqCursor(resume) : parseSeqCursor(c.req.query("after"));
  const signal = c.req.raw.signal;
  // Set before streamSSE builds the Response: version parity with the read API, and
  // X-Accel-Buffering:no so an nginx/relay hop forwards the stream unbuffered
  // (API §3 "SSE passes through unbuffered").
  c.header(X_MAESTRO_PROTOCOL, String(MAESTRO_PROTOCOL_VERSION));
  c.header("X-Accel-Buffering", "no");
  return streamSSE(c, (stream) =>
    tailEvents(stream, signal, { db, sessionId, startCursor, pollMs, heartbeatMs }),
  );
}

/**
 * Register the API §"The event stream" SSE routes on `app`, backed by `deps.db`.
 * Called by `createApp` alongside `registerReadRoutes` once an index handle exists
 * — both ride the same `if (index)` gate, so the compiled-binary /health-only
 * degradation (no libSQL addon) drops the stream too (404), never a crash.
 */
export function registerStreamRoutes(app: Hono, deps: StreamDeps): void {
  // GET /api/stream — the global feed: every event, node change, gate arrival.
  app.get("/api/stream", (c) => openStream(c, deps, undefined));

  // GET /api/sessions/:id/stream — one session's timeline (synthetics have a null
  // sessionId and so never match a session filter — consistent with the events page).
  app.get("/api/sessions/:id/stream", (c) => openStream(c, deps, c.req.param("id")));
}
