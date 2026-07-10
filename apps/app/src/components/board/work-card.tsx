// WorkCard (BRO-1780) — one node on the board. Matte Card (glass is forbidden on cards,
// CLAUDE.md §Glass); the Undertow halo is the ONLY running signal (`running` prop wraps the
// card, it stays matte); the plain-voice StatusBadge carries the state, accent-blue for
// "Needs you", never red. No progress percentage — the receipt is the crumb, age, and run branch.

import type { WorkItem } from "@maestro/protocol";
import { Card, cn, DotComet, StatusBadge, workStatusView } from "@maestro/ui";
import { relativeTime } from "./board-view";

export interface WorkCardProps {
  item: WorkItem;
  selected: boolean;
  onSelect: (id: string) => void;
}

export function WorkCard({ item, selected, onSelect }: WorkCardProps) {
  const v = workStatusView(item.state, item.kind);
  const crumb = [item.initiative, item.project].filter(Boolean).join(" › ");
  const age = relativeTime(item.lastEventAt ?? item.updatedAt);

  return (
    <Card
      interactive
      running={v.running}
      role="button"
      tabIndex={0}
      aria-pressed={selected}
      data-testid="work-card"
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
        <span className="truncate">{crumb || item.path || "—"}</span>
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
