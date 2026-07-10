import { ArrowUp } from "lucide-react";
import * as React from "react";
import { cn } from "./lib/cn";

/** The text an `onSend` should receive, or null when there is nothing to send (empty /
 * whitespace-only). Exported so the trim-and-guard rule is unit-testable without a DOM. */
export const composerSendText = (raw: string): string | null => {
  const trimmed = raw.trim();
  return trimmed.length > 0 ? trimmed : null;
};

/**
 * Composer — the chat composer, and **the one place glass and dramatic depth are allowed**
 * (CLAUDE.md: glass in exactly three places; the composer halo is the single dramatic depth
 * cue). `.bv-glass-composer` carries the 28px radius, the frosted-blue halo, and the inner
 * light line. Bare input inside; the ai-blue focus ring rides the capsule (`focus-within`),
 * not the input, so the whole composer reads as one surface.
 *
 * Controlled via `value`/`onChange`, uncontrolled otherwise. `onSend` fires with the trimmed
 * text on Enter (without Shift) or the send click; empty/whitespace never sends.
 */
export interface ComposerProps extends Omit<React.HTMLAttributes<HTMLDivElement>, "onChange"> {
  /** Input placeholder. Default "Message Maestro" — keep the "Message <agent>" shape (D-NAME). */
  placeholder?: string;
  /** Controlled value; uncontrolled if omitted. */
  value?: string;
  onChange?: (value: string) => void;
  /** Called with the trimmed text on Enter or send click. */
  onSend?: (text: string) => void;
  /** Optional leading element (e.g. an attach IconButton). */
  leading?: React.ReactNode;
}

export const Composer = React.forwardRef<HTMLDivElement, ComposerProps>(
  (
    { placeholder = "Message Maestro", value, onChange, onSend, leading, className, ...props },
    ref,
  ) => {
    const [internal, setInternal] = React.useState("");
    const controlled = value !== undefined;
    const text = controlled ? value : internal;

    const setText = (next: string) => {
      if (!controlled) setInternal(next);
      onChange?.(next);
    };

    const send = () => {
      const payload = composerSendText(text);
      if (payload) onSend?.(payload);
      if (!controlled) setInternal("");
    };

    return (
      <div
        ref={ref}
        className={cn(
          "bv-glass-composer grid items-center gap-1 p-2.5",
          leading ? "grid-cols-[auto_1fr_auto]" : "grid-cols-[1fr_auto]",
          "focus-within:[outline:2px_solid_var(--ring)] focus-within:[outline-offset:2px]",
          className,
        )}
        {...props}
      >
        {leading}
        <input
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              send();
            }
          }}
          placeholder={placeholder}
          className="min-w-0 border-none bg-transparent px-2.5 py-2 text-base text-foreground outline-none placeholder:text-muted-foreground focus-visible:outline-none"
        />
        <button
          type="button"
          aria-label="Send"
          onClick={send}
          className="inline-flex size-9 shrink-0 cursor-pointer items-center justify-center rounded-full bg-primary text-primary-foreground transition-colors hover:bg-[var(--bv-ink-hover)]"
        >
          <ArrowUp size={18} strokeWidth={2} />
        </button>
      </div>
    );
  },
);
Composer.displayName = "Composer";
