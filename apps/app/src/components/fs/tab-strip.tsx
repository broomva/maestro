// The chrome tab strip (BRO-1890 FID-4) — the app-level tabs under the header (`.mcc-ftabs`, placement
// B). It carries the pinned "Maestro" tab (the orchestrator's plane, route "/") and the open file tabs
// (server truth `openFilePaths`, opened from the file pane), plus the file-pane toggle at the right edge.
// The ACTIVE tab derives from the route — Maestro when "/", a file when "/file/$path" — never a second
// source of truth. Session tabs (multi-session) + column-resize + drag-to-split are deferred follow-ups.

import { Link, useNavigate, useRouterState } from "@tanstack/react-router";
import { FileText, PanelRight, X } from "lucide-react";
import { useStore } from "zustand";
import { maestroStore } from "@/store";

/** The last path segment — the tab label (the prototype's `p.split("/").pop()`). */
function basename(path: string): string {
  return path.split("/").filter(Boolean).pop() ?? path;
}

/** The open file's path when the route is "/file/$path", else null (the active-tab source). */
function activeFilePath(pathname: string): string | null {
  const prefix = "/file/";
  if (!pathname.startsWith(prefix)) return null;
  return decodeURIComponent(pathname.slice(prefix.length));
}

export function TabStrip() {
  const navigate = useNavigate();
  const openFilePaths = useStore(maestroStore, (s) => s.server.openFilePaths);
  const closeFile = useStore(maestroStore, (s) => s.closeFile);
  const fsOpen = useStore(maestroStore, (s) => s.fsOpen);
  const toggleFs = useStore(maestroStore, (s) => s.toggleFs);
  const pathname = useRouterState({ select: (st) => st.location.pathname });

  const activeFile = activeFilePath(pathname);
  const maestroActive = pathname === "/";

  const openFileTab = (path: string) => navigate({ to: "/file/$", params: { _splat: path } });
  const onCloseFile = (path: string) => {
    closeFile(path);
    // Closing the active file falls back to the plane (the pinned Maestro tab is always present).
    if (activeFile === path) navigate({ to: "/" });
  };

  return (
    <div className="mcc-ftabs" data-testid="tab-strip" data-screen-label="Tab strip">
      <Link
        to="/"
        className={`mcc-ftab${maestroActive ? " is-active" : ""}`}
        title="The orchestrator's plane — work grouped by attention"
      >
        <span className="mc-chip-dot" style={{ background: "var(--bv-info)" }} />
        <span className="mcc-ftab-name">Maestro</span>
      </Link>

      <span className="mcc-ftabs-spacer" />

      {openFilePaths.map((p) => {
        const name = basename(p);
        return (
          <div
            key={p}
            className={`mcc-ftab mcc-ftab--in${activeFile === p ? " is-active" : ""}`}
            title={p}
          >
            <button type="button" className="mcc-ftab-link" onClick={() => openFileTab(p)}>
              <FileText size={13} strokeWidth={2} />
              <span className="mcc-ftab-name">{name}</span>
            </button>
            <button
              type="button"
              className="mcc-ftab-x"
              aria-label={`Close ${name}`}
              onClick={() => onCloseFile(p)}
            >
              <X size={11} strokeWidth={2} />
            </button>
          </div>
        );
      })}

      <button
        type="button"
        className={`mcc-prompt-iconbtn${fsOpen ? " is-on" : ""}`}
        style={{ width: 26, height: 26 }}
        aria-label={fsOpen ? "Hide files" : "Show files"}
        aria-pressed={fsOpen}
        title={fsOpen ? "Hide files" : "Show files"}
        onClick={toggleFs}
      >
        <PanelRight size={14} strokeWidth={2} />
      </button>
    </div>
  );
}
