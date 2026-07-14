// The History page (BRO-1893 FID-6, `MccHistory`) — "your runs and the loop's". A session is a
// projection OF work, so History is the one page the run list belongs on. Ported from the prototype's
// MccHistory: a search bar + an organizing-axis toggle (by day / work / agent) + a you/autonomous
// filter, over a grouped list of run rows. The rows are REAL — derived from work STATE (`selectHistory`
// keys on the run states, since the client store has no session-row read path yet), each showing that
// run's real receipts (state, agent, folder, and its run branch once a read path attaches one). The
// deep per-run detail (duration, event count, transcript, lineage) lives behind the session-history
// read path and is NOT manufactured here (honest stub, same as FID-4/5).
//
// The disclosure ladder holds: work-as-runs, never worktrees / index.db / the engine room; the row
// shows receipts, never a progress percentage (CLAUDE.md §Work states).

import { DotComet, STATUS_DOT_VAR, workStatusView } from "@maestro/ui";
import { Folder, Search } from "lucide-react";
import { type KeyboardEvent, type ReactNode, useMemo, useState } from "react";
import { useStore } from "zustand";
import { type HistorySession, maestroStore, selectHistory } from "@/store";
import { relativeTime } from "../board/board-view";

type Axis = "day" | "work" | "agent";
type Filter = "all" | "you" | "loop";

const AXES: [Axis, string][] = [
  ["day", "By day"],
  ["work", "By work"],
  ["agent", "By agent"],
];
const FILTERS: [Filter, string][] = [
  ["all", "All"],
  ["you", "You"],
  ["loop", "Autonomous"],
];

/** The calendar-day bucket for the "by day" axis — Today / Yesterday / Earlier, from the session's age. */
function dayBucket(at: string, now: number): "Today" | "Yesterday" | "Earlier" {
  const then = new Date(at).getTime();
  const startOfToday = new Date(now);
  startOfToday.setHours(0, 0, 0, 0);
  const t0 = startOfToday.getTime();
  if (Number.isNaN(then) || then >= t0) return "Today";
  if (then >= t0 - 86_400_000) return "Yesterday";
  return "Earlier";
}

/** One session row — real receipts (dot tone, agent, folder, run branch, age); select is ephemeral. */
function HistRow({
  s,
  selected,
  onSelect,
  showFolder = true,
  showAgent = true,
}: {
  s: HistorySession;
  selected: boolean;
  onSelect: (id: string) => void;
  showFolder?: boolean;
  showAgent?: boolean;
}) {
  const v = workStatusView(s.state, "task");
  return (
    <button
      type="button"
      className={`mcc-hrow${selected ? " is-sel" : ""}`}
      aria-pressed={selected}
      onClick={() => onSelect(s.id)}
    >
      {v.running ? (
        <DotComet size={11} />
      ) : (
        <span className="mc-chip-dot" style={{ background: STATUS_DOT_VAR[v.tone] }} />
      )}
      <span className="mcc-hrow-body">
        <span className="mcc-hrow-title">{s.title}</span>
        <span className="mcc-hrow-meta">
          {showAgent ? <span className="mcc-hrow-who">{s.agent}</span> : null}
          {showFolder ? <span>{s.folder}</span> : null}
          {s.run ? <span className="mcc-hrow-run">{s.run}</span> : null}
        </span>
      </span>
      <span className={`mcc-hrow-kind mcc-hrow-kind--${s.kind}`}>
        {s.kind === "you" ? "you" : "loop"}
      </span>
      <span className="mcc-hrow-time">{relativeTime(s.at)}</span>
    </button>
  );
}

function GroupLabel({
  icon,
  count,
  children,
}: {
  icon?: ReactNode;
  count: number;
  children: string;
}) {
  return (
    <div className="mcc-hgroup">
      {icon}
      <span>{children}</span>
      <span className="mcc-hgroup-count">{count}</span>
    </div>
  );
}

/** The store-reading container — derives the session list, then renders the pure view. */
export function HistoryPage() {
  const server = useStore(maestroStore, (s) => s.server);
  const sessions = useMemo(() => selectHistory(server), [server]);
  return <HistoryView sessions={sessions} />;
}

/**
 * The pure presentational History page — the session list + its axis/filter/search chrome, over the
 * already-derived `sessions` (the projector's `selectHistory`). Separated from the store read (like
 * the Inspector takes `item`) so the rendering is unit-testable by props; the axis/filter/search state
 * is ephemeral view state, so it lives here.
 */
