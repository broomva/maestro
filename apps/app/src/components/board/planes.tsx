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
import { type KeyboardEvent, memo, useRef } from "react";
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
  const tabs = useRef<(HTMLButtonElement | null)[]>([]);
  const active = VIEWS.findIndex((v) => v.id === view);
  // WAI-ARIA tabs with automatic activation: Arrow/Home/End move selection AND focus (switching a view
  // is cheap + reversible, so activate-on-move is the right pattern), plus a roving tabindex so the
  // whole toggle is a single Tab stop. onClick still activates directly.
  const onKeyDown = (e: KeyboardEvent<HTMLButtonElement>) => {
    const last = VIEWS.length - 1;
    let next = -1;
    if (e.key === "ArrowRight" || e.key === "ArrowDown") next = active >= last ? 0 : active + 1;
    else if (e.key === "ArrowLeft" || e.key === "ArrowUp") next = active <= 0 ? last : active - 1;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = last;
    const target = next < 0 ? undefined : VIEWS[next];
    if (!target) return;
    e.preventDefault();
    onView(target.id);
    tabs.current[next]?.focus();
  };
  return (
    <div className="mcc-seg" role="tablist" aria-label="Plane view">
      {VIEWS.map(({ id, label, Icon }, i) => (
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
          onKeyDown={onKeyDown}
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
  headline,
  active,
  selectedId,
  onSelect,
  now,
}: PlaneProps & { sections: BoardSection[]; headline: string; active: number }) {
  return (
    <div className="mcc-plane-feed" data-testid="plane-feed">
      <div className="mc-triage">
        <div className="mc-triage-headline">
          {/* One source for the headline string — triage().headline (plane-view.ts), the tested path. */}
          <span className="mc-triage-title">{headline}</span>
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
export interface RowProps {
  item: WorkItem;
  selected: boolean;
  onSelect: (id: string) => void;
  now: number;
}

function ListRowImpl({ item, selected, onSelect, now }: RowProps) {
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

/** Re-render a row only when a field it renders — or selection / the board clock — changed. Mirrors
 *  work-card.tsx's `areEqual` (same rendered inputs) so an idle row skips the 30s tick + unrelated SSE
 *  events just like the feed/board cards do, instead of re-rendering the whole list on every event.
 *  Exported for the mutation-proof in planes.test.ts (the comparator must be neither `() => true` nor
 *  missing a field), matching work-card's exported-and-tested `areEqual`. */
export function rowAreEqual(a: RowProps, b: RowProps): boolean {
  const x = a.item;
  const y = b.item;
  return (
    a.selected === b.selected &&
    a.onSelect === b.onSelect &&
    a.now === b.now &&
    x.id === y.id &&
    x.state === y.state &&
    x.kind === y.kind &&
    x.title === y.title &&
    x.run === y.run &&
    x.initiative === y.initiative &&
    x.project === y.project &&
    x.path === y.path &&
    x.updatedAt === y.updatedAt &&
    x.lastEventAt === y.lastEventAt
  );
}
const ListRow = memo(ListRowImpl, rowAreEqual);

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
