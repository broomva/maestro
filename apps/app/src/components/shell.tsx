// Shell — the chrome the whole product lives in (BRO-1771 → BRO-1884 design fidelity, canon per
// docs/canon-map.md: the IA4 tree-led sidebar + McvTopBar). The mission-shell skeleton: a fixed
// matte workspace-tree sidebar + a 52px top bar + flex main. **The shell never scrolls — only the
// inner panels do** (the sidebar owns its overflow; the main owns its own; `.bv-app` is h-dvh +
// overflow-hidden). The sidebar width + collapse are the store's persisted UI-prefs (navOpen /
// cols.nav); the tree, the "needs you" count, and the narration are real server-truth selectors.
//
// It is the layout host: the matched product view renders into `children` (the layout's `<Outlet/>`,
// wired in routes/app.tsx, which also opens the one SSE connection). The mission plane replaces the
// placeholder board in a later fidelity ticket.

import { useNavigate, useRouterState } from "@tanstack/react-router";
import type { ReactNode } from "react";
import { useEffect, useMemo } from "react";
import { useStore } from "zustand";
import { activeFilePath } from "@/lib/file-route";
import {
  maestroStore,
  selectFileTree,
  selectNarration,
  selectNeedsYouCount,
  selectSidebarTree,
} from "@/store";
import { FilePane } from "./fs/file-pane";
import { TabStrip } from "./fs/tab-strip";
import { OverlayHost } from "./overlays/overlay-host";
import { Sidebar } from "./shell/sidebar";
import { TopBar } from "./shell/top-bar";

/** The default sidebar width when nothing is persisted (CLAUDE.md §Layout: 200px). */
const NAV_WIDTH_DEFAULT = 200;
/** The collapsed icon-rail width (matches `.mcc-rail` in shell.css + CLAUDE.md §Layout: 52px). */
const RAIL_WIDTH = 52;

/**
 * The chrome tab strip + FS pane belong to the WORK surface — the Maestro plane (`/`), an open file
 * (`/file/$`), or a session (`/session/$`). The full-page views (History / Knowledge / Settings /
 * Account) are standalone framed destinations reached from the sidebar nav: in the prototype each
 * renders its own `BvNavTree` + top bar with NO tab strip (`onOpenView` swaps the whole frame). So we
 * scope the tab strip + FS pane to the work surface and hide them on the view routes — the sidebar +
 * top bar stay on every route. Open-file state lives in the store, so returning to the work surface
 * restores the file tabs (no loss).
 */
function isWorkSurface(pathname: string): boolean {
  return pathname === "/" || pathname.startsWith("/file/") || pathname.startsWith("/session/");
}

/** Open the command palette (a later surface); dispatch the event the palette will listen for. */
function openCommandPalette() {
  if (typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent("bv:command-open"));
  }
}

/** Open the feedback drawer (a later surface); dispatch the event the drawer will listen for. */
function openFeedback() {
  if (typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent("bv:feedback-open"));
  }
}

export function Shell({ children }: { children?: ReactNode }) {
  // Select the STABLE `server` slice reference (changes only when the reducer applies an event) and
  // derive the chrome inputs in `useMemo` keyed on it — deriving selectors inline would return fresh
  // objects every render and thrash useSyncExternalStore. The derivations are pure + cheap.
  const server = useStore(maestroStore, (s) => s.server);
  const navOpen = useStore(maestroStore, (s) => s.navOpen);
  const navWidth = useStore(maestroStore, (s) => s.cols.nav ?? NAV_WIDTH_DEFAULT);
  const toggleNav = useStore(maestroStore, (s) => s.toggleNav);
  const fsOpen = useStore(maestroStore, (s) => s.fsOpen);
  const navigate = useNavigate();
  const pathname = useRouterState({ select: (st) => st.location.pathname });

  const tree = useMemo(() => selectSidebarTree(server), [server]);
  const needsYou = useMemo(() => selectNeedsYouCount(server), [server]);
  const narration = useMemo(() => selectNarration(server), [server]);
  // The FS pane's rows (workspace-as-files) + the active row (the open file). Deriving in useMemo keyed
  // on the stable `server` slice — inline derivation returns a fresh array every render and thrashes
  // useSyncExternalStore (the FID-2 getSnapshot lesson).
  const fileTree = useMemo(() => selectFileTree(server), [server]);
  const activeFile = activeFilePath(pathname);
  // The tab strip + FS pane are work-surface chrome, hidden on the full-page-view routes (see above).
  const workSurface = isWorkSurface(pathname);
  const showFs = workSurface && fsOpen;

  // ⌘K is global — open the palette from anywhere in the shell (the field in the top bar is one
  // affordance; the shortcut is another). The palette itself is a later fidelity ticket.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && (e.key === "k" || e.key === "K")) {
        e.preventDefault();
        openCommandPalette();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div
      className="bv-app"
      // The collapse slide (transition on grid-template-columns) lives in the `.bv-app` CSS rule,
      // not inline — so the prefers-reduced-motion block in shell.css can cancel it (an inline
      // transition would win over the stylesheet and keep animating; CLAUDE.md §Motion).
      style={{ gridTemplateColumns: `${navOpen ? navWidth : RAIL_WIDTH}px 1fr` }}
    >
      <Sidebar tree={tree} needsYou={needsYou} collapsed={!navOpen} onFeedback={openFeedback} />

      <div className="bv-main">
        <TopBar
          needsYou={needsYou}
          narration={narration}
          collapsed={!navOpen}
          onToggleCollapsed={toggleNav}
          onCommand={openCommandPalette}
        />
        {/* The chrome tab strip (BRO-1890) — the pinned Maestro tab + open file tabs + the FS toggle,
            under the header. Work-surface chrome only (BRO-1896): hidden on the full-page-view routes,
            which the prototype frames without a tab strip. */}
        {workSurface ? <TabStrip /> : null}
        {/* The main row: the matched view + the FS pane at the layout edge (a 248px column when open).
            The shell frame owns NO scroll — the matched view is the inner panel that scrolls (CLAUDE.md
            §Layout: the shell never scrolls; inner panels do). The mission plane fills `.mcc-fsmain` and
            owns its own scroll; the file pane owns its own. The FS pane is work-surface chrome too. */}
        <div className={`mcc-fsrow${showFs ? " has-fs" : ""}`}>
          <main className="mcc-fsmain min-h-0 overflow-hidden" data-testid="shell-main">
            {children ?? <ShellPlaceholder />}
          </main>
          {showFs ? (
            <div className="mcc-rpane" data-testid="fs-rpane">
              <FilePane
                entries={fileTree}
                openPath={activeFile}
                onOpen={(path) => navigate({ to: "/file/$", params: { _splat: path } })}
                label="Workspace"
              />
            </div>
          ) : null}
        </div>
      </div>

      {/* The transient overlay layer (BRO-1894 FID-7): the ⌘K command palette + the feedback drawer.
          Mounted once here so any surface can open one by firing `bv:command-open` / `bv:feedback-open`
          (dispatched by the ⌘K keydown above, the top-bar command field, and the sidebar footer). Both
          render null when closed, so this never touches the shell's SSR render. */}
      <OverlayHost />
    </div>
  );
}

/** Shown only on a standalone/test render with no child view. */
function ShellPlaceholder() {
  return (
    <div className="flex h-full items-center justify-center text-muted-foreground text-sm">
      The board, knowledge, history, and settings surfaces mount here.
    </div>
  );
}
