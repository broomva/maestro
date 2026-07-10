import * as React from "react";
import { cn } from "./lib/cn";

/**
 * The dot tone vocabulary (CLAUDE.md §Work states: "the dot carries the color"). The
 * capsule stays a matte gray pill; only the dot is colored.
 *
 * `accent` is accent-blue 235 — it owns **Needs you** (the human gate) and must never be
 * red (CLAUDE.md "Don't mark 'Needs you' / halt states in red — they're accent-blue").
 * The design-system `.d.ts` predates that rule and lists only success/info/warning/danger/
 * neutral; `accent` is the canon-required extension. `danger` exists for type completeness
 * but has no chrome use — nothing in Maestro renders a red dot.
 */
export type StatusTone = "success" | "info" | "warning" | "danger" | "neutral" | "accent";

/** Tone → dot color token. Consumed as CSS vars from @maestro/tokens (never hardcoded hex). */
export const STATUS_DOT_VAR: Record<StatusTone, string> = {
  success: "var(--bv-success)",
  info: "var(--bv-info)",
  warning: "var(--bv-warning)",
  danger: "var(--bv-danger)",
  neutral: "var(--bv-gray-400)",
  accent: "var(--bv-blue-accent)",
};

export interface StatusBadgeProps extends React.HTMLAttributes<HTMLSpanElement> {
  /** Dot color. Default "info". `accent` = accent-blue "Needs you". */
  status?: StatusTone;
  /** Pulse the dot (a standing routine listening for its trigger). Uses the canon
   * `.bv-dot--pulse` breath (1s), not a bespoke keyframe. Default false. */
  pulse?: boolean;
  /** The plain-voice label — sentence case, lead with the state (COMPONENTS.md §StatusBadge). */
  children?: React.ReactNode;
}

/**
 * StatusBadge — a soft gray capsule + a colored dot + a sentence-case plain-voice label
 * (Queued · Running · Stuck · Needs you · Done · Standing). The dot carries the state
 * color; the capsule stays matte gray so a wall of badges reads calm. For *running* work,
 * use `DotComet` as the dot rather than `pulse` — running is presence, not a breath.
 */
export const StatusBadge = React.forwardRef<HTMLSpanElement, StatusBadgeProps>(
  ({ status = "info", pulse = false, className, children, ...props }, ref) => (
    <span
      ref={ref}
      className={cn(
        "inline-flex h-[26px] items-center gap-1.5 rounded-full bg-muted px-3 font-medium text-foreground text-xs",
        className,
      )}
      {...props}
    >
      <span
        aria-hidden="true"
        className={cn("inline-block size-2 shrink-0 rounded-full", pulse && "bv-dot--pulse")}
        style={{ background: STATUS_DOT_VAR[status] }}
      />
      {children}
    </span>
  ),
);
StatusBadge.displayName = "StatusBadge";
