import * as React from "react";
import { cn } from "./lib/cn";

/**
 * 36px square ghost button holding one 20px Lucide icon (contract:
 * design-system/components/core/IconButton.d.ts). Toolbars, card row actions,
 * composer attachments. Hover frosts blue; never scales. `label` is required and
 * becomes both `aria-label` and `title`.
 */
export interface IconButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  /** Accessible name (becomes aria-label + title). Required. */
  label: string;
  /** The icon element (Lucide, 20px, stroke 2, currentColor). */
  children?: React.ReactNode;
}

export const IconButton = React.forwardRef<HTMLButtonElement, IconButtonProps>(
  ({ label, type = "button", className, children, ...props }, ref) => (
    <button
      ref={ref}
      type={type}
      aria-label={label}
      title={label}
      className={cn(
        "inline-flex h-9 w-9 shrink-0 cursor-pointer items-center justify-center rounded-row text-muted-foreground transition-colors hover:bg-[var(--bv-frost-8)] hover:text-foreground disabled:pointer-events-none disabled:opacity-50",
        className,
      )}
      {...props}
    >
      {children}
    </button>
  ),
);
IconButton.displayName = "IconButton";
