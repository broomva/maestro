import { createRootRoute, createRoute, createRouter, Outlet } from "@tanstack/react-router";
import { Shell } from "./components/shell";
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

// /app — the M2 shell (BRO-1771): sidebar + top bar + never-scroll main. Real product routes
// mount inside it in BRO-1824; for now it renders the shell's placeholder panel.
const appRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/app",
  component: Shell,
});

const routeTree = rootRoute.addChildren([indexRoute, kitchenSinkRoute, appRoute]);

export const router = createRouter({ routeTree });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
