# Contract — FS-as-truth + the derived index

> **Seam BRO-1754.** A contract-writing ticket: this doc + the row-shape types in
> `packages/protocol` are agreed and merged *before* any dependent starts. It pins the shape of
> the control-plane index and the one rule that governs it. The drizzle-orm/bun-sqlite tables and
> migrations that implement these shapes are **BRO-1796** (`p1-index-schema`), in `apps/runtime`.
>
> **Types:** [`packages/protocol/src/index-schema.ts`](../../packages/protocol/src/index-schema.ts)
> · **Tests:** [`index-schema.test.ts`](../../packages/protocol/src/index-schema.test.ts)
> (`bun test packages/protocol -t index-schema`).
> **Canon:** DATA-MODEL §B.1/§B.3/§B.5 · ARCHITECTURE §3/§7 · PATTERNS §1/§2/§10 ·
> canon-amendments D-DURABILITY / D-ORDER.

## 1. The authority rule (one direction, no exceptions)

**The filesystem is the system of record. The index is a derived, transactional projection. Authority
flows one way: the index indexes the FS; it never writes truth back** (ARCHITECTURE §3, PATTERNS §1).
Every fact about *work* originates in a file — `_work.md` frontmatter, `session.jsonl` lines, git
history. The index coordinates concurrency and answers fast queries; it invents nothing.

The one sanctioned line (PATTERNS §1): the index writes to the FS **only through a flow that commits** —
Loop 4's contract edits land in `_work.md`, then the index re-reads them. That is a flow *through the
FS*, not the index owning truth. Two anti-patterns this rule kills (PATTERNS anti-patterns): a "quick"
FS write from the index side, and authoritative client state.

**Corollary (the guarantee this contract exists to protect):** if the index is lost, rebuild it by
scanning the workspace + git + the FS event journal (§6). Treat it as *a cache with teeth*, not a
database of record.

## 2. Derived vs. authoritative — three categories

The seven tables (DATA-MODEL §B.1, ARCHITECTURE §3b) split by *what losing the row costs you*.
`TABLE_AUTHORITY` + `TABLE_REBUILD` encode this split as data (asserted in the tests).

| Table | Category | Rebuild source | On index loss |
|---|---|---|---|
| `node` | FS-derived | `fs-scan` | rescanned from `_work.md` frontmatter |
| `session` | FS-derived | `fs-scan` + `git-scan` | rescanned from `runs/run-<id>/` + the `run/<id>` branch |
| `event` | FS-derived | `journal-replay` | replayed from `session.jsonl` + the workspace synthetic journal |
| `gate` | FS-derived | `journal-replay` | replayed from `gate.opened` / `gate.decided` journal events |
| `schedule` | FS-derived | `fs-scan` | rescanned from routine `_work.md` trigger blocks |
| `run_budget` | **authoritative** | `journal-replay` | spend re-derived from journaled `budget.*` (D-DURABILITY) |
| `lease` | **authoritative** | `reconcile` | starts empty — leases expired on crash; reconcile against receipts |

- **FS-derived (a cache with teeth).** Drop them and re-derive. `node`/`session`/`event`/`gate`/`schedule`.
- **Authoritative live operational state.** Not in the FS, must be transactional under concurrency:
  `run_budget` (the budget-in-path guard — a lost FS write here is the `$86k/day` failure) and `lease`
  (idempotency + locks — the heartbeat-storm killer). PATTERNS §8, ARCHITECTURE §3b.
- **Durability addendum (D-DURABILITY).** So the rebuild guarantee holds **unqualified**, the two
  index-only facts a plain FS scan can't recover are journaled to the FS as events: `budget.*` (spend)
  and `gate.decided` (verdicts). The `event` table stays a pure projection of those journals, so a
  replay recovers spend counters and decided gates. `run_budget`/`lease` remain the in-path guards at
  runtime; the journal is their durable shadow. **`lease` is the one table with no FS shadow** — a
  fresh runtime holds none and they expire, so identity (§6) excludes it.

