// The server-truth reducer (BRO-1775) — pure functions that fold the event stream
// (and the initial read-API hydration) into the raw-row server-truth slice. The
// store's `applyEvent` action is a thin wrapper over `applyEvent` here, so the whole
// server-truth transition is unit-testable by replaying a recorded event stream
// (the done.check) with no store, no React, no network.

import type { EventEnvelope, LiveGate, LiveNode, LiveSession } from "@maestro/protocol";
import type { GatePayload, NodeUpdatedPayload, ScheduleFiredPayload, ServerTruth } from "./types";

/** Rows the initial read-API hydration seeds (the backlog the client fetches once). */
export interface HydrateRows {
  nodes?: LiveNode[];
  sessions?: LiveSession[];
  gates?: LiveGate[];
  /** the max `event.seq` the hydration reflects — the stream resumes after this. */
  cursor?: number;
}

/** Seed the server-truth slice from the read API (`/api/tree`, `/api/node/:id`). */
export function hydrate(state: ServerTruth, rows: HydrateRows): ServerTruth {
  const nodes = { ...state.nodes };
  for (const n of rows.nodes ?? []) nodes[n.id] = n;
  const sessions = { ...state.sessions };
  for (const s of rows.sessions ?? []) sessions[s.id] = s;
  const gates = { ...state.gates };
  for (const g of rows.gates ?? []) gates[g.id] = g;
  return {
    ...state,
    nodes,
    sessions,
    gates,
    cursor: rows.cursor ?? state.cursor,
  };
}

/**
 * Fold one wire event into the server-truth slice. Idempotent under re-delivery:
 * an event whose `seq` is at or behind the cursor is a no-op (the same
 * gapless-resume guarantee the SSE stream upholds — a reconnect that replays the
 * boundary event must not double-apply it). Returns the SAME reference when nothing
 * changed, so a store `set` is a genuine no-op (no needless re-render).
 */
export function applyEvent(state: ServerTruth, e: EventEnvelope): ServerTruth {
  // Already applied (or a stale re-delivery) — no-op, cursor unchanged.
  if (e.seq <= state.cursor) return state;
  const next: ServerTruth = { ...state, cursor: e.seq };

  switch (e.type) {
    case "node.updated": {
      const node = e.payload as NodeUpdatedPayload | undefined;
      if (node?.id) next.nodes = { ...state.nodes, [node.id]: node };
      break;
    }
    case "gate.opened":
    case "gate.decided": {
      const gate = e.payload as GatePayload | undefined;
      if (gate?.id) next.gates = { ...state.gates, [gate.id]: gate };
      break;
    }
    case "schedule.fired": {
      const p = e.payload as ScheduleFiredPayload | undefined;
      if (p?.scheduleId) {
        next.ticks = [
          ...state.ticks,
          { scheduleId: p.scheduleId, nodeId: p.nodeId, firedAt: p.firedAt ?? e.ts, seq: e.seq },
        ];
      }
      break;
    }
    default: {
      // Any other event is a session-scoped log line (run.* / tool.* / check.* /
      // the namespaced gate.*). It carries no row to upsert here, but it advances
      // the node's card age: record its ts against the session (the projector maps
      // session → node). Synthetics have a null sessionId and are skipped.
      if (e.sessionId) {
        next.lastEventAt = { ...state.lastEventAt, [e.sessionId]: e.ts };
      }
      break;
    }
  }
  return next;
}

/** Fold an ordered batch of events (the backlog replay + live tail) in one pass. */
export function applyEvents(state: ServerTruth, events: readonly EventEnvelope[]): ServerTruth {
  let acc = state;
  for (const e of events) acc = applyEvent(acc, e);
  return acc;
}
