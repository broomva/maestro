/// <reference types="bun" />

// WorkCard memo comparator (BRO-1825) — the load-bearing line of the M3 memoization pass. This is the
// ANTI-SLOP guard the P20 gate asked for: it fails if `areEqual` regresses to `() => true` (every
// "changed → false" case breaks) OR drops any compared field (that field's case breaks). The React.memo
// wrapper itself is React's; what MUST stay correct is that this comparator tracks every field the card
// renders, so an idle card skips re-render but a changed/(de)selected/tick-advanced card does not.

import { describe, expect, test } from "bun:test";
import type { WorkItem } from "@maestro/protocol";
import { areEqual, type WorkCardProps } from "./work-card";

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
const base: WorkCardProps = { item: ITEM, selected: false, onSelect, now: 1000 };

describe("WorkCard areEqual — the memo comparator", () => {
  test("equal props → true, even when the item is a FRESH object (refs churn every render)", () => {
    expect(areEqual(base, { ...base })).toBe(true);
    // selectBoard re-derives a new WorkItem object every render — identical fields must still equal.
    expect(areEqual(base, { ...base, item: { ...ITEM } })).toBe(true);
  });

  test("a change to ANY rendered field → false (proves no dropped field, and not `() => true`)", () => {
    expect(areEqual(base, { ...base, selected: true })).toBe(false);
    expect(areEqual(base, { ...base, now: 2000 })).toBe(false); // clock tick refreshes age
    expect(areEqual(base, { ...base, onSelect: (_: string) => {} })).toBe(false);
    const diff = (patch: Partial<WorkItem>) =>
      areEqual(base, { ...base, item: { ...ITEM, ...patch } });
    expect(diff({ id: "b" })).toBe(false);
    expect(diff({ state: "blocked" })).toBe(false);
    expect(diff({ kind: "project" })).toBe(false);
    expect(diff({ title: "Renamed" })).toBe(false);
    expect(diff({ run: "run/xyz" })).toBe(false);
    expect(diff({ initiative: "Platform" })).toBe(false);
    expect(diff({ project: "Runner" })).toBe(false);
    expect(diff({ path: "build2" })).toBe(false);
    expect(diff({ updatedAt: "2026-07-12T00:00:00.000Z" })).toBe(false);
    expect(diff({ lastEventAt: "2026-07-11T00:00:05.000Z" })).toBe(false);
  });
});
