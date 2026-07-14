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
import { compareGateQueue, isInGateQueue, WK_GROUP_ORDER } from "@maestro/protocol";
import type { ServerTruth } from "./types";

/** Format epoch-ms to ISO, tolerating a corrupt/out-of-range clock (sentinel, never a throw). */
function toIso(ms: number): string {
  const d = new Date(ms);
  return Number.isNaN(d.getTime()) ? new Date(0).toISOString() : d.toISOString();
}

/** The card title: node heading, else the last path segment, else "Untitled" (never empty, always trimmed). */
function titleOf(node: LiveNode): string {
  const heading = node.title?.trim();
  if (heading) return heading;
  const seg = node.path.split("/").filter(Boolean).pop()?.trim();
  return seg || "Untitled";
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
  // The node's sessions, most-recent first (node ↔ session is 1:many; project the
  // latest). A deterministic id secondary key breaks an exact `startedAt` tie so
  // the choice never depends on `Object.values` iteration order (which is numeric
  // for all-digit keys), keeping sessionId/run/lastEventAt stable.
  const nodeSessions = Object.values(s.sessions)
    .filter((sess) => sess.nodeId === node.id)
    .sort((a, b) => b.startedAt - a.startedAt || (a.id < b.id ? 1 : a.id > b.id ? -1 : 0));
  const latest = nodeSessions[0];
  const sessionId = latest?.id;

  // The open gate (verdict === null) joined through the node's sessions — surfaced
  // ONLY at `state === "review"` (contract §3: blocked has no gate row; the verb key).
  // Pick the MOST-RECENTLY-opened open gate: the contract pins 1:1 at the gate, but
  // a stale never-decided gate on a prior session must not be chosen as the verb key.
  let gateId: string | undefined;
  if (node.state === "review" && nodeSessions.length > 0) {
    const sessionIds = new Set(nodeSessions.map((sess) => sess.id));
    const openGate = Object.values(s.gates)
      .filter((g) => sessionIds.has(g.sessionId) && g.verdict === null)
      .sort((a, b) => b.openedAt - a.openedAt)[0];
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

/**
 * The mission plane's work items — LEAF WorkItems only (BRO-1886). The plane surfaces the
 * actionable work a human acts on; the container folders (initiative/project) are the sidebar
 * tree's job (the disclosure ladder — the plane is rung 1, the tree is the workspace structure).
 * Flat + path-sorted (stable, mirrors `/api/tree`); the plane component does the feed grouping /
 * board columns client-side, exactly like the prototype (WorkPlanes.jsx filters a flat list).
 */
export function selectPlaneItems(s: ServerTruth): WorkItem[] {
  return selectWorkItems(s).filter((i) => i.kind !== "initiative" && i.kind !== "project");
}

/** Epoch ms a card entered its attention state — the gate's `openedAt` / block ts (gate.ts
 *  `GateQueueOrder.attentionSince`). No such field is projected, so `lastEventAt ?? updatedAt` is the
 *  proxy: for a `review` node the gate-open event is its last event; a corrupt ts sorts as 0 (oldest). */
function attentionSince(i: WorkItem): number {
  const t = Date.parse(i.lastEventAt ?? i.updatedAt);
  return Number.isNaN(t) ? 0 : t;
}

/**
 * The gate queue (BRO-1888 FID-3) — the LEAF WorkItems that need a human (`review` + `blocked` =
 * `ATTENTION_STATES`, via gate.ts's `isInGateQueue` REFERENCED not re-declared), oldest-waiting first
 * (`compareGateQueue`: review before blocked, then ascending attention age so no gate rots at the
 * bottom). It is a DERIVED VIEW over the same server-truth leaves the plane shows — never a separate
 * store (gate.ts §Membership). A `review` card carries a `gateId` + `look` (work-item.ts), so the verbs
 * dispatch off `gateId`; a `blocked` card is Stuck-and-redispatchable (no gate row), keyed on the node.
 */
export function selectGateQueue(s: ServerTruth): WorkItem[] {
  return selectPlaneItems(s)
    .filter((i) => isInGateQueue(i.state))
    .sort((a, b) =>
      compareGateQueue(
        { state: a.state, attentionSince: attentionSince(a) },
        { state: b.state, attentionSince: attentionSince(b) },
      ),
    );
}

/** One project folder in the sidebar workspace tree. */
export interface SidebarProject {
  /** project title (the `project` ancestry field). */
  name: string;
  /** any item in the project is `running` (renders a live tidepool dot). */
  live: boolean;
  /** count of items at a gate or stuck (`review` + `blocked`) — the attention badge. */
  attn: number;
}

/** One initiative folder in the sidebar workspace tree — its projects + progress. */
export interface SidebarInitiative {
  /** initiative title (the `initiative` ancestry field). */
  name: string;
  /** count of `done` items under it (the `done/total` progress, never a percentage). */
  done: number;
  /** total items under it. */
  total: number;
  /** its project folders, ordered live-first then attention then name. */
  projects: SidebarProject[];
}

/** The sidebar workspace tree — initiatives (each with projects) + loose projects. */
export interface SidebarTree {
  initiatives: SidebarInitiative[];
  /** projects whose items carry no initiative ancestor (shown at the tree root). */
  looseProjects: SidebarProject[];
  /** top-level folder count for the root "N places" row. */
  placesCount: number;
}

// Folders are keyed by their display TITLE (the ancestry field), not node id — the tree presents
// and groups by title, mirroring the prototype. Two sibling folders with the same explicit title
// therefore merge into one row (their live/attn, and the initiative's done/total, combine). In
// practice `titleOf` falls back to the unique path segment, so a collision needs a hand-authored
// duplicate `title:`; if that ever matters, carry the ancestor id on the WorkItem and key on it.
/** Fold a work item into a project-folder accumulator (live OR-in, attention count-up). */
function foldProject(acc: Map<string, SidebarProject>, name: string, item: WorkItem): void {
  const p = acc.get(name) ?? { name, live: false, attn: 0 };
  if (item.state === "running") p.live = true;
  if (item.state === "review" || item.state === "blocked") p.attn += 1;
  acc.set(name, p);
}

/** Order project folders: live first, then by attention (desc), then name (stable, locale-free). */
function orderProjects(projects: SidebarProject[]): SidebarProject[] {
  return projects.sort(
    (a, b) =>
      Number(b.live) - Number(a.live) ||
      b.attn - a.attn ||
      (a.name < b.name ? -1 : a.name > b.name ? 1 : 0),
  );
}

/**
 * The sidebar workspace tree (BRO-1884) — the nav IS the workspace, derived from real
 * WorkItems (the McSidebar/MccTcSidebar logic, ported from seed to store): initiatives group
 * their projects; each project carries a live dot + an attention count; each initiative carries
 * `done/total` (receipts, never a percentage — CLAUDE.md §Work states). Items with a project but
 * no initiative ancestor surface as loose root folders. A leaf with NEITHER ancestor is not a
 * folder "place" — it has no home in the folder tree, so it is not surfaced here (it still appears
 * in the board/feed, which render leaf WorkItems directly). Insertion order follows
 * `selectWorkItems` (path-sorted → stable).
 */
export function selectSidebarTree(s: ServerTruth): SidebarTree {
  const items = selectWorkItems(s);
  // Per-initiative accumulators (Maps preserve first-seen order → stable tree order).
  const initProjects = new Map<string, Map<string, SidebarProject>>();
  const initTotals = new Map<string, { done: number; total: number }>();
  const loose = new Map<string, SidebarProject>();

  for (const item of items) {
    // Skip the container folders themselves (initiative / project nodes) — the tree structure comes
    // from the LEAF items' ancestry titles, and counting the folders would double-count done/total.
    if (item.kind === "initiative" || item.kind === "project") continue;
    if (item.initiative) {
      const projs = initProjects.get(item.initiative) ?? new Map<string, SidebarProject>();
      if (item.project) foldProject(projs, item.project, item);
      initProjects.set(item.initiative, projs);
      const tot = initTotals.get(item.initiative) ?? { done: 0, total: 0 };
      tot.total += 1;
      if (item.state === "done") tot.done += 1;
      initTotals.set(item.initiative, tot);
    } else if (item.project) {
      foldProject(loose, item.project, item);
    }
  }

  const initiatives: SidebarInitiative[] = [...initProjects].map(([name, projs]) => {
    const tot = initTotals.get(name) ?? { done: 0, total: 0 };
    return { name, done: tot.done, total: tot.total, projects: orderProjects([...projs.values()]) };
  });
  const looseProjects = orderProjects([...loose.values()]);

  return { initiatives, looseProjects, placesCount: initiatives.length + looseProjects.length };
}

/**
 * "Needs you" headline — the count of LEAF work at a gate or stuck (contract §"Reactive queries").
 * Leaf-only, matching the container-exclusion rule of `selectSidebarTree.attn` (skips containers,
 * line ~218) and `selectNarration` (line ~259): a container folder (initiative/project) can carry
 * an aggregate `review`/`blocked` state, and counting it here would inflate the badge above the
 * visible tree + narration — a number the tree cannot explain. All three chrome selectors agree on
 * this rule, so the count never over-reports because of a folder's own state.
 *
 * (One edge remains, tracked as a fast-follow: a leaf homed DIRECTLY under an initiative with no
 * project ancestor is counted here but its attention surfaces in the tree only at project
 * granularity — the tree has no initiative-level attention badge yet. Rare shape; the badge stays
 * the authoritative top-level count.)
 */
export function selectNeedsYouCount(s: ServerTruth): number {
  let n = 0;
  for (const node of Object.values(s.nodes)) {
    if (node.kind === "initiative" || node.kind === "project") continue;
    if (node.state === "review" || node.state === "blocked") n++;
  }
  return n;
}

/**
 * The orchestrator's last move, as one plain-voice line for the top-bar narration (BRO-1884) —
 * DERIVED from real state, never the prototype's hardcoded "maestro woke 2m ago". Attention-first:
 * a run at your gate (with its branch receipt) → stuck → running → the calm resting line. Leads with
 * the receipt, no fabricated timestamp (CLAUDE.md §Voice + §Work states).
 */
export function selectNarration(s: ServerTruth): string {
  // Leaf work only — a "3 running" line that counted the initiative + project folders above a single
  // running task would be misleading (the folders are structure, not the work).
  const items = selectWorkItems(s).filter((i) => i.kind !== "initiative" && i.kind !== "project");
  const review = items
    .filter((i) => i.state === "review")
    .sort((a, b) => (a.updatedAt < b.updatedAt ? 1 : a.updatedAt > b.updatedAt ? -1 : 0));
  if (review.length > 0) {
    const top = review[0]; // most-recent review item — lead with its branch receipt when present
    const n = review.length;
    if (top?.run)
      return n === 1 ? `${top.run} at your gate` : `${top.run} + ${n - 1} more at your gate`;
    return `${n} at your gate`;
  }
  const blocked = items.filter((i) => i.state === "blocked").length;
  if (blocked > 0) return `${blocked} stuck`;
  const running = items.filter((i) => i.state === "running").length;
  if (running > 0) return `${running} running`;
  return "standing · nothing at your gate";
}
