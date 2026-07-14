// Pure knowledge-graph helpers (BRO-1893 FID-6 slice 2) — the scope-tree math + the deterministic
// force layout, split out so they are unit-testable with no DOM (like `selectHistory`). The layout is
// SEEDED by a hash of the node id (kgHash), so it is fully deterministic: computed once per scope and
// cached by the component — NOT an ongoing animation loop. Ported from KgGraph.jsx / ConceptKnowledge.jsx.

import { KG_SCOPES, type KgNode, type KgNodeType, type KgScope, type KgScopes } from "./kg-data";

/** Scope (folder) node ink — blue-black, on the system's one cool hue family (hue 265). */
export const KG_GOLD = "var(--kg-scope, oklch(0.38 0.045 265))";

/** Entity kind → its colour + legend label (KgGraph.jsx `KG_TYPE`). All colours are cool-axis tokens. */
export const KG_TYPE: Record<KgNodeType, { color: string; label: string }> = {
  concept: { color: "var(--bv-blue)", label: "concept" },
  pattern: { color: "var(--bv-glow-indigo)", label: "pattern" },
  primitive: { color: "var(--bv-blue-accent)", label: "primitive" },
  tool: { color: "var(--bv-tool, oklch(0.60 0.09 245))", label: "tool" },
  person: { color: "var(--bv-gray-600)", label: "person" },
  paper: { color: "var(--bv-paper, oklch(0.70 0.06 260))", label: "paper" },
  decision: { color: "var(--bv-decision, oklch(0.50 0.14 260))", label: "decision" },
  doc: { color: "var(--bv-gray-500)", label: "doc" },
  session: { color: "var(--bv-info)", label: "session" },
  vault: { color: KG_GOLD, label: "meta-vault" },
  workspace: { color: KG_GOLD, label: "workspace" },
  initiative: { color: KG_GOLD, label: "initiative" },
  project: { color: KG_GOLD, label: "project" },
  task: { color: KG_GOLD, label: "task" },
  routine: { color: KG_GOLD, label: "routine" },
};

/** A folder node (one you can descend into). */
export const kgIsScope = (n: KgNode): boolean => !!n.scopeRef;
/** The filter category of a node: "folder" for scopes, else its type. */
export const kgCategory = (n: KgNode): string => (kgIsScope(n) ? "folder" : n.type);
/** A node's display colour — gold for folders, else its type colour. */
export const kgTypeColor = (n: KgNode): string =>
  kgIsScope(n) ? KG_GOLD : (KG_TYPE[n.type] ?? KG_TYPE.concept).color;

/** The Nous total (0–9) from a score triple, or null when unscored. */
export function kgScore(arr: KgNode["score"]): number | null {
  return arr ? arr[0] + arr[1] + arr[2] : null;
}

/** The scope path root→scope (the breadcrumb chain), following `parent`. */
export function kgPath(id: string, scopes: KgScopes = KG_SCOPES): KgScope[] {
  const out: KgScope[] = [];
  let s: KgScope | undefined = scopes[id];
  const seen = new Set<string>(); // guard a cyclic parent
  while (s && !seen.has(s.id)) {
    out.unshift(s);
    seen.add(s.id);
    s = s.parent ? scopes[s.parent] : undefined;
  }
  return out;
}

/** A flat index of every (scope, node) pair across the whole graph. */
export function kgFlatIndex(
  scopes: KgScopes = KG_SCOPES,
): { scopeId: string; scope: KgScope; node: KgNode }[] {
  const out: { scopeId: string; scope: KgScope; node: KgNode }[] = [];
  for (const sc of Object.values(scopes))
    for (const n of sc.nodes) out.push({ scopeId: sc.id, scope: sc, node: n });
  return out;
}

export interface KgEdge {
  s: string;
  t: string;
}

/** The undirected edge set — one edge per unordered `related:` pair that resolves within the scope. */
export function kgEdges(nodes: KgNode[]): KgEdge[] {
  const ids = new Set(nodes.map((n) => n.id));
  const seen = new Set<string>();
  const out: KgEdge[] = [];
  for (const n of nodes) {
    for (const r of n.related ?? []) {
      if (!ids.has(r) || n.id === r) continue;
      const key = [n.id, r].sort().join("|");
      if (seen.has(key)) continue;
      seen.add(key);
      out.push({ s: n.id, t: r });
    }
  }
  return out;
}

