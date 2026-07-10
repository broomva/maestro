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

// Every animated live-signal selector — all must be stopped under reduced motion.
const ANIMATED_SELECTORS = [
  ".bv-undertow::before",
  ".bv-undertow::after",
  ".bv-undertow-orbit",
  ".bv-dot-live::before",
  ".bv-dot-comet::before",
  ".bv-dot--pulse",
] as const;

describe("live signals · canon periods are unchanged (LIVE-SIGNALS.md)", () => {
  for (const { signal, decl } of CANON) {
    test(`${signal} runs on its canon period (${decl.split(" ").at(-1)})`, () => {
      expect(flat).toContain(decl);
    });
  }
});

describe("live signals · reduced motion stops everything (hard rule)", () => {
  test("a prefers-reduced-motion: reduce block exists and zeroes animation", () => {
    expect(css).toMatch(/@media\s*\(\s*prefers-reduced-motion:\s*reduce\s*\)/);
  });

  test("every animated live-signal selector is inside the reduced-motion block", () => {
    const start = css.search(/@media\s*\(\s*prefers-reduced-motion:\s*reduce\s*\)/);
    expect(start).toBeGreaterThanOrEqual(0);
    // The reduced-motion block is the last rule in the file; take from the @media to EOF.
    const block = css.slice(start);
    expect(block).toContain("animation: none");
    for (const selector of ANIMATED_SELECTORS) {
      expect(block).toContain(selector);
    }
  });
});

describe("live signals · blue → ice only, never warm, never red (hard rule)", () => {
  test("every oklch hue sits in the blue → cyan → ice band (220-270)", () => {
    const hues = [...css.matchAll(/oklch\(\s*[\d.]+\s+[\d.]+\s+([\d.]+)/g)].map((m) =>
      Number(m[1] ?? Number.NaN),
    );
    expect(hues.length).toBeGreaterThan(0);
    for (const hue of hues) {
      expect(hue).toBeGreaterThanOrEqual(220);
      expect(hue).toBeLessThanOrEqual(270);
    }
  });

  test("only blue / cyan / ice glow tokens are referenced — no warm glow", () => {
    const glows = new Set([...css.matchAll(/--bv-glow-([a-z]+)/g)].map((m) => m[1] ?? ""));
    expect(glows.size).toBeGreaterThan(0);
    for (const glow of glows) {
      expect(["blue", "cyan", "ice"]).toContain(glow);
    }
  });

  test("no warm or red color words leak into the live-signal layer", () => {
    for (const banned of ["orange", "amber", "yellow", "gold", "crimson", "scarlet"]) {
      expect(css.toLowerCase()).not.toContain(banned);
    }
  });
});
