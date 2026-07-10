import { createRootRoute, createRoute, createRouter, Outlet } from "@tanstack/react-router";
import { App } from "./routes/app";
import { KitchenSink } from "./routes/kitchen-sink";
import { Landing } from "./routes/landing";

// Code-based routing (no generated route tree) keeps the M0 scaffold explicit and
// lint-clean. File-based routing + the codegen plugin can arrive later if routes grow.
const rootRoute = createRootRoute({ component: () => <Outlet /> });

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  component: Landing,
});

// /kitchen-sink — the M1 primitive gallery (every variant/state). A developer surface.
const kitchenSinkRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/kitchen-sink",
  component: KitchenSink,
});

// /app — the live product surface (BRO-1780): the shell (BRO-1771) with the read-only Board
// mounted inside it, subscribed to the runtime SSE stream through the store.
const appRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/app",
  component: App,
});

const routeTree = rootRoute.addChildren([indexRoute, kitchenSinkRoute, appRoute]);

export const router = createRouter({ routeTree });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
