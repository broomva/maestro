/// <reference types="bun" />

// Selector tests for the sidebar workspace tree + top-bar narration (BRO-1884). Vocabulary-level,
// per porting-notes §Tests — the tree/narration are pure projections over server truth, so a fixture
// of live nodes is the whole harness. Locks: container folders (initiative/project) never inflate
// done/total; project order is live→attention→name; narration is derived, attention-first.

import { describe, expect, test } from "bun:test";
import type { LiveNode, LiveSession } from "@maestro/protocol";
import {
  selectGateQueue,
  selectNarration,
  selectNeedsYouCount,
  selectSidebarTree,
} from "./project";
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

/** A live session with sane defaults (the node ↔ session join; carries the branch receipt). */
function session(p: Partial<LiveSession> & Pick<LiveSession, "id" | "nodeId">): LiveSession {
  return {
    id: p.id,
    nodeId: p.nodeId,
    branch: p.branch ?? `run/${p.id}`,
    status: p.status ?? "review",
    startedAt: p.startedAt ?? 1,
    endedAt: p.endedAt ?? null,
    diffstatJson: p.diffstatJson ?? null,
    updatedAt: p.updatedAt ?? 1,
  };
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
    // task (w2 blocked). (selectNeedsYouCount is leaf-only, so a folder's state never counts — the
    // dedicated regression test below locks that; here the folders stay "running" to drive the tree's
    // live dots.)
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

  test("a leaf with neither ancestor is not a folder place (docstring contract)", () => {
    // A top-level task with no project/initiative ancestor has no home in the FOLDER tree, so it is
    // not surfaced here (it renders in the board/feed instead). placesCount counts folders, not it.
    const tree = selectSidebarTree(
      server([node({ id: "t", kind: "task", state: "running", title: "loose" })]),
    );
    expect(tree).toEqual({ initiatives: [], looseProjects: [], placesCount: 0 });
  });

  test("a project with no initiative ancestor surfaces as a loose root folder", () => {
    // The looseProjects path: a leaf with a project ancestor but no initiative → a root folder.
    const tree = selectSidebarTree(
      server([
        node({ id: "p", kind: "project", state: "blocked", title: "standalone" }),
        node({ id: "t", parentId: "p", kind: "task", state: "blocked", title: "wire it" }),
      ]),
    );
    expect(tree.initiatives).toEqual([]);
    expect(tree.looseProjects).toEqual([{ name: "standalone", live: false, attn: 1 }]);
    expect(tree.placesCount).toBe(1);
  });
});

describe("selectNeedsYouCount + selectNarration", () => {
  test("needs-you counts review + blocked", () => {
    expect(selectNeedsYouCount(workspace())).toBe(2); // w1 review + w2 blocked
  });

  test("needs-you is leaf-only — a container folder in review/blocked never inflates the badge", () => {
    // Regression (BRO-1884 P20 major): a container folder can carry an aggregate review/blocked
    // state, but the badge, tree attn, and narration must AGREE — all three are leaf-only. If the
    // count included the folder, the badge would read a number the tree/narration cannot explain.
    const s = server([
      node({ id: "p", kind: "project", state: "blocked", title: "api" }),
      node({ id: "t", parentId: "p", kind: "task", state: "blocked", title: "wire it" }),
    ]);
    expect(selectNeedsYouCount(s)).toBe(1); // only the leaf task, not the container folder
    expect(selectNarration(s)).toBe("1 stuck"); // narration agrees — one blocked leaf
    // starker: a lone container in review with no leaf must not fabricate a gate.
    const lone = server([node({ id: "i", kind: "initiative", state: "review", title: "solo" })]);
    expect(selectNeedsYouCount(lone)).toBe(0);
    expect(selectNarration(lone)).toBe("standing · nothing at your gate");
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

  test("narration leads with the branch receipt when the review item has a run", () => {
    // The receipt-leading branch (`${run} at your gate`) — the review node carries a session with a
    // branch, so the narration shows the run receipt, not the bare count.
    const one = server([
      node({ id: "p", kind: "project", state: "running", title: "core" }),
      node({
        id: "w",
        parentId: "p",
        kind: "task",
        state: "review",
        title: "ship it",
        updatedAt: 20,
      }),
    ]);
    one.sessions.s1 = session({ id: "s1", nodeId: "w", branch: "run/ab12" });
    expect(selectNarration(one)).toBe("run/ab12 at your gate");

    // two review items, most-recent carries the branch → "<branch> + N more at your gate".
    const two = server([
      node({ id: "w1", kind: "task", state: "review", title: "newer", updatedAt: 30 }),
      node({ id: "w2", kind: "task", state: "review", title: "older", updatedAt: 10 }),
    ]);
    two.sessions.s1 = session({ id: "s1", nodeId: "w1", branch: "run/cd34" });
    expect(selectNarration(two)).toBe("run/cd34 + 1 more at your gate");
  });
});

describe("selectGateQueue — the rung-2 gate queue (BRO-1888)", () => {
  test("keeps only LEAF review + blocked; drops containers and non-attention states", () => {
    const s = server([
      node({ id: "rev", kind: "task", state: "review", title: "gate me" }),
      node({ id: "blk", kind: "task", state: "blocked", title: "stuck" }),
      node({ id: "run", kind: "task", state: "running", title: "live" }),
      node({ id: "done", kind: "task", state: "done", title: "shipped" }),
      node({ id: "prop", kind: "task", state: "proposed", title: "queued" }),
      // A container folder in review must NEVER surface — the queue is actionable leaves (selectPlaneItems).
      node({ id: "proj", kind: "project", state: "review", title: "a project folder" }),
      node({ id: "init", kind: "initiative", state: "blocked", title: "an initiative folder" }),
    ]);
    expect(selectGateQueue(s).map((i) => i.id)).toEqual(["rev", "blk"]);
  });

  test("review sorts before blocked; within a state the OLDEST-waiting is first (no gate rots at the bottom)", () => {
    const s = server([
      node({ id: "rev-new", kind: "task", state: "review", title: "newer gate", updatedAt: 300 }),
      node({ id: "rev-old", kind: "task", state: "review", title: "older gate", updatedAt: 100 }),
      node({ id: "blk", kind: "task", state: "blocked", title: "stuck", updatedAt: 50 }),
    ]);
    // review (attention-first) before blocked; older review before newer review (ascending attention age).
    expect(selectGateQueue(s).map((i) => i.id)).toEqual(["rev-old", "rev-new", "blk"]);
  });

  test("empty when nothing needs a human", () => {
    const s = server([
      node({ id: "run", kind: "task", state: "running", title: "live" }),
      node({ id: "done", kind: "task", state: "done", title: "shipped" }),
    ]);
    expect(selectGateQueue(s)).toEqual([]);
    expect(selectGateQueue(emptyServerTruth())).toEqual([]);
  });
});
