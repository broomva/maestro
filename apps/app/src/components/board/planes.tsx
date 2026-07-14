// The three mission-plane views (BRO-1886) — feed / board / list, ported from WorkPlanes.jsx +
// WorkFeed.jsx (canon per docs/canon-map.md). All three shape the SAME flat leaf-WorkItem list the
// host passes; the feed/board reuse the app's <WorkCard> (which already wears the canonical
// .bv-undertow running halo — the one running treatment, no border comet), the list uses compact
// .mcc-rowitem rows. Pure presentational: selection + the board clock come from the host; no data
// derivation here (that stays selectPlaneItems + plane-view.ts). Receipts (run/<id>), never a
// percentage (CLAUDE.md §Work states).

import type { PlaneView, WorkItem } from "@maestro/protocol";
import { DotComet, STATUS_DOT_VAR, workStatusView } from "@maestro/ui";
import { Columns3, LayoutList, List } from "lucide-react";
import type { BoardSection } from "./board-view";
import { relativeTime } from "./board-view";
import type { PlaneColumn } from "./plane-view";
import { WorkCard } from "./work-card";

/** A static plain-voice dot (tone color); the running tidepool is a DotComet, handled by callers. */
function ToneDot({ tone }: { tone: BoardSection["tone"] }) {
  return <span className="mc-chip-dot" style={{ background: STATUS_DOT_VAR[tone] }} />;
}

// ── View toggle ─────────────────────────────────────────────────────────────
const VIEWS: readonly { id: PlaneView; label: string; Icon: typeof List }[] = [
  { id: "feed", label: "Feed", Icon: LayoutList },
  { id: "board", label: "Board", Icon: Columns3 },
  { id: "list", label: "List", Icon: List },
];

export function PlaneToggle({ view, onView }: { view: PlaneView; onView: (v: PlaneView) => void }) {
  return (
    <div className="mcc-seg" role="tablist" aria-label="Plane view">
      {VIEWS.map(({ id, label, Icon }) => (
        <button
          key={id}
          type="button"
          role="tab"
          aria-selected={view === id}
          className={`mcc-seg-btn${view === id ? " is-active" : ""}`}
          onClick={() => onView(id)}
        >
          <Icon size={13} />
          <span className="mcc-seg-label">{label}</span>
        </button>
      ))}
    </div>
  );
}

interface PlaneProps {
  selectedId: string | null;
  onSelect: (id: string) => void;
  /** the board clock (epoch-ms) threaded into every card's relative age (WorkCard §now). */
  now: number;
}

// ── Feed ─────────────────────────────────────────────────────────────────────
export function FeedPlane({
  sections,
  attention,
  active,
  selectedId,
  onSelect,
  now,
}: PlaneProps & { sections: BoardSection[]; attention: number; active: number }) {
  return (
    <div className="mcc-plane-feed" data-testid="plane-feed">
      <div className="mc-triage">
        <div className="mc-triage-headline">
          <span className="mc-triage-title">
            {attention > 0
              ? `${attention} ${attention === 1 ? "piece" : "pieces"} of work ${attention === 1 ? "needs" : "need"} you`
              : "All clear"}
          </span>
          <span className="mc-triage-sub">{active} active · workers handle the rest</span>
        </div>
      </div>
      {sections.map((s) => (
        <section key={s.state} data-testid={`board-group-${s.state}`} className="mc-group">
          <div className="mc-group-header">
            <span className="mc-group-label">
              <ToneDot tone={s.tone} />
              {s.label}
            </span>
            <span className="mc-group-count">{s.items.length}</span>
          </div>
          <div className="mc-group-cards">
            {s.items.map((item) => (
              <WorkCard
                key={item.id}
                item={item}
                selected={item.id === selectedId}
                onSelect={onSelect}
                now={now}
              />
            ))}
          </div>
        </section>
      ))}
    </div>
  );
}

// ── Board ────────────────────────────────────────────────────────────────────
export function BoardPlane({
  columns,
  selectedId,
  onSelect,
  now,
}: PlaneProps & { columns: PlaneColumn[] }) {
  return (
    <div className="mcc-plane-board" data-testid="plane-board">
      {columns.map((col) => (
        <div key={col.label} className="mcc-col" data-testid={`board-col-${col.label}`}>
          <div className="mcc-col-header">
            <ToneDot tone={col.tone} />
            {col.label}
            <span className="mcc-col-count">{col.items.length}</span>
          </div>
          <div className="mcc-col-hint">{col.hint}</div>
          <div className="mcc-col-body">
            {col.items.map((item) => (
              <WorkCard
                key={item.id}
                item={item}
                selected={item.id === selectedId}
                onSelect={onSelect}
                now={now}
              />
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}

// ── List ─────────────────────────────────────────────────────────────────────
function ListRow({
  item,
  selected,
  onSelect,
  now,
}: {
  item: WorkItem;
  selected: boolean;
  onSelect: (id: string) => void;
  now: number;
}) {
  const v = workStatusView(item.state, item.kind);
  const crumb = [item.initiative, item.project].filter(Boolean).join(" › ");
  const age = relativeTime(item.lastEventAt ?? item.updatedAt, now);
  return (
    <button
      type="button"
      data-testid="work-row"
      aria-pressed={selected}
      className={`mcc-rowitem${selected ? " is-selected" : ""}`}
      onClick={() => onSelect(item.id)}
    >
      {v.running ? <DotComet size={10} /> : <ToneDot tone={v.tone} />}
      <span className="mcc-row-title">{item.title}</span>
      <span className="mcc-row-crumb">{crumb || item.path}</span>
      {item.run ? <span className="mc-receipt">{item.run}</span> : <span />}
      <span className="mcc-row-time">{age}</span>
    </button>
  );
}

export function ListPlane({
  sections,
  selectedId,
  onSelect,
  now,
}: PlaneProps & { sections: BoardSection[] }) {
  return (
    <div className="mcc-plane-list" data-testid="plane-list">
      {sections.map((s) => (
        <div key={s.state} data-testid={`board-group-${s.state}`}>
          <div className="mcc-list-group">
            <ToneDot tone={s.tone} />
            {s.label}
            <span className="mc-group-count">{s.items.length}</span>
          </div>
          {s.items.map((item) => (
            <ListRow
              key={item.id}
              item={item}
              selected={item.id === selectedId}
              onSelect={onSelect}
              now={now}
            />
          ))}
        </div>
      ))}
    </div>
  );
}
