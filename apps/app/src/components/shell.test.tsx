/// <reference types="bun" />

// Shell structure (BRO-1771). renderToStaticMarkup — no DOM harness, so it runs under CI's
// plain bun test (the never-scroll *behavior* + the resize verify are the browser concern of
// shell.pw.ts). ThemeToggle is SSR-safe (typeof document guard), so the whole shell renders.

import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Shell } from "./shell";

describe("Shell — the M2 chrome", () => {
  const html = renderToStaticMarkup(<Shell />);

  test("is a never-scroll 200px + flex grid — the shell owns no scroll", () => {
    expect(html).toContain("grid-cols-[200px_1fr]");
    expect(html).toContain("h-dvh");
    expect(html).toContain("overflow-hidden");
  });

  test("the sidebar is matte with the inline brand mark (no #000 raster) and a Lucide nav", () => {
    expect(html).toContain("bg-sidebar");
    // The brand mark is an inline SVG on a cool-axis --bv-ink chip — never the opaque raster
    // that painted a pure-#000 tile on the light sidebar (BRO-1771 P20).
    expect(html).toContain("bg-[var(--bv-ink)]");
    expect(html).not.toContain("broomva-blackhole-logo");
    expect(html).not.toContain("<img");
    for (const label of ["Board", "Knowledge", "History", "Settings"]) {
      expect(html).toContain(label);
    }
  });

  test("the top bar carries the orchestrator presence (tidepool) — an agent, not a menu", () => {
    expect(html).toContain("bv-dot-live");
    expect(html).toContain("maestro");
  });

  test("the main panel owns the scroll", () => {
    // Couple the assertion to <main> — `overflow-y-auto` is also on <aside>, so a bare
    // toContain would stay green even if <main> lost its scroll (BRO-1771 P20 nit).
    expect(html).toContain('overflow-y-auto p-6" data-testid="shell-main"');
  });

  test("the active nav item is marked for assistive tech", () => {
    expect(html).toContain('aria-current="page"');
  });

  test("renders children in place of the placeholder when given", () => {
    const withChild = renderToStaticMarkup(<Shell>hello panel</Shell>);
    expect(withChild).toContain("hello panel");
    expect(withChild).not.toContain("Panel row 1");
  });
});
