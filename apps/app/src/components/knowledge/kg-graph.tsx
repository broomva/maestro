// The KG graph (BRO-1893 FID-6 slice 2a) — a STATIC render of the scope graph: nodes coloured by kind,
// `related:` edges, folder nodes ringed, click an entity to open its page, click a folder to descend.
// The layout is the deterministic force layout from kg.ts (computed once, memoised by scope) — NOT an
// ongoing simulation. The interactive layer (pan / zoom / drag / scope-morph / minimap / hover-card) is
// slice 2b; this slice renders the graph and makes every node keyboard-reachable (role=button + arrows
// are unnecessary since each node is a Tab stop with an accessible label; the List view is the fuller
// a11y fallback). Ported from KgGraph.jsx `drawNet` + the legend.

import { type KeyboardEvent, useMemo } from "react";
import {
  KG_GOLD,
  KG_TYPE,
  kgCategory,
  kgDegree,
  kgEdges,
  kgIsScope,
  kgLayout,
  kgNeighbors,
  kgNodeR,
} from "@/lib/kg";
import type { KgNode, KgScope } from "@/lib/kg-data";

const LEGEND: string[] = ["folder", "concept", "decision", "primitive", "session"];

export function KgGraph({
  scope,
  selectedId,
  onSelectNode,
  onNavigate,
  typeFilter,
  width = 820,
  height = 660,
}: {
  scope: KgScope;
  selectedId: string | null;
  onSelectNode: (id: string) => void;
  onNavigate: (scopeId: string) => void;
  typeFilter: ReadonlySet<string>;
  width?: number;
  height?: number;
}) {
  // The layout is pure + deterministic, so memoise it by scope id + dimensions (recomputing only when
  // the scope actually changes — never every render, never on selection/filter).
  const { pos, edges, deg } = useMemo(() => {
    const eg = kgEdges(scope.nodes);
    return {
      pos: kgLayout(scope.nodes, eg, width, height),
      edges: eg,
      deg: kgDegree(scope.nodes, eg),
    };
  }, [scope, width, height]);

  const nodeById = useMemo(() => Object.fromEntries(scope.nodes.map((n) => [n.id, n])), [scope]);
  const offType = (n: KgNode) => typeFilter.size > 0 && !typeFilter.has(kgCategory(n));
  const focusSet = selectedId ? kgNeighbors(selectedId, edges) : null;

  const activate = (n: KgNode) => {
    if (kgIsScope(n) && n.scopeRef) onNavigate(n.scopeRef);
    else onSelectNode(n.id);
  };
  const onNodeKey = (e: KeyboardEvent<SVGGElement>, n: KgNode) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      activate(n);
    }
  };

  return (
    <div className="kg-stage" data-testid="kg-graph">
      {/* biome-ignore lint/a11y/useSemanticElements: an <svg> can't be a <fieldset>; role="group" + aria-label is the correct way to label this container of interactive graph nodes. */}
      <svg
        className="kg-svg"
        viewBox={`0 0 ${width} ${height}`}
        preserveAspectRatio="xMidYMid meet"
        role="group"
        aria-label={`${scope.crumb} knowledge graph · ${scope.nodes.length} entities`}
      >
        {edges.map((e) => {
          const a = pos[e.s];
          const b = pos[e.t];
          const es = nodeById[e.s];
          const et = nodeById[e.t];
          if (!a || !b || !es || !et) return null;
          const on = selectedId != null && (e.s === selectedId || e.t === selectedId);
          const faded = (focusSet && !on) || offType(es) || offType(et);
          return (
            <line
              key={`${e.s}|${e.t}`}
              x1={a.x}
              y1={a.y}
              x2={b.x}
              y2={b.y}
              className={`kg-edge${on ? " is-on" : ""}`}
              style={{ opacity: faded ? 0.06 : on ? 0.9 : 0.3 }}
            />
          );
        })}
        {scope.nodes.map((n) => {
          const p = pos[n.id];
          if (!p) return null;
          const t = KG_TYPE[n.type] ?? KG_TYPE.concept;
          const r = kgNodeR(n, deg[n.id] ?? 0);
          const folder = kgIsScope(n);
          const sel = n.id === selectedId;
          // A type-filtered node leaves the tab order AND the a11y tree, so the chip filter applies
          // uniformly to keyboard/SR users (the List view removes filtered rows entirely — this keeps
          // the two surfaces in agreement). Focus-dimming (neighbour highlight) only fades, never hides.
          const filteredOut = offType(n);
          const faded = filteredOut || (focusSet != null && !focusSet.has(n.id));
          return (
            // biome-ignore lint/a11y/useSemanticElements: an SVG <g> can't be a <button>; role=button + tabIndex + onKeyDown is the accessible pattern for an in-canvas graph node (the List view is the fuller fallback).
            <g
              key={n.id}
              transform={`translate(${p.x.toFixed(2)} ${p.y.toFixed(2)})`}
              className="kg-node"
              role="button"
              tabIndex={filteredOut ? undefined : 0}
              aria-hidden={filteredOut || undefined}
              aria-label={`${n.label} · ${folder ? "folder" : t.label}${sel ? " · selected" : ""}`}
              aria-pressed={sel}
              style={{ opacity: faded ? 0.18 : 1, cursor: "pointer" }}
              onClick={() => activate(n)}
              onKeyDown={(e) => onNodeKey(e, n)}
            >
              <circle className="kg-focus-ring" r={r + 6} />
              {sel ? <circle className="kg-ring" r={r + 6} /> : null}
              {folder ? <circle className="kg-scope-ring" r={r + 5} /> : null}
              <circle
                r={r}
                fill={folder ? KG_GOLD : t.color}
                stroke="var(--background)"
                strokeWidth={folder ? 2.5 : 2}
              />
              {folder ? (
                <path
                  d="M-4.5 -2.2 h2.4 l1 1.3 h4.6 a0.7 0.7 0 0 1 0.7 0.7 v3.4 a0.7 0.7 0 0 1 -0.7 0.7 h-8 a0.7 0.7 0 0 1 -0.7 -0.7 v-4.7 a0.7 0.7 0 0 1 0.7 -0.7 z"
                  fill="var(--card)"
                  opacity="0.92"
                />
              ) : null}
              {n.live ? <circle className="kg-pulse" r={r} fill="none" stroke={t.color} /> : null}
              <text
                className="kg-label"
                x={0}
                y={r + 13}
                textAnchor="middle"
                style={{ fontWeight: folder || sel ? 600 : 400 }}
              >
                {n.label}
              </text>
            </g>
          );
        })}
      </svg>

      <div className="kg-legend">
        {LEGEND.map((k) => (
          <span key={k} className="kg-legend-item">
            <span
              className="kg-legend-dot"
              style={{
                background:
                  k === "folder"
                    ? KG_GOLD
                    : (KG_TYPE[k as KgNode["type"]] ?? KG_TYPE.concept).color,
              }}
            />
            {k}
          </span>
        ))}
      </div>
    </div>
  );
}
