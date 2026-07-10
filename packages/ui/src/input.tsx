import * as React from "react";
import { cn } from "./lib/cn";

/**
 * Single-line text input (contract: design-system/components/core/Input.d.ts).
 * 36px tall, rounded-md, 1px gray-200 edge. The ai-blue focus ring is the global
 * `:focus-visible` from the token base — never add your own. Placeholders are
 * sentence-case nouns ("Prompt"), not instructions.
 */
export type InputProps = React.InputHTMLAttributes<HTMLInputElement>;

export const Input = React.forwardRef<HTMLInputElement, InputProps>(
  ({ type = "text", className, ...props }, ref) => (
    <input
      ref={ref}
      type={type}
      className={cn(
        "h-9 w-full rounded-input border border-input bg-background px-3 text-foreground text-sm transition-colors placeholder:text-muted-foreground disabled:cursor-not-allowed disabled:opacity-50",
        className,
      )}
      {...props}
    />
  ),
);
Input.displayName = "Input";
