/// <reference types="bun" />

import { describe, expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { resolveInitialTheme } from "./theme";

// M0 token-sanity suite (TOKENS-INTEGRATION §6), the statically-checkable subset.
// These run under `bun test` in CI's `quality` job — the repo's test runner is
// bun:test (not vitest; introducing vitest would fragment the runner and skip the
// existing gate). They read COMMITTED sources of truth (the vendored handoff
// tokens + the app wiring files), not the gitignored dist, so they need no build
// step in CI. The rendered §6 checks (real computed styles in light + dark) live
// in the Playwright smoke, tests/m0.pw.ts.

const HANDOFF_TOKENS = "../../../handoff/design_handoff_maestro/build-docs/design-system/tokens";

const read = (rel: string): string => readFileSync(new URL(rel, import.meta.url), "utf8");
const readToken = (name: string): string => read(`${HANDOFF_TOKENS}/${name}`);

describe("M0 · glass is earned (§6.4)", () => {
  test("backdrop-filter appears only in the glass token layer", () => {
    expect(readToken("glass.css")).toContain("backdrop-filter");
    for (const file of ["colors.css", "typography.css", "spacing.css", "motion.css", "base.css"]) {
      expect(readToken(file)).not.toContain("backdrop-filter");
    }
  });

  test("glass backdrop-filter is confined to .bv-glass*/.bv-scrim selectors", () => {
    // Every rule that sets backdrop-filter must be one of the four glass classes.
    for (const block of readToken("glass.css").split("}")) {
      if (!block.includes("backdrop-filter")) continue;
      expect(block).toMatch(/\.bv-(glass|glass-heavy|glass-composer|scrim)\b/);
    }
  });

  test("no M0 app surface wears glass (the page is matte)", () => {
    const srcDir = new URL("./", import.meta.url);
    for (const entry of readdirSync(srcDir, { recursive: true, encoding: "utf8" })) {
      if (!/\.(tsx?|css)$/.test(entry) || entry.endsWith(".test.ts")) continue;
      // Strip comments so a doc reference to glass (like this file's own prose)
      // is not mistaken for glass being applied to a surface.
      const code = readFileSync(new URL(entry, srcDir), "utf8")
        .replace(/\/\*[\s\S]*?\*\//g, "")
        .replace(/\/\/.*$/gm, "");
      expect(code).not.toContain("bv-glass");
      expect(code).not.toContain("backdrop-filter");
    }
  });
});

describe("M0 · closed type scale (§3)", () => {
  test("exactly 12/14/16/18/22/24/28 — no one-off sizes", () => {
    const scale = new Map<string, number>();
    for (const m of readToken("typography.css").matchAll(/--bv-text-([\w-]+):\s*(\d+)px/g)) {
      scale.set(m[1] as string, Number(m[2]));
    }
    expect([...scale.values()].sort((a, b) => a - b)).toEqual([12, 14, 16, 18, 22, 24, 28]);
    expect(scale.get("xs")).toBe(12);
    expect(scale.get("h1")).toBe(28);
  });
});

describe("M0 · focus is ai-blue (§6.2)", () => {
  test("the ring token resolves to ai-blue and :focus-visible uses it", () => {
    const colors = readToken("colors.css");
    expect(colors).toMatch(/--ring:\s*var\(--bv-blue\)/);
    expect(colors).toMatch(/--bv-blue:\s*oklch\(0\.60 0\.12 260\)/);
    const base = readToken("base.css");
    expect(base).toMatch(/:focus-visible\s*\{[^}]*outline:\s*2px solid var\(--ring\)/);
  });
});

describe("M0 · dark canvas is deep blue-purple, never black (§6.1)", () => {
  test("light background is white; dark background is a dark oklch, not #000", () => {
    const colors = readToken("colors.css");
    expect(colors).toMatch(/--background:\s*var\(--bv-white\)/);
    const dark = colors.slice(colors.indexOf('[data-theme="dark"]'));
    expect(dark.length).toBeGreaterThan(0);
    const bg = dark.match(/--background:\s*(oklch\([^)]*\))/);
    expect(bg).not.toBeNull();
    // Lightness ~0.135 — dark but never pure-black lightness 0.
    const bgValue = bg?.[1] ?? "";
    const lightness = Number(bgValue.match(/oklch\(([\d.]+)/)?.[1]);
    expect(lightness).toBeGreaterThan(0.1);
    expect(lightness).toBeLessThan(0.2);
  });
});

describe("M0 · no-flash theme resolution (§2)", () => {
  test("light is the default; only explicit 'dark' opts in", () => {
    expect(resolveInitialTheme("dark")).toBe("dark");
    expect(resolveInitialTheme("light")).toBe("light");
    expect(resolveInitialTheme(null)).toBe("light");
    expect(resolveInitialTheme("nonsense")).toBe("light");
  });

  test("the index.html head script agrees with resolveInitialTheme", () => {
    const html = read("../index.html");
    expect(html).toContain("bv-theme");
    expect(html).toMatch(/dataset\.theme\s*=\s*stored === "dark" \? "dark" : "light"/);
  });
});

describe("M0 · token import order recorded (§1)", () => {
  test("tokens/base before tailwind before the @theme map", () => {
    const css = read("./styles/globals.css");
    const styles = css.indexOf('@import "@maestro/tokens/styles.css"');
    const tw = css.indexOf('@import "tailwindcss"');
    const theme = css.indexOf('@import "@maestro/tokens/theme.css"');
    expect(styles).toBeGreaterThanOrEqual(0);
    expect(styles).toBeLessThan(tw);
    expect(tw).toBeLessThan(theme);
  });
});
