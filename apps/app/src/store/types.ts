// The client store types (BRO-1775) — the three-slice taxonomy from
// docs/contracts/work-item-store.md §5 + porting-notes §"State taxonomy".
//
//   1. Server truth   — fed ONLY by the event-log subscription (+ intents); the
//                        raw index rows the projector derives WorkItems from.
//   2. Persisted prefs — the store's single persisted slice (replaces the ad-hoc
//                        localStorage keys mc4-view / bv-nav-open / bv-ml-cols).
//   3. Ephemeral       — stays component-local `useState` (NOT here): hover, drag,
//                        scope (tree rung), open item. Selection's server-truth
//                        half (open sessions) lives in the server slice per §6.
//
// The store holds ZERO derived WorkItems — it holds the raw `node`/`session`/
// `gate` live rows and the projector (`project.ts`) derives WorkItems on read, so
// there is one derivation path and it stays the projector's (contract §3, BRO-1775).

import type { Intent, LiveGate, LiveNode, LiveSession, PlaneView } from "@maestro/protocol";

// ── The P1 synthetic-event payload contract (client-expected) ────────────────
// P1 has no event *writers* yet (they land in P2). These are the payload shapes
// the reducer EXPECTS on the closed synthetic list (events.ts SYNTHETIC_EVENT_TYPES)
// — a seam this ticket declares for the client side, to be matched by the P2
// writers. Kept minimal + row-carrying so the server-truth slice is rebuildable by
// replaying the stream alone (contract §4 "fed ONLY by the event-log subscription").

/** `node.updated` — carries the updated LIVE node row (an upsert; tombstones never cross the wire). */
export type NodeUpdatedPayload = LiveNode;

/** `gate.opened` / `gate.decided` — carries the LIVE gate row (opened → pending, decided → verdict set). */
export type GatePayload = LiveGate;

/** `schedule.fired` — the tick projection source (the rail's wake log). */
export interface ScheduleFiredPayload {
  scheduleId: string;
  nodeId: string;
  /** ISO ts of the fire; falls back to the event `ts` when absent. */
  firedAt?: string;
}

// ── Server-truth slice ───────────────────────────────────────────────────────

/** A `schedule.fired` projection — one entry in the orchestrator's wake log. */
export interface TickEntry {
  scheduleId: string;
  nodeId: string;
  /** ISO ts. */
  firedAt: string;
  /** the event `seq` that produced it (stable ordering key). */
  seq: number;
}

/**
 * A gate's grace window — a human verb chosen but NOT yet sent to the runtime
 * (the undo timer is still running). This is intent-not-yet-dispatched, which is
 * store/transport logic, not component state (porting-notes §State taxonomy).
 */
export interface GateGraceEntry {
  intent: Intent;
  /** epoch ms after which the intent is actually sent (the undo deadline). */
  undoAt: number;
}

/**
 * The server-truth slice — raw live index rows + the derived aggregates the UI
 * needs, fed by the event stream. Never mutated by components (only via
 * `applyEvent` / intents). Holds no WorkItems (the projector derives those).
 */
export interface ServerTruth {
  /** live `node` rows, keyed by id (the projector filters/derives WorkItems). */
  nodes: Record<string, LiveNode>;
  /** live `session` rows, keyed by id (the node ↔ session 1:many join). */
  sessions: Record<string, LiveSession>;
  /** live `gate` rows, keyed by id (joined to a node through the session). */
  gates: Record<string, LiveGate>;
  /** sessionId → ISO ts of that session's latest event (refines a card's age). */
  lastEventAt: Record<string, string>;
  /** the orchestrator's wake log (schedule.fired projections), oldest first. */
  ticks: TickEntry[];
  /** open sessions — server truth, "sessions come from the runtime" (contract §6). */
  openSessionIds: string[];
  /** the focused open session (the chat tab in view). */
  activeSessionId: string | null;
  /** open workspace files (the FS pane's tabs). */
  openFilePaths: string[];
  /** gateId → a chosen-but-unsent verb + its undo deadline (the grace window). */
  gateGrace: Record<string, GateGraceEntry>;
  /** the last applied `event.seq` — the Last-Event-ID resume cursor (DATA-MODEL §B.5). */
  cursor: number;
}

/** The empty server-truth slice — a fresh, un-hydrated store. */
export const emptyServerTruth = (): ServerTruth => ({
  nodes: {},
  sessions: {},
  gates: {},
  lastEventAt: {},
  ticks: [],
  openSessionIds: [],
  activeSessionId: null,
  openFilePaths: [],
  gateGrace: {},
  cursor: 0,
});

// ── Persisted UI-prefs slice ─────────────────────────────────────────────────

/**
 * The single persisted slice — the three ad-hoc localStorage keys, absorbed
 * (contract §5, UI_PREF_KEYS). The ONLY slice `persist` writes to storage;
 * server-truth never touches localStorage (the load-bearing invariant of §5).
 */
export interface Prefs {
  /** mission-plane view (was `mc4-view`), default `feed`. */
  view: PlaneView;
  /** sidebar open (was `bv-nav-open`), default true. */
  navOpen: boolean;
  /** column widths (was `bv-ml-cols`), default `{ nav: 200 }`. */
  cols: Record<string, number>;
  /**
   * FS pane open (BRO-1890 FID-4) — the chrome-level file pane at the layout's right edge. A layout
   * preference (rule-of-thumb: losing it loses only a layout choice → persisted slice), default true.
   * The prototype's responsive auto-collapse is deferred to FID-8 (mobile).
   */
  fsOpen: boolean;
}

/** The persisted-slice defaults (the prototype's localStorage defaults). */
export const defaultPrefs = (): Prefs => ({
  view: "feed",
  navOpen: true,
  cols: { nav: 200 },
  fsOpen: true,
});
