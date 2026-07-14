// Mission-plane view helpers (BRO-1886) — the PURE, testable shaping of a flat leaf-WorkItem
// list into the feed's plain-voice sections and the board's four columns. Mirrors the prototype
// (WorkPlanes.jsx groups/columns a flat `items` list client-side). The feed path reuses the
// board's `toSections` (one plain-voice collapse, already tested) via a state-grouping step; the
// list view reuses the same feed sections. No re-derivation of the WorkItem join — that stays the
// projector's (contract §3); these only bucket what `selectPlaneItems` already produced.

import { WK_GROUP_ORDER, type WorkItem } from "@maestro/protocol";
import type { StatusTone } from "@maestro/ui";
import type { BoardGroup } from "@/store";
import { type BoardSection, toSections } from "./board-view";

/** Group flat items into per-OrchState `BoardGroup`s in attention order (review first), non-empty only. */
function groupByState(items: WorkItem[]): BoardGroup[] {
  const groups: BoardGroup[] = [];
  for (const state of WK_GROUP_ORDER) {
    const inState = items.filter((i) => i.state === state);
    if (inState.length > 0) groups.push({ state, items: inState });
  }
  return groups;
}

/**
 * Feed / list sections — plain-voice buckets (Needs you → Stuck → Running → Queued → Done),
 * attention-first, recency within a bucket. Identical shaping to the board's `toSections` so the
 * feed and the board agree on grouping; the ONLY difference is the plane feeds leaf items.
 */
export function feedSections(items: WorkItem[]): BoardSection[] {
  return toSections(groupByState(items));
}

/** One board-view column — a fixed plain-voice bucket over a set of OrchStates. */
export interface PlaneColumn {
  label: string;
  tone: StatusTone;
  /** the OrchStates this column collects. */
  states: readonly WorkItem["state"][];
  /** the calm one-line hint under the column header. */
  hint: string;
  items: WorkItem[];
}

/** The four board columns (prototype MCV_COLS), mapped to our 8-state OrchState + plain-voice tones. */
const COLUMNS: readonly Omit<PlaneColumn, "items">[] = [
  {
    label: "Queued",
    tone: "neutral",
    states: ["proposed", "reviewing", "triggered"],
    hint: "Specs and next ticks",
  },
  { label: "Running", tone: "info", states: ["running"], hint: "Live in worktrees" },
  {
    label: "Needs you",
    tone: "accent",
    states: ["review", "blocked"],
    hint: "At your gate or stuck",
  },
  {
    label: "Done",
    tone: "success",
    states: ["done", "canceled"],
    hint: "The branch is the receipt",
  },
];

/**
 * The board plane — four fixed columns (Queued · Running · Needs you · Done). Always all four
 * (empty columns still render their frame, unlike the feed which drops empty groups) so the board
 * shape is stable. Within a column, recency (updatedAt desc — ISO compares lexically).
 */
export function toColumns(items: WorkItem[]): PlaneColumn[] {
  return COLUMNS.map((col) => ({
    ...col,
    items: items
      .filter((i) => col.states.includes(i.state))
      .sort((a, b) => (a.updatedAt < b.updatedAt ? 1 : a.updatedAt > b.updatedAt ? -1 : 0)),
  }));
}

/** The feed triage headline — attention count + active count, plain voice (no percentages). */
export function triage(items: WorkItem[]): { attention: number; active: number; headline: string } {
  const attention = items.filter((i) => i.state === "review" || i.state === "blocked").length;
  const active = items.filter((i) => i.state !== "done" && i.state !== "canceled").length;
  const headline =
    attention > 0
      ? `${attention} ${attention === 1 ? "piece" : "pieces"} of work ${attention === 1 ? "needs" : "need"} you`
      : "All clear";
  return { attention, active, headline };
}
