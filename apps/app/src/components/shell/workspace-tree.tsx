// The workspace tree (BRO-1884) — "the sidebar IS the workspace" (MccTcSidebar / NavTreeRows,
// ConceptTreeClick.jsx + ConceptNavIA.jsx). Initiatives → projects, folders at depth, live
// sessions as tidepool DotComets, work at a gate / stuck as attention badges, progress as
// `done/total` receipts (never a percentage — CLAUDE.md §Work states). Derived from real
// WorkItems by `selectSidebarTree`, not seed data.
//
// Selection scopes the plane (root → initiative → project); the scoped mission plane is a later
// fidelity ticket, so for now a row navigates to the board (`/`) — the workspace at root scope.
// Rows stay real buttons (keyboard-reachable) rather than inert text.

import { DotComet } from "@maestro/ui";
import { Folder, FolderOpen } from "lucide-react";
import { memo, type ReactNode } from "react";
import type { SidebarTree } from "@/store";

interface WorkspaceTreeProps {
  tree: SidebarTree;
  /** navigate/scope on row click (the board `/` for now; folder-scoped planes land later). */
  onSelect: () => void;
}

/** Indented row shared by every tree rung; the glyph carries live/folder state. */
function TreeRow({
  depth,
  glyph,
  label,
  trailing,
  title,
  onSelect,
}: {
  depth: 0 | 1 | 2;
  glyph: ReactNode;
  label: string;
  trailing?: ReactNode;
  title?: string;
  onSelect: () => void;
}) {
  return (
    <button
      className="bv-sb-item"
      type="button"
      onClick={onSelect}
      title={title}
      style={depth === 0 ? undefined : { paddingLeft: depth === 1 ? 24 : 42 }}
    >
      {glyph}
      <span className="mcc-sb-text">{label}</span>
      {trailing}
    </button>
  );
}

/** A project folder row — a live tidepool dot when running, else a folder; an attention badge. */
function ProjectRow({
  depth,
  name,
  live,
  attn,
  onSelect,
}: {
  depth: 1 | 2;
  name: string;
  live: boolean;
  attn: number;
  onSelect: () => void;
}) {
  return (
    <TreeRow
      depth={depth}
      glyph={live ? <DotComet size={13} /> : <Folder size={depth === 1 ? 14 : 13} />}
      label={name}
      title={live ? `${name} · live` : name}
      trailing={attn > 0 ? <span className="bv-sb-badge">{attn}</span> : undefined}
      onSelect={onSelect}
    />
  );
}

export const WorkspaceTree = memo(function WorkspaceTree({ tree, onSelect }: WorkspaceTreeProps) {
  const { initiatives, looseProjects, placesCount } = tree;
  const isEmpty = placesCount === 0;
  return (
    <div className="mcc-sb-col">
      <TreeRow
        depth={0}
        glyph={<FolderOpen size={14} />}
        label="Broomva"
        title="The workspace root"
        trailing={
          <span className="mc-init-progress">
            {placesCount} {placesCount === 1 ? "place" : "places"}
          </span>
        }
        onSelect={onSelect}
      />
      {initiatives.map((init) => (
        <div key={init.name} className="mcc-sb-col">
          <TreeRow
            depth={1}
            glyph={<FolderOpen size={14} />}
            label={init.name}
            trailing={
              <span className="mc-init-progress">
                {init.done}/{init.total}
              </span>
            }
            onSelect={onSelect}
          />
          {init.projects.map((p) => (
            <ProjectRow
              key={`${init.name}/${p.name}`}
              depth={2}
              name={p.name}
              live={p.live}
              attn={p.attn}
              onSelect={onSelect}
            />
          ))}
        </div>
      ))}
      {looseProjects.map((p) => (
        <ProjectRow
          key={p.name}
          depth={1}
          name={p.name}
          live={p.live}
          attn={p.attn}
          onSelect={onSelect}
        />
      ))}
      {isEmpty ? (
        <span
          className="mcc-sb-text"
          style={{ padding: "6px 10px", color: "var(--muted-foreground)", fontSize: 12 }}
        >
          No work yet. Start a mission and it appears here.
        </span>
      ) : null}
    </div>
  );
});
