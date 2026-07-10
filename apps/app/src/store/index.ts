// The client store (BRO-1775) — public surface. Server-truth slice fed by the
// event stream (BRO-1816) + a single persisted UI-prefs slice; the projector
// derives WorkItems on read. See docs/contracts/work-item-store.md.

export * from "./project";
export * from "./reducer";
export * from "./store";
export * from "./stream";
export * from "./types";
