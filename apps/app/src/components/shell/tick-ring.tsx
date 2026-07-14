// The tick-timer ring (BRO-1884) — the orchestrator's presence chip in the top bar, ported
// from MccTickTimer (WorkPanel.jsx): a countdown ring around a live core. It reads as "the
// loop is holding, next tick soon" and, later, opens the wake log.
//
// FID-1 honesty (CLAUDE.md §"receipts, never fake progress"): the scheduler that owns the
// real countdown is P4 (BRO-1749+). Until it ships there is no honest fraction to draw, so
// `progress` defaults to 0 (a resting outline ring + the live core, no fabricated countdown)
// and the label is neutral ("the loop"). When the scheduler lands, feed the real elapsed
// fraction + "next Nm" here — the SVG structure is already the prototype's.

const R = 8;
const CIRC = 2 * Math.PI * R;

export interface TickRingProps {
  /** elapsed fraction of the current interval, 0–1 (0 = resting; no arc drawn). */
  progress?: number;
  /** the meta label, e.g. "next 13m" once the scheduler lands; "the loop" at rest. */
  label?: string;
  /** opens the wake log (a later surface); a no-op placeholder for now keeps it inert. */
  onClick?: () => void;
  disabled?: boolean;
}

export function TickRing({ progress = 0, label = "the loop", onClick, disabled }: TickRingProps) {
  const clamped = Math.max(0, Math.min(1, progress));
  return (
    <div className="mcc-timer">
      <button
        className="mcc-orch-chip"
        type="button"
        onClick={onClick}
        disabled={disabled}
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
            {clamped > 0 && (
              <circle
                cx="10"
                cy="10"
                r={R}
                fill="none"
                stroke="var(--bv-blue)"
                strokeWidth="2"
                strokeLinecap="round"
                strokeDasharray={CIRC}
                strokeDashoffset={CIRC * (1 - clamped)}
                transform="rotate(-90 10 10)"
                className="mcc-ring-arc"
              />
            )}
            <circle cx="10" cy="10" r="3" fill="var(--bv-info)" />
          </svg>
        </span>
        <span className="mcc-orch-meta">{label}</span>
      </button>
    </div>
  );
}
