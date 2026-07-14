// The Maestro client store (BRO-1775) — a Zustand store with exactly two data
// slices (contract §5): the SERVER-TRUTH slice (raw live rows fed by the event
// stream + intents) and the PERSISTED UI-PREFS slice (view / navOpen / cols). The
// load-bearing invariant: `persist` writes ONLY the prefs slice — server truth
// never touches storage (`partialize` below), so components never sprinkle
// localStorage keys and a refresh loses no work, only re-derives from the stream.
//
// A vanilla store (`zustand/vanilla`) so the skeleton is framework-light and fully
// unit-testable via `.getState()` / `.setState()`; the board/feed tickets bind it
// to React with `useStore(maestroStore, selector)` (BRO-1780). `createMaestroStore`
// is a factory (fresh store + injectable storage) for test isolation; `maestroStore`
// is the app singleton.

import type { EventEnvelope, PlaneView } from "@maestro/protocol";
import { createJSONStorage, persist, type StateStorage } from "zustand/middleware";
import { createStore } from "zustand/vanilla";
import {
  type HydrateRows,
  hydrate as hydrateRows,
  applyEvent as reduceEvent,
  applyEvents as reduceEvents,
} from "./reducer";
import {
  defaultPrefs,
  emptyServerTruth,
  type GateGraceEntry,
  type Prefs,
  type ServerTruth,
} from "./types";

/** The full store surface — the two data slices + the actions over them. */
export interface MaestroStore extends Prefs {
  /** the server-truth slice (raw rows fed by the stream; the projector reads it). */
  server: ServerTruth;

  // ── server-truth ingestion (fed ONLY by the stream / read-API hydration) ──
  /** seed from the read API (`/api/tree`, `/api/node/:id`) before the stream opens. */
  hydrate(rows: HydrateRows): void;
  /** fold one wire event into server truth (idempotent under re-delivery). */
  applyEvent(e: EventEnvelope): void;
  /** fold an ordered batch (the backlog replay + live tail). */
  applyEvents(events: readonly EventEnvelope[]): void;

  // ── selection (open sessions are server truth — contract §6) ──
  openSession(id: string): void;
  closeSession(id: string): void;
  focusSession(id: string | null): void;
  openFile(path: string): void;
  closeFile(path: string): void;

  // ── the gate grace window (chosen-but-unsent verb + undo timer) ──
  setGateGrace(gateId: string, entry: GateGraceEntry): void;
  clearGateGrace(gateId: string): void;

  // ── persisted UI prefs (the ONLY persisted slice) ──
  setView(view: PlaneView): void;
  setNavOpen(navOpen: boolean): void;
  toggleNav(): void;
  setCol(key: string, width: number): void;
  /** toggle the FS pane (BRO-1890) — a persisted layout pref. */
  toggleFs(): void;

  /** clear server truth (a hard reconnect / stream reset); prefs are untouched. */
  reset(): void;
}

/** An in-memory `StateStorage` — the fallback when there is no `localStorage` (SSR / tests). */
function memoryStorage(): StateStorage {
  const m = new Map<string, string>();
  return {
    getItem: (k) => m.get(k) ?? null,
    setItem: (k, v) => {
      m.set(k, v);
    },
    removeItem: (k) => {
      m.delete(k);
    },
  };
}

/** Real `localStorage` in the browser, an in-memory shim otherwise (never throws at import). */
function defaultStorage(): StateStorage {
  return typeof globalThis !== "undefined" && globalThis.localStorage
    ? globalThis.localStorage
    : memoryStorage();
}

/**
 * Wrap a storage so `setItem` skips a write when the serialized value is unchanged.
 * Load-bearing: the persisted prefs slice shares this store with the high-churn
 * server-truth slice, and zustand `persist` re-writes the partialized blob on EVERY
 * `set` (no diffing). Without this, each streamed SSE event would trigger a
 * synchronous `localStorage.setItem` of the UNCHANGED `{view,navOpen,cols}` — write
 * amplification on the hot path. This collapses it to one write per real pref change.
 */
function writeIfChanged(inner: StateStorage): StateStorage {
  let last: string | null = null;
  return {
    getItem: (k) => inner.getItem(k),
    setItem: (k, v) => {
      if (v === last) return;
      last = v;
      inner.setItem(k, v);
    },
    removeItem: (k) => {
      last = null;
      inner.removeItem(k);
    },
  };
}

/** Build a fresh store; pass `storage`/`name` to isolate persistence (tests, multi-tenant). */
export function createMaestroStore(opts?: { storage?: StateStorage; name?: string }) {
  return createStore<MaestroStore>()(
    persist(
      (set) => ({
        server: emptyServerTruth(),
        ...defaultPrefs(),

        hydrate: (rows) => set((st) => ({ server: hydrateRows(st.server, rows) })),
        applyEvent: (e) => set((st) => ({ server: reduceEvent(st.server, e) })),
        applyEvents: (events) => set((st) => ({ server: reduceEvents(st.server, events) })),

        openSession: (id) =>
          set((st) => ({
            server: {
              ...st.server,
              openSessionIds: st.server.openSessionIds.includes(id)
                ? st.server.openSessionIds
                : [...st.server.openSessionIds, id],
              activeSessionId: id,
            },
          })),
        closeSession: (id) =>
          set((st) => {
            const openSessionIds = st.server.openSessionIds.filter((s) => s !== id);
            const activeSessionId =
              st.server.activeSessionId === id
                ? (openSessionIds[openSessionIds.length - 1] ?? null)
                : st.server.activeSessionId;
            return { server: { ...st.server, openSessionIds, activeSessionId } };
          }),
        focusSession: (id) => set((st) => ({ server: { ...st.server, activeSessionId: id } })),
        openFile: (path) =>
          set((st) =>
            st.server.openFilePaths.includes(path)
              ? {}
              : { server: { ...st.server, openFilePaths: [...st.server.openFilePaths, path] } },
          ),
        closeFile: (path) =>
          set((st) => ({
            server: {
              ...st.server,
              openFilePaths: st.server.openFilePaths.filter((p) => p !== path),
            },
          })),

        setGateGrace: (gateId, entry) =>
          set((st) => ({
            server: { ...st.server, gateGrace: { ...st.server.gateGrace, [gateId]: entry } },
          })),
        clearGateGrace: (gateId) =>
          set((st) => {
            const gateGrace = { ...st.server.gateGrace };
            delete gateGrace[gateId];
            return { server: { ...st.server, gateGrace } };
          }),

        setView: (view) => set({ view }),
        setNavOpen: (navOpen) => set({ navOpen }),
        toggleNav: () => set((st) => ({ navOpen: !st.navOpen })),
        setCol: (key, width) => set((st) => ({ cols: { ...st.cols, [key]: width } })),
        toggleFs: () => set((st) => ({ fsOpen: !st.fsOpen })),

        reset: () => set({ server: emptyServerTruth() }),
      }),
      {
        name: opts?.name ?? "maestro-ui-prefs",
        storage: createJSONStorage(() => writeIfChanged(opts?.storage ?? defaultStorage())),
        // THE invariant: only the prefs keys are persisted — never server truth.
        partialize: (s) => ({ view: s.view, navOpen: s.navOpen, cols: s.cols, fsOpen: s.fsOpen }),
        version: 1,
      },
    ),
  );
}

/** The app-wide store singleton (the board/feed tickets bind it to React). */
export const maestroStore = createMaestroStore();

/** The store handle type (`.getState()` / `.subscribe()` / `.setState()`). */
export type MaestroStoreApi = ReturnType<typeof createMaestroStore>;
