// The shell LAYOUT route (BRO-1824) — the chrome every product view lives inside. It wires the client
// store to the runtime SSE stream ONCE on mount (hydrate /api/tree, then live off /api/stream through
// the vite `/api` proxy) and renders the matched child view into the shell's main via <Outlet/>. The
// board (/), knowledge, history, settings, and account routes are its children (production-notes §1);
// switching views does NOT re-open the SSE connection. (Was the /app route in BRO-1780; generalized to a
// layout when real routing landed.)

import { Outlet } from "@tanstack/react-router";
import { useEffect } from "react";
import { Shell } from "@/components/shell";
import { connectStream, maestroStore } from "@/store";

export function ShellLayout() {
  useEffect(() => {
    // Same-origin `/api/*` (default baseUrl) → the vite proxy forwards to the runtime.
    const handle = connectStream(maestroStore);
    return () => handle.close();
  }, []);

  return (
    <Shell>
      <Outlet />
    </Shell>
  );
}
