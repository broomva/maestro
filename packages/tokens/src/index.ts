// @maestro/tokens — the design tokens, consumed as a package (never copied).
//
// The token canon lives in the vendored handoff; `bun run --filter @maestro/tokens
// build` re-publishes it into `dist/` (the CSS entry + glass utilities + the
// CalSans WOFF2) and `check:sync` guards it against drift. This module is the
// programmatic surface: the token-name manifest and the built asset paths.

export { findDrift } from "./check-sync";
export { buildThemeCss, THEME_TOKENS, type ThemeTokens } from "./manifest";

export const TOKENS_PACKAGE = "@maestro/tokens" as const;

/**
 * The built asset entry points, relative to the package root (available after
 * `build`). The app imports the CSS entry before Tailwind and the theme block
 * after it (TOKENS-INTEGRATION §1/§3).
 */
export const ASSETS = {
  /** Token entry: the @import chain (colors, typography, spacing, glass, motion, base). */
  styles: "dist/styles.css",
  /** The `@theme inline` block — import after `@import "tailwindcss"`. */
  theme: "dist/theme.css",
  /** Machine-readable token-name manifest. */
  manifest: "dist/manifest.json",
  /** Display font (opt in per-surface with [data-display-font="calsans"]). */
  fontWoff2: "dist/fonts/CalSans-SemiBold.woff2",
  fontTtf: "dist/fonts/CalSans-SemiBold.ttf",
} as const;
