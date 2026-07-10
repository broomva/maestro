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
import { WorkCard } from "./work-card";

export function Board() {
  const server = useStore(maestroStore, (s) => s.server);
  const sections = useMemo(() => toSections(selectBoard(server)), [server]);
  const needsYou = useMemo(() => selectNeedsYouCount(server), [server]);
  // Selection is ephemeral + component-local (the inspector stub, contract: "selection wiring
  // stub for the M5 inspector"). The server-truth slice never holds board selection.
  const [selectedId, setSelectedId] = useState<string | null>(null);

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
    <div className="flex flex-col gap-6" data-testid="board">
      <header className="flex items-baseline justify-between">
        <h1 className="text-foreground text-h2">Board</h1>
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
  );
}
