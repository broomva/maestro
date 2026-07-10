import * as React from "react";
import { cn } from "./lib/cn";

/**
 * Circular avatar — initials over an accent, or an image (contract:
 * design-system/components/core/Avatar.d.ts). Pill radius is allowed here (avatars
 * and buttons only). Agent accents come from the user's pick; default is ai-blue.
 * May render a Unicode char a user typed as data, never as decoration.
 */
export interface AvatarProps extends React.HTMLAttributes<HTMLSpanElement> {
  /** Full name; initials derived from it. */
  name?: string;
  /** Accent fill behind initials. Default ai-blue. */
  color?: string;
  /** Diameter in px. Default 22. */
  size?: number;
  /** Optional image URL (replaces initials). */
  src?: string;
}

export const Avatar = React.forwardRef<HTMLSpanElement, AvatarProps>(
  ({ name = "", color = "var(--bv-blue)", size = 22, src, className, style, ...props }, ref) => {
    const initials = name
      .trim()
      .split(/\s+/)
      // Code-point-aware first char: an astral (surrogate-pair) glyph stays whole.
      .map((word) => [...word][0] ?? "")
      .slice(0, 2)
      .join("")
      .toUpperCase();
    return (
      <span
        ref={ref}
        title={name || undefined}
        className={cn(
          "inline-flex shrink-0 select-none items-center justify-center overflow-hidden rounded-full font-semibold text-[var(--bv-white)]",
          className,
        )}
        style={{
          width: size,
          height: size,
          background: src ? "transparent" : color,
          fontSize: Math.max(9, Math.round(size * 0.42)),
          ...style,
        }}
        {...props}
      >
        {src ? <img src={src} alt={name} className="h-full w-full object-cover" /> : initials}
      </span>
    );
  },
);
Avatar.displayName = "Avatar";
