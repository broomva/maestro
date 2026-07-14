// The KG right rail (BRO-1893 FID-6 slice 2) — recently viewed · pinned · what's new (the freshly
// bookkept feed). Ported from KnowledgeApp.jsx `KgRail`. Recent/pinned are ephemeral view state held by
// the page; "what's new" is the KG_FRESH sample feed.

import { Pin } from "lucide-react";
import { KG_TYPE, kgTypeColor } from "@/lib/kg";
import { KG_FRESH, KG_SCOPES } from "@/lib/kg-data";

/** A scope+node reference (a rail entry / a recent / a pin). */
export interface KgRef {
  scopeId: string;
  nodeId: string;
}

function KgRailItem({
  scopeId,
  nodeId,
  meta,
  onPick,
  onUnpin,
}: KgRef & {
  meta?: string;
  onPick: (scopeId: string, nodeId: string) => void;
  onUnpin?: (scopeId: string, nodeId: string) => void;
}) {
  const sc = KG_SCOPES[scopeId];
  const n = sc?.nodes.find((x) => x.id === nodeId);
  if (!sc || !n) return null;
  // A container div (not a button) so the unpin control is a SIBLING button, not nested inside one
  // (a button-in-button is invalid). The whole row highlights on hover; the main area is the pick target.
  return (
    <div className="kg-rail-item">
      <button type="button" className="kg-rail-main" onClick={() => onPick(scopeId, nodeId)}>
        <span className="kg-legend-dot" style={{ background: kgTypeColor(n) }} />
        <span className="kg-rail-body">
          <span className="kg-rail-label">{n.label}</span>
          <span className="kg-rail-sub">
            {sc.crumb} · {meta ?? (KG_TYPE[n.type] ?? KG_TYPE.concept).label}
          </span>
        </span>
      </button>
      {onUnpin ? (
        <button
          type="button"
          className="kg-iconbtn kg-rail-pin"
          aria-label={`Unpin ${n.label}`}
          onClick={() => onUnpin(scopeId, nodeId)}
        >
          <Pin size={13} strokeWidth={2} />
        </button>
      ) : null}
    </div>
  );
}

export function KgRail({
  recent,
  pinned,
  onPick,
  onUnpin,
}: {
  recent: KgRef[];
  pinned: KgRef[];
  onPick: (scopeId: string, nodeId: string) => void;
  onUnpin: (scopeId: string, nodeId: string) => void;
}) {
  return (
    <div className="kg-rail" data-testid="kg-rail">
      {pinned.length > 0 ? (
        <div className="kg-rail-sec">
          <div className="mcc-panel-label">
            <Pin size={12} strokeWidth={2} /> Pinned
          </div>
          {pinned.map((p) => (
            <KgRailItem key={`${p.scopeId}${p.nodeId}`} {...p} onPick={onPick} onUnpin={onUnpin} />
          ))}
        </div>
      ) : null}
      <div className="kg-rail-sec">
        <div className="mcc-panel-label">Recently viewed</div>
        {recent.length === 0 ? (
          <p className="kg-rail-empty">Open an entity and it lands here.</p>
        ) : (
          recent.map((p) => <KgRailItem key={`${p.scopeId}${p.nodeId}`} {...p} onPick={onPick} />)
        )}
      </div>
      <div className="kg-rail-sec">
        <div className="mcc-panel-label">
          What's new <span className="kg-rail-count">freshly bookkept</span>
        </div>
        {KG_FRESH.map((f) => (
          <KgRailItem
            key={`${f.scopeId}${f.nodeId}`}
            scopeId={f.scopeId}
            nodeId={f.nodeId}
            meta={`${f.when} ago`}
            onPick={onPick}
          />
        ))}
      </div>
    </div>
  );
}