/** Node degree (edge count) per id — drives the node radius. */
export function kgDegree(nodes: KgNode[], edges: KgEdge[]): Record<string, number> {
  const deg: Record<string, number> = {};
  for (const n of nodes) deg[n.id] = 0;
  for (const e of edges) {
    if (e.s in deg) deg[e.s] = (deg[e.s] ?? 0) + 1;
    if (e.t in deg) deg[e.t] = (deg[e.t] ?? 0) + 1;
  }
  return deg;
}

/** Deterministic 0–1 hash of a string (FNV-1a) — the layout seed (no Math.random → stable + testable). */
export function kgHash(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return (h >>> 0) / 4294967295;
}

export interface KgPoint {
  x: number;
  y: number;
  vx: number;
  vy: number;
}

/**
 * The deterministic force-directed layout — 340 fixed relaxation steps (repulsion + edge springs +
 * a gentle centre pull), seeded by kgHash. Runs ONCE per scope (the component caches the result), so
 * there is no ongoing simulation loop. Returns final positions keyed by node id.
 */
export function kgLayout(
  nodes: KgNode[],
  edges: KgEdge[],
  W: number,
  H: number,
): Record<string, KgPoint> {
  const pos: Record<string, KgPoint> = {};
  for (const n of nodes) {
    const a = kgHash(n.id) * Math.PI * 2;
    const r = (kgIsScope(n) ? 20 : 55) + kgHash(`${n.id}r`) * Math.min(W, H) * 0.28;
    pos[n.id] = { x: W / 2 + Math.cos(a) * r, y: H / 2 + Math.sin(a) * r, vx: 0, vy: 0 };
  }
  const cx = W / 2;
  const cy = H / 2;
  const ideal = 116;
  for (let it = 0; it < 340; it++) {
    const alpha = 1 - it / 340;
    for (let i = 0; i < nodes.length; i++) {
      for (let j = i + 1; j < nodes.length; j++) {
        const ni = nodes[i];
        const nj = nodes[j];
        if (!ni || !nj) continue;
        const a = pos[ni.id];
        const b = pos[nj.id];
        if (!a || !b) continue;
        const dx = a.x - b.x;
        const dy = a.y - b.y;
        const d2 = dx * dx + dy * dy || 0.01;
        const d = Math.sqrt(d2);
        const f = 4600 / d2;
        const ux = dx / d;
        const uy = dy / d;
        a.vx += ux * f;
        a.vy += uy * f;
        b.vx -= ux * f;
        b.vy -= uy * f;
      }
    }
    for (const e of edges) {
      const a = pos[e.s];
      const b = pos[e.t];
      if (!a || !b) continue;
      const dx = b.x - a.x;
      const dy = b.y - a.y;
      const d = Math.sqrt(dx * dx + dy * dy) || 0.01;
      const f = (d - ideal) * 0.022;
      const ux = dx / d;
      const uy = dy / d;
      a.vx += ux * f;
      a.vy += uy * f;
      b.vx -= ux * f;
      b.vy -= uy * f;
    }
    for (const n of nodes) {
      const p = pos[n.id];
      if (!p) continue;
      const pull = kgIsScope(n) ? 0.03 : 0.009;
      p.vx += (cx - p.x) * pull;
      p.vy += (cy - p.y) * pull;
      p.x += p.vx * alpha * 0.85;
      p.y += p.vy * alpha * 0.85;
      p.vx *= 0.82;
      p.vy *= 0.82;
      p.x = Math.max(46, Math.min(W - 46, p.x));
      p.y = Math.max(40, Math.min(H - 36, p.y));
    }
  }
  return pos;
}

/** The render radius of a node — folders larger, scaled gently by degree. */
export function kgNodeR(n: KgNode, deg: number): number {
  return kgIsScope(n) ? 13 + Math.min(deg || 0, 5) : 7 + Math.min(deg || 0, 6) * 1.4;
}

/** The neighbours of a node id within a scope (both edge directions). */
export function kgNeighbors(nodeId: string, edges: KgEdge[]): Set<string> {
  const s = new Set<string>([nodeId]);
  for (const e of edges) {
    if (e.s === nodeId) s.add(e.t);
    if (e.t === nodeId) s.add(e.s);
  }
  return s;
}

/** Entities linked to `node` within a scope (bidirectional) — the inspector's backlinks. */
export function kgBacklinks(node: KgNode, scope: KgScope): KgNode[] {
  // Exclude the node itself, mirroring kgEdges' self-loop guard: a reflexive `related:` link must not
  // count as its own backlink, or the inspector's `related · N` header + the list's Links column
  // over-count and disagree with the graph (which shows zero self-edges).
  return scope.nodes.filter(
    (n) =>
      n.id !== node.id &&
      ((n.related ?? []).includes(node.id) || (node.related ?? []).includes(n.id)),
  );
}
