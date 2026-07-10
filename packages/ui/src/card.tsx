import * as React from "react";
import { cn } from "./lib/cn";

/**
 * Card — the matte content surface (work items, board cards, settings groups, integration
 * rows). **Matte always** — never glass, never pill-radius (CLAUDE.md: glass lives in exactly
 * three places, and Card is not one). Radius `--bv-radius-xl` (0.75rem, `rounded-card`),
 * whisper border, edge shadow at rest.
 *
 * - `interactive` → lifts to a diffuse blue-tinted shadow on hover (`--bv-shadow-card-hover`).
 *   Hover lifts the shadow only — it never scales (CLAUDE.md motion rule).
 * - `running` → the card stays matte and wears the **Undertow** halo (a contained 4px frame:
 *   breathing pools, counter-phase tide, faint 9s orbit — `.bv-undertow` + `.bv-undertow-orbit`
 *   from the token layer, LIVE-SIGNALS.md). The old border comet is retired; pair the running
 *   card with a `DotComet` on its status row.
 */
export interface CardProps extends React.HTMLAttributes<HTMLDivElement> {
  /** Lifts with a blue-tinted shadow on hover. Default false. */
  interactive?: boolean;
  /** Wraps the card in the Undertow halo. The card itself stays matte. Default false. */
  running?: boolean;
  children?: React.ReactNode;
}

export const Card = React.forwardRef<HTMLDivElement, CardProps>(
  ({ interactive = false, running = false, className, children, ...props }, ref) => {
    const inner = (
      <div
        ref={ref}
        className={cn(
          "flex flex-col gap-2 rounded-card border border-border bg-card px-3.5 py-3 shadow-[var(--bv-shadow-edge)] transition-shadow",
          interactive && "cursor-pointer hover:shadow-[var(--bv-shadow-card-hover)]",
          className,
        )}
        {...props}
      >
        {children}
      </div>
    );
    if (!running) return inner;
    return (
      <div className="bv-undertow">
        <span className="bv-undertow-orbit" aria-hidden="true" />
        {inner}
      </div>
    );
  },
);
Card.displayName = "Card";
