// The sidebar (BRO-1884) — the IA4 tree-led nav (BvNavTree, ConceptNavIA.jsx; the chosen
// direction per canon-map). Structure: workspace switcher → adaptive lens bar (Needs you ⇄
// Maestro · History · Knowledge) → the Workspace tree → the autonomy scoreboard → footer
// (Feedback · Settings · profile). "The sidebar IS the workspace": the tree is real WorkItems.
//
// Navigation is TanStack `<Link>` to the product routes (the prototype's `onOpenView` state
// switch becomes real URL routing — porting-notes: state in its taxonomy home). Collapses to a
// 52px icon rail when `collapsed`; the collapse toggle lives in the top bar (it migrates to the
// tab strip in a later fidelity ticket).

import { Avatar, BlackholeMark } from "@maestro/ui";
import { Link, useNavigate } from "@tanstack/react-router";
import {
  Boxes,
  ChevronsUpDown,
  Folder,
  FolderOpen,
  History,
  Inbox,
  type LucideIcon,
  MessageSquare,
  Settings,
  Share2,
} from "lucide-react";
import { useCallback } from "react";
import type { SidebarTree } from "@/store";
import { AutonomyScoreboard } from "./autonomy-scoreboard";
import { WorkspaceTree } from "./workspace-tree";

interface SidebarProps {
  tree: SidebarTree;
  /** count of work at a gate or stuck — drives the adaptive primary lens + its badge. */
  needsYou: number;
  /** collapsed to the icon rail (driven by the store `prefs.navOpen`). */
  collapsed: boolean;
  /** open the feedback drawer — the Shell wires this to dispatch `bv:feedback-open` (the drawer
   *  surface lands in a later fidelity ticket, same forward-wire pattern as the ⌘K palette). */
  onFeedback?: () => void;
}

/** The dark cool-axis brand chip (never the opaque raster — BRO-1771 P20). */
function BrandChip() {
  return (
    <span
      data-testid="brand-mark"
      className="grid size-6 shrink-0 place-items-center rounded-lg bg-[var(--bv-ink)] text-[var(--bv-white)] ring-1 ring-[var(--bv-border-15)] ring-inset"
    >
      <BlackholeMark size={16} />
    </span>
  );
}

export function Sidebar({ tree, needsYou, collapsed, onFeedback }: SidebarProps) {
  const navigate = useNavigate();
  // Stable identity so WorkspaceTree's memo can short-circuit when only unrelated chrome state
  // changes (an inline arrow would be a fresh prop every render, defeating the memo).
  const toBoard = useCallback(() => navigate({ to: "/" }), [navigate]);

  if (collapsed) {
    return <RailSidebar tree={tree} needsYou={needsYou} onFeedback={onFeedback} />;
  }

  return (
    <aside className="bv-sidebar mcc-nav" data-screen-label="Sidebar">
      <button className="bv-ws-switch" type="button" title="Switch workspace">
        <BrandChip />
        <span className="bv-ws-name">Broomva</span>
        <ChevronsUpDown size={14} className="shrink-0 text-muted-foreground" />
      </button>

      {/* Adaptive lens bar: the primary reads "Needs you" (+count) while work waits at your
          gate, and falls back to "Maestro" the moment the queue clears. */}
      <div className="mcc-lensbar">
        <Link
          to="/"
          activeOptions={{ exact: true }}
          className="mcc-lens"
          activeProps={{ className: "is-active" }}
        >
          {needsYou > 0 ? (
            <>
              <Inbox size={14} />
              Needs you
              <span className="mcc-lens-badge">{needsYou}</span>
            </>
          ) : (
            <>
              <Boxes size={14} />
              Maestro
            </>
          )}
        </Link>
        <Link to="/history" className="mcc-lens" activeProps={{ className: "is-active" }}>
          <History size={14} />
          History
        </Link>
        <Link to="/knowledge" className="mcc-lens" activeProps={{ className: "is-active" }}>
          <Share2 size={14} />
          Knowledge
        </Link>
      </div>

      <div className="bv-sb-section-label">Workspace</div>
      <WorkspaceTree tree={tree} onSelect={toBoard} />

      <div className="bv-sb-spacer" />

      <AutonomyScoreboard />

      <div className="mcc-nav-foot">
        <button className="mcc-foot-btn" type="button" onClick={onFeedback} title="Send feedback">
          <MessageSquare size={15} />
          Feedback
        </button>
        <Link to="/settings" className="mcc-foot-btn" activeProps={{ className: "is-active" }}>
          <Settings size={15} />
          Settings
        </Link>
        <Link
          to="/account"
          className="mcc-foot-btn mcc-foot-profile"
          activeProps={{ className: "is-active" }}
        >
          <Avatar name="Ana Diaz" color="var(--bv-gray-600)" size={20} />
          <span>Ana Diaz</span>
        </Link>
      </div>
    </aside>
  );
}

