// The read API wire surface — response envelopes for API §1 Reads (BRO-1812).
//
// These are the typed responses the runtime's read routes return and the client
// hydrates from (`/api/tree`, `/api/node/:id`, `/api/board`, …). They are
// COMPOSED from the index row shapes (index-schema.ts) — the read API is "cheap
// index queries" (API §1), so it ships the derived `node`/`session`/`event`/
// `gate`/`schedule` rows almost verbatim, with one transform:
//
//   The internal sync bookkeeping (`deletedAt`) is STRIPPED — a tombstoned row
//   never crosses the wire (API §1 "only live rows … tombstones are internal").
//   Every route filters `deletedAt IS NULL` server-side and returns a `Live*`.
//
// What this surface is NOT:
//   - It is NOT `WorkItem` (work-item.ts). `WorkItem` is the fully-DERIVED read
//     projection (title fallback, `look`, `worker`, `run`, `gateId`, initiative/
//     project ancestry) — that derivation is the projector's job (BRO-1775). The
//     read API serves the raw live rows; the client store projects them.
//   - Timestamps stay NUMERIC (epoch ms), exactly as the index stores them. The
//     ISO-8601 conversion (`WorkItem.updatedAt: string`) is again the projector's.
//
// Canon: API.md §1 Reads / §4 Error shape, DATA-MODEL §B.5 (the reactive queries),
// canon-amendments D-ORDER (the board attention order).

import type { EventEnvelope } from "./events";
import type { GateRow, NodeRow, ScheduleRow, SessionRow } from "./index-schema";
import type { OrchState } from "./state";

// ── Live row views — the wire projection of each derived table (deletedAt off) ─

/**
 * A `node` row on the wire: the full `NodeRow` minus `deletedAt`. Every read route
 * returns only live nodes (`deletedAt IS NULL`), so the tombstone marker is always
 * null here and is dropped from the type — the wire never carries a ghost card.
 */
export type LiveNode = Omit<NodeRow, "deletedAt">;

/** A `session` row on the wire — `SessionRow` minus the internal `deletedAt`. */
export type LiveSession = Omit<SessionRow, "deletedAt">;

/** A `gate` row on the wire — `GateRow` minus the internal `deletedAt`. */
export type LiveGate = Omit<GateRow, "deletedAt">;

/** A `schedule` row on the wire — `ScheduleRow` minus the internal `deletedAt`. */
export type LiveSchedule = Omit<ScheduleRow, "deletedAt">;

// ── GET /api/tree — the work tree (live node rows; nesting via `parentId`) ─────

/**
 * The work tree. A FLAT array of live nodes, sorted by `path` (a parent always
 * precedes its children, so a client folds the tree from `parentId` in one pass).
 * The board and the tree both derive from this same live-node set.
 */
export interface TreeResponse {
  nodes: LiveNode[];
}

// ── GET /api/node/:id — one node: its row + its sessions + its gates ───────────

/**
 * One node with the rows a client needs to render its inspector: the node row,
 * every session that ran against it (newest first), and every gate opened on
 * those sessions. Gates join through the session (`gate.sessionId` →
 * `session.id` → `session.nodeId`) — a node has no direct gate column.
 */
export interface NodeDetail {
  node: LiveNode;
  sessions: LiveSession[];
  gates: LiveGate[];
}

// ── GET /api/node/:id/brief — the `_work.md` body (the look's source) ──────────

/**
 * The raw `_work.md` body of a node — the source the gate "look" and the node
 * detail render from. `path` is the node's workspace-relative folder; `brief` is
 * the file body with the YAML frontmatter stripped (the human-readable prose).
 */
export interface BriefResponse {
  path: string;
  brief: string;
}

// ── GET /api/sessions/:id — session row + diffstat receipt ─────────────────────

/** One session — the run row plus its diffstat receipt (carried on the row itself). */
export interface SessionDetail {
  session: LiveSession;
}

// ── GET /api/sessions/:id/events?after=<seq> — a timeline page ─────────────────

