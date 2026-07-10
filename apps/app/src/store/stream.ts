// The event-stream subscription (BRO-1775) — the thin glue that feeds the
// server-truth slice from the runtime SSE stream (BRO-1816). Hydrate the backlog
// once off the read API, then live off the stream: exactly the API §1 contract
// ("clients hydrate once, then live off the stream").
//
// Resume is the browser-native SSE contract, and it composes with the runtime's
// cursor precedence: on an EventSource auto-reconnect the browser sends
// `Last-Event-ID: <last seq>`, which the runtime prioritises over the stale
// `?after` (the BRO-1816 precedence fix), so a mid-stream reconnect resumes with no
// gap, no dupe.
//
// KNOWN LIMITATIONS (BRO-1845 — stream resume hardening, out of the P1 skeleton):
//   1. The FIRST open passes `?after=<cursor>` where the cursor is 0 after a fresh
//      `/api/tree` hydrate (the tree carries no watermark seq). Until a P2 event
//      writer exists the event table is empty, so this delivers nothing; once it
//      does, the client re-replays the whole backlog over the hydrated tree each
//      load (idempotent — node.updated is a full-row upsert — so it CONVERGES, but
//      it is wasteful). The fix is a watermark seq on `/api/tree` (or paging events
//      instead of hydrating the tree).
//   2. Native EventSource commits `lastEventId` from the `id:` field BEFORE firing
//      `onmessage`, independent of whether our handler throws. A frame that fails
//      to apply is thus skipped AND (because the runtime honours the browser's
//      `Last-Event-ID`) not replayed on reconnect — a gap. Malformed frames do not
//      occur against a correct runtime; the robust fix is a fetch-based reader that
//      advances the cursor only on a successful apply (the injected factory already
//      supports this shape).
//
// Injectable `fetchImpl` / `eventSourceFactory` keep it unit-testable without a DOM.

import type { EventEnvelope, LiveNode } from "@maestro/protocol";
import type { MaestroStoreApi } from "./store";

/** The minimal EventSource surface this module uses (a subset of the DOM type). */
export interface EventSourceLike {
  onmessage: ((ev: { data: string }) => void) | null;
  onerror: ((ev: unknown) => void) | null;
  close(): void;
}
export type EventSourceFactory = (url: string) => EventSourceLike;

export interface ConnectOptions {
  /** Runtime origin, e.g. `http://localhost:4319`. Default `""` (same origin). */
  baseUrl?: string;
  /** When set, subscribe to one session's stream instead of the global feed. */
  sessionId?: string;
  /** Hydrate `/api/tree` before subscribing (default true). */
  hydrate?: boolean;
  /** Injected `fetch` (default the global). */
  fetchImpl?: typeof fetch;
  /** Injected EventSource factory (default `new EventSource(url)`). */
  eventSourceFactory?: EventSourceFactory;
  /**
   * Notified on a stream error / a failed hydrate. Native EventSource then auto-
   * reconnects on a TRANSIENT drop (after an established connection); a fatal
   * handshake failure (non-2xx / wrong content-type) closes for good — the caller's
   * concern, not retried here.
   */
  onError?: (err: unknown) => void;
}

/** A live subscription — call `close()` to end it. */
export interface StreamHandle {
  close(): void;
}

/** The runtime `/api/tree` response (the read API's `TreeResponse`). */
interface TreeResponse {
  nodes: LiveNode[];
}

/**
 * Subscribe the store to the runtime event stream: hydrate the node backlog off
 * `/api/tree`, then apply every streamed event through `store.applyEvent`. Returns
 * a handle whose `close()` ends the subscription. Safe to call once at app start
 * (BRO-1780 wires it into the SPA shell).
 */
export function connectStream(store: MaestroStoreApi, opts: ConnectOptions = {}): StreamHandle {
  const {
    baseUrl = "",
    sessionId,
    hydrate = true,
    fetchImpl = fetch,
    eventSourceFactory = (url: string) => new EventSource(url) as unknown as EventSourceLike,
    onError,
  } = opts;

  let source: EventSourceLike | null = null;
  let closed = false;

  const subscribe = () => {
    if (closed) return;
    // Resume from the highest seq the store has applied (0 after a fresh hydrate).
    const after = store.getState().server.cursor;
    const path = sessionId
      ? `/api/sessions/${encodeURIComponent(sessionId)}/stream`
      : "/api/stream";
    source = eventSourceFactory(`${baseUrl}${path}?after=${after}`);
    source.onmessage = (ev) => {
      try {
        store.getState().applyEvent(JSON.parse(ev.data) as EventEnvelope);
      } catch (err) {
        // A malformed frame must not tear down the subscription.
        onError?.(err);
      }
    };
    source.onerror = (err) => onError?.(err);
  };

  if (hydrate) {
    fetchImpl(`${baseUrl}/api/tree`)
      .then((r) => {
        // fetch() resolves for any HTTP status — guard on r.ok so a 5xx/HTML body
        // does not reach r.json() and silently skip hydration.
        if (!r.ok) throw new Error(`hydrate /api/tree failed: ${r.status}`);
        return r.json() as Promise<TreeResponse>;
      })
      .then((body) => {
        if (!closed) store.getState().hydrate({ nodes: body.nodes });
      })
      .catch((err) => onError?.(err))
      .finally(subscribe);
  } else {
    subscribe();
  }

  return {
    close() {
      closed = true;
      source?.close();
      source = null;
    },
  };
}