Beyond the seven, this contract adds one **index-internal** table — `scan_cursor` (§5). Losing it
forces a full re-scan, never lost truth; it is excluded from the identity dump.

## 3. The seven row shapes

All shapes live in `packages/protocol/src/index-schema.ts` as plain-TS interfaces — see §7 for why
they are here and not drizzle. Enum-typed columns **reference** the already-merged protocol types
(`OrchState`, `SessionStatus`, `GateMode`, `GateVerdict`, `GateKind`, `Kind`, `TriggerKind`) rather
than restate them (PATTERNS §10). Physical column names are the DATA-MODEL §B.3 sketch.

- **`NodeRow`** — `id` (= frontmatter UUID), `path` (unique among live rows), `parentId`, `kind`,
  `state: OrchState`, `owner`, `gate: GateMode`, `budgetJson`, `doneJson`, `title`, `createdAt` +
  `SyncFields`.
- **`SessionRow`** — `id` (= run id), `nodeId`, `branch` (`run/<id>`), `status: SessionStatus`,
  `startedAt`, `endedAt`, `diffstatJson` + `SyncFields`.
- **`EventRow`** — the STORAGE row for an event. It reuses the wire envelope (`EventEnvelope`, events.ts)
  but differs on the **three** fields the storage representation changes: `ts` is **epoch ms** (not the
  ISO wire string), `payload` is the raw `payload_json` **TEXT** (not the rehydrated object), and
  `sessionId` is **required-nullable** (`string | null` — a stored row is never `undefined`; the wire type
  leaves it optional). Expressed as
  `Omit<EventEnvelope, "ts" | "payload" | "sessionId"> & { sessionId: string | null; ts: number; payload: string | null }`
  so `EventEnvelope` stays the one *wire* type (PATTERNS §10); the runtime parses `payload` + formats `ts`
  when it projects a row to the wire (BRO-1796). Append-only + immutable, so **no `SyncFields`** (an event
  never updates or soft-deletes; it *is* the log). See §4, §5, §6.
- **`GateRow`** — `id`, `sessionId`, `kind: GateKind`, `proposalJson` (the gate-card source),
  `verdict: GateVerdict | null` (null = pending), `decidedBy`, `openedAt`, `decidedAt` + `SyncFields`.
- **`ScheduleRow`** — `id`, `nodeId`, `triggerKind: TriggerKind`, `spec`, `nextFireAt`, `enabled` +
  `SyncFields`.
- **`RunBudgetRow`** — `sessionId` (pk), `spentUsd`, `iterations`, `lastCallAt`. No `SyncFields`
  (per-runtime, never synced).
- **`LeaseRow`** — `key` (pk), `holder`, `acquiredAt`, `expiresAt`. No `SyncFields`.

### Ownership boundary — `gate.kind`
`GateKind` is `"completion" | "irreversible-action"` today. **Seam-gate-queue (BRO-1789)** widens it to
add `"question"` and closes the enum. `GateRow.kind` follows automatically — this seam does not touch it.

## 4. IDs, timestamps, sync-readiness (ARCHITECTURE §7)

Sync-ready from day one, so the team tier is additive, not a migration: **stable UUIDs · `updatedAt`
· soft deletes · runtime-owned IDs.** `SyncFields` (`updatedAt: number`, `deletedAt: number | null`)
carries the last two on every syncable derived row (`node`, `session`, `gate`, `schedule`).

- **ID ownership** (sync arbitration needs to know which side mints each key):
  - `node.id` — **FS-authored** (the `_work.md` frontmatter UUID); survives rename/move; the runtime
    never mints it (DATA-MODEL §A.2).
  - all other PKs — **runtime-owned**: `session.id` = run id; `gate.id`/`schedule.id` = runtime UUIDs;
    `lease.key` = a node id or a schedule idempotency key.
