import { ArrowUp, Square } from "lucide-react";
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
  /** True while a turn is streaming — the send arrow becomes a stop square and Enter is inert (the
   *  visible affordance is Stop). Drives the one glass surface's send/stop verb (BRO-1826 M4). */
  busy?: boolean;
  /** Called when the stop square is clicked (only reachable while `busy`) — aborts the in-flight turn. */
  onStop?: () => void;
  /** Optional leading element (e.g. an attach IconButton). */
  leading?: React.ReactNode;
}

export const Composer = React.forwardRef<HTMLDivElement, ComposerProps>(
  (
    {
      placeholder = "Message Maestro",
      value,
      onChange,
      onSend,
      busy = false,
      onStop,
      leading,
      className,
      ...props
    },
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

    // The one action button: Stop while a turn streams (aborts it), Send otherwise. The icon + label
    // flip together so the affordance is honest at every moment (a11y: label tracks the action).
    const act = () => {
      if (busy) onStop?.();
      else send();
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
            // Enter sends — but never while an IME candidate is composing (CJK/JP/KR).
            // Committing a candidate dispatches Enter with isComposing=true; sending then
            // would swallow the half-composed text and clear the input. While a turn is
            // streaming (`busy`) Enter is inert — the visible verb is Stop, reached by the button.
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing && !busy) {
              e.preventDefault();
              send();
            }
          }}
          placeholder={placeholder}
          // The ai-blue focus ring rides the capsule (focus-within), not the input. The global
          // :focus-visible ring (@maestro/tokens base.css) is emitted UNLAYERED, so a layered
          // `focus-visible:outline-none` utility can't suppress it (unlayered beats @layer
          // regardless of specificity) — only an inline style wins.
          style={{ outline: "none" }}
          className="min-w-0 border-none bg-transparent px-2.5 py-2 text-base text-foreground placeholder:text-muted-foreground"
        />
        <button
          type="button"
          aria-label={busy ? "Stop" : "Send"}
          onClick={act}
          className="inline-flex size-9 shrink-0 cursor-pointer items-center justify-center rounded-full bg-primary text-primary-foreground transition-colors hover:bg-[var(--bv-ink-hover)]"
        >
          {busy ? <Square size={16} strokeWidth={2} /> : <ArrowUp size={18} strokeWidth={2} />}
        </button>
      </div>
    );
  },
);
Composer.displayName = "Composer";
