// The KG list view (BRO-1893 FID-6 slice 2) — the graph's data as a keyboard-first table (entity ·
// kind · Nous · links). The a11y-complete counterpart to the graph canvas + the graph⇄list toggle's
// other half. Ported from KnowledgeApp.jsx `KgListView`.

import { DotComet } from "@maestro/ui";
import { KG_TYPE, kgBacklinks, kgCategory, kgScore, kgTypeColor } from "@/lib/kg";
import type { KgScope } from "@/lib/kg-data";

export function KgListView({
  scope,
  selectedId,
  onSelect,
  onNavigate,
  typeFilter,
}: {
  scope: KgScope;
  selectedId: string | null;
  onSelect: (id: string) => void;
  onNavigate: (scopeId: string) => void;
  typeFilter: ReadonlySet<string>;
}) {
  const rows = scope.nodes.filter((n) => typeFilter.size === 0 || typeFilter.has(kgCategory(n)));
  return (
    <div className="kg-list" data-testid="kg-list">
      <div className="kg-list-head">
        <span>Entity</span>
        <span>Kind</span>
        <span>Nous</span>
        <span>Links</span>
      </div>
      <div className="kg-list-rows">
        {rows.map((n) => {
          const total = kgScore(n.score);
          const links = kgBacklinks(n, scope).length;
          return (
            <button
              type="button"
              key={n.id}
              className={`kg-list-row${n.id === selectedId ? " is-sel" : ""}`}
              aria-pressed={n.id === selectedId}
              onClick={() => (n.scopeRef ? onNavigate(n.scopeRef) : onSelect(n.id))}
            >
              <span className="kg-list-name">
                <span className="kg-legend-dot" style={{ background: kgTypeColor(n) }} />
                {n.label}
                {n.live ? <DotComet size={10} /> : null}
              </span>
              <span className="kg-list-kind">
                {n.scopeRef ? "folder ›" : (KG_TYPE[n.type] ?? KG_TYPE.concept).label}
              </span>
              <span className="kg-list-score">
                {total != null ? (
                  <span
                    className="kg-list-pip"
                    data-v={total >= 7 ? "hi" : total >= 3 ? "mid" : "lo"}
                  >
                    {total}
                  </span>
                ) : (
                  <span className="kg-list-pip">·</span>
                )}
              </span>
              <span className="kg-list-links">{links}</span>
            </button>
          );
        })}
      </div>
    </div>
  );
}
