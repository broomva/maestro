// Board view helpers (BRO-1780) — the PURE, testable half of the board. The store owns
// the data (selectBoard → per-OrchState groups in WK_GROUP_ORDER, review first); these
// shape it for render without re-deriving anything.

import { type StatusTone, workStatusView } from "@maestro/ui";
import type { BoardGroup } from "@/store";

/** A rendered board section — one plain-voice bucket, its cards, and its badge tone. */
export interface BoardSection {
  /** Plain-voice label (Needs you · Stuck · Running · Queued · Done). */
  label: string;
  /** StatusBadge tone for the section dot (accent = accent-blue "Needs you"). */
  tone: StatusTone;
  /** A representative OrchState — stable key + test id. */
  state: BoardGroup["state"];
  items: BoardGroup["items"];
}

/**
 * Collapse the per-OrchState board groups into plain-voice SECTIONS, merging states that
 * share a plain-voice label (proposed/reviewing/triggered → "Queued"; done/canceled → "Done")
 * so a section header never repeats. First-appearance order is preserved, and because
 * `selectBoard` emits in `WK_GROUP_ORDER` (review first), the sections come out attention-first:
 * Needs you → Stuck → Running → Queued → Done. Within a merged section, items re-sort by
 * recency (`updatedAt` desc, ISO strings compare lexically) so the merge stays newest-first.
 */
export function toSections(groups: BoardGroup[]): BoardSection[] {
  const sections: BoardSection[] = [];
  const byLabel = new Map<string, BoardSection>();
  for (const g of groups) {
    const v = workStatusView(g.state);
    const existing = byLabel.get(v.label);
    if (existing) {
      existing.items = existing.items.concat(g.items);
    } else {
      const section: BoardSection = {
        label: v.label,
        tone: v.tone,
        state: g.state,
        items: [...g.items],
      };
      byLabel.set(v.label, section);
      sections.push(section);
    }
  }
  for (const section of sections) {
    if (section.items.length > 1) {
      section.items.sort((a, b) =>
        a.updatedAt < b.updatedAt ? 1 : a.updatedAt > b.updatedAt ? -1 : 0,
      );
    }
  }
  return sections;
}

/**
 * Compact relative age ("12s" · "5m" · "3h" · "2d") from an ISO timestamp — the card receipt,
 * never a progress percentage (CLAUDE.md §Work states). Empty string on a corrupt timestamp.
 */
export function relativeTime(iso: string, now: number = Date.now()): string {
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return "";
  const s = Math.max(0, Math.round((now - then) / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h}h`;
  return `${Math.round(h / 24)}d`;
}
