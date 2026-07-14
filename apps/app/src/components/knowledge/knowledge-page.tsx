// The Knowledge page (BRO-1893 FID-6 slice 2a, `MccKnowledge`) — "the context engine made visible as a
// graph that is itself the filesystem": every file with frontmatter is a node, every `related:` link an
// edge, every folder a node you can enter (descending re-scopes the graph). Ported from
// KnowledgeApp.jsx `MccKnowledge`. Scope navigation is IN-PAGE (breadcrumb crumbs + folder nodes) — the
// shell sidebar stays the shared workspace tree (not hijacked). SAMPLE data (KG_SCOPES) until a real KG
// read path lands — the "sample" badge makes that honest, never faked as live.
//
// Slice 2a: the static graph + list + type chips + detail drawer (inspector + neighbourhood) + right
// rail. The interactive graph layer (pan / zoom / drag / scope-morph / minimap / hover-card / command
// palette) is slice 2b. The disclosure ladder holds — entities + claims + receipts, no engine room.

import { List, Network, Pin, X } from "lucide-react";
import { type KeyboardEvent, useMemo, useRef, useState } from "react";
import { KG_GOLD, KG_TYPE, kgCategory, kgPath } from "@/lib/kg";
import { KG_SCOPES } from "@/lib/kg-data";
import { KgGraph } from "./kg-graph";
import { KgInspector, KgMiniGraph } from "./kg-inspector";
import { KgListView } from "./kg-list";
import { KgRail, type KgRef } from "./kg-rail";

type View = "graph" | "list";
const VIEWS: [View, string][] = [
  ["graph", "Graph"],
  ["list", "List"],
];
// The chip order the prototype uses; filtered to the categories actually present in the scope.
const CAT_ORDER = [
  "folder",
  "concept",
  "decision",
  "primitive",
  "tool",
  "person",
  "paper",
  "doc",
  "session",
  "pattern",
];

/** The graph⇄list view toggle — a WAI-ARIA tab list (roving tabindex; arrows move selection AND focus). */
function ViewToggle({ view, onView }: { view: View; onView: (v: View) => void }) {
  const tabs = useRef<(HTMLButtonElement | null)[]>([]);
  const onKey = (e: KeyboardEvent<HTMLButtonElement>) => {
    const i = VIEWS.findIndex(([id]) => id === view);
    const last = VIEWS.length - 1;
    let n = -1;
    if (e.key === "ArrowRight" || e.key === "ArrowDown") n = i >= last ? 0 : i + 1;
    else if (e.key === "ArrowLeft" || e.key === "ArrowUp") n = i <= 0 ? last : i - 1;
    else if (e.key === "Home") n = 0;
    else if (e.key === "End") n = last;
    const next = n < 0 ? undefined : VIEWS[n];
    if (!next) return;
    e.preventDefault();
    onView(next[0]);
    tabs.current[n]?.focus();
  };
  return (
    <div className="mcc-seg kg-viewtoggle" role="tablist" aria-label="Graph or list view">
      {VIEWS.map(([id, label], i) => (
        <button
          key={id}
          ref={(el) => {
            tabs.current[i] = el;
          }}
          type="button"
          role="tab"
          aria-selected={view === id}
          tabIndex={view === id ? 0 : -1}
          className={`mcc-seg-btn${view === id ? " is-active" : ""}`}
          onClick={() => onView(id)}
          onKeyDown={onKey}
        >
          {id === "graph" ? (
            <Network size={13} strokeWidth={2} />
          ) : (
            <List size={13} strokeWidth={2} />
          )}
          <span className="mcc-seg-label">{label}</span>
        </button>
      ))}
    </div>
  );
}

