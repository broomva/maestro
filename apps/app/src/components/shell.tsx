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

import type { ReactNode } from "react";
import { useEffect, useMemo } from "react";
import { useStore } from "zustand";
import { maestroStore, selectNarration, selectNeedsYouCount, selectSidebarTree } from "@/store";
import { Sidebar } from "./shell/sidebar";
import { TopBar } from "./shell/top-bar";

/** The default sidebar width when nothing is persisted (CLAUDE.md §Layout: 200px). */
const NAV_WIDTH_DEFAULT = 200;
/** The collapsed icon-rail width (matches `.mcc-rail` in shell.css + CLAUDE.md §Layout: 52px). */
const RAIL_WIDTH = 52;

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

  const tree = useMemo(() => selectSidebarTree(server), [server]);
  const needsYou = useMemo(() => selectNeedsYouCount(server), [server]);
  const narration = useMemo(() => selectNarration(server), [server]);

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
        <main className="min-h-0 flex-1 overflow-y-auto p-6" data-testid="shell-main">
          {children ?? <ShellPlaceholder />}
        </main>
      </div>
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
