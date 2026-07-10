// The event wire projection — the ONE place a stored `event` row becomes its
// `EventEnvelope` (API §"The event stream": the envelope is the row "verbatim").
//
// Lifted out of reads.ts (BRO-1812) so the read API's timeline pages
// (`/api/sessions/:id/events`) and the SSE stream (BRO-1816, `/api/stream`,
// `/api/sessions/:id/stream`) project a row IDENTICALLY — the seq a client pages
// to off `EventPage.nextAfter` and the seq it then resumes the stream from carry
// byte-identical payloads. A second projection here would let the two surfaces
// drift silently.

import type { EventEnvelope } from "@maestro/protocol";
import type { event } from "../db/schema";

/** Parse the numeric `payload_json`, tolerating a corrupt row (raw string, never a 500). */
export function parsePayload(raw: string | null): unknown {
  if (raw === null) return undefined;
  try {
    return JSON.parse(raw);
  } catch {
    return raw;
  }
}

/** Format epoch-ms to ISO, tolerating a corrupt/out-of-range `ts` (a sentinel, never a 500). */
export function toIso(ms: number): string {
  const d = new Date(ms);
  // A `ts` beyond the JS Date range (|ms| > 8.64e15) makes toISOString RangeError —
  // one corrupt row must not 500 a whole page NOR tear down a live stream mid-tail.
  return Number.isNaN(d.getTime()) ? new Date(0).toISOString() : d.toISOString();
}

/** Project a stored `event` row to its wire envelope (numeric ts → ISO, payload rehydrated). */
export function toEnvelope(row: typeof event.$inferSelect): EventEnvelope {
  return {
    seq: row.seq,
    sessionId: row.sessionId,
    ts: toIso(row.ts),
    actor: row.actor,
    type: row.type,
    payload: parsePayload(row.payload),
  };
}

/**
 * Parse a `seq` cursor (`?after=<seq>` query, or a `Last-Event-ID` header) — a
 * non-negative integer, defaulting to 0 (replay from the start). A malformed or
 * negative cursor is treated as 0 rather than rejected: a client that lost its
 * place re-hydrates the whole backlog, which is always safe.
 */
export function parseSeqCursor(raw: string | undefined | null): number {
  const n = Number(raw ?? 0);
  return Number.isInteger(n) && n >= 0 ? n : 0;
}
