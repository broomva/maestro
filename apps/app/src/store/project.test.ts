/// <reference types="bun" />

// Selector tests for the sidebar workspace tree + top-bar narration (BRO-1884). Vocabulary-level,
// per porting-notes §Tests — the tree/narration are pure projections over server truth, so a fixture
// of live nodes is the whole harness. Locks: container folders (initiative/project) never inflate
// done/total; project order is live→attention→name; narration is derived, attention-first.

import { describe, expect, test } from "bun:test";
import type { LiveNode } from "@maestro/protocol";
import { selectNarration, selectNeedsYouCount, selectSidebarTree } from "./project";
import { emptyServerTruth, type ServerTruth } from "./types";

let seq = 0;
/** A live node with sane defaults; override the fields a case cares about. */
function node(p: Partial<LiveNode> & Pick<LiveNode, "kind" | "state">): LiveNode {
  seq += 1;
  const id = p.id ?? `n${seq}`;
  return {
    id,
    path: p.path ?? id,
    parentId: p.parentId ?? null,
    kind: p.kind,
    state: p.state,
    owner: p.owner ?? null,
    gate: p.gate ?? "human",
    budgetJson: p.budgetJson ?? null,
    doneJson: p.doneJson ?? null,
    title: p.title ?? null,
    createdAt: p.createdAt ?? 1,
    updatedAt: p.updatedAt ?? 1,
  };
}

/** Seed a ServerTruth from a node list (keyed by id). */
function server(nodes: LiveNode[]): ServerTruth {
  const s = emptyServerTruth();
  for (const n of nodes) s.nodes[n.id] = n;
  return s;
}

/** The prototype's workspace shape: hawthorne (3 projects) + genesis + ops, task-level states. */
function workspace(): ServerTruth {
  return server([
    node({ id: "i-haw", kind: "initiative", state: "running", title: "hawthorne" }),
    node({
      id: "p-core",
      parentId: "i-haw",
      kind: "project",
      state: "running",
      title: "hawthorne-core",
    }),
    node({
      id: "w1",
      parentId: "p-core",
      kind: "task",
      state: "review",
      title: "Persist run transcripts",
    }),
    node({
      id: "w5",
      parentId: "p-core",
      kind: "task",
      state: "proposed",
      title: "Resume sessions",
    }),
    // Container folders carry an aggregate "running" state; the actionable state lives on the leaf
    // task (w2 blocked) — selectNeedsYouCount counts every node, so keep folders out of review/blocked.
    node({
      id: "p-db",
      parentId: "i-haw",
      kind: "project",
      state: "running",
      title: "hawthorne-db",
    }),
    node({
      id: "w2",
      parentId: "p-db",
      kind: "task",
      state: "blocked",
      title: "Import Linear cycles",
    }),
    node({
      id: "p-eng",
      parentId: "i-haw",
      kind: "project",
      state: "running",
      title: "hawthorne-engine",
    }),
    node({ id: "w6", parentId: "p-eng", kind: "task", state: "proposed", title: "Handoff: relay" }),
    node({ id: "w8", parentId: "p-eng", kind: "task", state: "done", title: "Close the loop" }),
    node({ id: "i-gen", kind: "initiative", state: "running", title: "genesis" }),
    node({
      id: "p-proj",
      parentId: "i-gen",
      kind: "project",
      state: "running",
      title: "@genesis/projection",
    }),
    node({
      id: "w3",
      parentId: "p-proj",
      kind: "task",
      state: "running",
      title: "Reduce the NDJSON stream",
    }),
    node({ id: "i-ops", kind: "initiative", state: "running", title: "ops" }),
    node({
      id: "p-book",
      parentId: "i-ops",
      kind: "project",
      state: "running",
      title: "bookkeeping",
    }),
    node({
      id: "w4",
      parentId: "p-book",
      kind: "task",
      state: "running",
      title: "Reconcile May invoices",
    }),
  ]);
}

describe("selectSidebarTree", () => {
  test("groups initiatives → projects with done/total counting only leaf items", () => {
    const tree = selectSidebarTree(workspace());
    expect(tree.placesCount).toBe(3);
    expect(tree.initiatives.map((i) => i.name)).toEqual(["hawthorne", "genesis", "ops"]);

    const haw = tree.initiatives.find((i) => i.name === "hawthorne");
    // 5 leaf tasks (w1,w5,w2,w6,w8); the 3 project folders + the initiative are NOT counted.
    expect(haw?.total).toBe(5);
    expect(haw?.done).toBe(1); // only w8
    expect(haw?.projects.map((p) => p.name)).toEqual([
      "hawthorne-core", // attn 1 (w1 review)
      "hawthorne-db", // attn 1 (w2 blocked)
      "hawthorne-engine", // attn 0 — quiet, still shown
    ]);
    expect(haw?.projects.find((p) => p.name === "hawthorne-core")?.attn).toBe(1);
    expect(haw?.projects.find((p) => p.name === "hawthorne-core")?.live).toBe(false);
  });

  test("a project with a running task renders live (tidepool)", () => {
    const tree = selectSidebarTree(workspace());
    const gen = tree.initiatives.find((i) => i.name === "genesis");
    expect(gen?.projects).toEqual([{ name: "@genesis/projection", live: true, attn: 0 }]);
  });

  test("projects order live → attention → name", () => {
    const tree = selectSidebarTree(
      server([
        node({ id: "i", kind: "initiative", state: "running", title: "acme" }),
        node({ id: "pq", parentId: "i", kind: "project", state: "proposed", title: "quiet" }),
        node({ id: "tq", parentId: "pq", kind: "task", state: "proposed", title: "t" }),
        node({ id: "pa", parentId: "i", kind: "project", state: "blocked", title: "attn" }),
        node({ id: "ta", parentId: "pa", kind: "task", state: "blocked", title: "t" }),
        node({ id: "pl", parentId: "i", kind: "project", state: "running", title: "live" }),
        node({ id: "tl", parentId: "pl", kind: "task", state: "running", title: "t" }),
      ]),
    );
    expect(tree.initiatives[0]?.projects.map((p) => p.name)).toEqual(["live", "attn", "quiet"]);
  });

  test("empty workspace → no places", () => {
    const tree = selectSidebarTree(emptyServerTruth());
    expect(tree).toEqual({ initiatives: [], looseProjects: [], placesCount: 0 });
  });
});

describe("selectNeedsYouCount + selectNarration", () => {
  test("needs-you counts review + blocked", () => {
    expect(selectNeedsYouCount(workspace())).toBe(2); // w1 review + w2 blocked
  });

  test("narration is attention-first and derived (no fabricated timestamp)", () => {
    // review present but no session → count form, not a fake run receipt.
    expect(selectNarration(workspace())).toBe("1 at your gate");
    // running-only → running summary.
    const running = server([
      node({ id: "i", kind: "initiative", state: "running", title: "a" }),
      node({ id: "p", parentId: "i", kind: "project", state: "running", title: "p" }),
      node({ id: "t", parentId: "p", kind: "task", state: "running", title: "t" }),
    ]);
    expect(selectNarration(running)).toBe("1 running");
    // nothing → the calm resting line.
    expect(selectNarration(emptyServerTruth())).toBe("standing · nothing at your gate");
  });
});
