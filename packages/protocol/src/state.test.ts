/// <reference types="bun" />
import { describe, expect, test } from "bun:test";
import {
  ATTENTION_STATES,
  GATE_VERDICTS,
  GateRequiredError,
  IllegalTransitionError,
  isAttentionState,
  isLegalTransition,
  isTerminalState,
  ORCH_STATES,
  type OrchState,
  resolveGateVerdict,
  TERMINAL_STATES,
  TRANSITIONS,
  transition,
} from "./state";

describe("OrchState enum (D-ENUM)", () => {
  test("has exactly the eight canon states, no `queued`", () => {
    expect(ORCH_STATES).toEqual([
      "proposed",
      "reviewing",
      "triggered",
      "running",
      "blocked",
      "review",
      "done",
      "canceled",
    ]);
    expect((ORCH_STATES as readonly string[]).includes("queued")).toBe(false);
  });

  test("attention set is blocked + review", () => {
    expect(ATTENTION_STATES).toEqual(["blocked", "review"]);
    expect(isAttentionState("blocked")).toBe(true);
    expect(isAttentionState("review")).toBe(true);
    expect(isAttentionState("running")).toBe(false);
  });

  test("terminal states are done + canceled", () => {
    expect(TERMINAL_STATES).toEqual(["done", "canceled"]);
    expect(isTerminalState("done")).toBe(true);
    expect(isTerminalState("canceled")).toBe(true);
    expect(isTerminalState("review")).toBe(false);
  });
});

describe("transition machine — legal edges (PATTERNS §7, FLOWS F1–F8)", () => {
  const legal: [OrchState, OrchState][] = [
    ["proposed", "reviewing"],
    ["proposed", "triggered"],
    ["proposed", "running"],
    ["reviewing", "triggered"],
    ["triggered", "running"],
    ["running", "review"],
    ["running", "blocked"],
    ["blocked", "triggered"],
    ["review", "triggered"],
    ["review", "canceled"],
  ];
  for (const [from, to] of legal) {
    test(`${from} → ${to} is legal`, () => {
      expect(transition(from, to)).toBe(to);
      expect(isLegalTransition(from, to)).toBe(true);
    });
  }
});

describe("transition machine — illegal edges throw", () => {
  const illegal: [OrchState, OrchState][] = [
    ["proposed", "done"], // cannot skip the run
    ["done", "running"], // terminal
    ["canceled", "running"], // terminal
    ["running", "canceled"], // kill goes to blocked, not canceled (F8)
    ["triggered", "review"], // must run first
    ["review", "running"], // gate does not resume in place
    ["blocked", "running"], // resume redispatches via triggered (F9)
    ["proposed", "blocked"],
    ["reviewing", "running"], // must pass through triggered
  ];
  for (const [from, to] of illegal) {
    test(`${from} → ${to} throws IllegalTransitionError`, () => {
      expect(() => transition(from, to)).toThrow(IllegalTransitionError);
      expect(isLegalTransition(from, to)).toBe(false);
    });
  }

  test("every terminal state has no outgoing edges", () => {
    for (const t of TERMINAL_STATES) {
      expect(TRANSITIONS[t]).toEqual([]);
    }
  });

  test("the error carries from/to", () => {
    try {
      transition("done", "running");
      throw new Error("expected throw");
    } catch (e) {
      expect(e).toBeInstanceOf(IllegalTransitionError);
      expect((e as IllegalTransitionError).from).toBe("done");
      expect((e as IllegalTransitionError).to).toBe("running");
    }
  });
});

describe("guarded edge — review → done requires an approve verdict (PATTERNS §7)", () => {
  test("throws GateRequiredError without a verdict", () => {
    expect(() => transition("review", "done")).toThrow(GateRequiredError);
  });
  test("throws with a non-approve verdict", () => {
    expect(() => transition("review", "done", { verdict: "revise" })).toThrow(GateRequiredError);
    expect(() => transition("review", "done", { verdict: "block" })).toThrow(GateRequiredError);
  });
  test("succeeds with an approve verdict", () => {
    expect(transition("review", "done", { verdict: "approve" })).toBe("done");
  });
});

describe("guarded edge — running → done requires gate:auto (D-AUTODONE, FLOWS F4)", () => {
  test("throws GateRequiredError under gate:human (must park at review)", () => {
    expect(() => transition("running", "done")).toThrow(GateRequiredError);
    expect(() => transition("running", "done", { gate: "human" })).toThrow(GateRequiredError);
  });
  test("succeeds under gate:auto", () => {
    expect(transition("running", "done", { gate: "auto" })).toBe("done");
  });
  test("running → review needs no gate (the human path)", () => {
    expect(transition("running", "review")).toBe("review");
  });
});

describe("resolveGateVerdict (D-GATE, FLOWS F5)", () => {
  test("the four verdicts map per canon", () => {
    expect(resolveGateVerdict("review", "approve")).toBe("done");
    expect(resolveGateVerdict("review", "revise")).toBe("triggered");
    expect(resolveGateVerdict("review", "block")).toBe("canceled");
    expect(resolveGateVerdict("review", "escalate")).toBe("review");
  });
  test("GATE_VERDICTS is the closed set of four", () => {
    expect(GATE_VERDICTS).toEqual(["approve", "revise", "block", "escalate"]);
  });
  test("a verdict off the gate throws", () => {
    expect(() => resolveGateVerdict("running", "approve")).toThrow(IllegalTransitionError);
  });
});
