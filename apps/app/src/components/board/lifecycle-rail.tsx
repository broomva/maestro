// The lifecycle rail (BRO-1891 FID-5) — the inspector's read-only progression visual, ported from the
// prototype's `McRail` (WorkDetail.jsx) + `.mc-rail` (styles.css). It shows where a work item sits on
// its lifecycle: passed stages behind it, the current stage lit, upcoming stages inert. Real data —
// derived purely from `item.state`, never a progress percentage (CLAUDE.md §Work states: receipts).
//
// FIDELITY NOTE: the prototype rail carries 5 stages (Proposed · Queued · Running · Your gate · Done),
// but the app's plain voice (protocol PLAIN_VOICE, canon per CLAUDE.md §Voice — "the system enums are a
// developer surface only") collapses proposed/reviewing/triggered into a single "Queued". So the
// faithful adaptation is a 4-stage rail using the app's exact plain-voice labels — the design system
// supersedes the prototype's raw stages (CLAUDE.md canon rule: START-HERE §2 wins). `blocked` is not a
// stage of its own — it is Stuck AT the Running stage (a warn dot + "· stuck"), matching the prototype's
// `blocked → running index` mapping.

import type { OrchState } from "@maestro/protocol";

/** The rail stages in lifecycle order, with the app's plain-voice label for each. */
const RAIL_STAGES = [
  { id: "queued", label: "Queued" },
  { id: "running", label: "Running" },
  { id: "review", label: "Needs you" },
  { id: "done", label: "Done" },
] as const;

/**
 * Where each OrchState sits on the rail (its current stage index). `blocked` sits at Running.
 * `canceled` is TERMINAL-NEUTRAL: index -1 → no stage is passed or current, so the rail stays inert
 * (all upcoming, gray) rather than fabricating a completed run. This mirrors the prototype, whose
 * MC_RAIL_INDEX has no `canceled` entry (cur = undefined → every stage falls to upcoming); a canceled
 * item claims no progression (and its `sessionId` may be undefined — the honest stub then reads "No
 * run yet", which an all-blue "reached Done" rail would flatly contradict). PLAIN_VOICE also pins
 * canceled to a muted/gray tone, never the blue of a real `done`.
 */
const RAIL_INDEX: Record<OrchState, number> = {
  proposed: 0,
  reviewing: 0,
  triggered: 0,
  running: 1,
  blocked: 1,
  review: 2,
  done: 3,
  canceled: -1,
};

export function LifecycleRail({ state }: { state: OrchState }) {
  const cur = RAIL_INDEX[state];
  const blocked = state === "blocked";
  return (
    <div>
      {/* An accessible stepper: the current stage carries aria-current="step" so a screen reader hears
          which stage the item is on (the header badge announces the state; the rail adds progression). */}
      <ol className="mc-rail" aria-label="Lifecycle">
        {RAIL_STAGES.map((s, i) => {
          // The stage's phase class, straight from its position relative to the current index — passed
          // behind it, current (or warn, if Stuck) on it, upcoming (inert) ahead. cur = -1 (canceled)
          // lands every stage in the upcoming/inert branch.
          const cls =
            i < cur ? " is-passed" : i === cur ? (blocked ? " is-warn" : " is-current") : "";
          return (
            <li
              key={s.id}
              className={`mc-rail-stage${cls}`}
              aria-current={i === cur ? "step" : undefined}
            >
              <span className="mc-rail-dot" />
              <span className="mc-rail-name">
                {s.label}
                {i === cur && blocked ? " · stuck" : ""}
              </span>
            </li>
          );
        })}
      </ol>
      <p className="mc-rail-note">
        Done is earned · the judge is its only source, and clean runs still pass your gate.
      </p>
    </div>
  );
}