export function HistoryView({ sessions }: { sessions: HistorySession[] }) {
  const [axis, setAxis] = useState<Axis>("day");
  const [filter, setFilter] = useState<Filter>("all");
  const [query, setQuery] = useState("");
  const [sel, setSel] = useState<string | null>(null);

  const rows = useMemo(() => {
    const q = query.trim().toLowerCase();
    return sessions.filter(
      (s) =>
        (filter === "all" || s.kind === filter) &&
        (!q || `${s.title} ${s.folder} ${s.agent}`.toLowerCase().includes(q)),
    );
  }, [sessions, filter, query]);

  // Group the filtered rows by the active axis. Each group is [label, icon, rows]; day preserves the
  // Today → Yesterday → Earlier order, the others are first-appearance order (rows are already sorted
  // most-recent-first, so groups surface in recency order).
  const groups = useMemo(() => {
    if (axis === "day") {
      const now = Date.now();
      const order = ["Today", "Yesterday", "Earlier"] as const;
      return order
        .map((label) => ({
          label,
          icon: undefined,
          rows: rows.filter((s) => dayBucket(s.at, now) === label),
        }))
        .filter((g) => g.rows.length > 0);
    }
    const key = axis === "work" ? (s: HistorySession) => s.folder : (s: HistorySession) => s.agent;
    const seen = new Map<string, HistorySession[]>();
    for (const s of rows) {
      const k = key(s);
      const bucket = seen.get(k);
      if (bucket) bucket.push(s);
      else seen.set(k, [s]);
    }
    return [...seen.entries()].map(([label, rs]) => ({
      label,
      icon: axis === "work" ? <Folder size={13} strokeWidth={2} /> : undefined,
      rows: rs,
    }));
  }, [axis, rows]);

  const onAxisKey = (e: KeyboardEvent<HTMLButtonElement>) => {
    const i = AXES.findIndex(([id]) => id === axis);
    const step =
      e.key === "ArrowRight" || e.key === "ArrowDown"
        ? 1
        : e.key === "ArrowLeft" || e.key === "ArrowUp"
          ? -1
          : 0;
    if (step === 0) return;
    e.preventDefault();
    const next = AXES[(i + step + AXES.length) % AXES.length];
    if (next) setAxis(next[0]);
  };

  return (
    <div className="mcc-hist" data-testid="view-history" data-screen-label="History page">
      <div className="mcc-hist-bar">
        <div className="mcc-hsearch">
          <Search size={14} strokeWidth={2} />
          <input
            type="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search sessions"
            aria-label="Search sessions"
          />
        </div>
        <div className="mcc-seg" role="tablist" aria-label="Organize history">
          {AXES.map(([id, label]) => (
            <button
              key={id}
              type="button"
              role="tab"
              aria-selected={axis === id}
              className={`mcc-seg-btn${axis === id ? " is-active" : ""}`}
              onClick={() => setAxis(id)}
              onKeyDown={onAxisKey}
            >
              <span className="mcc-seg-label">{label}</span>
            </button>
          ))}
        </div>
        {/* biome-ignore lint/a11y/useSemanticElements: role="group" over <fieldset> — toggle buttons, not form fields (matches the board's filter chips). */}
        <div
          className="mc-chips"
          role="group"
          aria-label="Filter sessions"
          style={{ marginLeft: "auto" }}
        >
          {FILTERS.map(([id, label]) => (
            <button
              key={id}
              type="button"
              className={`mc-chip${filter === id ? " is-active" : ""}`}
              aria-pressed={filter === id}
              onClick={() => setFilter(id)}
            >
              {label}
            </button>
          ))}
        </div>
      </div>

      {sessions.length === 0 ? (
        <div className="mcc-hist-empty" data-testid="history-empty">
          <span className="font-medium text-base text-foreground">No runs yet</span>
          <span className="max-w-[420px] text-muted-foreground text-sm">
            When you start a mission or the loop dispatches work, each run appears here as a
            session.
          </span>
        </div>
      ) : (
        <div className="mcc-hist-list">
          {groups.map((g) => (
            <div key={`${axis}:${g.label}`}>
              <GroupLabel icon={g.icon} count={g.rows.length}>
                {g.label}
              </GroupLabel>
              {g.rows.map((s) => (
                <HistRow
                  key={s.id}
                  s={s}
                  selected={sel === s.id}
                  onSelect={setSel}
                  showFolder={axis !== "work"}
                  showAgent={axis !== "agent"}
                />
              ))}
            </div>
          ))}
          {/* Honest end note (plain voice, no engine-room terms): these are the runs in view. The full
              archive of older, closed runs fills in over time — no faked count, no "read path" jargon. */}
          <div className="mcc-hist-end">
            {rows.length === 0
              ? "No runs match this search."
              : "Older runs join this list as the archive fills in."}
          </div>
        </div>
      )}
    </div>
  );
}
