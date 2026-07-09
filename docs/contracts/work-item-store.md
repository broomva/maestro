# Contract — the canonical work-item store (one shape, everything derives)

> **Seam BRO-1764.** A contract-writing ticket: this doc + `WorkItem` in `packages/protocol` are
> agreed before the store skeleton, board, feed, and inspector start (they all derive from this one
> shape). Nothing here is authoritative state — the work item is a *read-side projection*.
>
> **Types:** [`packages/protocol/src/work-item.ts`](../../packages/protocol/src/work-item.ts)
> · **Tests:** [`work-item.test.ts`](../../packages/protocol/src/work-item.test.ts)
> (`bun run typecheck && bun test packages/protocol -t work-item` — the type-level witnesses
> only bite under `tsc`; `bun test` strips types, so typecheck is part of the check).
> **Canon:** data-contract §"The work model" / §"The work item shape" · porting-notes §"State taxonomy"
> · START-HERE §5 seam 2 · DATA-MODEL §B.1/§B.3/§B.5 · CLAUDE.md §disclosure ladder.

## 1. The authority rule (the work item stores no truth)

The filesystem `_work.md` frontmatter is the system of record; the `node` index row is its derived,
transactional projection; **`WorkItem` is the client wire shape derived from `node` + `session` +
`event`** (DATA-MODEL §B.1, data-contract §"The work model"). It invents nothing. If the index is
lost, every work item is rebuildable by re-scanning the workspace + git (DATA-MODEL §B.1) — the work
item holds **zero** authoritative live state (`run_budget`/`lease` never surface on it).

## 2. Three shapes, one truth — the relationship map

The single most common way to get this wrong is to conflate the three. They are distinct:

```
_work.md frontmatter   =  WorkContract   (authoritative write side — packages/protocol/src/work.ts)
        │ indexed by the runtime
        ▼
   node table row       (derived index projection — DATA-MODEL §B.3)
        │ projected for the client
        ▼
     WorkItem           (read-side UI/wire shape — THIS contract)
```

