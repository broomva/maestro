// Work item — the read-side UI/wire PROJECTION of a work node.
//
// Distinct from WorkContract (./work.ts) and from the `node` index row: the chain
// is `_work.md` frontmatter = WorkContract (authoritative write side, the FS
// system-of-record) → `node` table row (derived index, DATA-MODEL §B.3) →
// WorkItem (this shape, the read side a client renders, derived from
// node + session + event). One shape; the board, feed, and inspector all derive
// from it (START-HERE §5 seam 2, data-contract §"The work item shape").
//
// It holds ZERO authoritative live state — `run_budget`/`lease` (the only
// authoritative rows, DATA-MODEL §B.1) never surface here — and is fully
// rebuildable from the FS.
//
// Deliberately NOT fields here (canon):
//  - `chat[]`  — chat is a SEPARATE projection (the UIMessage stream, the
//                ChatTransport seam); "a session renders work, it never owns it"
//                (data-contract §"The work model").
//  - `events[]`— the activity timeline is its own event-log subscription
//                (`event where session_id=?`, DATA-MODEL §B.5); the client joins the
//                SESSION timeline to the focused item by `sessionId`, never embeds
//                it. (Synthetic events have a null sessionId, so a node-scoped
//                timeline that includes them needs a nodeId join — see doc §7.)
//  - `budget`/`done`/`trigger` — engine-room internals kept off the read surface
//                by the disclosure ladder (CLAUDE.md §disclosure ladder).

import type { Kind } from "./intents";
import type { GateMode, OrchState } from "./state";

/** Where a worker runs (data-contract §"The work item shape"). */
export type WorkerLocation = "local worktree" | "cloud sandbox";

export const WORKER_LOCATIONS = [
  "local worktree",
  "cloud sandbox",
] as const satisfies readonly WorkerLocation[];

/** A run-branch ref — "the branch is the receipt" (data-contract §"The state machine"). */
export type RunBranch = `run/${string}`;

/**
 * A node's current-or-most-recent worker — `name` the agent id, `where` the isolation
 * mode. A SHAPE pin only: WHICH event payload or index column carries `{ name, where }`
 * is an upstream runtime/index concern (BRO-1790 / BRO-1754 / BRO-1796) — the canonical
 * `run.started` sketch (DATA-MODEL §A.3) does not yet enumerate them, and the `session`
 * row carries no name/location column.
 */
export interface WorkItemWorker {
  name: string;
  where: WorkerLocation;
}

/**
 * The gate compression — what the human sees at "Needs you" (data-contract
 * §"The work item shape"). Derived from the event log + the gate proposal.
 * `ran` is a receipt string ("2h 14m unsupervised · 41 events"), never a stored
 * percentage (CLAUDE.md "Never show progress percentages").
 */
export interface GateLook {
  ran: string;
  decided: string[];
  ask: string;
}

/**
 * The canonical work item — the read-side projection of a work node. Server-truth
 * fields mirror the `node` row (from `_work.md` frontmatter); derived fields are
 * computed from `session` + `event` and never stored authoritatively.
 */
export interface WorkItem {
  // ── server truth (node row, DATA-MODEL §B.3, from `_work.md` frontmatter) ──
  /** = node.id / frontmatter id — a stable UUID, survives renames (DATA-MODEL §A.2). */
  id: string;
  /** = node.state (references OrchState; not redefined). */
  state: OrchState;
  /** = node.kind (references Kind) — on the wire because the Standing overlay needs it. */
  kind: Kind;
  /** = node.title (first heading of `_work.md`). */
  title: string;
  /** = node.owner — `@handle` | `agent:name`. */
  owner?: string;
  /** = node.gate (references GateMode). */
  gate: GateMode;
  /** = node.path — workspace-relative folder path (the work tree). */
  path: string;
  /** = node.parentId — nesting is the work tree (DATA-MODEL §B.3). */
  parentId?: string | null;
  /**
   * = node.updatedAt, ISO-8601 on the wire. Present on EVERY node, so the universal
   * source for a card's relative age (`lastEventAt ?? updatedAt`) — the demo renders
   * an age on queued/proposed cards too, and those have no `lastEventAt`.
   */
  updatedAt: string;
  /**
   * frontmatter `created`, ISO date — sourced from `node.createdAt`, which BRO-1754's
   * merged `NodeRow` DOES carry (the DATA-MODEL §B.3 canon sketch omits it). OPTIONAL:
   * no read-side surface renders `created` today (a card's age routes through
   * `updatedAt`), so a consumer need not supply it — the "unconsumed field → optional"
   * rule (§7). Present it only where a view actually shows a creation date.
   */
  created?: string;

