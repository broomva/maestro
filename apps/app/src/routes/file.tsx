// The file route (BRO-1890 FID-4) — renders one open workspace file as a document. The `$` splat param
// carries the node's workspace-relative path (paths nest, so a splat, not a flat id). It looks the node
// up by path (`selectFileNode`) and paints the `FileView`. On mount it ensures the path is an open tab
// (`openFile`, idempotent) so a DEEP LINK to /file/... shows a tab too — the tab strip stays in sync
// whether the file was opened from the pane or navigated to directly. A missing node renders a calm
// "no such file" (the pane only offers real paths, but a stale deep link must not crash the shell).

import { useParams } from "@tanstack/react-router";
import { useEffect, useMemo } from "react";
import { useStore } from "zustand";
import { FileView } from "@/components/fs/file-view";
import { maestroStore, selectFileNode } from "@/store";

export function FileRoute() {
  // Loose param read (registered in router.tsx; `strict:false` avoids a config import cycle, mirroring
  // the session route). The catch-all param is `_splat`.
  const params = useParams({ strict: false }) as { _splat?: string };
  const path = params._splat ?? "";
  const server = useStore(maestroStore, (s) => s.server);
  const openFile = useStore(maestroStore, (s) => s.openFile);
  const node = useMemo(() => selectFileNode(server, path), [server, path]);

  // Keep the tab strip in sync for deep links (openFile is idempotent — a no-op if already open).
  useEffect(() => {
    if (path) openFile(path);
  }, [path, openFile]);

  return <FileView node={node} />;
}
