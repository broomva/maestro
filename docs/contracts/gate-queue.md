# Contract — the gate queue (derived view, attention-first comparator, verdict semantics)

> **Seam BRO-1789.** A contract-writing ticket: this doc + `gate.ts` are agreed before the board
> (BRO-1780), the F5 gate slice (BRO-1805), and approve = squash-merge (BRO-1802). It pins the derived
> attention view, the attention-set comparator (the board reuses the shared `compareByAttention` axis,
> not this comparator), the `data-gate` card payload, the verdict semantics, the closed `GateKind`, and
> the grace-window undo machine.
>
> **Types:** [`packages/protocol/src/gate.ts`](../../packages/protocol/src/gate.ts) (+ `GateKind` widened
> in `work.ts`) · **Tests:** [`gate.test.ts`](../../packages/protocol/src/gate.test.ts)
> (`bun run typecheck && bun test packages/protocol -t gate-queue`).
> **Canon:** DATA-MODEL §B.3 gate / §B.5 · PATTERNS §6 · FLOWS §F5 · porting-notes §MccMaestroLoopV2
> (grace) · START-HERE §5 seam 3 · canon-amendments D-GATE / D-ORDER.

## 1. The gate queue is a DERIVED VIEW, never a store

The queue is `node WHERE state ∈ {review, blocked}` — the **attention set**, `ATTENTION_STATES` (state.ts).
`gate.ts` REFERENCES that const (`isInGateQueue`), it does not fork a second set. There is no `gate_queue`
table: the queue is the same `node`/`gate` rows the fs-index (BRO-1754) already holds, filtered + ordered.
It is fully rebuildable, holds no truth of its own.

**`blocked` is in the queue but is NOT gate-decidable.** Both gate kinds surface as `review`;
`resolveGateVerdict` throws off-review (state.ts). So only a `review` node carries an open gate + a
`gateId` + the four verdicts. A `blocked` (Stuck) node is in the queue because it needs a human, but it is
cleared by **unblock / redispatch** (a `dispatch` intent keyed on the node id), not a verdict. The card
for a blocked node offers redispatch; the card for a review node offers the verbs.

## 2. The comparator (reuses the shared board axis — BRO-1780)

`compareGateQueue(a, b)` over `{ state, attentionSince }` — a **total order over the gate queue
{review, blocked}**, NOT a whole-board sort:

1. **Cross-group** — `compareByAttention` (plain-voice.ts, D-ORDER `WK_GROUP_ORDER`), REFERENCED not
   re-declared: `review` before `blocked`, then the rest. This is the **shared board axis** — valid over
   all 8 states — and the gate queue adds no fork of it.
2. **Within-group tiebreak** — **oldest-waiting first** (ascending `attentionSince`). `attentionSince` is
   the epoch ms the node ENTERED its attention state (a gate's `openedAt` for review, the block event ts
   for blocked) — **NOT `createdAt`** (sorting the attention queue by creation time buries freshly-
   actionable old work — the BRO-1764 §8 sort-key decoupling). The ticket calls this "age descending": the
   gate that has waited longest for a human sits at the top so no gate rots at the bottom.

**The board (BRO-1780) reuses the shared `compareByAttention` axis for grouping, then supplies its OWN
within-group recency key per group** — a `running` node has no gate `openedAt`, so `attentionSince` is not
its sort key. Do not `nodes.sort(compareGateQueue)` over non-attention nodes: the cross-group order is
right, but the within-group tiebreak only means anything for {review, blocked}. `attentionSince` is
defined only for the attention set.

Total order on finite `attentionSince` (proven in the test: reflexive → 0, antisymmetric, transitive — and
that the shared axis still orders non-attention states after the attention set). Non-finite `attentionSince`
is out of contract (the runtime emits a real epoch).

## 3. The `data-gate` card payload (`GateCard`)

`GateCard { gateId, kind: GateKind, look: GateLook }` — the `data-gate` part payload, reconciled across
every open client by `gateId` (FLOWS F5). `look` (the display compression — what changed · what it decided
· what it asks) is BRO-1764's `GateLook`, **imported not redefined**. This seam OWNS `data-gate`: chat.ts
(BRO-1776) left `MaestroDataParts.gate` out, and `gate.ts` adds it by **TypeScript module augmentation**
(`declare module "./chat" { interface MaestroDataParts { gate: GateCard } }`) — no edit to chat.ts, no
fork. It also pins the wire surface — `DATA_GATE_NAME` / `DATA_GATE_PART` (`"data-gate"`) + the tag-only
`isGateDataPart` guard + the narrowed `GateDataPart` — mirroring chat.ts's `data-tick`, so BRO-1805 matches
the constant, never a hand-written `"data-gate"` literal. The part `id` is the `gateId` (the F5
reconciliation key), not a singleton stable id like the tick's `DATA_TICK_ID`.

