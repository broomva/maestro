/// <reference types="bun" />

// Plane-view unit tests (BRO-1886) — the pure shaping of leaf WorkItems into the feed's plain-voice
// sections and the board's four columns. Locks: feed is attention-first + collapses OrchStates to
// plain voice; the board always renders all four columns (empty ones too) with the right state map
// and recency order; triage counts attention + active in plain voice (never a percentage).

import { describe, expect, test } from "bun:test";
import type { WorkItem } from "@maestro/protocol";
import { feedSections, toColumns, triage } from "./plane-view";

let seq = 0;
/** A WorkItem with sane defaults; override what a case cares about. */
function item(p: Partial<WorkItem> & Pick<WorkItem, "state">): WorkItem {
  seq += 1;
  const id = p.id ?? `w${seq}`;
  return {
    id,
    state: p.state,
    kind: p.kind ?? "task",
    title: p.title ?? id,
    gate: p.gate ?? "human",
    path: p.path ?? id,
    parentId: p.parentId ?? null,
    updatedAt: p.updatedAt ?? "2026-01-01T00:00:00.000Z",
    ...(p.run ? { run: p.run } : {}),
  };
}

describe("feedSections", () => {
  test("attention-first, plain-voice collapse, drops empty buckets", () => {
    const sections = feedSections([
      item({ state: "done", title: "shipped" }),
      item({ state: "running", title: "live" }),
      item({ state: "review", title: "gate" }),
      item({ state: "blocked", title: "stuck" }),
      item({ state: "proposed", title: "queued" }),
    ]);
    // Needs you (review) → Stuck (blocked) → Running → Queued (proposed) → Done. No empty buckets.
    expect(sections.map((s) => s.label)).toEqual([
      "Needs you",
      "Stuck",
      "Running",
      "Queued",
      "Done",
    ]);
  });

  test("collapses proposed/reviewing/triggered into one Queued section", () => {
    const sections = feedSections([
      item({ state: "proposed" }),
      item({ state: "reviewing" }),
      item({ state: "triggered" }),
    ]);
    expect(sections.map((s) => s.label)).toEqual(["Queued"]);
    expect(sections[0]?.items.length).toBe(3);
  });
});

describe("toColumns", () => {
  test("always four columns in a fixed order, even when empty", () => {
    const cols = toColumns([item({ state: "running" })]);
    expect(cols.map((c) => c.label)).toEqual(["Queued", "Running", "Needs you", "Done"]);
    expect(cols.find((c) => c.label === "Running")?.items.length).toBe(1);
    expect(cols.find((c) => c.label === "Queued")?.items).toEqual([]); // empty column still present
  });

  test("maps OrchStates to the right column and sorts by recency", () => {
    const cols = toColumns([
      item({ id: "old", state: "review", updatedAt: "2026-01-01T00:00:00.000Z" }),
      item({ id: "new", state: "blocked", updatedAt: "2026-01-02T00:00:00.000Z" }),
      item({ id: "prop", state: "proposed" }),
      item({ id: "done", state: "done" }),
      item({ id: "cx", state: "canceled" }),
    ]);
    const needsYou = cols.find((c) => c.label === "Needs you");
    expect(needsYou?.items.map((i) => i.id)).toEqual(["new", "old"]); // review + blocked, recent first
    expect(cols.find((c) => c.label === "Queued")?.items.map((i) => i.id)).toEqual(["prop"]);
    // done + canceled both collapse into Done.
    expect(
      cols
        .find((c) => c.label === "Done")
        ?.items.map((i) => i.id)
        .sort(),
    ).toEqual(["cx", "done"]);
  });

  test("the Needs you column is accent-toned (never red)", () => {
    const cols = toColumns([]);
    expect(cols.find((c) => c.label === "Needs you")?.tone).toBe("accent");
  });
});

describe("triage", () => {
  test("counts attention + active in plain voice, no percentage", () => {
    const t = triage([
      item({ state: "review" }),
      item({ state: "blocked" }),
      item({ state: "running" }),
      item({ state: "done" }),
    ]);
    expect(t.attention).toBe(2);
    expect(t.active).toBe(3); // everything not done/canceled
    expect(t.headline).toBe("2 pieces of work need you");
    expect(t.headline).not.toContain("%");
  });

  test("singular + all-clear phrasing", () => {
    expect(triage([item({ state: "review" })]).headline).toBe("1 piece of work needs you");
    expect(triage([item({ state: "running" })]).headline).toBe("All clear");
  });
});
