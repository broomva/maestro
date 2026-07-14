// The top bar (BRO-1884) — McvTopBar (WorkPanel.jsx): the orchestrator's presence + the
// command axis. Left: the sidebar toggle + the wake-log narration (maestro's last move). Center:
// the ⌘K command field on the true center axis. Right: the "N needs you" chip · the tick ring ·
// the theme toggle. Matte chrome (never glass — CLAUDE.md §Glass).
//
// FID-1 honesty: the narration is DERIVED from real store state (below), not the prototype's
// hardcoded "maestro woke 2m ago"; the ⌘K field opens the command palette, a later surface, so
// it dispatches the `bv:command-open` event the palette will listen for (harmless until then).

import { DotComet } from "@maestro/ui";
import { Link } from "@tanstack/react-router";
import { MessageCircle, PanelLeft } from "lucide-react";
import { ThemeToggle } from "../theme-toggle";
import { TickRing } from "./tick-ring";

interface TopBarProps {
  /** count of work at a gate or stuck — the "N needs you" chip. */
  needsYou: number;
  /** the orchestrator's last move, derived from real state (see selectNarration). */
  narration: string;
  collapsed: boolean;
  onToggleCollapsed: () => void;
  /** open the command palette (⌘K) — a later surface; a placeholder dispatch for now. */
  onCommand: () => void;
}

export function TopBar({
  needsYou,
  narration,
  collapsed,
  onToggleCollapsed,
  onCommand,
}: TopBarProps) {
  return (
    <header className="bv-top-bar mcv-top" data-screen-label="Top bar">
      <div
        className="mcv-narr"
        style={{ display: "flex", alignItems: "center", gap: 4, minWidth: 0 }}
      >
        <button
          type="button"
          onClick={onToggleCollapsed}
          aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}
          title={collapsed ? "Expand sidebar" : "Collapse sidebar"}
          className="grid size-8 shrink-0 place-items-center rounded-row text-muted-foreground transition-colors motion-reduce:transition-none hover:bg-[var(--bv-frost-8)] hover:text-foreground"
        >
          <PanelLeft size={16} strokeWidth={2} />
        </button>
        <Link
          to="/session/$sessionId"
          params={{ sessionId: "orchestrator" }}
          className="mcc-quiet mcc-narr"
          title="Open the orchestrator's session"
        >
          <DotComet size={13} />
          <span className="mcv-narr-text">maestro · {narration}</span>
        </Link>
      </div>

      <button
        type="button"
        className="mcc-cmd"
        onClick={onCommand}
        aria-label="Ask, find, or start work (Command K)"
      >
        <MessageCircle size={14} className="shrink-0" />
        <span className="mcc-cmd-ph">Ask, find, or start work…</span>
        <span className="mcc-cmd-kbd">⌘K</span>
      </button>

      <div className="mc-topbar-right">
        {needsYou > 0 ? (
          <Link
            to="/"
            activeOptions={{ exact: true }}
            className="mcc-attn-chip mcc-attn-btn"
            title="Waiting on you"
          >
            <span className="mc-chip-dot" style={{ background: "var(--bv-blue-accent)" }} />
            {needsYou} need{needsYou === 1 ? "s" : ""} you
          </Link>
        ) : null}
        <TickRing />
        <ThemeToggle />
      </div>
    </header>
  );
}
