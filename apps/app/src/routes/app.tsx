// The /app route (BRO-1780) — the live product surface. Wires the client store to the runtime
// SSE stream ONCE on mount (hydrate /api/tree, then live off /api/stream through the vite `/api`
// proxy), and mounts the read-only Board inside the shell. connectStream was authored by the
// store (BRO-1775) but left unwired until a real surface consumed it — this is that surface.

import { useEffect } from "react";
import { Board } from "@/components/board/board";
import { Shell } from "@/components/shell";
import { connectStream, maestroStore } from "@/store";

export function App() {
  useEffect(() => {
    // Same-origin `/api/*` (default baseUrl) → the vite proxy forwards to the runtime.
    const handle = connectStream(maestroStore);
    return () => handle.close();
  }, []);

  return (
    <Shell>
      <Board />
    </Shell>
  );
}
