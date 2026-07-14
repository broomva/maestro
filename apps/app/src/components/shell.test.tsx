/// <reference types="bun" />

// Shell structure (BRO-1771 → BRO-1884 design fidelity). renderToStaticMarkup — no DOM harness, so
// it runs under CI's plain bun test (the never-scroll *behavior* is shell.pw.ts's browser concern).
// The chrome is now the IA4 tree-led sidebar + McvTopBar, so the nav is TanStack `<Link>`s (lens bar
// + footer) + a workspace tree — the Shell needs router context; a loaded memory router mounts it at
// `/` (the Maestro/board lens active), exactly how it renders in the app.

import { beforeAll, describe, expect, test } from "bun:test";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import type { ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { Shell } from "./shell";

// Every route the shell's Links target must exist in the tree for hrefs to resolve — the lens bar
// (/, /history, /knowledge), the footer (/settings, /account), and the orchestrator presence.
const STATIC_PATHS = ["/", "/knowledge", "/history", "/settings", "/account"];

/** Render the Shell inside a loaded memory router at `at` (default `/`, the board lens) — its real context. */
async function renderShell(children?: ReactNode, at = "/"): Promise<string> {
  const rootRoute = createRootRoute({ component: () => <Shell>{children}</Shell> });
  const staticRoutes = STATIC_PATHS.map((path) =>
    createRoute({ getParentRoute: () => rootRoute, path, component: () => null }),
  );
  const sessionRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/session/$sessionId",
    component: () => null,
  });
  const fileRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/file/$",
    component: () => null,
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([...staticRoutes, sessionRoute, fileRoute]),
    history: createMemoryHistory({ initialEntries: [at] }),
  });
  await router.load();
  return renderToStaticMarkup(<RouterProvider router={router} />);
}

describe("Shell — the tree-led chrome (BRO-1884)", () => {
  let html = "";
  beforeAll(async () => {
    html = await renderShell();
  });

  test("the shell is the bv-app grid at the 200px sidebar width (store-driven)", () => {
    expect(html).toContain('class="bv-app"');
    // Width comes from the persisted prefs slice (default 200 / CLAUDE.md §Layout), set inline.
    expect(html).toContain("grid-template-columns:200px 1fr");
  });

  test("the sidebar is a matte workspace tree with the inline brand chip (no #000 raster)", () => {
    expect(html).toContain('class="bv-sidebar mcc-nav"');
    // Brand mark is an inline SVG on a cool-axis --bv-ink chip — never the opaque raster that
    // painted a pure-#000 tile on the light sidebar (BRO-1771 P20).
    expect(html).toContain('data-testid="brand-mark"');
    expect(html).toContain("bg-[var(--bv-ink)]");
    expect(html).not.toContain("broomva-blackhole-logo");
    expect(html).not.toContain("<img");
    // The "Workspace" tree section is the backbone of the tree-led sidebar.
    expect(html).toContain("Workspace");
  });

  test("the adaptive lens bar + footer link to the product routes", () => {
    // Primary lens falls back to Maestro when the gate is clear (empty store → needsYou 0).
    expect(html).toContain('class="mcc-lensbar"');
    expect(html).toContain("Maestro");
    // Lenses + footer are real links to the routes.
    for (const path of ["/history", "/knowledge", "/settings", "/account"]) {
      expect(html).toContain(`href="${path}"`);
    }
  });

  test("the top bar carries the orchestrator presence (tidepool) linking to its session", () => {
    expect(html).toContain("bv-dot-live");
    expect(html).toContain("maestro");
    // The presence is an agent you can open, not a settings button (CLAUDE.md §What Maestro is).
    expect(html).toContain('href="/session/orchestrator"');
    // ...and the ⌘K command field on the center axis.
    expect(html).toContain('class="mcc-cmd"');
    expect(html).toContain("⌘K");
  });

  test("the main region is a no-scroll frame; the matched view owns the scroll (BRO-1886)", () => {
    // The shell frame owns NO scroll — the inner panel (the mission plane's .mcc-plane-body, or a
    // stub's own scroll wrapper) is the one that scrolls (CLAUDE.md §Layout: the shell never scrolls;
    // inner panels do). Coupled to <main> via its testid so it can't pass on an unrelated element.
    expect(html).toContain('overflow-hidden" data-testid="shell-main"');
  });

  test("the active lens (Maestro, at /) is marked for assistive tech", () => {
    expect(html).toContain('aria-current="page"');
  });

  test("renders children in place of the placeholder when given", async () => {
    const withChild = await renderShell("hello panel");
    expect(withChild).toContain("hello panel");
    expect(withChild).not.toContain("surfaces mount here");
  });
});

describe("Shell — the tab strip is work-surface chrome (BRO-1896)", () => {
  // The prototype frames the full-page views (History/Knowledge/Settings/Account) with NO tab strip;
  // the tab strip belongs to the Maestro plane (/), files (/file/$), and sessions (/session/$). The
  // sidebar + top bar stay on every route (the way back to Maestro).
  test("present on the work surface: the board /, a file, and a session", async () => {
    for (const at of ["/", "/file/spec.md", "/session/s1"]) {
      const html = await renderShell(undefined, at);
      expect(html).toContain('data-testid="tab-strip"');
    }
  });

  test("ABSENT on the full-page-view routes, but the sidebar + top bar stay", async () => {
    for (const at of ["/history", "/knowledge", "/settings", "/account"]) {
      const html = await renderShell(undefined, at);
      expect(html).not.toContain('data-testid="tab-strip"');
      // chrome the views keep: the sidebar (brand mark) + the top bar (⌘K command field).
      expect(html).toContain('data-testid="brand-mark"');
      expect(html).toContain('class="mcc-cmd"');
    }
  });
});
