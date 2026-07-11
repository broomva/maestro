/// <reference types="bun" />

// Shell structure (BRO-1771; router context BRO-1824). renderToStaticMarkup — no DOM harness, so it runs
// under CI's plain bun test (the never-scroll *behavior* is shell.pw.ts's browser concern). The nav is
// now TanStack `<Link>`s, so the Shell needs router context — a loaded memory router mounts it at `/`
// (Board active), which is exactly how it renders in the app.

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

const NAV_PATHS = ["/", "/knowledge", "/history", "/settings", "/account"];

/** Render the Shell inside a loaded memory router at `/` (Board active) — its real render context. */
async function renderShell(children?: ReactNode): Promise<string> {
  const rootRoute = createRootRoute({ component: () => <Shell>{children}</Shell> });
  // The Shell's nav Links target these paths; they must exist in the tree for the active match to resolve.
  const routes = NAV_PATHS.map((path) =>
    createRoute({ getParentRoute: () => rootRoute, path, component: () => null }),
  );
  const router = createRouter({
    routeTree: rootRoute.addChildren(routes),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  await router.load();
  return renderToStaticMarkup(<RouterProvider router={router} />);
}

describe("Shell — the M2 chrome", () => {
  let html = "";
  beforeAll(async () => {
    html = await renderShell();
  });

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

  test("the nav items are links to the product routes", () => {
    for (const path of ["/knowledge", "/history", "/settings", "/account"]) {
      expect(html).toContain(`href="${path}"`);
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

  test("the active nav item (Board, at /) is marked for assistive tech", () => {
    expect(html).toContain('aria-current="page"');
  });

  test("renders children in place of the placeholder when given", async () => {
    const withChild = await renderShell("hello panel");
    expect(withChild).toContain("hello panel");
    expect(withChild).not.toContain("Panel row 1");
  });
});
