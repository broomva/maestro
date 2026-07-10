import { cva } from "class-variance-authority";
import * as React from "react";
import { cn } from "./lib/cn";

/**
 * Pill-shaped action button (contract: design-system/components/core/Button.d.ts).
 * Primary = ink fill (dark blue, never black); it lightens one step on hover. Every
 * other variant frosts blue. No transform — hover never scales (CLAUDE.md motion rule).
 * Focus is the global ai-blue `:focus-visible` ring from the token base — never re-added.
 */
export const buttonVariants = cva(
  "inline-flex cursor-pointer items-center justify-center gap-1.5 whitespace-nowrap rounded-full font-medium text-sm transition-colors disabled:pointer-events-none disabled:opacity-50",
  {
    variants: {
      variant: {
        primary:
          "border border-transparent bg-primary text-primary-foreground hover:bg-[var(--bv-ink-hover)]",
        secondary:
          "border border-[var(--bv-border-15)] bg-card text-foreground hover:bg-[var(--bv-frost-8)] active:bg-[var(--bv-frost-12)]",
        soft: "border border-transparent bg-[var(--bv-frost-8)] text-foreground hover:bg-[var(--bv-frost-12)] active:bg-[var(--bv-frost-12)]",
        ghost:
          "border border-transparent bg-transparent text-foreground hover:bg-[var(--bv-frost-8)] active:bg-[var(--bv-frost-12)]",
      },
      // Heights 28 / 36 / 44px per the contract (h-7 / h-9 / h-11 — the codebase idiom,
      // see landing.tsx). Padding scales with size; the label stays text-sm (14).
      size: {
        sm: "h-7 px-2.5",
        md: "h-9 px-3.5",
        lg: "h-11 px-[18px]",
      },
    },
    defaultVariants: { variant: "primary", size: "md" },
  },
);

export interface ButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  /** Visual style. Default "primary". */
  variant?: "primary" | "secondary" | "soft" | "ghost";
  /** Height step: 28 / 36 / 44px. Default "md". */
  size?: "sm" | "md" | "lg";
}

export const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
  ({ variant = "primary", size = "md", type = "button", className, ...props }, ref) => (
    <button
      ref={ref}
      type={type}
      className={cn(buttonVariants({ variant, size }), className)}
      {...props}
    />
  ),
);
Button.displayName = "Button";