export function KnowledgePage() {
  const [scopeId, setScopeId] = useState("broomva");
  const [sel, setSel] = useState<string | null>(null);
  const [drawer, setDrawer] = useState(false);
  const [view, setView] = useState<View>("graph");
  const [filter, setFilter] = useState<ReadonlySet<string>>(new Set());
  const [recent, setRecent] = useState<KgRef[]>([]);
  const [pinned, setPinned] = useState<KgRef[]>([{ scopeId: "broomva", nodeId: "p6" }]);

  const scope = KG_SCOPES[scopeId];
  const path = useMemo(() => kgPath(scopeId), [scopeId]);
  const cats = useMemo(() => {
    if (!scope) return [];
    const present = new Set(scope.nodes.map(kgCategory));
    return CAT_ORDER.filter((c) => present.has(c));
  }, [scope]);

  if (!scope) return null;

  const navigate = (id: string) => {
    if (!KG_SCOPES[id]) return;
    setScopeId(id);
    setSel(null);
    setDrawer(false);
  };
  const pushRecent = (sid: string, nid: string) =>
    setRecent((r) =>
      [
        { scopeId: sid, nodeId: nid },
        ...r.filter((x) => !(x.scopeId === sid && x.nodeId === nid)),
      ].slice(0, 6),
    );
  const selectNode = (nid: string) => {
    setSel(nid);
    setDrawer(true);
    pushRecent(scopeId, nid);
  };
  const pickEntity = (sid: string, nid: string) => {
    if (sid !== scopeId) setScopeId(sid);
    setSel(nid);
    setDrawer(true);
    pushRecent(sid, nid);
  };
  const togglePin = (sid: string, nid: string) =>
    setPinned((p) =>
      p.some((x) => x.scopeId === sid && x.nodeId === nid)
        ? p.filter((x) => !(x.scopeId === sid && x.nodeId === nid))
        : [...p, { scopeId: sid, nodeId: nid }],
    );
  const toggleCat = (c: string) =>
    setFilter((f) => {
      const n = new Set(f);
      if (n.has(c)) n.delete(c);
      else n.add(c);
      return n;
    });

  const selectedNode = sel ? (scope.nodes.find((n) => n.id === sel) ?? null) : null;
  const isPinned = sel != null && pinned.some((p) => p.scopeId === scopeId && p.nodeId === sel);

  return (
    <div className="kg-page" data-testid="view-knowledge" data-screen-label="Knowledge page">
      <div className="kg-bar">
        <div className="kg-path">
          {path.map((s, i) => (
            <span key={s.id} style={{ display: "inline-flex", alignItems: "center", gap: 5 }}>
              {i > 0 ? <span className="kg-crumb-sep">›</span> : null}
              <button
                type="button"
                className={`kg-crumb-btn${s.id === scopeId ? " is-active" : ""}`}
                onClick={() => navigate(s.id)}
              >
                {s.crumb}
              </button>
            </span>
          ))}
          <span className="kg-scopekind">
            {scope.kind} · {scope.nodes.length}
          </span>
          <span
            className="kg-sample"
            title="Sample data — the live knowledge graph lands with its read path."
          >
            sample
          </span>
        </div>
        <div className="kg-bar-right">
          <ViewToggle view={view} onView={setView} />
        </div>
      </div>

      <div className="kg-body">
        <div className="kg-main">
          <div className="kg-chips">
            <button
              type="button"
              className={`kg-chip${filter.size === 0 ? " is-active" : ""}`}
              aria-pressed={filter.size === 0}
              onClick={() => setFilter(new Set())}
            >
              All
            </button>
            {cats.map((c) => (
              <button
                type="button"
                key={c}
                className={`kg-chip${filter.has(c) ? " is-active" : ""}`}
                aria-pressed={filter.has(c)}
                onClick={() => toggleCat(c)}
              >
                <span
                  className="kg-legend-dot"
                  style={{
                    background:
                      c === "folder"
                        ? KG_GOLD
                        : (KG_TYPE[c as keyof typeof KG_TYPE] ?? KG_TYPE.concept).color,
                  }}
                />
                {c === "folder"
                  ? "folders"
                  : (KG_TYPE[c as keyof typeof KG_TYPE] ?? KG_TYPE.concept).label}
              </button>
            ))}
          </div>
          <div className="kg-graphwrap">
            {view === "graph" ? (
              <KgGraph
                scope={scope}
                selectedId={sel}
                onSelectNode={selectNode}
                onNavigate={navigate}
                typeFilter={filter}
              />
            ) : (
              <KgListView
                scope={scope}
                selectedId={sel}
                onSelect={selectNode}
                onNavigate={navigate}
                typeFilter={filter}
              />
            )}
            {drawer && selectedNode ? (
              <div className="kg-drawer" data-testid="kg-drawer" data-screen-label="Node detail">
                <div className="kg-drawer-head">
                  <span className="mc-detail-breadcrumb">
                    {path.map((s) => s.crumb).join(" / ")}
                  </span>
                  <div className="kg-drawer-actions">
                    <button
                      type="button"
                      className={`kg-iconbtn${isPinned ? " is-on" : ""}`}
                      aria-pressed={isPinned}
                      title={isPinned ? "Unpin" : "Pin"}
                      onClick={() => togglePin(scopeId, sel as string)}
                    >
                      <Pin size={15} strokeWidth={2} />
                    </button>
                    <button
                      type="button"
                      className="kg-iconbtn"
                      title="Close"
                      aria-label="Close detail"
                      onClick={() => {
                        setDrawer(false);
                        setSel(null);
                      }}
                    >
                      <X size={15} strokeWidth={2} />
                    </button>
                  </div>
                </div>
                <div className="kg-drawer-body">
                  <KgInspector node={selectedNode} scope={scope} onSelect={selectNode} big />
                  <div className="kg-inspect" style={{ paddingTop: 0 }}>
                    <div className="kg-ent-sec">
                      <div className="mcc-panel-label">neighbourhood</div>
                      <div className="kg-mini-wrap">
                        <KgMiniGraph scope={scope} centerId={sel as string} />
                      </div>
                    </div>
                  </div>
                </div>
              </div>
            ) : null}
          </div>
        </div>
        <div className="kg-panel">
          <KgRail recent={recent} pinned={pinned} onPick={pickEntity} onUnpin={togglePin} />
        </div>
      </div>
    </div>
  );
}
