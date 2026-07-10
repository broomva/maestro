/// <reference types="bun" />
// board-view.test.ts — the pure board-shaping logic (BRO-1780). No React, no store: the
// group-merge + ordering + relative-age contract, deterministic.

import { describe, expect, test } from "bun:test";
import { workStatusView } from "@maestro/ui";
import type { BoardGroup } from "@/store";
import { relativeTime, toSections } from "./board-view";

const item = (
  id: string,
  state: BoardGroup["state"],
  updatedAt: string,
): BoardGroup["items"][number] => ({
  id,
  state,
  kind: "task",
  title: id,
  gate: "human",
  path: id,
  parentId: null,
  updatedAt,
});

describe("toSections — plain-voice sections, attention-first", () => {
  // selectBoard emits groups in WK_GROUP_ORDER (review first); toSections preserves that.
  const groups: BoardGroup[] = [
    { state: "review", items: [item("r1", "review", "2026-07-10T01:00:00.000Z")] },
    { state: "blocked", items: [item("b1", "blocked", "2026-07-10T01:00:00.000Z")] },
    { state: "running", items: [item("run1", "running", "2026-07-10T01:00:00.000Z")] },
    { state: "proposed", items: [item("p1", "proposed", "2026-07-10T01:00:00.000Z")] },
    { state: "triggered", items: [item("t1", "triggered", "2026-07-10T03:00:00.000Z")] },
    { state: "done", items: [item("d1", "done", "2026-07-10T01:00:00.000Z")] },
  ];

  test("review (Needs you) is the first section", () => {
    const sections = toSections(groups);
    expect(sections[0]?.state).toBe("review");
    expect(sections[0]?.label).toBe(workStatusView("review").label);
    expect(sections[0]?.tone).toBe("accent"); // accent-blue "Needs you", never red
  });

  test("states sharing a plain-voice label merge into ONE section (proposed + triggered → Queued)", () => {
    // Precondition: these two states DO share a label (the merge is meaningful).
    expect(workStatusView("proposed").label).toBe(workStatusView("triggered").label);
    const sections = toSections(groups);
    // 6 input groups → 5 sections (proposed+triggered collapsed).
    expect(sections).toHaveLength(5);
    const queued = sections.find((s) => s.label === workStatusView("proposed").label);
    expect(queued?.items.map((i) => i.id).sort()).toEqual(["p1", "t1"]);
  });

  test("within a merged section, items stay newest-first (updatedAt desc)", () => {
    const sections = toSections(groups);
    const queued = sections.find((s) => s.label === workStatusView("proposed").label);
    // t1 (03:00) is newer than p1 (01:00) → t1 first after the merge re-sort.
    expect(queued?.items.map((i) => i.id)).toEqual(["t1", "p1"]);
  });

  test("empty input → no sections", () => {
    expect(toSections([])).toEqual([]);
  });
});

describe("relativeTime — compact receipt age", () => {
  const now = Date.parse("2026-07-10T12:00:00.000Z");
  test("scales s → m → h → d", () => {
    expect(relativeTime("2026-07-10T11:59:48.000Z", now)).toBe("12s");
    expect(relativeTime("2026-07-10T11:55:00.000Z", now)).toBe("5m");
    expect(relativeTime("2026-07-10T09:00:00.000Z", now)).toBe("3h");
    expect(relativeTime("2026-07-08T12:00:00.000Z", now)).toBe("2d");
  });
  test("a future/near timestamp clamps to 0s, and a corrupt one is empty", () => {
    expect(relativeTime("2026-07-10T12:00:05.000Z", now)).toBe("0s"); // clamped, never negative
    expect(relativeTime("not-a-date", now)).toBe("");
  });
});
