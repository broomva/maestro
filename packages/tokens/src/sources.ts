/// <reference types="bun" />
// The handoff token source of truth + path/hashing helpers shared by the build
// and the drift check. The canon lives in the vendored handoff; this package
// re-publishes it (never copies raw values into committed source) and pins its
// content hash in tokens.lock.json so a handoff token edit can't ship silently.

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

/** packages/tokens/ (the package root — src is one level down). */
export const PACKAGE_ROOT = resolve(import.meta.dir, "..");

/** The maestro repo root (packages/tokens → packages → repo root). */
export const REPO_ROOT = resolve(import.meta.dir, "../../..");

/** The vendored design-system directory (the token canon). */
export const HANDOFF_DS = resolve(
  REPO_ROOT,
  "handoff/design_handoff_maestro/build-docs/design-system",
);

/** The build output directory (gitignored — `dist/`). */
export const DIST = resolve(PACKAGE_ROOT, "dist");

/** The committed drift anchor. */
export const LOCK_PATH = resolve(PACKAGE_ROOT, "tokens.lock.json");

/**
 * The CSS entry chain (styles.css @imports the six token files). Paths are
 * relative to HANDOFF_DS; the build mirrors this layout EXACTLY under `dist/`
 * (styles.css at the root, tokens/*.css under `dist/tokens/`) so the relative
 * `@import url("tokens/…")` and `url("../fonts/…")` still resolve — a `css/`
 * wrapper on the CSS but not the fonts would break the font path.
 */
export const SOURCE_CSS = [
  "styles.css",
  "tokens/base.css",
  "tokens/colors.css",
  "tokens/glass.css",
  "tokens/motion.css",
  "tokens/spacing.css",
  "tokens/typography.css",
] as const;

/** The display font (CalSans, TTF; the build adds a WOFF2 beside it). */
export const SOURCE_FONT = "fonts/CalSans-SemiBold.ttf" as const;

/** Every handoff file whose content the package depends on (drift surface). */
export const ALL_SOURCES = [...SOURCE_CSS, SOURCE_FONT] as const;

/** SHA-256 of a buffer, hex. */
export const sha256 = (buf: Uint8Array): string => createHash("sha256").update(buf).digest("hex");

/** The lock file shape: relative source path → sha256 of the handoff file. */
export type Lockfile = Record<string, string>;

/** Compute the current drift map from the handoff sources on disk. */
export function computeLock(): Lockfile {
  const lock: Lockfile = {};
  for (const rel of ALL_SOURCES) {
    lock[rel] = sha256(readFileSync(resolve(HANDOFF_DS, rel)));
  }
  return lock;
}
