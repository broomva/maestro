import * as React from "react";
import { cn } from "./lib/cn";

/**
 * DotComet — the tidepool dot: a small blue disc with a comet of light orbiting inside
 * (`.bv-dot-live`, LIVE-SIGNALS.md §dot). It is the *running* signal — presence, not a
 * static pulse. The weather is canon (blue → ice, `bv-dot-tide` 3.2s, stops under
 * prefers-reduced-motion); the animation lives in the token layer (@maestro/tokens
 * motion.css, guarded by packages/ui signals.test.ts BRO-1747), so this component only
 * applies the class name — it never redraws the gradient.
 */
export interface DotCometProps extends React.HTMLAttributes<HTMLSpanElement> {
  /** Diameter in px. Default 15 (the canon dot size). */
  size?: number;
  /** Base tint under the animated core — reserved for API parity with the contract;
   * the tidepool weather itself stays canon blue → ice and is not recolorable. */
  color?: string;
}

export const DotComet = React.forwardRef<HTMLSpanElement, DotCometProps>(
  ({ size = 15, color = "var(--bv-info)", className, style, ...props }, ref) => (
    <span
      ref={ref}
      aria-hidden="true"
      className={cn("bv-dot-live", className)}
      style={{ width: size, height: size, background: color, ...style }}
      {...props}
    />
  ),
);
DotComet.displayName = "DotComet";
