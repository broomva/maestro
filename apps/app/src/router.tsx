import { createRootRoute, createRoute, createRouter, Outlet } from "@tanstack/react-router";
import { Landing } from "./routes/landing";

// Code-based routing (no generated route tree) keeps the M0 scaffold explicit and
// lint-clean. File-based routing + the codegen plugin can arrive later if routes grow.
const rootRoute = createRootRoute({ component: () => <Outlet /> });

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  component: Landing,
});

const routeTree = rootRoute.addChildren([indexRoute]);

export const router = createRouter({ routeTree });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
