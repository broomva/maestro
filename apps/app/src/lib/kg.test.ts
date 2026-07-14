/// <reference types="bun" />

// Pure KG-math tests (BRO-1893 FID-6 slice 2) — the scope-tree + deterministic layout, no DOM. Locks:
// edges dedup + resolve within scope, layout is deterministic + in-bounds, backlinks are bidirectional,
// the breadcrumb path follows parent (with a cyclic guard).

import { describe, expect, test } from "bun:test";
import {
  kgBacklinks,
  kgCategory,
  kgDegree,
  kgEdges,
  kgHash,
  kgIsScope,
  kgLayout,
  kgNeighbors,
  kgPath,
  kgScore,
} from "./kg";
import { KG_SCOPES, type KgNode, type KgScope } from "./kg-data";

const node = (p: Partial<KgNode> & Pick<KgNode, "id" | "type">): KgNode => ({
  label: p.label ?? p.id,
  claim: p.claim ?? "",
  ...p,
});

describe("kgEdges — the undirected edge set", () => {
  test("one edge per unordered related pair; a mirror link does not double it", () => {
    const edges = kgEdges([
      node({ id: "a", type: "concept", related: ["b"] }),
      node({ id: "b", type: "concept", related: ["a"] }), // mirrors a→b
    ]);
    expect(edges).toHaveLength(1);
  });

  test("drops edges to ids outside the scope, and self-loops", () => {
    const edges = kgEdges([
      node({ id: "a", type: "concept", related: ["ghost", "a", "b"] }),
      node({ id: "b", type: "concept" }),
    ]);
    expect(edges).toEqual([{ s: "a", t: "b" }]); // ghost (unresolved) + a (self) dropped
  });
});

describe("kgDegree", () => {
  test("counts each incident edge on both endpoints; isolated nodes are 0", () => {
    const nodes = [
      node({ id: "a", type: "concept", related: ["b", "c"] }),
      node({ id: "b", type: "concept" }),
      node({ id: "c", type: "concept" }),
      node({ id: "d", type: "concept" }),
    ];
    const deg = kgDegree(nodes, kgEdges(nodes));
    expect(deg.a).toBe(2);
    expect(deg.b).toBe(1);
    expect(deg.d).toBe(0);
  });
});

describe("kgLayout — deterministic force layout", () => {
  const nodes = KG_SCOPES.broomva?.nodes ?? [];
  const edges = kgEdges(nodes);

  test("same input → identical output (seeded by id hash, no randomness)", () => {
    const a = kgLayout(nodes, edges, 800, 600);
    const b = kgLayout(nodes, edges, 800, 600);
    expect(a).toEqual(b);
  });

  test("every node gets a position, clamped inside the canvas bounds", () => {
    const pos = kgLayout(nodes, edges, 800, 600);
    for (const n of nodes) {
      const p = pos[n.id];
      expect(p).toBeDefined();
      expect(p?.x).toBeGreaterThanOrEqual(46);
      expect(p?.x).toBeLessThanOrEqual(800 - 46);
      expect(p?.y).toBeGreaterThanOrEqual(40);
      expect(p?.y).toBeLessThanOrEqual(600 - 36);
      expect(Number.isFinite(p?.x)).toBe(true);
      expect(Number.isFinite(p?.y)).toBe(true);
    }
  });

  test("kgHash is stable + in [0,1)", () => {
    expect(kgHash("drun")).toBe(kgHash("drun"));
    expect(kgHash("drun")).toBeGreaterThanOrEqual(0);
    expect(kgHash("drun")).toBeLessThan(1);
    expect(kgHash("a")).not.toBe(kgHash("b"));
  });
});

describe("kgPath — the breadcrumb chain", () => {
  test("root → scope, following parent", () => {
    expect(kgPath("hawthorne-core").map((s) => s.id)).toEqual([
      "broomva",
      "hawthorne",
      "hawthorne-core",
    ]);
    expect(kgPath("broomva").map((s) => s.id)).toEqual(["broomva"]);
  });

  test("a cyclic parent chain terminates (never loops forever)", () => {
    const cyclic: Record<string, KgScope> = {
      a: { id: "a", crumb: "a", kind: "vault", desc: "", parent: "b", nodes: [] },
      b: { id: "b", crumb: "b", kind: "vault", desc: "", parent: "a", nodes: [] },
    };
    expect(kgPath("a", cyclic).length).toBeLessThanOrEqual(2);
  });
});

describe("kgScore / kgCategory / kgIsScope", () => {
  test("kgScore sums the triple, null when unscored", () => {
    expect(kgScore([3, 3, 3])).toBe(9);
    expect(kgScore([2, 3, 3])).toBe(8);
    expect(kgScore(undefined)).toBeNull();
  });

  test("a node with scopeRef is a folder category", () => {
    const folder = node({ id: "f", type: "project", scopeRef: "child" });
    const leaf = node({ id: "l", type: "decision" });
    expect(kgIsScope(folder)).toBe(true);
    expect(kgCategory(folder)).toBe("folder");
    expect(kgCategory(leaf)).toBe("decision");
  });
});

describe("kgNeighbors / kgBacklinks — bidirectional", () => {
  const scope = KG_SCOPES["hawthorne-core"] as KgScope;
  const edges = kgEdges(scope.nodes);

  test("neighbours include the node itself + both edge directions", () => {
    const nb = kgNeighbors("drun", edges);
    expect(nb.has("drun")).toBe(true);
    expect(nb.has("spec3")).toBe(true); // drun.related includes spec3
    expect(nb.has("run7c")).toBe(true);
  });

  test("backlinks resolve a link declared on EITHER side", () => {
    const spec = scope.nodes.find((n) => n.id === "spec3") as KgNode;
    const ids = kgBacklinks(spec, scope).map((n) => n.id);
    // spec3.related lists drun/notes/run7c; drun also lists spec3 → still one entry each (no dupes here).
    expect(ids).toContain("drun");
    expect(ids).toContain("run7c");
  });
});