  // ── derived projections (data-contract §"The work item shape") ────────────
  /**
   * The node's current-or-most-recent session id — the session whose receipts the
   * inspector renders. A session is born only at dispatch (`triggered → running`).
   * DISPATCH-HISTORY-KEYED, NOT state-keyed: undefined ONLY if the node has never
   * been dispatched (no `session where node_id=? order by started_at desc limit 1`
   * row) — typically `proposed` / `reviewing` / never-fired `triggered` backlog work,
   * and work canceled before dispatch. Present on any node that has dispatched at
   * least once, INCLUDING a fired standing routine idling back at `triggered` (which
   * keeps its most-recent session so the routine's last-run receipts survive — the
   * receipts a Standing routine exists to surface) and a `done` node (so "the branch
   * is the receipt" survives completion). NOT live-only. The join key for the session
   * timeline (`event where session_id=?`).
   */
  sessionId?: string;
  /**
   * The id of the node's OPEN gate when `state ∈ {review, blocked}` (1:1 at the gate),
   * else undefined. The gate-queue JOIN + verb-DISPATCH key — the counterpart to
   * `sessionId` (the timeline join key): every gate verb (`approve` / `revise` /
   * `block` / `escalate` / `grant`) takes a `gateId` (intents.ts), so the rung-2
   * controls dispatch through THIS, never the node id (the runtime rejects a nodeId as
   * a gateId). The gate-card composition + queue ordering are BRO-1789's; this field is
   * only the key. `look` (below) is display-only.
   */
  gateId?: string;
  /** Ancestor initiative label — derived from the `parentId` ancestry chain. */
  initiative?: string;
  /** Ancestor project label — derived from the `parentId` ancestry chain. */
  project?: string;
  /**
   * ISO ts of the last *event* — present only once the node has events (dispatched
   * work). REFINES the card age for live/dispatched items; the universal relative age
   * falls back to the always-present `updatedAt` (`lastEventAt ?? updatedAt`), so a
   * queued/proposed card still renders an age (the demo's per-card `time`).
   */
  lastEventAt?: string;
  /** The current-or-most-recent session's worker (derived from the `session` row) — present on completed items too, not only live ones. */
  worker?: WorkItemWorker;
  /** The run branch — the receipt (= session.branch, DATA-MODEL §B.3). */
  run?: RunBranch;
  /** Judge output summary (derived from `verdict.md` / the VerdictReceipt on `check.verdict`). */
  verdict?: string;
  /** Blocked cause (derived from the blocking run event payload). */
  reason?: string;
  /** The gate compression shown at "Needs you". */
  look?: GateLook;
}

/**
 * The fields a WorkItem deliberately never carries — the read-surface exclusions
 * the disclosure ladder + the projection model pin (chat/events are separate
 * slices; budget/done/trigger are engine-room). Exported so the contract test and
 * downstream reducers can assert the shape stays clean.
 */
export const WORK_ITEM_EXCLUDED_FIELDS = ["chat", "events", "budget", "done", "trigger"] as const;
export type WorkItemExcludedField = (typeof WORK_ITEM_EXCLUDED_FIELDS)[number];

// ── Client persisted UI-prefs (porting-notes §State taxonomy) ────────────────
// The slice itself lives in apps/app; these typed keys are the shared vocabulary
// so the persisted-slice contract is expressed once. Components NEVER read
// localStorage directly — the single persisted slice replaces the ad-hoc keys
// `mc4-view` / `bv-nav-open` / `bv-ml-cols`.

/** The mission-plane view mode (was localStorage `mc4-view`; porting-notes). */
export type PlaneView = "feed" | "board" | "list";

export const PLANE_VIEWS = ["feed", "board", "list"] as const satisfies readonly PlaneView[];

/** The persisted UI-prefs slice keys — the three ad-hoc localStorage keys, absorbed. */
export const UI_PREF_KEYS = ["view", "navOpen", "cols"] as const;
export type UiPrefKey = (typeof UI_PREF_KEYS)[number];

/**
 * The three homes every prototype `useState` lands in (porting-notes §State
 * taxonomy). Doc-level contract for the client store; the slices live in apps/app.
 * Rule of thumb: lose-work-or-context → server-truth; lose-a-layout-pref →
 * persisted; nobody-notices → ephemeral.
 */
export type StoreSlice = "server-truth" | "persisted-ui-prefs" | "ephemeral";

export const STORE_SLICES = [
  "server-truth",
  "persisted-ui-prefs",
  "ephemeral",
] as const satisfies readonly StoreSlice[];
