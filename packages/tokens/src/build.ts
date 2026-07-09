/// <reference types="bun" />
// Build @maestro/tokens: re-publish the handoff token canon into `dist/` and add
// the WOFF2 the app pipeline wants (TOKENS-INTEGRATION §1/§3/§4).
//
//   dist/styles.css + dist/tokens/*.css   the token entry (the @import chain)
//   dist/fonts/                           CalSans TTF + the built WOFF2
//   dist/theme.css                        the @theme inline block (from the manifest)
//   dist/manifest.json                    the machine-readable token-name manifest
//
// Run: `bun run --filter @maestro/tokens build`. Reads from the handoff, writes
// only to dist (gitignored) — no raw token values are copied into committed src.

import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
// @ts-expect-error — wawoff2 ships no types; compress(ttf) → Promise<Uint8Array>
import wawoff2 from "wawoff2";
import { buildThemeCss, THEME_TOKENS } from "./manifest";
import { DIST, HANDOFF_DS, SOURCE_CSS, SOURCE_FONT } from "./sources";

const FONT_TTF = "CalSans-SemiBold.ttf";
const FONT_WOFF2 = "CalSans-SemiBold.woff2";

/**
 * Rewrite the aggregator's nested imports from the `@import url("tokens/x.css")`
 * form to explicit relative `@import "./tokens/x.css";`. The bare `url()` form does
 * not rebase through node_modules under Vite/postcss-import (the empirical
 * TOKENS-INTEGRATION §1 snag, found wiring @maestro/app in BRO-1782); the
 * `./`-prefixed string form resolves in every standard bundler. dist is a copy of
 * the handoff, so this only changes the emitted consumer entry, not the canon.
 */
function withResolvableImports(stylesCss: string): string {
  const before = /@import\s+url\(\s*(['"])tokens\/([^'"]+)\1\s*\)\s*;?/g;
  const rewritten = stylesCss.replace(before, '@import "./tokens/$2";');
  if (rewritten.includes("@import url(")) {
    throw new Error(
      "build: styles.css still has an unrewritten @import url() after the rebase pass",
    );
  }
  return rewritten;
}

/** Rewrite the @font-face `src:` to prefer WOFF2 with the TTF as fallback (§4). */
function withWoff2Src(typographyCss: string): string {
  const ttfSrc =
    /src:\s*url\(["']\.\.\/fonts\/CalSans-SemiBold\.ttf["']\)\s*format\(["']truetype["']\);/;
  const replacement =
    'src: url("../fonts/CalSans-SemiBold.woff2") format("woff2"),\n' +
    '       url("../fonts/CalSans-SemiBold.ttf") format("truetype");';
  if (!ttfSrc.test(typographyCss)) {
    throw new Error("build: typography.css @font-face src did not match the expected TTF pattern");
  }
  return typographyCss.replace(ttfSrc, replacement);
}

export async function build(): Promise<void> {
  rmSync(DIST, { recursive: true, force: true });
  mkdirSync(resolve(DIST, "tokens"), { recursive: true });
  mkdirSync(resolve(DIST, "fonts"), { recursive: true });

  // 1. Copy the CSS chain, mirroring the handoff design-system/ layout EXACTLY
  //    under dist/ so both relative chains survive: styles.css's
  //    @import url("tokens/…") and typography.css's url("../fonts/…") (from
  //    dist/tokens/, ../fonts resolves to dist/fonts/ — a `css/` wrapper would
  //    break the font path). typography.css is rewritten to add the WOFF2 src.
  for (const rel of SOURCE_CSS) {
    const out = resolve(DIST, rel);
    mkdirSync(dirname(out), { recursive: true });
    const src = readFileSync(resolve(HANDOFF_DS, rel), "utf8");
    let content = src;
    if (rel.endsWith("typography.css")) content = withWoff2Src(content);
    else if (rel.endsWith("styles.css")) content = withResolvableImports(content);
    writeFileSync(out, content);
  }

  // 2. Copy the TTF and build the WOFF2 beside it.
  const ttf = readFileSync(resolve(HANDOFF_DS, SOURCE_FONT));
  cpSync(resolve(HANDOFF_DS, SOURCE_FONT), resolve(DIST, "fonts", FONT_TTF));
  const woff2: Uint8Array = await wawoff2.compress(ttf);
  if (Buffer.from(woff2.slice(0, 4)).toString("ascii") !== "wOF2") {
    throw new Error("build: WOFF2 conversion produced an invalid file (bad magic)");
  }
  writeFileSync(resolve(DIST, "fonts", FONT_WOFF2), woff2);

  // 3. Emit the @theme block + the JSON manifest.
  writeFileSync(resolve(DIST, "theme.css"), buildThemeCss());
  writeFileSync(resolve(DIST, "manifest.json"), `${JSON.stringify(THEME_TOKENS, null, 2)}\n`);

  console.log(
    `@maestro/tokens built → dist/ (${SOURCE_CSS.length} css, TTF+WOFF2 ${ttf.length}→${woff2.length}, theme.css, manifest.json)`,
  );
}

if (import.meta.main) {
  await build();
}
