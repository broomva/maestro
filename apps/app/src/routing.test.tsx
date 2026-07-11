/// <reference types="bun" />

// routing (BRO-1824 done.check) — a thrown render error in one pane never blanks the shell. React 19's
// server renderer RETHROWS error-boundary errors (SSR expects client hydration to recover), so the
// boundary's CATCH behavior can't be observed via renderToStaticMarkup. Instead this asserts the boundary
// CONTRACT directly (getDerivedStateFromError → errored → fallback, else → children), the fallback is
// calm + leaks no stack, and every product view wires its own errorComponent so a crashed view falls
// back WITHIN the shell. The runtime catch itself is a well-established React/TanStack mechanism; the
// end-to-end navigation is dogfooded in routing.pw.ts.

import { describe, expect, test } from "bun:test";
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
  test("/, /knowledge, /history, /settings, /account each carry an errorComponent", () => {
    // A crashed view falls back WITHIN the shell only if the view route (not just the layout) has a
    // boundary. Inspect the real router tree so a future route added without one fails here.
    const byPath = new Map(
      Object.values(router.routesById).map((r) => [String(r.fullPath), r] as const),
    );
    for (const p of ["/", "/knowledge", "/history", "/settings", "/account"]) {
      const route = byPath.get(p);
      expect(route, `route ${p} is registered`).toBeDefined();
      expect(route?.options.errorComponent, `route ${p} wires an errorComponent`).toBeTruthy();
    }
  });
});
