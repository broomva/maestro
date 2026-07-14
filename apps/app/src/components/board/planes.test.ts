/// <reference types="bun" />

// ListRow memo comparator (BRO-1886) — the load-bearing line of the list plane's memoization. Same
// ANTI-SLOP guard as work-card.test.tsx: it fails if `rowAreEqual` regresses to `() => true` (every
// "changed → false" case breaks) OR drops any compared field (that field's case breaks). Idle rows
// must skip the 30s board tick + unrelated SSE events, but a changed / (de)selected / tick-advanced
// row must re-render. Kept in lockstep with work-card's `areEqual` — the two must track the same
// rendered fields, since the list row and the card render the same WorkItem inputs.

import { describe, expect, test } from "bun:test";
import type { WorkItem } from "@maestro/protocol";
import { type RowProps, rowAreEqual } from "./planes";

const ITEM: WorkItem = {
  id: "a",
  state: "running",
  kind: "task",
  title: "Build the runner",
  gate: "human",
  path: "build",
  updatedAt: "2026-07-11T00:00:00.000Z",
};
const onSelect = (_: string) => {};
const base: RowProps = { item: ITEM, selected: false, onSelect, now: 1000 };

describe("ListRow rowAreEqual — the memo comparator", () => {
  test("equal props → true, even when the item is a FRESH object (refs churn every render)", () => {
    expect(rowAreEqual(base, { ...base })).toBe(true);
    // selectPlaneItems re-derives a new WorkItem object every render — identical fields must still equal.
    expect(rowAreEqual(base, { ...base, item: { ...ITEM } })).toBe(true);
  });

  test("a change to ANY rendered field → false (proves no dropped field, and not `() => true`)", () => {
    expect(rowAreEqual(base, { ...base, selected: true })).toBe(false);
    expect(rowAreEqual(base, { ...base, now: 2000 })).toBe(false); // clock tick refreshes the age
    expect(rowAreEqual(base, { ...base, onSelect: (_: string) => {} })).toBe(false);
    const diff = (patch: Partial<WorkItem>) =>
      rowAreEqual(base, { ...base, item: { ...ITEM, ...patch } });
    expect(diff({ id: "b" })).toBe(false);
    expect(diff({ state: "blocked" })).toBe(false);
    expect(diff({ kind: "routine" })).toBe(false);
    expect(diff({ title: "Renamed" })).toBe(false);
    expect(diff({ run: "run/xyz" })).toBe(false);
    expect(diff({ initiative: "Platform" })).toBe(false);
    expect(diff({ project: "Runner" })).toBe(false);
    expect(diff({ path: "build2" })).toBe(false);
    expect(diff({ updatedAt: "2026-07-12T00:00:00.000Z" })).toBe(false);
    expect(diff({ lastEventAt: "2026-07-11T00:00:05.000Z" })).toBe(false);
  });
});
