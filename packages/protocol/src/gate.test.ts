/// <reference types="bun" />
import { describe, expect, test } from "bun:test";
import type { MaestroDataParts } from "./chat";
import {
  compareGateQueue,
  GATE_GRACE_WINDOW_MS,
  GATE_VERDICT_VERBS,
  type GateCard,
  type GateQueueOrder,
  isInGateQueue,
  isTerminatingVerdict,
  isWithinGrace,
  type PendingVerdict,
  TERMINATING_VERDICTS,
} from "./gate";
import { ATTENTION_STATES, GATE_VERDICTS, type OrchState, resolveGateVerdict } from "./state";
import { GATE_KINDS, isGateKind } from "./work";

// done.check for seam-gate-queue (BRO-1789): `bun run typecheck && bun test
// packages/protocol -t gate-queue`. `--filter` is a no-op in bun test (only `-t`
// filters by name); every describe carries "gate-queue" so `-t gate-queue` isolates.
// typecheck is REQUIRED: `bun test` strips types, so the augmentation + exhaustive-verb
// witnesses only bite under `tsc --noEmit`.

describe("gate-queue · membership is the attention set (single-source, no duplicate)", () => {
  test("isInGateQueue is exactly {review, blocked} — referencing ATTENTION_STATES", () => {
    expect(isInGateQueue("review")).toBe(true);
    expect(isInGateQueue("blocked")).toBe(true);
    for (const s of [
      "proposed",
      "reviewing",
      "triggered",
      "running",
      "done",
      "canceled",
    ] as const) {
      expect(isInGateQueue(s)).toBe(false);
    }
    // it IS the attention set, not a fork
    expect([...ATTENTION_STATES].sort()).toEqual((["blocked", "review"] as const).slice().sort());
  });
});

describe("gate-queue · the comparator (also orders the board, D-ORDER)", () => {
  const at = (state: OrchState, attentionSince: number): GateQueueOrder => ({
    state,
    attentionSince,
  });

  test("review sorts before blocked (attention-first, cross-group)", () => {
    expect(compareGateQueue(at("review", 100), at("blocked", 1))).toBeLessThan(0);
    expect(compareGateQueue(at("blocked", 1), at("review", 100))).toBeGreaterThan(0);
  });

  test("within a group, oldest-waiting first (ascending attentionSince = ticket's 'age descending')", () => {
    // two review gates: the one that entered attention EARLIER (smaller ts) sorts first.
    expect(compareGateQueue(at("review", 10), at("review", 20))).toBeLessThan(0);
    const rows: GateQueueOrder[] = [
      at("review", 30),
      at("blocked", 5),
      at("review", 10),
      at("blocked", 50),
    ];
    const sorted = [...rows].sort(compareGateQueue);
    expect(sorted.map((r) => `${r.state}:${r.attentionSince}`)).toEqual([
      "review:10", // review group, oldest first
      "review:30",
      "blocked:5", // then blocked group, oldest first
      "blocked:50",
    ]);
  });

  test("the shared board axis holds for non-attention states — but attentionSince is NOT their key", () => {
    // compareGateQueue REUSES compareByAttention (valid over all 8 states) for cross-group
    // order: a non-attention node still sorts AFTER the attention set. This locks the
    // narrowed contract — the board (BRO-1780) may reuse this cross-group axis, but must
    // NOT treat attentionSince as the within-group key for non-attention groups.
    expect(compareGateQueue(at("review", 999), at("running", 1))).toBeLessThan(0); // review before running
    expect(compareGateQueue(at("blocked", 999), at("running", 1))).toBeLessThan(0); // blocked before running
    expect(compareGateQueue(at("running", 1), at("done", 1))).toBeLessThan(0); // running before done (WK_GROUP_ORDER)
    // within a NON-attention group the tiebreak is attentionSince here, but the board owns
    // that group's real recency key — so this is out-of-contract, only asserted total-order-safe.
    expect(Number.isFinite(compareGateQueue(at("running", 5), at("running", 9)))).toBe(true);
  });

  test("compareGateQueue is a strict total order over real fixtures (irreflexive, antisymmetric, transitive)", () => {
    const items: GateQueueOrder[] = [
      at("review", 10),
      at("review", 30),
      at("blocked", 5),
      at("blocked", 50),
      at("review", 10), // a duplicate — must compare equal (0)
      at("running", 7), // non-attention: the mechanism stays a total order (no NaN, no crash)
      at("done", 7),
    ];
    const sign = (n: number) => (n > 0 ? 1 : n < 0 ? -1 : 0);
    for (const a of items) {
      expect(sign(compareGateQueue(a, a))).toBe(0); // reflexive → 0
      for (const b of items) {
        expect(sign(compareGateQueue(a, b)) === -sign(compareGateQueue(b, a))).toBe(true); // antisymmetric
        for (const c of items) {
          if (compareGateQueue(a, b) <= 0 && compareGateQueue(b, c) <= 0) {
            expect(compareGateQueue(a, c)).toBeLessThanOrEqual(0); // transitive
          }
        }
      }
    }
  });
});

