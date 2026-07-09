/// <reference types="bun" />
import { describe, expect, test } from "bun:test";
import {
  IDEMPOTENCY_KEY_HEADER,
  INTENT_TYPES,
  type Intent,
  KINDS,
  TICK_CAUSES,
  TRIGGER_KINDS,
} from "./intents";

describe("Intent union (API.md §1)", () => {
  // one concrete instance of every variant
  const samples: Intent[] = [
    { type: "new_mission", parentPath: "growth", title: "Fix meta tags", brief: "…", kind: "task" },
    { type: "dispatch", nodeId: "7f3a9c" },
    { type: "approve", gateId: "g1" },
    { type: "revise", gateId: "g1", feedback: "tighten the diff" },
    { type: "block", gateId: "g1", reason: "out of scope" },
    { type: "escalate", gateId: "g1", to: "@alex" },
    { type: "grant", gateId: "g1", capability: "deploy" },
    { type: "kill", sessionId: "7f3a" },
    { type: "set_routine", nodeId: "n1", trigger: { on: "cron", at: "0 6 * * *" } },
    { type: "set_state", nodeId: "n1", state: "canceled" },
    { type: "tick", cause: "worker_return" },
  ];

  test("INTENT_TYPES lists all eleven variants", () => {
    expect(INTENT_TYPES).toHaveLength(11);
    expect(new Set(samples.map((s) => s.type))).toEqual(new Set(INTENT_TYPES));
  });

  test("every variant round-trips through JSON unchanged", () => {
    for (const intent of samples) {
      expect(JSON.parse(JSON.stringify(intent))).toEqual(intent);
      expect(intent.type).toBeTruthy();
    }
  });

  test("the discriminant narrows the union", () => {
    const intent: Intent = { type: "revise", gateId: "g1", feedback: "x" };
    if (intent.type === "revise") {
      expect(intent.feedback).toBe("x");
    } else {
      throw new Error("discriminant failed to narrow");
    }
  });
});

describe("sub-enums", () => {
  test("Kind has the five work scales", () => {
    expect(KINDS).toEqual(["question", "task", "project", "initiative", "routine"]);
  });
  test("TriggerKind matches the schedule table", () => {
    expect(TRIGGER_KINDS).toEqual(["heartbeat", "cron", "hook", "goal"]);
  });
  test("TickCause includes the worker_return amendment", () => {
    expect(TICK_CAUSES).toEqual(["interval", "hook", "manual", "worker_return"]);
    expect((TICK_CAUSES as readonly string[]).includes("worker_return")).toBe(true);
  });
  test("idempotency header name is canon", () => {
    expect(IDEMPOTENCY_KEY_HEADER).toBe("Idempotency-Key");
  });
});
