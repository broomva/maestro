import { useEffect, useState } from "react";

export type Theme = "light" | "dark";

/** localStorage key the no-flash head script in index.html also reads. */
export const THEME_KEY = "bv-theme";

/**
 * Resolve the initial theme from the persisted value. Light is the default;
 * only an explicit stored "dark" opts into dark (TOKENS-INTEGRATION §2). Pure so
 * the head script and the app agree, and so it is unit-testable without a DOM.
 */
export function resolveInitialTheme(stored: string | null): Theme {
  return stored === "dark" ? "dark" : "light";
}

/** The theme currently applied to <html> (the tokens key on data-theme). */
export function getTheme(): Theme {
  return document.documentElement.dataset.theme === "dark" ? "dark" : "light";
}

/** Apply a theme to <html> and persist it. */
export function setTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme;
  try {
    localStorage.setItem(THEME_KEY, theme);
  } catch {
    // storage unavailable (private mode / disabled) — in-memory theme still holds
  }
}

/** Flip light/dark, persist, and return the new theme. */
export function toggleTheme(): Theme {
  const next: Theme = getTheme() === "dark" ? "light" : "dark";
  setTheme(next);
  return next;
}

/**
 * React state kept in sync with the applied `<html data-theme>` — so EVERY theme control (the top-bar
 * toggle and the Settings > Appearance segmented) reflects live changes made by any of them, instead of a
 * stale mount-time snapshot. Returns `[theme, apply]`; `apply` writes through (`<html>` + localStorage)
 * and the observer echoes the change back into every subscriber's state. SSR/test-safe (no document → the
 * effect never runs; the initial value is "light").
 */
export function useThemeState(): [Theme, (t: Theme) => void] {
  const [theme, setLocal] = useState<Theme>(() =>
    typeof document === "undefined" ? "light" : getTheme(),
  );
  useEffect(() => {
    const el = document.documentElement;
    const sync = () => setLocal(getTheme());
    sync(); // reconcile any change between the initial render and this effect
    const obs = new MutationObserver(sync);
    obs.observe(el, { attributes: true, attributeFilter: ["data-theme"] });
    return () => obs.disconnect();
  }, []);
  const apply = (t: Theme) => {
    setTheme(t); // the observer will echo this back into state; setLocal keeps the current tab instant
    setLocal(t);
  };
  return [theme, apply];
}