**Precondition:** the barrel `export * from "./gate"` (index.ts) — without it the augmentation never loads
and `data-gate` fails to typecheck at the composition site. The test's compile-witness proves the
augmentation BINDS (disabling the `declare module` block fails `tsc`); the barrel export itself is a
same-package re-export, checked by review, not a separate through-barrel test.

## 4. Gate row lifecycle (resolves the DATA-MODEL ambiguity)

`opened` → `verdict = null` (pending) → `decided`. The four verdicts (state.ts `GateVerdict`) resolve via
`resolveGateVerdict` (D-GATE):

| Verdict | UI verb | Next state | Terminating? |
|---|---|---|---|
| `approve` | Approve *(primary)* | `done` | yes |
| `revise` | Send back *(primary)* | `triggered` | yes |
| `block` | Block *(secondary)* | `canceled` | yes |
| `escalate` | Point *(secondary)* | `review` (stays) | **no** — re-decidable |

`escalate` (reassign owner) and `grant` (attach a capability — a **separate intent**, not a verdict) leave
the row at `review`, so the gate **can be decided again**. `TERMINATING_VERDICTS` = {approve, revise,
block} is pinned in `gate.ts` AND cross-checked against `resolveGateVerdict` in the test, so the set can
never drift from the state machine. `GATE_VERDICT_VERBS` is exhaustive over `GateVerdict` (a new verdict
fails `tsc` until given a verb).

## 5. `GateKind` — the closed enum, widened

`GateKind = completion | irreversible-action | question` — widened here (this seam adds `question`,
HARNESS §4 exit-20) in its home `work.ts`, using the const→type idiom (`GATE_KINDS` → `type` derived) so a
new kind can't escape `tsc`. `isGateKind` guards it. `GateRow.kind` (index-schema.ts) picks up the widened
type automatically.

**`kind` drives which verbs the F5 card SURFACES (BRO-1805), not the verdict set itself.** The four
`GATE_VERDICT_VERBS` are fixed; a `completion` / `irreversible-action` gate shows all four, but a
`question`-kind gate resolves on the **answer path** (FLOWS F5) — `revise` carries the answer as feedback,
and Approve/Block are suppressed or relabelled. A choice-question's answer options are **not yet modelled**:
`GateLook` today is `{ ran, decided, ask }` (BRO-1764) with no options field — adding one is a BRO-1764
follow-up, not a new verdict here. This keeps BRO-1805 from rendering Approve/Block on a pure question.

## 6. The grace window (the one sanctioned timing component)

`GATE_GRACE_WINDOW_MS = 5000`. When the human clicks a verdict it becomes a `PendingVerdict` in phase
`grace`: **undoable, and the intent is NOT sent**. When the window lapses (`isWithinGrace` → false) it
moves `sending` → `sent` (the intent is sent exactly once, at the end of the window). A transport failure
moves it to `failed` — the card **re-queues with an error chip** (porting-notes), never silently dropped.
`GracePhase = grace | sending | sent | failed`. This is the seam's one timing component test.

## 7. Cross-seam ownership

| Concern | This seam (BRO-1789) | Adjacent owner |
|---|---|---|
| `data-gate` payload | `GateCard` + the `MaestroDataParts.gate` augmentation | the `data-*` map + `data-tick` → BRO-1776 (chat.ts) |
| `look` shape | imported (`GateLook`) | declared by BRO-1764 (work-item.ts) |
| `gateId` presence (review-only) | consumed as the verb key | pinned by BRO-1764 (§8) |
| the board comparator | `compareGateQueue` (composes it) | `compareByAttention` / `WK_GROUP_ORDER` ship in plain-voice.ts (BRO-1785) |
| `GateVerdict` + `resolveGateVerdict` | imported + cross-checked | declared by BRO-1785 (state.ts) |
| the four verb **intents** | verb names + terminating semantics | the write path (`approve`/`revise`/… intents) → BRO-1785 (intents.ts) |
| `attentionSince` source | the field + that it is attention-entry, not `createdAt` | which column/event carries `openedAt` / block-ts → BRO-1790 / BRO-1754 |

---

_Contract for `seam-gate-queue` (BRO-1789). Provenance: DATA-MODEL / PATTERNS / FLOWS / porting-notes /
START-HERE under `handoff/design_handoff_maestro/`. Derived view over the attention set; references (never
forks) `ATTENTION_STATES` + `compareByAttention`; owns `data-gate` via module augmentation of BRO-1776's
`MaestroDataParts`._
