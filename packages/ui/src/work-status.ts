import {
  type Dot,
  type Kind,
  type OrchState,
  PLAIN_VOICE,
  type PlainVoice,
  plainVoiceForNode,
} from "@maestro/protocol";
import type { StatusTone } from "./status-badge";

/**
 * The plain-voice → chrome bridge (BRO-1757). `@maestro/protocol` owns the OrchState →
 * plain-voice mapping (label + tone + dot); this module maps that dot vocabulary onto the
 * StatusBadge tone the UI renders, so the board never re-derives it and the accent-blue
 * "Needs you" rule (D-COLOR) lives in exactly one tested place.
 */

/**
 * Protocol dot → StatusBadge tone. `accent-blue` → `accent` is the canon join that keeps
 * "Needs you" accent-blue and never red. `pulse` is not a static color (it renders via the
 * badge's `pulse` flag, dot color neutral), so it maps to `neutral`.
 */
const DOT_TO_TONE: Record<Dot, StatusTone> = {
  gray: "neutral",
  info: "info",
  warning: "warning",
  "accent-blue": "accent",
  success: "success",
  pulse: "neutral",
};

export const dotToTone = (dot: Dot): StatusTone => DOT_TO_TONE[dot];

/** Everything the chrome needs to render a node's state, derived once from OrchState. */
export interface WorkStatusView {
  /** Sentence-case plain-voice label (Queued · Running · Stuck · Needs you · Done · Standing). */
  label: PlainVoice;
  /** StatusBadge tone → dot color (accent = accent-blue "Needs you"). */
  tone: StatusTone;
  /** The raw protocol dot (kept for callers that need the source token). */
  dot: Dot;
  /** True for a standing routine — render the badge with `pulse`. */
  pulse: boolean;
  /** True while a run is live — render the dot as a `DotComet`, not a static pulse. */
  running: boolean;
}

/**
 * Derive the full render view for a work node from its OrchState (+ kind for the routine
 * "Standing" overlay). This is the mapping the board (BRO-1780) consumes:
 *
 *   const v = workStatusView(node.state, node.kind, { isRunning });
 *   v.running ? <StatusBadge status={v.tone} dot={<DotComet size={8} />}>{v.label}</StatusBadge>
 *             : <StatusBadge status={v.tone} pulse={v.pulse}>{v.label}</StatusBadge>
 *
 * The running case uses the `dot` slot so the tidepool replaces the static dot (no double
 * dot); the board never hand-rolls the capsule.
 */
export function workStatusView(
  state: OrchState,
  kind: Kind = "task",
  opts: { isRunning?: boolean } = {},
): WorkStatusView {
  const entry = plainVoiceForNode(state, kind, opts);
  return {
    label: entry.label,
    tone: dotToTone(entry.dot),
    dot: entry.dot,
    pulse: entry.dot === "pulse",
    running: opts.isRunning ?? state === "running",
  };
}

/** Re-export the protocol table for tests / callers that want the canonical source. */
export { PLAIN_VOICE };
