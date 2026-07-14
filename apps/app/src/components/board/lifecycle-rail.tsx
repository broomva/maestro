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

/** Where each OrchState sits on the rail (its current stage index). `blocked` sits at Running. */
const RAIL_INDEX: Record<OrchState, number> = {
  proposed: 0,
  reviewing: 0,
  triggered: 0,
  running: 1,
  blocked: 1,
  review: 2,
  done: 3,
  canceled: 3,
};

type StagePhase = "passed" | "current" | "warn" | "upcoming";

export function LifecycleRail({ state }: { state: OrchState }) {
  const cur = RAIL_INDEX[state];
  const blocked = state === "blocked";
  return (
    <div>
      {/* An accessible stepper: the current stage carries aria-current="step" so a screen reader hears
          which stage the item is on (the header badge announces the state; the rail adds progression). */}
      <ol className="mc-rail" aria-label="Lifecycle">
        {RAIL_STAGES.map((s, i) => {
          const phase: StagePhase =
            i < cur ? "passed" : i === cur ? (blocked ? "warn" : "current") : "upcoming";
          const cls =
            phase === "passed"
              ? " is-passed"
              : phase === "warn"
                ? " is-warn"
                : phase === "current"
                  ? " is-current"
                  : "";
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
