// The KG inspector (BRO-1893 FID-6 slice 2) — an entity page rendered from a node (kind badge, claim,
// Nous score bars, sources, bidirectional backlinks) + the neighbourhood mini-graph. Ported from
// ConceptKnowledge.jsx `KgInspector` + KgGraph.jsx `KgMiniGraph`. Pure (props in, SVG/markup out) so it
// unit-tests under renderToStaticMarkup.

import { DotComet } from "@maestro/ui";
import { Network } from "lucide-react";
import {
  KG_TYPE,
  kgBacklinks,
  kgEdges,
  kgIsScope,
  kgNeighbors,
  kgScore,
  kgTypeColor,
} from "@/lib/kg";
import type { KgNode, KgScope } from "@/lib/kg-data";

function verdict(total: number): string {
  if (total >= 7) return "fast-path promote";
  if (total >= 3) return "second opinion";
  return "discard";
}

/** The entity page — the same frontmatter, rendered. `big` is the full-size (drawer) variant. */
export function KgInspector({
  node,
  scope,
  onSelect,
  big = false,
}: {
  node: KgNode | null;
  scope: KgScope;
  onSelect?: (id: string) => void;
  big?: boolean;
}) {
  if (!node) {
    const folders = scope.nodes.filter((n) => n.scopeRef).length;
    return (
      <div className="kg-inspect kg-inspect--empty" data-testid="kg-inspect-empty">
        <div className="mcc-panel-label">
          {scope.crumb}/ · {scope.kind}
        </div>
        <p className="kg-ent-claim" style={{ color: "var(--muted-foreground)" }}>
          {scope.nodes.length} entities
          {folders > 0 ? ` · ${folders} sub-folder${folders > 1 ? "s" : ""}` : ""} · {scope.desc}.
        </p>
        <p className="kg-ent-claim" style={{ color: "var(--muted-foreground)" }}>
          Click an entity to open its page. The folder nodes (with a dashed ring) are sub-scopes ·
          click one to enter its graph.
        </p>
        <div className="kg-empty-hint">
          <Network size={15} strokeWidth={2} />
          Every file with frontmatter is a node; every link an edge.
        </div>
      </div>
    );
  }

  const t = KG_TYPE[node.type] ?? KG_TYPE.concept;
  const total = kgScore(node.score);
  const backlinks = kgBacklinks(node, scope);
  const subs: [string, number | undefined][] = [
    ["novelty", node.score?.[0]],
    ["specificity", node.score?.[1]],
    ["relevance", node.score?.[2]],
  ];
  return (
    <div className={`kg-inspect${big ? " kg-inspect--big" : ""}`} data-testid="kg-inspect">
      <div className="kg-ent-head">
        <span
          className="kg-ent-kind"
          style={{
            color: t.color,
            borderColor: `color-mix(in oklch, ${t.color} 42%, transparent)`,
          }}
        >
          <span className="kg-legend-dot" style={{ background: t.color }} />
          {t.label}
        </span>
        {node.live ? (
          <span className="kg-ent-live">
            <DotComet size={11} />
            live
          </span>
        ) : null}
      </div>
      <div className="kg-ent-title">
        {node.label}
        {node.type !== "session" && !node.scopeRef ? <span className="kg-ent-ext">.md</span> : null}
      </div>
      <p className="kg-ent-claim">“{node.claim}”</p>

      {total != null ? (
        <div className="kg-score">
          <div className="kg-score-top">
            <span>Nous score</span>
            <b>
              {total}
              <i>/9</i>
            </b>
            <span className="kg-score-verdict">{verdict(total)}</span>
          </div>
          {subs.map(([k, v]) => (
            <div key={k} className="kg-score-row">
              <span className="kg-score-k">{k}</span>
              <span className="kg-score-bar">
                <i style={{ width: `${((v ?? 0) / 3) * 100}%` }} />
              </span>
              <span className="kg-score-v">{v ?? 0}</span>
            </div>
          ))}
        </div>
      ) : null}

      {node.sources ? (
        <div className="kg-ent-sec">
          <div className="mcc-panel-label">sources</div>
          <div className="kg-src-list">
            {node.sources.map((s) => (
              <span key={s} className="mc-receipt">
                {s}
              </span>
            ))}
          </div>
        </div>
      ) : null}

      <div className="kg-ent-sec">
        <div className="mcc-panel-label">related · {backlinks.length}</div>
        <div className="kg-back-list">
          {backlinks.map((n) => {
            const bt = KG_TYPE[n.type] ?? KG_TYPE.concept;
            return (
              <button key={n.id} type="button" className="kg-back" onClick={() => onSelect?.(n.id)}>
                <span className="kg-legend-dot" style={{ background: bt.color }} />
                {n.label}
              </button>
            );
          })}
        </div>
      </div>
    </div>
  );
}

/**
 * A tiny radial neighbourhood graph for the detail drawer (centre + its neighbours). Presentational
 * (role="img") — the keyboard path to these same entities is the inspector's "related" backlink
 * buttons + the List view, so the SVG stays a visual, not a second interactive surface.
 */
export function KgMiniGraph({
  scope,
  centerId,
  w = 300,
  h = 190,
}: {
  scope: KgScope;
  centerId: string;
  w?: number;
  h?: number;
}) {
  const center = scope.nodes.find((n) => n.id === centerId);
  if (!center) return null;
  const edges = kgEdges(scope.nodes);
  const nbIds = kgNeighbors(centerId, edges);
  const nb = scope.nodes.filter((n) => n.id !== centerId && nbIds.has(n.id));
  const cx = w / 2;
  const cy = h / 2;
  const R = Math.min(w, h) / 2 - 30;
  const pts = nb.map((n, i) => {
    const a = -Math.PI / 2 + (i / Math.max(nb.length, 1)) * Math.PI * 2;
    return { n, x: cx + Math.cos(a) * R, y: cy + Math.sin(a) * R };
  });
  return (
    <svg
      viewBox={`0 0 ${w} ${h}`}
      className="kg-mini"
      preserveAspectRatio="xMidYMid meet"
      role="img"
      aria-label={`${center.label} and ${nb.length} related ${nb.length === 1 ? "entity" : "entities"}`}
    >
      <title>{`${center.label} · neighbourhood`}</title>
      {pts.map((p) => (
        <line
          key={`e-${p.n.id}`}
          x1={cx}
          y1={cy}
          x2={p.x}
          y2={p.y}
          className="kg-edge"
          style={{ opacity: 0.35 }}
        />
      ))}
      {pts.map((p) => (
        <g key={p.n.id} transform={`translate(${p.x} ${p.y})`}>
          <circle
            r={kgIsScope(p.n) ? 8 : 6}
            fill={kgTypeColor(p.n)}
            stroke="var(--card)"
            strokeWidth="2"
          />
          <text className="kg-mini-label" x={0} y={kgIsScope(p.n) ? 19 : 17} textAnchor="middle">
            {p.n.label}
          </text>
        </g>
      ))}
      <g transform={`translate(${cx} ${cy})`}>
        <circle r={11} fill={kgTypeColor(center)} stroke="var(--card)" strokeWidth="2.5" />
      </g>
    </svg>
  );
}
