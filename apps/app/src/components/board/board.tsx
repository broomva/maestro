// The mission plane (BRO-1780 board → BRO-1886 fidelity) — the center surface of the shell: the
// prototype's feed / board / list planes over the live work (MccMaestroLoopV2, canon per
// docs/canon-map.md). It renders LEAF WorkItems (selectPlaneItems) — the actionable work; the
// container folders are the sidebar tree's job (the disclosure ladder). The convergence of both P1
// chains still holds: it reads the store's server-truth slice (fed by the SSE stream), so a
// hand-edit to a `_work.md` on disk propagates here with no reload.
//
// The plane owns its height + scroll: `.mcc-plane` fills the shell's <main> frame, the plane-bar is
// fixed, and `.mcc-plane-body` is the one inner panel that scrolls (CLAUDE.md §Layout). The view
// (feed/board/list) is the persisted `prefs.view`; the filter is ephemeral local state.
//
// Live subscription note (zustand v5): select the STABLE `server` slice reference and derive the
// plane data in `useMemo` keyed on it — deriving selectors inline returns fresh objects every render
// and thrashes useSyncExternalStore ("getSnapshot should be cached"). The derivations are pure.

import { STATUS_DOT_VAR } from "@maestro/ui";
import { X } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useStore } from "zustand";
import { postIntent } from "@/intents/client";
import { maestroStore, selectPlaneItems } from "@/store";
import { ErrorBoundary } from "../error-boundary";
import { Inspector } from "./inspector";
import { feedSections, toColumns, triage } from "./plane-view";
import { BoardPlane, FeedPlane, ListPlane, PlaneToggle } from "./planes";

/** Board clock cadence (ms) — refreshes every card's relative age. Coarse on purpose: the age is a
 *  receipt, not a per-second counter (the Undertow is the liveness signal), and a low frequency keeps
 *  the memo win (SSE events between ticks still skip idle cards). */
const AGE_TICK_MS = 30_000;

