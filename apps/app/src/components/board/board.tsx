// Board (BRO-1780) — the read-only work board: real nodes in attention order over the live
// stream. The convergence of both P1 chains — it renders the store's `selectBoard` (server
// truth fed by the SSE stream), so a hand-edit to a `_work.md` on disk propagates here with no
// reload (the P1 exit behavior). Groups are plain-voice sections, review ("Needs you") first.
//
// Live subscription note (zustand v5): select the STABLE `server` slice reference (it changes
// only when the reducer applies an event) and derive the board in `useMemo` keyed on it —
// selecting `selectBoard(...)` directly would return a fresh array every render and thrash
// useSyncExternalStore ("getSnapshot should be cached"). The derivation is pure + cheap.

import { useMemo, useState } from "react";
import { useStore } from "zustand";
import { maestroStore, selectBoard, selectNeedsYouCount } from "@/store";
import { toSections } from "./board-view";
import { Inspector } from "./inspector";
import { WorkCard } from "./work-card";

export function Board() {
  const server = useStore(maestroStore, (s) => s.server);
  const sections = useMemo(() => toSections(selectBoard(server)), [server]);
  const needsYou = useMemo(() => selectNeedsYouCount(server), [server]);
  // Selection is ephemeral + component-local — it drives the inspector (M5 stub), never server truth.
  // `setSelectedId` is a stable ref, so passing it straight to WorkCard keeps its React.memo intact.
  const [selectedId, setSelectedId] = useState<string | null>(null);
  // The selected work item, looked up across the rendered sections. Memoized so the lookup (and the
  // inspector's re-render) fire only when the selection or the board data actually changed.
  const selectedItem = useMemo(
    () =>
      selectedId === null
        ? null
        : (sections.flatMap((s) => s.items).find((i) => i.id === selectedId) ?? null),
    [sections, selectedId],
  );

  if (sections.length === 0) {
    return (
      <div
        data-testid="board-empty"
        className="flex h-full items-center justify-center text-muted-foreground text-sm"
      >
        No work yet. Add a _work.md to the workspace and it appears here.
      </div>
    );
  }

  return (
    <div className="flex items-start gap-6" data-testid="board-layout">
      <div className="flex min-w-0 flex-1 flex-col gap-6" data-testid="board">
        <header className="flex items-baseline justify-between">
          <h1 className="text-foreground text-h1">Board</h1>
          {needsYou > 0 ? (
            <span data-testid="needs-you" className="text-[var(--bv-blue-accent)] text-sm">
              {needsYou} {needsYou === 1 ? "thing needs" : "things need"} you
            </span>
          ) : null}
        </header>

        {sections.map((section) => (
          <section
            key={section.label}
            data-testid={`board-group-${section.state}`}
            data-group-label={section.label}
            className="flex flex-col gap-2.5"
          >
            <div className="flex items-center gap-2">
              <h2 className="font-medium text-foreground text-sm">{section.label}</h2>
              <span className="text-muted-foreground text-xs tabular-nums">
                {section.items.length}
              </span>
            </div>
            <div className="grid grid-cols-[repeat(auto-fill,minmax(260px,1fr))] gap-3">
              {section.items.map((item) => (
                <WorkCard
                  key={item.id}
                  item={item}
                  selected={item.id === selectedId}
                  onSelect={setSelectedId}
                />
              ))}
            </div>
          </section>
        ))}
      </div>

      {/* Selection drives the inspector (M5 stub) — a matte panel (never glass, CLAUDE.md §Glass),
          ~45% viewport / 380px min (CLAUDE.md §Layout), sticky so it holds while the board column
          scrolls, own overflow so the shell still never scrolls. Present on selection. */}
      {selectedItem ? (
        <div
          data-testid="inspector-panel"
          className="sticky top-0 max-h-[calc(100dvh-52px-3rem)] w-[45%] min-w-[380px] shrink-0 self-start overflow-y-auto rounded-card border border-border bg-card p-4"
        >
          <Inspector item={selectedItem} />
        </div>
      ) : null}
    </div>
  );
}
