/// <reference types="bun" />
import { describe, expect, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { build } from "./build";
import { parseLock } from "./check-sync";
import { buildThemeCss, THEME_TOKENS } from "./manifest";
import { computeLock, DIST, HANDOFF_DS, LOCK_PATH, sha256 } from "./sources";

describe("THEME_TOKENS manifest", () => {
  test("radii use the SOURCE var names, not the TOKENS-INTEGRATION §3 example typos", () => {
    // §3's example writes --bv-radius-input / --bv-radius-row, but spacing.css
    // defines --bv-radius-md / --bv-radius-lg. §3 line 81: the source wins.
    expect(THEME_TOKENS.radius.input).toBe("--bv-radius-md");
    expect(THEME_TOKENS.radius.row).toBe("--bv-radius-lg");
    expect(Object.values(THEME_TOKENS.radius)).not.toContain("--bv-radius-input");
    expect(Object.values(THEME_TOKENS.radius)).not.toContain("--bv-radius-row");
  });

  test("every manifest var name actually exists in the handoff token source", () => {
    // Guards the manifest against referencing a var the CSS never defines.
    const css = ["tokens/colors.css", "tokens/spacing.css", "tokens/typography.css"]
      .map((f) => readFileSync(resolve(HANDOFF_DS, f), "utf8"))
      .join("\n");
    const allVars = Object.values(THEME_TOKENS).flatMap((group) => Object.values(group));
    for (const v of allVars) {
      // each var must appear as a declaration `<var>:` somewhere in the source
      expect(css.includes(`${v}:`)).toBe(true);
    }
  });

  test("the type scale is the closed 7-value set (12/14/16/18/22/24/28)", () => {
    expect(Object.keys(THEME_TOKENS.text)).toEqual(["xs", "sm", "base", "lg", "xl", "2xl", "h1"]);
  });

  test("exposes the focus ring (--ring) and the one ai-blue accent (--bv-blue) per §3", () => {
    // §3 maps only --color-blue → --bv-blue (ai-blue, hue 260). The "Needs you"
    // accent-blue (--bv-blue-accent, hue 235) is a StatusBadge/plain-voice dot
    // token (BRO-1785), not a @theme utility, so it is intentionally not here.
    expect(THEME_TOKENS.colors.ring).toBe("--ring");
    expect(THEME_TOKENS.colors.blue).toBe("--bv-blue");
    expect(Object.values(THEME_TOKENS.colors)).not.toContain("--bv-blue-accent");
  });
});

describe("buildThemeCss", () => {
  const css = buildThemeCss();

  test("emits a Tailwind @theme inline block", () => {
    expect(css).toContain("@theme inline {");
    expect(css.trimEnd().endsWith("}")).toBe(true);
  });

  test("prefixes each group correctly and wraps in var()", () => {
    expect(css).toContain("--color-background: var(--background);");
    expect(css).toContain("--radius-input: var(--bv-radius-md);");
    expect(css).toContain("--text-h1: var(--bv-text-h1);");
    expect(css).toContain("--font-display: var(--bv-font-display);");
  });

  test("does not leak a raw hex or a text-3xl beyond the closed scale", () => {
    expect(css).not.toMatch(/#[0-9a-fA-F]{3,6}/);
    expect(css).not.toContain("--text-3xl");
  });
});

describe("drift check (tokens.lock.json)", () => {
  test("the committed lock is in sync with the handoff canon", () => {
    expect(existsSync(LOCK_PATH)).toBe(true);
    const pinned = JSON.parse(readFileSync(LOCK_PATH, "utf8")) as Record<string, string>;
    expect(computeLock()).toEqual(pinned);
  });

  test("a changed source byte would flip the hash (drift is detectable)", () => {
    const original = readFileSync(resolve(HANDOFF_DS, "tokens/colors.css"));
    const mutated = Buffer.concat([original, Buffer.from("/* x */")]);
    expect(sha256(mutated)).not.toBe(sha256(original));
  });

  test("parseLock treats a corrupt/non-object lock as missing (fail closed, no crash)", () => {
    // A valid lock parses; a merge-conflicted / truncated / non-object lock is
    // null (→ the missing-lock 'run sync:lock' path), never an uncaught throw.
    expect(parseLock('{"a":"b"}')).toEqual({ a: "b" });
    expect(parseLock(null)).toBeNull();
    expect(parseLock("<<<<<<< HEAD\n{}")).toBeNull(); // merge conflict marker
    expect(parseLock("not json")).toBeNull();
    expect(parseLock("null")).toBeNull();
    expect(parseLock("[1,2]")).toBeNull(); // array is not a lock object
    expect(parseLock("42")).toBeNull();
  });
});

describe("build output layout (P20 regression: font path must resolve)", () => {
  test("every @font-face url() in the built typography.css points at a real file", async () => {
    await build();
    const typographyPath = resolve(DIST, "tokens/typography.css");
    const css = readFileSync(typographyPath, "utf8");
    const urls = [...css.matchAll(/url\(["']([^"']+)["']\)/g)].map((m) => m[1]);
    expect(urls.length).toBeGreaterThan(0);
    for (const url of urls) {
      // resolve the url relative to the CSS file's own dist location — the exact
      // thing a browser does. A `css/` wrapper on the styles but not the fonts
      // would make ../fonts/ miss (dist/css/fonts vs dist/fonts).
      const resolved = resolve(dirname(typographyPath), url as string);
      expect(existsSync(resolved)).toBe(true);
    }
    // and the woff2 is preferred over the ttf in the src list
    expect(css.indexOf("woff2")).toBeLessThan(css.indexOf("truetype"));
  });

  test("styles.css sits at the dist root so its @import tokens/* chain resolves", () => {
    expect(existsSync(resolve(DIST, "styles.css"))).toBe(true);
    const styles = readFileSync(resolve(DIST, "styles.css"), "utf8");
    for (const m of styles.matchAll(/@import\s+url\(["']([^"']+)["']\)/g)) {
      expect(existsSync(resolve(DIST, m[1] as string))).toBe(true);
    }
  });
});