/** One icon button on the collapsed rail (a Link when it routes, a button otherwise). */
function RailLink({
  to,
  icon: Icon,
  label,
  badge,
}: {
  to: string;
  icon: LucideIcon;
  label: string;
  badge?: number;
}) {
  return (
    <Link
      to={to}
      activeOptions={to === "/" ? { exact: true } : undefined}
      className="mcc-rail-btn"
      activeProps={{ className: "is-on" }}
      title={label}
      aria-label={label}
    >
      <Icon size={16} />
      {badge != null && badge > 0 ? <span className="mcc-rail-badge">{badge}</span> : null}
    </Link>
  );
}

/** The collapsed sidebar — a 52px icon rail (lenses + a few workspace folders + footer). */
function RailSidebar({ tree, needsYou, onFeedback }: Omit<SidebarProps, "collapsed">) {
  const navigate = useNavigate();
  return (
    <aside className="mcc-rail mcc-rail--side" data-screen-label="Sidebar (rail)">
      <span
        data-testid="brand-mark"
        className="mb-1.5 grid size-6 shrink-0 place-items-center rounded-lg bg-[var(--bv-ink)] text-[var(--bv-white)] ring-1 ring-[var(--bv-border-15)] ring-inset"
      >
        <BlackholeMark size={15} />
      </span>
      <RailLink
        to="/"
        icon={needsYou > 0 ? Inbox : Boxes}
        label={needsYou > 0 ? "Needs you" : "Maestro"}
        badge={needsYou}
      />
      <RailLink to="/history" icon={History} label="History" />
      <RailLink to="/knowledge" icon={Share2} label="Knowledge" />
      <div className="mcc-rail-div" />
      {tree.initiatives.slice(0, 5).map((init) => (
        <button
          key={init.name}
          className="mcc-rail-btn"
          type="button"
          onClick={() => navigate({ to: "/" })}
          title={init.name}
          aria-label={init.name}
        >
          <FolderOpen size={15} />
          {init.projects.some((p) => p.attn > 0) ? (
            <span className="mcc-rail-badge">{init.projects.reduce((n, p) => n + p.attn, 0)}</span>
          ) : null}
        </button>
      ))}
      {tree.initiatives.length === 0 ? (
        <button
          className="mcc-rail-btn"
          type="button"
          onClick={() => navigate({ to: "/" })}
          title="Workspace"
          aria-label="Workspace"
        >
          <Folder size={15} />
        </button>
      ) : null}
      <div className="bv-sb-spacer" />
      <button
        className="mcc-rail-btn"
        type="button"
        onClick={onFeedback}
        title="Feedback"
        aria-label="Feedback"
      >
        <MessageSquare size={15} />
      </button>
      <RailLink to="/settings" icon={Settings} label="Settings" />
      <Link
        to="/account"
        className="mcc-rail-btn mcc-rail-avatar"
        title="Ana Diaz"
        aria-label="Account"
      >
        <Avatar name="Ana Diaz" color="var(--bv-gray-600)" size={20} />
      </Link>
    </aside>
  );
}
