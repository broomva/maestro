// WorkCard (BRO-1780; M3 BRO-1825) — one node on the board. Matte Card (glass is forbidden on cards,
// CLAUDE.md §Glass); the Undertow halo is the ONLY running signal (`running` prop wraps the
// card, it stays matte); the plain-voice StatusBadge carries the state, accent-blue for
// "Needs you", never red. No progress percentage — the receipt is the crumb, age, and run branch.
//
// M3 memoization (scope: "React.memo on list items"): `selectBoard` RE-DERIVES fresh WorkItem objects
// every render (deriveWorkItem, s-dependent for ancestry), so item refs churn on every SSE event and a
// bare React.memo (shallow ref compare) would still re-render every card. The comparator below compares
// the fields this card actually RENDERS, so a card re-renders only when ITS visible content — or its
// selection — changed; one node.updated no longer thrashes the whole grid. `onSelect` is assumed stable
// (Board passes the state setter directly) so it is not compared.

import type { WorkItem } from "@maestro/protocol";
import { Card, cn, DotComet, StatusBadge, workStatusView } from "@maestro/ui";
import { memo } from "react";
import { relativeTime } from "./board-view";

export interface WorkCardProps {
  item: WorkItem;
  selected: boolean;
  onSelect: (id: string) => void;
  /** A coarse board clock (epoch-ms) the age is computed against, threaded from Board's low-frequency
   *  tick. Injecting it (vs reading ambient `Date.now()`) is what keeps ages HONEST under the memo: the
   *  comparator can't see the wall clock, so without a compared time input an idle card's age would
   *  freeze until its own fields change while running cards advance. The tick bumps `now` for every card
   *  at once; between ticks an unrelated node.updated still skips the idle cards (the memo win). */
  now: number;
}

function WorkCardImpl({ item, selected, onSelect, now }: WorkCardProps) {
  const v = workStatusView(item.state, item.kind);
  const crumb = [item.initiative, item.project].filter(Boolean).join(" › ");
  const age = relativeTime(item.lastEventAt ?? item.updatedAt, now);

  return (
    <Card
      interactive
      running={v.running}
      role="button"
      tabIndex={0}
      aria-pressed={selected}
      data-testid="work-card"
      // Stable DOM hook for the running state — the Undertow wrapper (`.bv-undertow`) carries no testid,
      // so board-m3.pw.ts asserts the live running signal on this attribute (M3 done.check).
      data-running={v.running ? "" : undefined}
      onClick={() => onSelect(item.id)}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onSelect(item.id);
        }
      }}
      className={cn("flex cursor-pointer flex-col gap-2", selected && "ring-2 ring-[var(--ring)]")}
    >
      <div className="flex items-center justify-between gap-2 text-muted-foreground text-xs">
        {/* crumb is empty for the workspace-root node (path "", no ancestry) — render nothing
            rather than a placeholder glyph (CLAUDE.md §Voice forbids em dashes in chrome). */}
        <span className="truncate">{crumb || item.path}</span>
        {age ? <span className="shrink-0 tabular-nums">{age}</span> : null}
      </div>
      <div className="truncate font-medium text-foreground text-sm">{item.title}</div>
      <div className="flex items-center justify-between gap-2">
        <StatusBadge
          status={v.tone}
          pulse={v.pulse}
          dot={v.running ? <DotComet size={8} /> : undefined}
        >
          {v.label}
        </StatusBadge>
        {item.run ? (
          <span className="truncate font-mono text-muted-foreground text-xs">{item.run}</span>
        ) : null}
      </div>
    </Card>
  );
}

/** Re-render only when a field this card renders — or its selection, or the board clock — changed.
 *  Covers every visible input: the status (state/kind → workStatusView), the crumb
 *  (initiative/project/path), the age (lastEventAt/updatedAt + `now`), the title, and the run branch.
 *  Any node change bumps `updatedAt`, but the crumb derives from ancestors (an ancestor rename need not
 *  bump this node's updatedAt) so the crumb fields are compared explicitly, and `now` is compared so a
 *  clock tick refreshes every card's age. `onSelect` is compared because a broken (unstable) parent would
 *  otherwise silently defeat the memo — cheap insurance. Exported for a mutation-proving unit test: the
 *  whole point of this file is that this comparator is neither `() => true` nor missing a field. */
export function areEqual(a: WorkCardProps, b: WorkCardProps): boolean {
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

export const WorkCard = memo(WorkCardImpl, areEqual);
