// Plain voice — the OrchState → UI mapping (DATA-MODEL §B.2, data-contract.md,
// D-ENUM / D-COLOR / D-ORDER).
//
// The system enum is a developer surface; **plain voice is canon in the UI**.
// The eight OrchStates collapse to five plain-voice labels; routines between
// fires read as a sixth, "Standing" (an overlay, not an OrchState).

import type { Kind } from "./intents";
import { isAttentionState, isTerminalState, type OrchState } from "./state";

/** The six plain-voice states the UI shows (five from OrchState + Standing). */
export type PlainVoice = "Queued" | "Running" | "Stuck" | "Needs you" | "Done" | "Standing";

/** The tone token driving the dot + text color (data-contract.md table). */
export type Tone = "muted" | "active" | "warn" | "accent" | "success";

/** The status dot (CLAUDE.md §Work states: gray / info / warning / accent-blue / success). */
export type Dot = "gray" | "info" | "warning" | "accent-blue" | "success" | "pulse";

export interface PlainVoiceEntry {
  label: PlainVoice;
  tone: Tone;
  dot: Dot;
}

/**
 * OrchState → plain voice (DATA-MODEL §B.2 mapping table, data-contract.md).
 * "Needs you" (review) is accent-blue, never red (D-COLOR). `canceled` renders
 * under the Done group with a neutral (muted/gray) tone.
 */
export const PLAIN_VOICE: Record<OrchState, PlainVoiceEntry> = {
  proposed: { label: "Queued", tone: "muted", dot: "gray" },
  reviewing: { label: "Queued", tone: "muted", dot: "gray" },
  triggered: { label: "Queued", tone: "muted", dot: "gray" },
  running: { label: "Running", tone: "active", dot: "info" },
  blocked: { label: "Stuck", tone: "warn", dot: "warning" },
  review: { label: "Needs you", tone: "accent", dot: "accent-blue" },
  done: { label: "Done", tone: "success", dot: "success" },
  canceled: { label: "Done", tone: "muted", dot: "gray" },
};

/** The plain-voice label for an OrchState. */
export const plainVoice = (state: OrchState): PlainVoice => PLAIN_VOICE[state].label;

/**
 * The "Standing" overlay (DATA-MODEL §B.2 last row): a routine between fires
 * reads as **Standing** (pulse dot, never closes). Standing is not an OrchState
 * — it is a display overlay computed from `kind === "routine"`.
 */
export const STANDING: PlainVoiceEntry = { label: "Standing", tone: "muted", dot: "pulse" };

/**
 * The plain-voice entry for a node, applying the routine "Standing" overlay.
 *
 * A routine reads **Standing** only when it is genuinely idle *between fires* —
 * NOT when a run has parked it at an attention state. A routine at the human
 * gate (`review`) or stuck (`blocked`) must still surface "Needs you"/"Stuck":
 * DATA-MODEL §B.2 defines Standing as "routine between fires", and
 * data-contract.md pins `review` + `blocked` as the attention set (D-ORDER).
 * (P20 catch — a stuck/gated routine masked as calm Standing hides the product's
 * most important signal.)
 */
export function plainVoiceForNode(
  state: OrchState,
  kind: Kind,
  opts: { isRunning?: boolean } = {},
): PlainVoiceEntry {
  const running = opts.isRunning ?? state === "running";
  if (kind === "routine" && !running && !isTerminalState(state) && !isAttentionState(state)) {
    return STANDING;
  }
  return PLAIN_VOICE[state];
}

/**
 * Board / gate attention comparator order — review-first (D-ORDER, DATA-MODEL
 * §B.5, `WK_GROUP_ORDER`). The attention set (`review`, `blocked`) sorts first.
 */
export const WK_GROUP_ORDER = [
  "review",
  "blocked",
  "running",
  "triggered",
  "reviewing",
  "proposed",
  "done",
  "canceled",
] as const satisfies readonly OrchState[];

const ORDER_INDEX: Record<OrchState, number> = Object.fromEntries(
  WK_GROUP_ORDER.map((s, i) => [s, i]),
) as Record<OrchState, number>;

/** Sort comparator: attention states first, per D-ORDER. */
export const compareByAttention = (a: OrchState, b: OrchState): number =>
  ORDER_INDEX[a] - ORDER_INDEX[b];
