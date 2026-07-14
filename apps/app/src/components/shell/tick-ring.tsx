// The tick-timer ring (BRO-1884) — the orchestrator's presence chip in the top bar, ported
// from MccTickTimer (WorkPanel.jsx): a ring around a live core that reads as "the loop is
// holding." Clicking it opens the wake log.
//
// FID-1 honesty (CLAUDE.md §"receipts, never fake progress"): the scheduler that owns the real
// countdown is P4 (BRO-1749+). Until it ships there is no honest fraction to draw, so FID-1 draws
// only the resting outline ring + the live core (no fabricated countdown arc) and a neutral label
// ("the loop"). The `progress` fraction + "next Nm" label + the countdown arc are reintroduced in
// the P4 PR that wires the real scheduler — the prototype's SVG structure is the reference. The
// wake-log surface is a later fidelity ticket, so the chip dispatches the event it will listen for
// (the same forward-wire pattern as the ⌘K palette; harmless until then).

const R = 8;

/** Open the wake log (a later surface); dispatch the event it will listen for. */
function openWakeLog() {
  if (typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent("bv:wake-log-open"));
  }
}

export interface TickRingProps {
  /** the meta label; "the loop" at rest (the P4 scheduler feeds "next 13m"). */
  label?: string;
}

export function TickRing({ label = "the loop" }: TickRingProps) {
  return (
    <div className="mcc-timer">
      <button
        className="mcc-orch-chip"
        type="button"
        onClick={openWakeLog}
        title="The loop · the orchestrator holds between ticks"
      >
        <span className="mcc-ring">
          <svg viewBox="0 0 20 20" width="20" height="20" aria-hidden="true">
            <circle
              cx="10"
              cy="10"
              r={R}
              fill="none"
              stroke="var(--bv-border-15)"
              strokeWidth="2"
            />
            <circle cx="10" cy="10" r="3" fill="var(--bv-info)" />
          </svg>
        </span>
        <span className="mcc-orch-meta">{label}</span>
      </button>
    </div>
  );
}
