// Gate queue — the derived attention view + verdict semantics + the grace window.
//
// The gate queue is a DERIVED VIEW over nodes in the attention set (state ∈
// {review, blocked} = `ATTENTION_STATES`) — never a separate store. Its comparator IS
// the board order (D-ORDER, DATA-MODEL §B.5). This seam pins: the comparator, the
// `data-gate` card payload, the verdict→verb + terminating semantics, the closed
// `GateKind`, and the grace-window undo state machine.
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

// ── The comparator (also orders the board, BRO-1780) ───────────────────────────

/** The minimal shape the queue comparator reads. */
export interface GateQueueOrder {
  /** = node.state — must be in the attention set to appear in the queue. */
  state: OrchState;
  /**
   * Epoch ms the node ENTERED its attention state — a gate's `openedAt` (review) or the
   * block event ts (blocked). NOT `createdAt`: sorting the attention queue by creation
   * time buries freshly-actionable old work (the BRO-1764 §8 sort-key decoupling). A
   * finite epoch (ms); the runtime supplies `openedAt ?? blockedAt`.
   */
  attentionSince: number;
}

/**
 * The gate-queue / board comparator (D-ORDER, DATA-MODEL §B.5). Cross-group order is
 * `compareByAttention` (review before blocked, then the rest — the shipped protocol
 * comparator, NOT re-declared). Within a group, OLDEST-waiting first (ascending
 * `attentionSince`) — the ticket's "age descending": the gate that has waited longest
 * for a human sits at the top so no gate rots at the bottom. Total order on finite
 * `attentionSince` (a real epoch ms; non-finite is out of contract — the runtime never
 * emits it).
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

// ── Verdict semantics (D-GATE, FLOWS F5) ───────────────────────────────────────

/**
 * The UI verb for each of the four gate verdicts (`GateVerdict`, state.ts). Primary
 * verbs (Approve / Send back) lead; Block / Point are secondary in the inspector.
 * `escalate` surfaces as "Point" (reassign owner, intents.ts). Exhaustive over
 * `GateVerdict` — a new verdict fails tsc here until given a verb. NOTE: `grant` (attach
 * a capability) is a SEPARATE intent, not a verdict, so it is deliberately not here.
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