`WorkItem` carries a **subset** of `WorkContract` (id, state, kind, owner, gate, title, created,
updated) **plus derived projections** (worker, run, verdict, reason, look, lastEventAt,
initiative/project, sessionId). It deliberately **omits** `budget` / `done` / `trigger` — those are
contract internals held off the read surface by the disclosure ladder ("never expose worktrees,
index.db, or the engine room").

## 3. The canonical `WorkItem` shape — field by field

Every field resolved as **store** (mirrors the `node` row) or **derived** (computed from
`session` + `event`, never stored). Enum-typed fields **reference** the merged protocol types
(`OrchState`, `Kind`, `GateMode`) — never redefined.

| Field | Class | Source / meaning |
|---|---|---|
| `id` | store | = node.id / frontmatter UUID; survives renames (DATA-MODEL §A.2) |
| `state` | store | = node.state (`OrchState`) |
| `kind` | store | = node.kind (`Kind`) — on the wire because the Standing overlay needs it (`plainVoiceForNode`) |
| `title` | store | = node.title (first heading of `_work.md`) |
| `owner?` | store | = node.owner (`@handle` \| `agent:name`) |
| `gate` | store | = node.gate (`GateMode`) |
| `path` | store | = node.path (workspace-relative) |
| `parentId?` | store | = node.parentId (the work tree) |
| `updatedAt` | store | = node.updatedAt, ISO on the wire — **present on every node**, so the universal source for a card's relative age (refined by `lastEventAt` once dispatched) |
| `created?` | store | frontmatter `created`, ISO — via `node.createdAt` (which BRO-1754's merged `NodeRow` **carries**; only the DATA-MODEL §B.3 sketch omits it). **Optional** — no read surface renders it today (age routes through `updatedAt`), so a consumer supplies it only where a view shows a creation date (§7 "unconsumed → optional") |
| `sessionId?` | derived | the node's current-or-most-recent session id. **Dispatch-history-keyed, NOT state-keyed:** undefined only if the node has *never* been dispatched (no `session where node_id=? order by started_at desc limit 1` row) — typically `proposed` / `reviewing` / never-fired `triggered` backlog + work canceled before dispatch. Present on any node that has dispatched ≥ once, **including a fired standing routine idling back at `triggered`** (it keeps its last session so the Standing routine's last-run receipts survive) and a `done` node. The session-timeline join key; **not live-only** |
| `gateId?` | derived | the id of the node's **open gate** when `state ∈ {review, blocked}` (1:1 at the gate), else undefined. The gate-queue join + verb-**dispatch** key: every gate verb (`approve`/`revise`/`block`/`escalate`/`grant`) takes a `gateId` (intents.ts), so rung-2 controls dispatch through THIS, never the node id. `look` is display-only; queue composition/ordering is BRO-1789 (§8) |
| `initiative?` / `project?` | derived | ancestor labels from the `parentId` ancestry chain |
| `lastEventAt?` | derived | ISO ts of the last *event* — present only once the node has events (dispatched work). REFINES the card age for live/dispatched items; the **universal** relative age falls back to the always-present `updatedAt` (`lastEventAt ?? updatedAt`), so queued/proposed cards still render an age |
| `worker?` | derived | `{ name, where }` — a **shape** pin (agent id + isolation mode); WHICH event payload or index column carries them is upstream (BRO-1790/1754/1796) — the `run.started` sketch (DATA-MODEL §A.3) does not yet enumerate them, and the `session` row has no name/location column. Present on completed items too; `where ∈ local worktree \| cloud sandbox` |
| `run?` | derived | = session.branch (`run/<id>`) — the receipt |
| `verdict?` | derived | judge summary string (full `VerdictReceipt` stays on the event / inspector) |
| `reason?` | derived | blocked cause from the blocking run event |
| `look?` | derived | the gate compression `{ ran, decided[], ask }`; `ran` is a receipt string, never a percentage |

## 4. What the work-item store does NOT hold

`WORK_ITEM_EXCLUDED_FIELDS` names them as data (the test asserts the shape stays clean):

- **`chat[]` — out.** Chat is a separate projection (the UIMessage stream, the ChatTransport seam —
  BRO-1776); "a session renders work, it never owns it" (data-contract §"The work model").
- **`events[]` — out.** The activity timeline is its own reactive query (`event where session_id=?
  order by seq`, DATA-MODEL §B.5); the server-truth slice is "fed ONLY by the event-log subscription"
  (porting-notes §State taxonomy). The client joins events to the focused item by `sessionId`.
  **Caveat (read before shipping a timeline):** the `sessionId` join is **lossy for synthetics** —
  `node.updated` / `gate.decided` / `schedule.fired` carry a null `sessionId` (D-DURABILITY), so a
  node-scoped timeline that includes them needs an `event.node_id` join (BRO-1796), not solved here
  (see §7 #2). A consumer reading only this mechanism must not ship a lossy inspector timeline.
- **`budget` / `done` / `trigger` — out.** Engine-room internals; the disclosure ladder keeps the read
  surface to signals / verbs / receipts.

## 5. The client store taxonomy — three slices (doc-level contract)

Every prototype `useState` lands in exactly one of three homes (porting-notes §"State taxonomy — the
one real refactor"); `STORE_SLICES` names them. The slices themselves live in `apps/app` — this
contract governs their **shape**, not their code.

1. **Server-truth slice** — in the store, **fed ONLY by the event-log subscription**, mutated **ONLY**
   via intents (`Intent` union, intents.ts); components read selectors, never `useState`. Holds: work
   items, open sessions, open files, gate items + grace window, tick history.
2. **Persisted UI-prefs slice** — the store's single persisted slice; absorbs the ad-hoc localStorage
   keys. Holds `view` / `navOpen` / `cols` (`UI_PREF_KEYS`).
3. **Ephemeral component state** — stays local `useState`: hover, drag, open/closed, cursor, `mode`,
   `scope`, responsive collapse.

**Invariant: components never touch localStorage directly** — the single persisted slice replaces the
sprinkled keys. Rule of thumb: lose-work-or-context → server-truth; lose-a-layout-pref → persisted;
nobody-notices → ephemeral.

### Legacy localStorage → persisted slice
| Old key | Field | Meaning |
|---|---|---|
| `mc4-view` | `view: PlaneView` | plane view (`feed` \| `board` \| `list`, default `feed`) |
| `bv-nav-open` | `navOpen: boolean` | sidebar open (default true) |
| `bv-ml-cols` | `cols: Record<string, number>` | column widths (incl. `nav`, default 200) |

## 6. The selection model

Per README §State management × porting-notes decomposition ("selection drives both surfaces: a tree
rung scopes the plane and retunes the inspector"):

- **tree rung / scope** (`scopeId`) → ephemeral local — re-scopes the plane.
- **open item** → ephemeral selection — which `WorkItem` the inspector shows.
- **open session** (`chatAct` / `chatTabs`) → server truth — open sessions come from the runtime.

> **Reconciliation.** README loosely groups all three under "the canonical store"; porting-notes (the
> owner of the refactor) splits them across slices. This contract follows porting-notes; README defers
> per the START-HERE §2 canon rule.

## 7. Canon reconciliations (production wire vs the `WK_ITEMS` demo shape)

Three deliberate, auditable deviations from the prototype shape (data-contract §"The work item shape";
`WorkData.jsx WK_ITEMS`):

1. **`time` (relative string) → an absolute ISO timestamp the client formats (never a stored string).**
   The demo renders a per-card age (`12m` / `2h` / `3d`) on **every** card — including queued and
   proposed backlog. Route that universal age at **`updatedAt`** (a required store field, present on
   every node), refined by **`lastEventAt`** once the node has events: `lastEventAt ?? updatedAt`.
   Routing it at `lastEventAt` alone would render blank/NaN on every never-dispatched card (no session
   ⇒ no `lastEventAt` — see the optionality witness in the test) — a fidelity regression a board/feed
   ticket (BRO-1789/BRO-1780) would inherit. The data-contract calls the field "relative age of last
   event"; for backlog work with no events yet, that age is the node's own last update. Never store the
   formatted string (mirrors the never-store-a-percentage rule).
2. **`events[]` is not embedded.** The demo embeds an events array per item; DATA-MODEL §B.5 pins the
   timeline as its own query and porting-notes pins the server-truth slice as event-subscription-fed.
   The client joins the **session** timeline by `sessionId` (a first-class field added for exactly this)
   — the work item has no `events`. **Caveat — the `sessionId` join is not lossless for synthetics:**
   synthetic events (`node.updated`, `gate.decided`, `schedule.fired`) carry a null `sessionId`
   (D-DURABILITY), so the join does not reach them. Reproducing the demo's per-item `events[]` as a
   *node-scoped* timeline (synthetics included) needs a `nodeId` column on `event` — an upstream
   fs-index / read-API consideration (flagged for BRO-1796 / the read-API ticket), not solved here.
3. **`initiative` / `project` are derived ancestor labels, not opaque ids.** The demo uses flat ids;
   production derives them from the `parentId` ancestry chain, which stays authoritative.
4. **One node, many sessions — the projection carries only the latest.** A node re-runs over its
   lifetime, so `node ↔ session` is 1:many; `sessionId` (and `worker`/`run`) project the
   **current-or-most-recent** session only. A node's *prior*-session receipts are reached by a
   `session where node_id=?` join (a list, ordered by start) — **joined, never embedded** in the work
   item, the same not-embedded discipline as the timeline. That join is an upstream read-API concern
   (flagged for BRO-1796); pinning it here keeps BRO-1775's inspector from being built
   single-session-only (rendering just `sessionId` and losing the earlier runs' branches/verdicts).

## 8. Cross-seam ownership — every boundary field has ONE owner

This seam pins the read-side **shape**. Every field that touches an adjacent seam has an explicit owner
so no downstream ticket resolves it two ways or builds a dead control. The rule: **this seam pins the
field + its meaning; the adjacent seam owns the behavior behind it.**

| Field / concern | This seam (BRO-1764) pins | Adjacent owner |
|---|---|---|
| `gateId` | the field + that it is the verb-**dispatch** key (`state ∈ {review, blocked}` ⇒ the open gate; 1:1 at review) | the five gate **verbs** are `intents.ts` (`approve`/`revise`/`block`/`escalate`/`grant`, each takes `gateId`); the gate **queue** ordering + card composition is **BRO-1789** |
| `look` | its **shape** `{ ran, decided[], ask }` (a display compression) | how the gate queue **composes** it from receipts + orders it → **BRO-1789**. `look` is display-only — it carries NO dispatch key; verbs go through `gateId` |
| `worker` | its **shape** `{ name, where }` | which event payload / index column carries it → BRO-1790 / BRO-1754 / BRO-1796 |
| `created` | the field (optional), sourced from `node.createdAt` | the `NodeRow.createdAt` column → **BRO-1754** (merged; the column exists) |
| activity timeline | the `sessionId` join key; that it is lossy for synthetics | the `event.node_id` join for a node-scoped timeline → **BRO-1796** (see §4 caveat) |
| the board **comparator** | nothing — `compareByAttention` + `WK_GROUP_ORDER` already ship in `plain-voice.ts` | BRO-1789 owns only the gate-queue tiebreak layered **on top of** the existing protocol comparator (no fork, no double-export) |
| `chat[]` / the UIMessage stream | that it is **excluded** (§4) | seam-chat-transport → **BRO-1776** |
| the store slices' **code** | their **shape** (§5) | p1-store-skeleton → **BRO-1775** (`apps/app`) |
| `GateKind` | **not touched** here — it governs `gate` rows, not work items | BRO-1789 widens it (`+question`) |

---

_Contract for `seam-work-item-store` (BRO-1764). Provenance: data-contract / porting-notes /
START-HERE / DATA-MODEL under `handoff/design_handoff_maestro/`. Supersedes nothing in canon; where
README and porting-notes diverge on selection, porting-notes wins (START-HERE §2)._
