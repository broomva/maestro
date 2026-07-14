// A minimal focus trap for the overlay layer (FID-7 · BRO-1894). Keeps Tab / Shift+Tab focus cycling
// within a container so a modal surface (the feedback drawer) never leaks focus to the app behind the
// scrim (WAI-ARIA dialog). The command palette uses a lighter trap (real focus stays on its input; the
// active option is virtual, via aria-activedescendant), so it does not need this.
//
// Runs only in a real browser (it reads layout + document.activeElement) — exercised by the Playwright
// specs, not the DOM-less unit renders.

import type { KeyboardEvent as ReactKeyboardEvent } from "react";

const FOCUSABLE =
  'a[href],button:not([disabled]),input:not([disabled]),textarea:not([disabled]),select:not([disabled]),[tabindex]:not([tabindex="-1"])';

/** The tabbable elements inside `el`, in DOM order, skipping ones that are not laid out (hidden). */
export function focusableWithin(el: HTMLElement): HTMLElement[] {
  return Array.from(el.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
    (n) => n.offsetParent !== null || n === document.activeElement,
  );
}

/**
 * On a Tab keydown, wrap focus at the edges of `container` so it cannot escape the dialog. Call from the
 * container's `onKeyDown`. A no-op for non-Tab keys or when there is nothing focusable.
 */
export function handleTrapTab(
  container: HTMLElement | null,
  e: KeyboardEvent | ReactKeyboardEvent,
): void {
  if (!container || e.key !== "Tab") return;
  const items = focusableWithin(container);
  if (items.length === 0) return;
  const first = items[0];
  const last = items[items.length - 1];
  const active = document.activeElement;
  if (e.shiftKey && active === first) {
    e.preventDefault();
    last?.focus();
  } else if (!e.shiftKey && active === last) {
    e.preventDefault();
    first?.focus();
  }
}