/**
 * A page of a session's event timeline (`event where session_id = ? and seq >
 * after order by seq`, DATA-MODEL §B.5). `events` are wire envelopes (numeric row
 * `ts` formatted to ISO, `payload` rehydrated). `nextAfter` is the `seq` to pass
 * as the next `?after` cursor, or null when the page reached the tail — the client
 * pages the backlog, then switches to the SSE stream (BRO-1816) at the same seq.
 */
export interface EventPage {
  events: EventEnvelope[];
  nextAfter: number | null;
}

/** Default page size for a timeline page when the client sends no explicit limit. */
export const DEFAULT_EVENT_PAGE_SIZE = 200;

// ── GET /api/board — nodes grouped by state, attention order (D-ORDER) ─────────

/**
 * One board column: a state and its live nodes. Within a group the nodes are
 * ordered by `updatedAt` DESCENDING (most-recently-touched first) — a plain index
 * recency default. The AUTHORITATIVE within-group attention key for {review,
 * blocked} (a gate's `openedAt` / the block event ts, `compareGateQueue`) is
 * owned by seam-gate-queue (BRO-1789) + the board UI (BRO-1780); the read API does
 * not compute it. Cross-group order is the shared `compareByAttention` axis.
 */
export interface BoardGroup {
  state: OrchState;
  nodes: LiveNode[];
}

/**
 * The board — live nodes grouped by state, groups in D-ORDER attention order
 * (`WK_GROUP_ORDER`: review, blocked, running, triggered, reviewing, proposed,
 * done, canceled — review first). Only NON-EMPTY groups are returned (the board is
 * a faithful projection of what exists; the UI pads empty columns from the known
 * state set). "Needs you" is `count of review + blocked` — derivable from the
 * first groups without a separate field.
 */
export interface BoardResponse {
  groups: BoardGroup[];
}

// ── GET /api/schedules — the orchestrator's bench (enabled routines) ───────────

/** The bench: the enabled routines and their next fire (`schedule where enabled`). */
export interface SchedulesResponse {
  schedules: LiveSchedule[];
}

// ── GET /api/ledger?since=<ms>&until=<ms> — the autonomy scoreboard (BRO-1818) ─

/**
 * The autonomy ledger — the product's own KPI, DERIVED from the event log over the half-open window
 * `[since, until)` (epoch ms), NEVER a stored percentage (design canon "the branch is the receipt /
 * show receipts not percentages"; the product thesis is "the scarce resource is unsupervised hours").
 * There is no `ledger` table and no stored KPI column: every field here is computed on read from `event`
 * rows (`api/ledger` → `deriveLedger`), so it can never drift from the log it summarizes.
 *
 * - `unsupervisedMs` — wall-clock the system worked autonomously = the UNION of run-active intervals
 *   intersected with the window (union, not sum: parallel runs are one unsupervised hour, not many).
 * - `humanLooks` — a notch per human look in the window: a gate decision/escalation (actor "user") or a
 *   kill (`run.killed`). The scarce resource trends DOWN as autonomy improves.
 * - `activeRuns` — a receipt: how many runs are still live at `until`.
 * - `segments` / `notches` — the scoreboard bar geometry in POSITIONAL percent of the window (0–100): a
 *   timeline of the unsupervised stretches + the look marks. This is layout position, NOT a progress %.
 * - `label` — the plain-voice line the chrome renders verbatim ("2h 14m unsupervised · 3 looks"); no `%`.
 */
export interface LedgerResponse {
  since: number;
  until: number;
  unsupervisedMs: number;
  humanLooks: number;
  activeRuns: number;
  segments: LedgerBarSegment[];
  notches: number[];
  label: string;
}

/** One unsupervised stretch on the scoreboard bar — positional percent of the window (0–100), never a
 *  progress percentage. `live` marks the stretch still running at the window end. */
export interface LedgerBarSegment {
  start: number;
  width: number;
  live?: boolean;
}
