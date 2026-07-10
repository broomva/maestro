// The projector (BRO-1775) — the ONE place a raw `node` row becomes a `WorkItem`
// (contract §3 "WorkItem derivation stays the projector's"). Pure functions over
// the server-truth slice; every read surface (board, feed, inspector) derives from
// these, never re-implementing the join.
//
// The chain (contract §2): `_work.md` frontmatter → `node` row (server truth here)
// → `WorkItem` (this projection). The projection joins the node to its
// current-or-most-recent `session` (worker/run/sessionId), its OPEN `gate`
// (gateId, review-only), and the `parentId` ancestry (initiative/project).
//
// Deliberately LEFT to their owners (not manufactured here from absent data):
//   - `worker` — the `session` row carries no name/where column; a `run.started`
//     payload will (BRO-1790). Undefined until then.
//   - `verdict` / `reason` — need event payloads not pinned in P1. Undefined.
//   - `look` — the gate compression is composed by the gate queue (BRO-1789).
//   - `created` — optional + unconsumed today (contract §7 "unconsumed → optional").

import type { LiveNode, WorkItem } from "@maestro/protocol";
import { WK_GROUP_ORDER } from "@maestro/protocol";
import type { ServerTruth } from "./types";

/** Format epoch-ms to ISO, tolerating a corrupt/out-of-range clock (sentinel, never a throw). */
function toIso(ms: number): string {
  const d = new Date(ms);
  return Number.isNaN(d.getTime()) ? new Date(0).toISOString() : d.toISOString();
}

/** The card title: node heading, else the last path segment, else "Untitled" (never empty). */
function titleOf(node: LiveNode): string {
  if (node.title && node.title.trim() !== "") return node.title;
  const seg = node.path.split("/").filter(Boolean).pop();
  return seg && seg.trim() !== "" ? seg : "Untitled";
}

/** Walk the `parentId` chain, returning the first ancestor title of `kind`, if any. */
function ancestorTitle(
  node: LiveNode,
  nodes: Record<string, LiveNode>,
  kind: LiveNode["kind"],
): string | undefined {
  const seen = new Set<string>([node.id]); // guard a cyclic parentId
  let cur = node.parentId ? nodes[node.parentId] : undefined;
  while (cur && !seen.has(cur.id)) {
    if (cur.kind === kind) return titleOf(cur);
    seen.add(cur.id);
    cur = cur.parentId ? nodes[cur.parentId] : undefined;
  }
  return undefined;
}

/**
 * Derive the `WorkItem` for one live node against the server-truth slice. The node
 * MUST be live (the caller filters tombstones — the store only holds live rows).
 */
export function deriveWorkItem(node: LiveNode, s: ServerTruth): WorkItem {
  // The node's sessions, most-recent first (node ↔ session is 1:many; project the latest).
  const nodeSessions = Object.values(s.sessions)
    .filter((sess) => sess.nodeId === node.id)
    .sort((a, b) => b.startedAt - a.startedAt);
  const latest = nodeSessions[0];
  const sessionId = latest?.id;

  // The open gate (verdict === null) joined through the node's sessions — surfaced
  // ONLY at `state === "review"` (contract §3: blocked has no gate row; the verb key).
  let gateId: string | undefined;
  if (node.state === "review" && nodeSessions.length > 0) {
    const sessionIds = new Set(nodeSessions.map((sess) => sess.id));
    const openGate = Object.values(s.gates).find(
      (g) => sessionIds.has(g.sessionId) && g.verdict === null,
    );
    gateId = openGate?.id;
  }

  const item: WorkItem = {
    // server truth (mirrors the node row)
    id: node.id,
    state: node.state,
    kind: node.kind,
    title: titleOf(node),
    gate: node.gate,
    path: node.path,
    parentId: node.parentId,
    updatedAt: toIso(node.updatedAt),
    // derived
    ...(node.owner ? { owner: node.owner } : {}),
    ...(sessionId ? { sessionId } : {}),
    ...(gateId ? { gateId } : {}),
    ...(latest?.branch ? { run: latest.branch as WorkItem["run"] } : {}),
    // card age: the session's last event ts REFINES the always-present updatedAt.
    ...(sessionId && s.lastEventAt[sessionId] ? { lastEventAt: s.lastEventAt[sessionId] } : {}),
  };

  const initiative = ancestorTitle(node, s.nodes, "initiative");
  if (initiative) item.initiative = initiative;
  const project = ancestorTitle(node, s.nodes, "project");
  if (project) item.project = project;

  return item;
}

/** All live work items, path-sorted (parent before child — mirrors `/api/tree`). */
export function selectWorkItems(s: ServerTruth): WorkItem[] {
  return Object.values(s.nodes)
    .sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0))
    .map((n) => deriveWorkItem(n, s));
}

/** One work item by node id (the inspector's focus), or undefined if unknown. */
export function selectWorkItem(s: ServerTruth, id: string): WorkItem | undefined {
  const node = s.nodes[id];
  return node ? deriveWorkItem(node, s) : undefined;
}

/** One board column — a state + its work items (within-group recency order). */
export interface BoardGroup {
  state: WorkItem["state"];
  items: WorkItem[];
}

/**
 * The board — work items grouped by state in the shared attention order
 * (`WK_GROUP_ORDER`, review first — imported, never redefined; contract §8). Only
 * non-empty groups; within a group the recency default (`updatedAt` desc), the same
 * default the read API's `/api/board` uses (the authoritative attention tiebreak is
 * BRO-1789's, layered on top — not forked here).
 */
export function selectBoard(s: ServerTruth): BoardGroup[] {
  const items = selectWorkItems(s);
  const byState = new Map<WorkItem["state"], WorkItem[]>();
  for (const it of items) {
    const list = byState.get(it.state);
    if (list) list.push(it);
    else byState.set(it.state, [it]);
  }
  const groups: BoardGroup[] = [];
  for (const state of WK_GROUP_ORDER) {
    const group = byState.get(state);
    if (!group || group.length === 0) continue;
    group.sort((a, b) => (a.updatedAt < b.updatedAt ? 1 : a.updatedAt > b.updatedAt ? -1 : 0));
    groups.push({ state, items: group });
  }
  return groups;
}

/** "Needs you" headline — the count of work at a gate or stuck (contract §"Reactive queries"). */
export function selectNeedsYouCount(s: ServerTruth): number {
  let n = 0;
  for (const node of Object.values(s.nodes)) {
    if (node.state === "review" || node.state === "blocked") n++;
  }
  return n;
}
