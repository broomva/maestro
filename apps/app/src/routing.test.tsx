/// <reference types="bun" />

// routing (BRO-1824 done.check) — a thrown render error in one pane never blanks the shell. React 19's
// server renderer RETHROWS error-boundary errors (SSR expects client hydration to recover), so the
// boundary's CATCH behavior can't be observed via renderToStaticMarkup. Instead this asserts the boundary
// CONTRACT directly (getDerivedStateFromError → errored → fallback, else → children), the fallback is
// calm + leaks no stack, and every product view wires its own errorComponent so a crashed view falls
// back WITHIN the shell. The runtime catch itself is a well-established React/TanStack mechanism; the
// end-to-end navigation is dogfooded in routing.pw.ts.

import { describe, expect, test } from "bun:test";
import { type ComponentType, createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { ErrorBoundary, PaneErrorFallback } from "./components/error-boundary";
import { router } from "./router";

describe("routing — the calm pane-error fallback", () => {
  test("renders plain voice, labelled, role=alert, and leaks no raw error/stack", () => {
    const html = renderToStaticMarkup(<PaneErrorFallback label="The inspector" />);
    expect(html).toContain('data-testid="pane-error"');
    expect(html).toContain("The inspector hit a snag.");
    expect(html).toContain('role="alert"');
    expect(html).not.toContain("Error"); // no stack / no scary word in the chrome
  });

  test("unlabelled fallback still reads calm", () => {
    expect(renderToStaticMarkup(<PaneErrorFallback />)).toContain("This pane hit a snag.");
  });

  test("shell scope (whole-app backstop) does NOT claim the rest of the app is fine", () => {
    // The default/root errorComponent fires when the shell itself crashed — the pane copy would lie.
    const html = renderToStaticMarkup(<PaneErrorFallback scope="shell" />);
    expect(html).toContain("Something went wrong.");
    expect(html).not.toContain("The rest of the app is fine");
  });
});

describe("routing — ErrorBoundary contains a pane crash", () => {
  test("getDerivedStateFromError flips to errored", () => {
    expect(ErrorBoundary.getDerivedStateFromError()).toEqual({ errored: true });
  });

  test("errored → renders the calm fallback (with the label), NOT the children", () => {
    const eb = new ErrorBoundary({
      children: <div>live pane content</div>,
      label: "The inspector",
    });
    eb.state = { errored: true };
    const html = renderToStaticMarkup(<>{eb.render()}</>);
    expect(html).toContain('data-testid="pane-error"');
    expect(html).toContain("The inspector hit a snag.");
    expect(html).not.toContain("live pane content");
  });

  test("healthy → renders its children untouched", () => {
    const eb = new ErrorBoundary({ children: <div>live pane content</div> });
    const html = renderToStaticMarkup(<>{eb.render()}</>);
    expect(html).toContain("live pane content");
    expect(html).not.toContain('data-testid="pane-error"');
  });
});

describe("routing — every product view wires its own error boundary", () => {
  test("EVERY view under the shell layout carries an errorComponent (future views auto-caught)", () => {
    // A crashed view falls back WITHIN the shell only if the view route (not just the layout) has a
    // boundary. Key by the stable route id, NOT fullPath (three routes share fullPath "/": __root__, the
    // pathless /shell layout, and the board /shell/ — a fullPath map is ambiguous). Every child of the
    // shell (id under "/shell/") must carry an errorComponent, so a future view added without one fails.
    const viewRoutes = Object.values(router.routesById).filter((r) =>
      String(r.id).startsWith("/shell/"),
    );
    expect(viewRoutes.length).toBeGreaterThanOrEqual(5); // board + 4 stubs (+ the crash-probe fixture)
    for (const r of viewRoutes) {
      expect(r.options.errorComponent, `view ${String(r.id)} wires an errorComponent`).toBeTruthy();
    }
  });

  test("the five product views are all registered + protected (a removed view is caught)", () => {
    const byId = router.routesById;
    for (const id of [
      "/shell/",
      "/shell/knowledge",
      "/shell/history",
      "/shell/settings",
      "/shell/account",
    ]) {
      expect(byId[id as keyof typeof byId], `${id} registered`).toBeDefined();
      expect(
        byId[id as keyof typeof byId]?.options.errorComponent,
        `${id} wires an errorComponent`,
      ).toBeTruthy();
    }
  });

  test("a view's errorComponent actually RENDERS the calm fallback (not merely registered)", () => {
    // Registration (toBeTruthy above) is not enough — render the board's errorComponent and confirm it
    // produces the pane-error fallback, so a mis-wired errorComponent (registered but rendering nothing
    // or a stack) is caught.
    const board = router.routesById["/shell/" as keyof typeof router.routesById];
    const EC = board?.options.errorComponent;
    expect(EC).toBeTruthy();
    const html = renderToStaticMarkup(createElement(EC as ComponentType));
    expect(html).toContain('data-testid="pane-error"');
    expect(html).toContain("The board hit a snag.");
  });
});
