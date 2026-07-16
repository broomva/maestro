// The autonomy scoreboard (BRO-1884) — ported from the design-system component
// (handoff/components/work/AutonomyScoreboard.jsx / .d.ts). Keep score in unsupervised
// HOURS, never a "% done": blue segments are unsupervised stretches, accent notches are
// human looks, the live segment is the run happening now (CLAUDE.md §Work states: receipts,
// never percentages).
//
// FID-1 note: the real autonomy ledger (unsupervised hours) is BRO-1818 (P3) — until it
// ships there is no honest data to plot, so the Shell passes no segments and no `hours`; the
// component renders a calm empty state (the "no unsupervised runs yet" sub-line, no headline
// glyph) rather than the prototype's hardcoded "6h 24m" demo values, and never an em-dash
// placeholder (CLAUDE.md §Voice: no em dashes in chrome). Wire the real ledger here when it lands.

import { formatUnsupervised, type LedgerResponse } from "@maestro/protocol";
import type { CSSProperties, ReactNode } from "react";

/** One unsupervised stretch on the bar (percent units, 0–100). */
export interface AutonomySegment {
  start: number;
  width: number;
  /** the stretch running right now (full-opacity blue→ice gradient). */
  live?: boolean;
}

export interface AutonomyScoreboardProps {
  /** default "unsupervised today". */
  label?: ReactNode;
  /** the headline duration, e.g. "6h 24m"; omit it entirely when there is nothing to show yet
   *  (the empty-state sub-line carries the message — never an em-dash placeholder). */
  hours?: ReactNode;
  /** footnote, e.g. "2 looks · longest run 3h 50m". */
  sub?: ReactNode;
  segments?: AutonomySegment[];
  /** human looks as percent positions, 0–100. */
  notches?: number[];
  children?: ReactNode;
  style?: CSSProperties;
}

export function AutonomyScoreboard({
  label = "unsupervised today",
  hours,
  sub,
  segments = [],
  notches = [],
  children,
  style,
}: AutonomyScoreboardProps) {
  const empty = segments.length === 0;
  return (
    <div
      // Matte card (never glass — CLAUDE.md §Glass), cool-axis border, token radii.
      style={{
        margin: "0 2px",
        padding: "9px 10px",
        border: "1px solid var(--bv-border-5)",
        borderRadius: "var(--bv-radius-lg)",
        background: "var(--card)",
        display: "flex",
        flexDirection: "column",
        gap: 6,
        ...style,
      }}
    >
      <div
        style={{
          display: "flex",
          alignItems: "baseline",
          justifyContent: "space-between",
          fontSize: 11,
          color: "var(--muted-foreground)",
        }}
      >
        <span>{label}</span>
        {hours != null ? (
          <b
            style={{
              fontSize: 12.5,
              fontWeight: 500,
              color: "var(--foreground)",
              fontVariantNumeric: "tabular-nums",
            }}
          >
            {hours}
          </b>
        ) : null}
      </div>
      <div
        style={{
          position: "relative",
          height: 4,
          borderRadius: 99,
          background: "var(--bv-border-5)",
          overflow: "visible",
        }}
      >
        {segments.map((s, i) => (
          <span
            // biome-ignore lint/suspicious/noArrayIndexKey: segments are a positional bar, order is identity
            key={`s${i}`}
            style={{
              position: "absolute",
              top: 0,
              bottom: 0,
              borderRadius: 99,
              left: `${s.start}%`,
              width: `${s.width}%`,
              background: s.live
                ? "linear-gradient(90deg, var(--bv-blue), oklch(0.82 0.09 230))"
                : "var(--bv-blue)",
              opacity: s.live ? 1 : 0.55,
            }}
          />
        ))}
        {notches.map((n, i) => (
          <i
            // biome-ignore lint/suspicious/noArrayIndexKey: notches are positional marks, order is identity
            key={`n${i}`}
            style={{
              position: "absolute",
              top: -2.5,
              width: 1.5,
              height: 9,
              borderRadius: 1,
              left: `${n}%`,
              background: "var(--bv-blue-accent)",
            }}
          />
        ))}
      </div>
      <div style={{ fontSize: 10.5, color: "var(--muted-foreground)" }}>
        {sub ?? (empty ? "no unsupervised runs yet" : null)}
      </div>
      {children}
    </div>
  );
}

/** The scoreboard-prop subset {@link ledgerToScoreboardProps} produces from a `LedgerResponse`. */
export type LedgerScoreboardProps = Pick<
  AutonomyScoreboardProps,
  "hours" | "sub" | "segments" | "notches"
>;

/**
 * Map the DERIVED ledger (BRO-1818 `GET /api/ledger`) to the scoreboard's props — the one place the wire
 * shape becomes chrome. Pure (SSR-safe, unit-tested), so the fetch hook stays a thin transport.
 *
 * A `null` ledger (still loading, or no index) OR a genuinely empty day (no hours, no looks, no active
 * run) → the calm empty state: no `hours`, no `sub`, an empty bar (the component shows "no unsupervised
 * runs yet"). Never a percentage, never an em-dash placeholder (CLAUDE.md §Voice). Otherwise the headline
 * is the unsupervised duration ("1h 0m"), the footnote counts looks + any running work ("3 looks today ·
 * 1 running"), and the bar carries the real positional geometry the endpoint derived.
 */
export function ledgerToScoreboardProps(l: LedgerResponse | null): LedgerScoreboardProps {
  if (!l || (l.unsupervisedMs === 0 && l.humanLooks === 0 && l.activeRuns === 0)) {
    return { segments: [], notches: [] };
  }
  const looks = l.humanLooks === 1 ? "1 look" : `${l.humanLooks} looks`;
  const running = l.activeRuns > 0 ? ` · ${l.activeRuns} running` : "";
  return {
    hours: formatUnsupervised(l.unsupervisedMs),
    sub: `${looks} today${running}`,
    segments: l.segments,
    notches: l.notches,
  };
}
