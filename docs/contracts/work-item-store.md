# Contract — the canonical work-item store (one shape, everything derives)

> **Seam BRO-1764.** A contract-writing ticket: this doc + `WorkItem` in `packages/protocol` are
> agreed before the store skeleton, board, feed, and inspector start (they all derive from this one
> shape). Nothing here is authoritative state — the work item is a *read-side projection*.
>
> **Types:** [`packages/protocol/src/work-item.ts`](../../packages/protocol/src/work-item.ts)
> · **Tests:** [`work-item.test.ts`](../../packages/protocol/src/work-item.test.ts)
> (`bun test packages/protocol -t work-item`).
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
| `updatedAt` | store | = node.updatedAt, ISO on the wire |
| `created` | store | frontmatter `created`, ISO |
| `sessionId?` | derived | the node's current-or-most-recent session id (present on running / attention / terminal / standing; undefined only for `proposed`) — the join key for the session timeline; **not live-only** |
| `initiative?` / `project?` | derived | ancestor labels from the `parentId` ancestry chain |
| `lastEventAt?` | derived | ISO ts of the last event; the client formats the relative age |
| `worker?` | derived | `{ name, where }` from the current-or-most-recent `session` row (present on completed items too); `where ∈ local worktree \| cloud sandbox` |
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

1. **`time` (relative string) → `lastEventAt` (ISO).** The demo stores a pre-formatted `"12m"`;
   data-contract itself calls the field "relative age of last event" (a derivation). The wire carries
   an absolute ISO timestamp; the client formats. Never store the formatted string (mirrors the
   never-store-a-percentage rule).
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

## 8. Boundaries (adjacent seams / tickets own these)

- **`chat[]` / the UIMessage stream** → seam-chat-transport (BRO-1776).
- **The gate-card / `look` composition + the board comparator** → seam-gate-queue (BRO-1789). This doc
  carries `look` as a derived field shape; how the gate queue orders + composes it is BRO-1789's.
- **The store slices' code** (the Zustand/Redux slices) → p1-store-skeleton (BRO-1775) in `apps/app`.
- **`GateKind` is NOT touched** here — it governs `gate` rows, not work items.

---

_Contract for `seam-work-item-store` (BRO-1764). Provenance: data-contract / porting-notes /
START-HERE / DATA-MODEL under `handoff/design_handoff_maestro/`. Supersedes nothing in canon; where
README and porting-notes diverge on selection, porting-notes wins (START-HERE §2)._