- **Soft deletes** — `deletedAt` is an **addition** to the DATA-MODEL sketch. A vanished FS node
  *tombstones* (a peer runtime must learn it is gone, never silently re-learn it as present) rather
  than disappearing. Carried by the syncable derived rows only.
- **Additions beyond the §B.3 sketch** (each flagged; the drizzle column + migration is BRO-1796's):
  `deletedAt` (soft-delete tombstone, syncable derived rows); `updatedAt` on `session`/`gate`/`schedule`
  (the sketch put it only on `node`); `NodeRow.createdAt` (frontmatter `created`, the age the board
  groups by); `GateRow.openedAt` (gate-row lifecycle start, distinct from `decidedAt`); the `scan_cursor`
  table (§5).
- **`updatedAt` provenance (resolve before BRO-1796).** `updatedAt` is the **index-assigned mutation
  clock** (`Date.now()` at index write), NOT the frontmatter `updated:` field — frontmatter `updated` is
  intentionally not indexed. This is why it is wall-clock (non-deterministic across rebuilds) and why the
  §6.1 identity holds only *modulo* `updatedAt`. `NodeRow.createdAt`, by contrast, IS FS-derived
  (frontmatter `created`), so it is deterministic and part of the identity dump.
- **Soft-delete narrows the natural-key UNIQUE to a PARTIAL index.** The §B.3 sketch declares
  `node.path .notNull().unique()` (global). With tombstones retained, a global UNIQUE breaks
  delete-then-recreate (the retained tombstone collides on re-INSERT of the same path). **BRO-1796 must
  implement `node.path` as a PARTIAL unique index scoped to `WHERE deleted_at IS NULL`, not `.unique()`.**
  The same rule applies to any syncable row whose natural key can be reused after a tombstone.
- **Timestamp representation (a deliberate decision, with an explicit drizzle mode).** Every index **row**
  stores timestamps as **epoch ms (`number`)** — the last-writer-wins / range-query semantics need ms
  resolution. **BRO-1796 must use `integer(col, {mode:"number"})` storing `Date.now()`** for every `*At` /
  `ts` column. Do **not** use `{mode:"timestamp"}` (infers to `Date`, persists Unix **seconds** — loses ms
  and yields `$inferSelect: Date`, violating the `number` pin) or `{mode:"timestamp_ms"}` (also
  `Date`-typed). This includes the `event` row's `ts`
  (`EventRow = Omit<EventEnvelope,"ts"|"payload"> & {ts:number; payload:string|null}`). The **wire**
  envelope (`EventEnvelope.ts: string`) carries ISO-8601; the runtime projects row→wire at the boundary
  (BRO-1796). "Types are the contract, columns a sketch" (DATA-MODEL §B) — this pins the mode so the
  column and the contract agree.

## 5. The high-water mark (two levels — both pinned)

- **Global consumer cursor: `event.seq`** — integer autoincrement, total order, no gaps. It is the SSE
  resume cursor and the fan-out cursor (DATA-MODEL §B.5, PATTERNS §2, events.ts). Clients resume a
  stream by replaying events with `seq >` their last-seen value. Its values are **rebuild-scoped** — a
  rebuild renumbers `seq` in canonical order, so a cursor does not carry across one (§6.2).
- **Per-file ingest cursor: `ScanCursorRow.byteOffset`** — one row per journal file
  (`runs/run-<id>/session.jsonl`, the workspace journal), the byte offset the watcher has consumed, so
  an incremental scan (p1-watcher) tails only new bytes. `lastSeq` records the highest `event.seq`
  produced from that file. This is an **addition** to the sketch. Index-internal: a full rebuild resets
  every offset to 0 and re-replays.

The invariant tying them together: incremental scan reads lines past `byteOffset`, assigning fresh
monotonic `seq` in `compareReplay` order (§6), then advances `byteOffset`/`lastSeq`.

## 6. The rebuild algorithm + the identity guarantee

`node`/`session`/`event`/`gate`/`schedule` are recoverable; `run_budget` replays from the journal;
`lease` starts empty. The algorithm (DATA-MODEL §B intro, ARCHITECTURE §3b, PATTERNS §1/§2):

1. **Clear** the FS-derived tables + `scan_cursor`.
2. **Walk the workspace git repo.** Parse every `_work.md` frontmatter → `node` rows (`id`, `path`,
   `parentId` from nesting, `kind`/`state`/`owner`/`gate`, budget/done snapshots, `title` = first
   heading).
3. **Scan git.** For each `runs/run-<id>/` + branch `run/<id>` → `session` rows (branch, status,
   diffstat from git).
4. **Replay the journals in canonical order.** Every `session.jsonl` + the workspace synthetic journal,
   line by line, ordered by `compareReplay` → `event` rows with **fresh `seq`**; derive `gate` rows from
   `gate.opened`/`gate.decided`; derive `schedule` rows from routine trigger blocks.
5. **Reconcile authoritative.** Re-derive `run_budget.spentUsd` by summing journaled `budget.*`
   (D-DURABILITY, DATA-MODEL open-Q#4); `lease` starts empty (crash-expired).

**Identity guarantee (the "cache with teeth" property).** Two distinct claims — keep them apart:

1. **Rebuild-vs-rebuild is byte-identical.** Two rebuilds of the same workspace produce byte-identical
   FS-derived tables (modulo wall-clock `updatedAt`). This is what the seam's property test underwrites:
   `event.seq` is assigned in `compareReplay((ts, sourcePath, line))` order — a strict total order
   (`ts`, then path, then line; `(sourcePath, line)` is globally unique so ties break deterministically),
   so two rebuilds assign identical `seq`. The property test proves `compareReplay` is irreflexive,
   antisymmetric, transitive, and total. p1-rebuild-invariant is the full kill/rebuild/diff (a
   `test.skip` skeleton is named here) — and it **must use a multi-file journal with a cross-file
   out-of-`ts`-order line**, the exact case that separates ingest order from replay order.

   > **Shipped (BRO-1808 · P1 slice).** `apps/runtime/src/db/rebuild.ts` (`rebuildIndex` + the
   > `dumpIndex` canonical, `updatedAt`-stripped dump) and the operator command `maestro-runtime
   > --rebuild` (algorithm steps 1–2: clear + walk-the-workspace → `node` rows). The kill/rebuild/diff
   > identity test is `apps/runtime/src/db/rebuild.test.ts` — build a file index over a fixture, delete
   > it, rebuild at a *different* clock, and the derived `node` rows are byte-identical modulo
   > `updatedAt` (the clock genuinely moves, so the timestamp-strip is load-bearing, not vacuous). It
   > also proves a rebuild *deletes* the prior file (an unjustified ghost row does not survive) and
   > creates the `.maestro/` parent dir on a first-ever rebuild. **Steps 3–5 (journal replay →
   > `session`/`event`/`gate`/`schedule` + `run_budget` re-derivation) land with the tables that carry
   > that data (P2+: budget/gate journaling, BRO-1788 onward)** — at P1 the only populated FS-derived
   > table is `node`, so the identity guarantee is proven where it currently has teeth.

2. **A rebuild reproduces every query answer, but NOT the live `seq` integers.** A rebuild re-derives
   the *canonical* order; it does not reproduce the pre-loss LIVE index's `seq` VALUES. Live `seq` is
   ingest-ordered (byte-arrival — the SSE cursor), and under concurrent sessions writing separate
   `run/<id>/session.jsonl` files, ingest order differs from `compareReplay` order; that cross-file
   interleaving lived only in the dropped autoincrement column and is unrecoverable. What **survives**
   is every reactive query in DATA-MODEL §B.5 — the Needs-you count, the board in D-ORDER, each
   **per-session** timeline (`event where session_id=? order by seq`), the bench. What does **not**
   survive is the absolute `seq` value: **an SSE cursor does not carry across a rebuild — a rebuild is a
   stream reset, and clients re-subscribe** (rebuild is a rare recovery event). `run_budget.spentUsd`
   re-derives from journaled `budget.*` (D-DURABILITY); `lease` starts empty.

   > **Precondition (why the per-session timeline is stable).** The within-session `order by seq` equals
   > `(ts, line)` order, which equals write/file order **only when the session's `ts` is monotone
   > non-decreasing**. The Loop-1 append path (BRO-1790) enforces this by clamping each appended event's
   > `ts` to `max(prev_ts, now)`; absent that, a clock step-back (NTP, tool-stamped events) would re-order
   > a session's timeline across a rebuild. `sourcePath` must also be **byte-canonical** (one fixed Unicode
   > normalization form, no `./`/symlink/alias spellings) — `compareReplay` compares it raw, so two
   > spellings of one file would split into two keys and break §6.1 byte-identity. Both are scanner /
   > append-path preconditions owned by BRO-1796 / BRO-1790.

## 7. Decision — why row-shapes in `packages/protocol`, not drizzle

The handoff left one architecture decision open: where do the index-schema types live? **Decided:
plain-TS row-shape interfaces in `packages/protocol/src/index-schema.ts`; the `drizzle-orm/bun-sqlite`
table definitions + migrations are deferred to BRO-1796 in `apps/runtime`.**

- `@maestro/protocol` is imported by **both** `apps/runtime` (Bun) and `apps/app` (the browser Vite
  SPA) and is dependency-free by design (PATTERNS §10). `drizzle-orm/bun-sqlite` is a server/node dep — it
  must never reach the browser bundle. Row *shapes* are pure TS and ship anywhere.
- DATA-MODEL §B is explicit: **"types are the contract, columns a sketch."** The contract is the shape;
  the ORM binding is an implementation detail BRO-1796 owns.
- The done.check filter (`bun test packages/protocol …`) already points the schema *types* at
  `packages/protocol` — this decision matches it while keeping the wire package pure.

## 8. Out of scope (adjacent seams own these)

- **`drizzle-orm/bun-sqlite` tables + migrations** → BRO-1796 (`p1-index-schema`).
- **The board / gate attention comparator (D-ORDER)** → seam-gate-queue (BRO-1789). This doc *cites*
  D-ORDER (`review, blocked, running, triggered, reviewing, proposed, done, canceled`) but does not
  define the comparator, to avoid a double definition across parallel seams.
- **Widening `GateKind` to add `question`** → BRO-1789.
- **Scanner / watcher / rebuild implementations** → p1-scanner, p1-watcher, p1-rebuild-invariant.
- **Event-log rotation/summarization** (DATA-MODEL open-Q#2) and **multi-runtime lease arbitration**
  (open-Q#3) — unresolved; not pinned here.

### Synthetic-event note (resolved)
The authoritative closed synthetic list is `SYNTHETIC_EVENT_TYPES` (events.ts, BRO-1785):
`node.updated`, `gate.opened`, `gate.decided`, `schedule.fired` — no `node.created` (node creation
surfaces as `node.updated`, D-DURABILITY). The DATA-MODEL §B.3 inline comment on `event.session_id`
lists synthetics loosely (it predates the closed list); events.ts is the wire owner (PATTERNS §10) and
wins. Synthetics are persisted in the `event` table with `sessionId` null.

---

_Contract for `seam-fs-index` (BRO-1754). Provenance: DATA-MODEL / ARCHITECTURE / PATTERNS under
`handoff/design_handoff_maestro/`, amended by `docs/canon-amendments.md` (D-DURABILITY, D-ORDER).
Supersedes nothing in canon; interprets the "columns a sketch" latitude DATA-MODEL §B grants._