export function Board() {
  const server = useStore(maestroStore, (s) => s.server);
  const view = useStore(maestroStore, (s) => s.view);
  const setView = useStore(maestroStore, (s) => s.setView);

  const items = useMemo(() => selectPlaneItems(server), [server]);
  const sections = useMemo(() => feedSections(items), [items]);
  const columns = useMemo(() => toColumns(items), [items]);
  const tri = useMemo(() => triage(items), [items]);

  // Selection is ephemeral + component-local — drives the inspector, never server truth. Stable
  // `select` ref (useCallback) so WorkCard's memo comparator (which checks onSelect) is not defeated.
  const [selectedId, setSelectedId] = useState<string | null>(null);
  // The feed's state filter — the STABLE plain-voice label ("Needs you" · "Queued" · "Done" · …), or
  // null for "All". Ephemeral, feed-only. Keyed on the label, NOT a section's representative OrchState:
  // a merged bucket's representative sub-state is unstable (when the first-in-order sub-state empties
  // while the bucket still holds items, the representative shifts), which would silently drop the active
  // filter. The label is invariant across sub-state churn within a bucket.
  const [filter, setFilter] = useState<string | null>(null);

  // A coarse board clock threaded into every card so relative ages stay honest under the memo (the
  // comparator can't see the wall clock). Ticks are far apart, so between them a node.updated still
  // re-renders only the changed card.
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), AGE_TICK_MS);
    return () => clearInterval(id);
  }, []);

  const selectedItem = useMemo(
    () => (selectedId === null ? null : (items.find((i) => i.id === selectedId) ?? null)),
    [items, selectedId],
  );
  // Clear a dangling selection: if the selected node LEFT the plane (tombstoned / filtered to a
  // container / re-grouped away), drop the id so a later reappearance of the same stable UUID does
  // not silently re-open the inspector.
  useEffect(() => {
    if (selectedId !== null && selectedItem === null) setSelectedId(null);
  }, [selectedId, selectedItem]);
  const select = useCallback((id: string) => {
    setSelectedId((cur) => (cur === id ? null : id));
  }, []);
  // Escape dismisses the inspector while it is open (a discoverable, keyboard-first close path).
  useEffect(() => {
    if (selectedId === null) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setSelectedId(null);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [selectedId]);

  // The feed body honours the filter; the board/list always show the full set (the prototype filters
  // only the feed). A stale filter (its bucket drained via a live update) falls back to "All" rather
  // than blanking — and the effect below then clears it so the chip highlight agrees with the content.
  const filteredFeed = useMemo(() => {
    if (filter === null) return sections;
    const only = sections.filter((s) => s.label === filter);
    return only.length > 0 ? only : sections;
  }, [sections, filter]);
  // Clear a stale filter: if the filtered bucket is gone entirely (drained by an SSE update), drop it
  // so the tablist never sits with content-shows-all but no chip selected. Label-keyed, so it fires
  // only when the whole plain-voice bucket empties — not on sub-state churn within it.
  useEffect(() => {
    if (filter !== null && !sections.some((s) => s.label === filter)) setFilter(null);
  }, [sections, filter]);

  const empty = items.length === 0;

  return (
    <div className="flex h-full min-h-0" data-testid="board-layout">
      <div className="mcc-plane flex-1" data-testid="board">
        <div className="mcc-plane-bar">
          {view === "feed" && !empty ? (
            // Toggle chips, not tabs: each is independently pressable AND deselectable (clicking the
            // active chip clears to "All"), so the honest ARIA is a group of aria-pressed toggles — a
            // tablist would promise one-always-selected + arrow roving, which these deliberately are not.
            // biome-ignore lint/a11y/useSemanticElements: role="group" over <fieldset> — toggle buttons, not form fields.
            <div className="mc-chips" role="group" aria-label="Filter by state">
              <button
                type="button"
                aria-pressed={filter === null}
                className={`mc-chip${filter === null ? " is-active" : ""}`}
                onClick={() => setFilter(null)}
              >
                All
              </button>
              {sections.map((s) => (
                <button
                  key={s.label}
                  type="button"
                  aria-pressed={filter === s.label}
                  className={`mc-chip${filter === s.label ? " is-active" : ""}`}
                  onClick={() => setFilter(filter === s.label ? null : s.label)}
                >
                  <span className="mc-chip-dot" style={{ background: STATUS_DOT_VAR[s.tone] }} />
                  {s.label}
                  <span className="mc-chip-count">{s.items.length}</span>
                </button>
              ))}
            </div>
          ) : (
            <span className="text-foreground text-sm" style={{ fontWeight: 500 }}>
              Work
            </span>
          )}
          <PlaneToggle view={view} onView={setView} />
        </div>

        <div className="mcc-plane-body" data-view={view}>
          {empty ? (
            <div
              data-testid="board-empty"
              className="flex h-full items-center justify-center text-muted-foreground text-sm"
            >
              No work yet. Start a mission and it appears here.
            </div>
          ) : view === "board" ? (
            <BoardPlane columns={columns} selectedId={selectedId} onSelect={select} now={now} />
          ) : view === "list" ? (
            <ListPlane sections={sections} selectedId={selectedId} onSelect={select} now={now} />
          ) : (
            <FeedPlane
              sections={filteredFeed}
              headline={tri.headline}
              active={tri.active}
              selectedId={selectedId}
              onSelect={select}
              now={now}
            />
          )}
        </div>
      </div>

      {/* Selection drives the inspector — a matte panel (never glass, CLAUDE.md §Glass), ~45%
          viewport / 380px min, full-height with its own scroll so the shell still never scrolls. */}
      {selectedItem ? (
        <div
          data-testid="inspector-panel"
          className="flex h-full w-[45%] min-w-[380px] shrink-0 flex-col overflow-y-auto border-border border-l bg-card p-4"
        >
          <div className="mb-2 flex justify-end">
            <button
              type="button"
              data-testid="inspector-close"
              aria-label="Close inspector"
              onClick={() => setSelectedId(null)}
              className="flex size-7 items-center justify-center rounded-row text-muted-foreground transition-colors motion-reduce:transition-none hover:bg-[var(--bv-frost-8)] hover:text-foreground"
            >
              <X size={16} strokeWidth={2} />
            </button>
          </div>
          {/* A crashed inspector must not take down the plane (porting-notes §Production hardening).
              Keyed by id ALONE so an A→B switch remounts fresh, but a live node.updated on the SAME item
              does NOT force-remount the healthy subtree — that would drop a gate verb's in-flight 5s grace
              window (BRO-1809, early-commit-on-remount). Crash recovery is preserved via resetKeys: an
              updatedAt change retries the boundary only when it is ERRORED. */}
          <ErrorBoundary
            key={selectedItem.id}
            resetKeys={[selectedItem.updatedAt]}
            label="The inspector"
          >
            <Inspector item={selectedItem} onIntent={postIntent} />
          </ErrorBoundary>
        </div>
      ) : null}
    </div>
  );
}
