import { createRootRoute, createRoute, createRouter, Outlet } from "@tanstack/react-router";
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

const routeTree = rootRoute.addChildren([indexRoute, kitchenSinkRoute]);

export const router = createRouter({ routeTree });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
