// Gate queue — the derived attention view + verdict semantics + the grace window.
//
// The gate queue is a DERIVED VIEW over nodes in the attention set (state ∈
// {review, blocked} = `ATTENTION_STATES`) — never a separate store. Its comparator
// REUSES the shared board axis (`compareByAttention`, D-ORDER, DATA-MODEL §B.5) for
// cross-group order — it does NOT re-order the whole board on its own (the board's
// non-attention groups carry their own within-group recency key; see `compareGateQueue`).
// This seam pins: the comparator, the `data-gate` card payload, the verdict→verb +
// terminating semantics, the closed `GateKind`, and the grace-window undo state machine.
//
// Cross-seam (single-source, learned from BRO-1764's review):
//   - membership REFERENCES `ATTENTION_STATES` (state.ts) — never a duplicate set;
//   - the comparator COMPOSES `compareByAttention` (plain-voice.ts) — never re-declared;
//   - `GateKind` is widened in its home (work.ts); this file imports it;
//   - `GateVerdict` (state.ts) + `GateLook` (work-item.ts) are imported, not redefined;
//   - `MaestroDataParts.gate` is added by MODULE AUGMENTATION of chat.ts (BRO-1776 left
//     it out) — no edit to chat.ts; the barrel `export * from "./gate"` makes it reach
//     the composition site.
//
// Canon: DATA-MODEL §B.3 gate / §B.5, PATTERNS §6, FLOWS §F5, porting-notes
// §MccMaestroLoopV2 (grace window), START-HERE §5 seam 3, canon-amendments D-GATE.

import { compareByAttention } from "./plain-voice";
import { ATTENTION_STATES, type GateVerdict, type OrchState } from "./state";
import type { GateKind } from "./work";
import type { GateLook } from "./work-item";

// ── Membership: the gate queue IS the attention set (single-source) ────────────

/**
 * True iff a node is in the gate queue — i.e. it needs a human. This IS the attention
 * set (`ATTENTION_STATES` = {review, blocked}), REFERENCED not re-declared, so the queue
 * can never drift from the canonical states.
 *
 * NOTE: `blocked` is IN the queue (Stuck needs you) but is NOT gate-decidable — only
 * `review` carries an open gate + a `gateId` + the four verdicts (`resolveGateVerdict`
 * throws off-review). A blocked card offers unblock / redispatch, not a verdict.
 */
export const isInGateQueue = (state: OrchState): boolean =>
  (ATTENTION_STATES as readonly OrchState[]).includes(state);

// ── The comparator (reuses the shared board axis, BRO-1780) ────────────────────

/** The minimal shape the gate-queue comparator reads. */
export interface GateQueueOrder {
  /** = node.state — must be in the attention set to appear in the gate queue. */
  state: OrchState;
  /**
   * Epoch ms the node ENTERED its attention state — a gate's `openedAt` (review) or the
   * block event ts (blocked). NOT `createdAt`: sorting the attention queue by creation
   * time buries freshly-actionable old work (the BRO-1764 §8 sort-key decoupling). A
   * finite epoch (ms); the runtime supplies `openedAt ?? blockedAt`. Defined ONLY for
   * the attention set {review, blocked} — the board's other groups (running, done, …)
   * sort within-group by their own recency key, not this (see `compareGateQueue`).
   */
  attentionSince: number;
}

/**
 * The GATE-QUEUE comparator (D-ORDER, DATA-MODEL §B.5) — a total order over the
 * attention set {review, blocked}, NOT a whole-board sort. Cross-group order is
 * `compareByAttention` (the shared board axis: review before blocked, then the rest —
 * the shipped protocol comparator, REFERENCED not re-declared, valid over all 8 states).
 * Within a group, OLDEST-waiting first (ascending `attentionSince`) — the ticket's "age
 * descending": the gate that has waited longest for a human sits at the top so no gate
 * rots at the bottom.
 *
 * The board (BRO-1780) REUSES the shared `compareByAttention` axis for grouping and
 * supplies its OWN within-group recency key per group (a `running` node has no gate
 * `openedAt`, so `attentionSince` is not its sort key). Do NOT `nodes.sort(compareGateQueue)`
 * over non-attention nodes — the cross-group order is right, but the within-group tiebreak
 * is only meaningful for {review, blocked}. Total order on finite `attentionSince` (a real
 * epoch ms; non-finite is out of contract — the runtime never emits it).
 */
export const compareGateQueue = (a: GateQueueOrder, b: GateQueueOrder): number =>
  compareByAttention(a.state, b.state) || a.attentionSince - b.attentionSince;

// ── The `data-gate` card payload (this seam owns it; chat.ts left it out) ───────

/**
 * The F5 gate "look" card — the `data-gate` part payload, reconciled across every open
 * client by `gateId` (FLOWS F5). `look` is the display compression (what changed · what
 * it decided · what it asks), sourced from receipts. Registered on `MaestroDataParts`
 * by the module augmentation below.
 */
export interface GateCard {
  /** the open gate's id — the verb-dispatch key (present only at `review`). */
  gateId: string;
  /** the gate kind (closed enum, work.ts). */
  kind: GateKind;
  /** the display compression (BRO-1764 `GateLook`, imported not redefined). */
  look: GateLook;
}

// Register the gate data part on `MaestroDataParts` (BRO-1776 owns the map + left
// `gate` out for exactly this). Module augmentation — no edit to chat.ts, no fork. The
// barrel MUST `export * from "./gate"` for this to reach the composition site.
declare module "./chat" {
  interface MaestroDataParts {
    gate: GateCard;
  }
}