describe("gate-queue · the data-gate card payload + MaestroDataParts augmentation", () => {
  test("a GateCard carries gateId + kind + the imported GateLook", () => {
    const card: GateCard = {
      gateId: "g-7f3a",
      kind: "completion",
      look: { ran: "2h 14m unsupervised · 41 events", decided: ["ran the suite"], ask: "Merge?" },
    };
    expect(card.gateId).toBe("g-7f3a");
    expect(card.look.ask).toBe("Merge?");
    expect(JSON.parse(JSON.stringify(card))).toEqual(card);
  });

  test("MaestroDataParts.gate is registered by module augmentation (compile-checked)", () => {
    // If the `declare module "./chat"` augmentation in gate.ts did NOT take effect,
    // `MaestroDataParts["gate"]` would not exist and this assignment fails `tsc`.
    const gateData: MaestroDataParts["gate"] = {
      gateId: "g1",
      kind: "question",
      look: { ran: "just now", decided: [], ask: "Which region?" },
    };
    const asCard: GateCard = gateData;
    expect(asCard.kind).toBe("question");
  });
});

describe("gate-queue · verdict semantics (D-GATE, FLOWS F5)", () => {
  test("GATE_VERDICT_VERBS covers all four verdicts, escalate surfaces as Point", () => {
    for (const v of GATE_VERDICTS) {
      expect(typeof GATE_VERDICT_VERBS[v]).toBe("string");
    }
    expect(GATE_VERDICT_VERBS.approve).toBe("Approve");
    expect(GATE_VERDICT_VERBS.revise).toBe("Send back");
    expect(GATE_VERDICT_VERBS.escalate).toBe("Point");
  });

  test("TERMINATING_VERDICTS matches resolveGateVerdict — NO DRIFT (the single-source cross-check)", () => {
    // The load-bearing test: the pinned set must equal exactly the verdicts that move a
    // review node OUT of the queue per the merged state machine. If either drifts, this
    // fails. (resolveGateVerdict throws off-review, so `current` is always "review".)
    for (const v of GATE_VERDICTS) {
      const leavesQueue = resolveGateVerdict("review", v) !== "review";
      expect(isTerminatingVerdict(v)).toBe(leavesQueue);
    }
    expect([...TERMINATING_VERDICTS].sort()).toEqual(["approve", "block", "revise"]);
    // escalate is the sole non-terminating verdict (stays at review, re-decidable)
    expect(isTerminatingVerdict("escalate")).toBe(false);
  });
});

describe("gate-queue · GateKind is the closed enum widened with question", () => {
  test("GATE_KINDS is the three closed kinds and isGateKind guards them", () => {
    expect([...GATE_KINDS].sort()).toEqual(["completion", "irreversible-action", "question"]);
    expect(isGateKind("question")).toBe(true);
    expect(isGateKind("nope")).toBe(false);
  });
});

describe("gate-queue · the grace window (the one sanctioned timing component)", () => {
  test("the grace window is 5s and undo blocks the send until it lapses", () => {
    expect(GATE_GRACE_WINDOW_MS).toBe(5000);
    const pending: PendingVerdict = {
      gateId: "g1",
      verdict: "approve",
      phase: "grace",
      chosenAt: 1000,
    };
    expect(isWithinGrace(pending, 1000)).toBe(true); // t=0
    expect(isWithinGrace(pending, 1000 + 4999)).toBe(true); // just inside
    expect(isWithinGrace(pending, 1000 + 5000)).toBe(false); // window lapsed → intent sends
    expect(isWithinGrace({ ...pending, phase: "sending" }, 1000)).toBe(false); // no longer undoable
  });
});
