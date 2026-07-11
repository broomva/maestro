import { createRootRoute, createRoute, createRouter, Outlet } from "@tanstack/react-router";
import type { ReactNode } from "react";
import { Board } from "./components/board/board";
import { PaneErrorFallback } from "./components/error-boundary";
import { ShellLayout } from "./routes/app";
import { KitchenSink } from "./routes/kitchen-sink";
import { AccountView, HistoryView, KnowledgeView, SettingsView } from "./routes/stubs";

// Code-based routing (no generated route tree) keeps routing explicit + lint-clean. The product views
// map 1:1 to /, /knowledge, /history, /settings, /account (production-notes §1); they are CHILDREN of a
// pathless shell LAYOUT route so they share one Shell + one SSE connection (BRO-1824). Each view sets an
// `errorComponent` so a crashed view falls back to a calm plain-voice pane WITHIN the shell — the
// sidebar + header stay (the done.check: a thrown render error in one pane never blanks the shell).

const rootRoute = createRootRoute({ component: () => <Outlet /> });

/** The shell layout — pathless, so every product view renders inside the same chrome + live stream. */
const shellRoute = createRoute({
  getParentRoute: () => rootRoute,
  id: "shell",
  component: ShellLayout,
});

// `const P` preserves the path LITERAL through createRoute so TanStack's typed `<Link to>` union keeps
// every view path (a plain `string` param would erase them → `to="/account"` would not typecheck).
const view = <const P extends string>(path: P, component: () => ReactNode, label: string) =>
  createRoute({
    getParentRoute: () => shellRoute,
    path,
    component,
    errorComponent: () => <PaneErrorFallback label={label} />,
  });

const boardRoute = view("/", Board, "The board");
const knowledgeRoute = view("/knowledge", KnowledgeView, "Knowledge");
const historyRoute = view("/history", HistoryView, "History");
const settingsRoute = view("/settings", SettingsView, "Settings");
const accountRoute = view("/account", AccountView, "Account");

// Test-fixture route (BRO-1824 done-check): a view that DELIBERATELY throws on render, so routing.pw.ts
// can prove at RUNTIME that a crashed view falls back within the shell (chrome survives) — React 19 SSR
// rethrows boundary errors, so the client catch is only observable in a real browser. Harmless in prod
// (an obscure, undocumented path that just shows the calm fallback), same class of dev surface as
// /kitchen-sink. It is a child of the shell so its errorComponent renders inside the shell's Outlet.
const CrashProbe = (): never => {
  throw new Error("crash probe (BRO-1824 test fixture)");
};
const crashRoute = view("/__crash-probe", CrashProbe, "This view");

// /kitchen-sink — the M1 primitive gallery (every variant/state). A developer surface, OUTSIDE the shell.
const kitchenSinkRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/kitchen-sink",
  component: KitchenSink,
});

const routeTree = rootRoute.addChildren([
  shellRoute.addChildren([
    boardRoute,
    knowledgeRoute,
    historyRoute,
    settingsRoute,
    accountRoute,
    crashRoute,
  ]),
  kitchenSinkRoute,
]);

export const router = createRouter({
  routeTree,
  // Whole-app backstop — fires if the shell LAYOUT itself crashes (it has no errorComponent of its own),
  // so the copy must NOT claim the rest of the app is fine (`scope="shell"`); per-view crashes hit their
  // own pane-scoped errorComponent above, where the shell survives.
  defaultErrorComponent: () => <PaneErrorFallback scope="shell" />,
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
