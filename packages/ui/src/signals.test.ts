/// <reference types="bun" />

// Live-signal canon guard (BRO-1747). The Undertow / tidepool / pulse are the
// signature running signals and the easiest thing in the system to get subtly wrong
// (LIVE-SIGNALS.md). They live in the token layer (@maestro/tokens motion.css, chained
// into the app via styles.css) — packages/ui components (DotComet BRO-1757, Card
// BRO-1762) only apply the class names, so this guard pins the SOURCE OF TRUTH rather
// than copying it (CLAUDE.md canon: tokens are consumed as a package, never copied).
//
// Reads the committed handoff token source (not the gitignored dist), the same pattern
// as apps/app m0.test.ts — so it needs no build step and runs under CI's plain bun test.
// This IS the done.check `bun run --filter ui test:signals`; the rendered half (computed
// durations + reduced-motion in a real browser) is the /kitchen-sink Playwright dogfood.

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";

const MOTION_CSS =
  "../../../handoff/design_handoff_maestro/build-docs/design-system/tokens/motion.css";
const css = readFileSync(new URL(MOTION_CSS, import.meta.url), "utf8");

// Collapse whitespace so multi-line `animation:` declarations match as one string.
const flat = css.replace(/\s+/g, " ");

// The five canon periods, each bound to its keyframe animation. Re-timing any layer
// (what makes the Undertow feel alive rather than looped) fails the build.
const CANON = [
  { signal: "undertow pools", decl: "animation: bv-undertow-pools 4.2s" },
  { signal: "undertow tide", decl: "animation: bv-undertow-tide 3.4s" },
  { signal: "undertow orbit", decl: "animation: bv-undertow-orbit 9s" },
  { signal: "tidepool dot", decl: "animation: bv-dot-tide 3.2s" },
  { signal: "standing pulse", decl: "animation: bv-dot-pulse 1s" },
] as const;

// Balanced { ... } block starting at the first "{" at/after `from` — brace-matched so a
// nested rule inside the @media doesn't fool the scan, and it stops at the block's close
// brace rather than running to EOF.
function blockAt(source: string, from: number): string {
  const open = source.indexOf("{", from);
  if (open < 0) return "";
  let depth = 0;
  for (let i = open; i < source.length; i += 1) {
    if (source[i] === "{") depth += 1;
    else if (source[i] === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(open, i + 1);
    }
  }
  return source.slice(open);
}

// Derive the animated selectors from the file itself (not a hardcoded list) so a NEW
// animated rule that isn't gated under reduced-motion fails the test rather than slipping
// through. Everything before the @media block is the "live" layer that must be stopped.
const reducedIdx = css.search(/@media\s*\(\s*prefers-reduced-motion:\s*reduce\s*\)/);
const beforeReduced = reducedIdx >= 0 ? css.slice(0, reducedIdx) : css;
const animatedSelectors: string[] = [];
for (const match of beforeReduced.matchAll(/animation:\s*(?!none)[^;}]+/g)) {
  const head = beforeReduced.slice(0, match.index);
  const selector = head
    .slice(head.lastIndexOf("}") + 1, head.lastIndexOf("{"))
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .trim();
  for (const part of selector.split(",")) {
    if (part.trim()) animatedSelectors.push(part.trim());
  }
}

describe("live signals · canon periods are unchanged (LIVE-SIGNALS.md)", () => {
  for (const { signal, decl } of CANON) {
    test(`${signal} runs on its canon period (${decl.split(" ").at(-1)})`, () => {
      expect(flat).toContain(decl);
    });
  }
});

describe("live signals · reduced motion stops everything (hard rule)", () => {
  test("a prefers-reduced-motion: reduce block exists", () => {
    expect(reducedIdx).toBeGreaterThanOrEqual(0);
  });

  test("the file actually defines live animations to gate (guards the guard)", () => {
    // If the derivation found nothing, the subset check below would be vacuously true.
    expect(animatedSelectors.length).toBeGreaterThanOrEqual(5);
  });

  test("every animated selector is bound to animation: none under reduced motion", () => {
    const block = blockAt(css, reducedIdx);
    expect(block).toContain("animation: none");
    // The only animation the reduced-motion block may set is `none`.
    for (const decl of block.matchAll(/animation:\s*([^;}]+)/g)) {
      expect(decl[1]?.trim()).toBe("none");
    }
    // Every live animation must be gated — a newly-added, un-gated one fails here.
    for (const selector of animatedSelectors) {
      expect(block, `${selector} must be stopped under reduced motion`).toContain(selector);
    }
  });
});

describe("live signals · blue → indigo → cyan → ice only, never warm, never red (hard rule)", () => {
  // Canon palette spans cyan (hue 220) → ice (240) → blue (258/260) → indigo (280);
  // the band brackets it while still excluding green (~145) and every warm hue.
  test("every inline oklch hue sits in the blue → indigo → cyan → ice band (210-290)", () => {
    const hues = [...css.matchAll(/oklch\(\s*[\d.]+%?\s+[\d.]+\s+([\d.]+)/g)].map((m) =>
      Number(m[1] ?? Number.NaN),
    );
    expect(hues.length).toBeGreaterThan(0);
    for (const hue of hues) {
      expect(hue).toBeGreaterThanOrEqual(210);
      expect(hue).toBeLessThanOrEqual(290);
    }
  });

  test("only blue / indigo / cyan / ice glow tokens are referenced — no warm glow", () => {
    const glows = new Set([...css.matchAll(/--bv-glow-([a-z]+)/g)].map((m) => m[1] ?? ""));
    expect(glows.size).toBeGreaterThan(0);
    for (const glow of glows) {
      expect(["blue", "indigo", "cyan", "ice"]).toContain(glow);
    }
  });

  test("no warm / status color token or color word leaks into the live-signal layer", () => {
    // Word-boundary match so "red" doesn't false-fire on "prefers-reduced-motion".
    for (const word of [
      "orange",
      "amber",
      "yellow",
      "gold",
      "crimson",
      "scarlet",
      "tomato",
      "coral",
      "firebrick",
      "magenta",
      "red",
    ]) {
      expect(css.toLowerCase()).not.toMatch(new RegExp(`\\b${word}\\b`));
    }
    // The live signals are blue-family; they must not pull the warm status tokens.
    for (const token of ["--bv-danger", "--bv-warning", "--bv-success", "--bv-error"]) {
      expect(css).not.toContain(token);
    }
  });
});