// The `data-gate` wire surface — the constant + narrowed part + guard this seam owns,
// mirroring chat.ts's `data-tick` (`DATA_TICK_NAME`/`DATA_TICK_PART`/`isTickDataPart`).
// Without these BRO-1805 hand-writes the `"data-gate"` magic literal the way `data-tick`
// pins it — the exact single-source drift this package exists to prevent.

/** The `data-gate` part NAME — the `MaestroDataParts` key the augmentation above adds (ai keys the map by the bare name). */
export const DATA_GATE_NAME = "gate" as const;
/** The full part `type` string — `data-gate` (what a guard / renderer matches). Mirrors `DATA_TICK_PART`. */
export const DATA_GATE_PART = "data-gate" as const;

/**
 * The gate card as an ai `DataUIPart`: `{ type: "data-gate"; id?; data: GateCard }` — a
 * Maestro-owned narrowing of ai's data part (not a re-declaration of ai's part union),
 * mirroring `TickDataPart`. The part `id` is the `gateId`, the reconciliation key across
 * every open client (FLOWS F5) — NOT a singleton stable id like the tick's `DATA_TICK_ID`
 * (there is one card per open gate; re-sends at the same `gateId` update it in place).
 */
export interface GateDataPart {
  type: typeof DATA_GATE_PART;
  /** the reconciliation key — the `gateId` (FLOWS F5). */
  id?: string;
  data: GateCard;
}

/**
 * True for a `data-gate` part. TAG-ONLY (matches on `type` alone, mirroring
 * `isTickDataPart`): it narrows the type but does NOT validate `data`, so a consumer MUST
 * read `data` defensively rather than trust the narrow blindly. Operates structurally
 * (`{ type: string }`) so it needs no `ai` import.
 */
export const isGateDataPart = (part: { type: string }): part is GateDataPart =>
  part.type === DATA_GATE_PART;

// ── Verdict semantics (D-GATE, FLOWS F5) ───────────────────────────────────────

/**
 * The UI verb for each of the four gate verdicts (`GateVerdict`, state.ts). Primary
 * verbs (Approve / Send back) lead; Block / Point are secondary in the inspector.
 * `escalate` surfaces as "Point" (reassign owner, intents.ts). Exhaustive over
 * `GateVerdict` — a new verdict fails tsc here until given a verb. NOTE: `grant` (attach
 * a capability) is a SEPARATE intent, not a verdict, so it is deliberately not here.
 *
 * These four verbs are the verdict vocabulary; the F5 card decides which to SURFACE by
 * `GateCard.kind` (BRO-1805). A `completion` / `irreversible-action` gate shows all four;
 * a `question`-kind gate (HARNESS §4 exit-20) resolves on the ANSWER path (FLOWS F5) —
 * `revise` carries the answer as feedback, and Approve/Block are suppressed or relabelled.
 * A choice-question's answer options are NOT yet modelled — `GateLook` today is
 * `{ ran, decided, ask }` (BRO-1764) with no options field; adding one is a BRO-1764
 * follow-up, not a new verdict here.
 */
export const GATE_VERDICT_VERBS = {
  approve: "Approve",
  revise: "Send back",
  block: "Block",
  escalate: "Point",
} as const satisfies Record<GateVerdict, string>;

/**
 * Verdicts that REMOVE the node from the gate queue. Derived from `resolveGateVerdict`
 * (state.ts): approve→done, revise→triggered, block→canceled all leave the queue;
 * `escalate`→review stays (re-decidable). Pinned as a set here AND cross-checked against
 * `resolveGateVerdict` in the test, so the two can never drift.
 */
export const TERMINATING_VERDICTS = ["approve", "revise", "block"] as const;
export type TerminatingVerdict = (typeof TERMINATING_VERDICTS)[number];

export const isTerminatingVerdict = (v: GateVerdict): boolean =>
  (TERMINATING_VERDICTS as readonly GateVerdict[]).includes(v);

// ── Grace window — the one sanctioned timing component (porting-notes) ──────────

/** The undo window (ms) before a chosen verdict's intent is actually sent (porting-notes). */
export const GATE_GRACE_WINDOW_MS = 5000 as const;

/** The phases a chosen verdict passes through. */
export const GRACE_PHASES = ["grace", "sending", "sent", "failed"] as const;
export type GracePhase = (typeof GRACE_PHASES)[number];

/**
 * A verdict the human has chosen but that is NOT yet committed. During `grace`
 * (`GATE_GRACE_WINDOW_MS` from `chosenAt`) it is undoable and the intent is NOT sent;
 * when the window lapses it moves `sending` → `sent`. A transport failure moves it to
 * `failed` — the card RE-QUEUES with an error chip (porting-notes), never silently
 * dropped. The intent is sent exactly once, at the end of the grace window.
 */
export interface PendingVerdict {
  gateId: string;
  verdict: GateVerdict;
  phase: GracePhase;
  /** epoch ms the human clicked; the grace window is [chosenAt, chosenAt + GATE_GRACE_WINDOW_MS). */
  chosenAt: number;
}

/** True while a pending verdict is still inside its grace window (given `now` epoch ms). */
export const isWithinGrace = (pending: PendingVerdict, now: number): boolean =>
  pending.phase === "grace" && now - pending.chosenAt < GATE_GRACE_WINDOW_MS;
