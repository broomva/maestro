/// <reference types="bun" />
import { describe, expect, test } from "bun:test";
import {
  ACTORS,
  ERROR_CODES,
  type ErrorResponse,
  EVENT_NAMESPACES,
  EVENT_TYPES,
  type EventEnvelope,
  eventNamespace,
  isSyntheticEventType,
  isWireEventType,
  SYNTHETIC_EVENT_TYPES,
} from "./events";
import { MAESTRO_PROTOCOL_VERSION, X_MAESTRO_PROTOCOL } from "./version";

describe("event envelope (API.md §stream)", () => {
  test("a session event round-trips", () => {
    const ev: EventEnvelope<{ tool: string }> = {
      seq: 42,
      sessionId: "7f3a",
      ts: "2026-06-25T06:00:14Z",
      actor: "agent",
      type: "tool.call",
      payload: { tool: "edit" },
    };
    expect(JSON.parse(JSON.stringify(ev))).toEqual(ev);
  });

  test("a synthetic event has no session (sessionId nullable — D-DURABILITY)", () => {
    const ev: EventEnvelope = {
      seq: 43,
      sessionId: null,
      ts: "2026-06-25T06:00:15Z",
      actor: "system",
      type: "node.updated",
    };
    expect(JSON.parse(JSON.stringify(ev))).toEqual(ev);
    // sessionId is optional too
    const ev2: EventEnvelope = { seq: 44, ts: "…", actor: "system", type: "schedule.fired" };
    expect(ev2.sessionId).toBeUndefined();
  });

  test("actors are the closed set of four", () => {
    expect(ACTORS).toEqual(["agent", "user", "tool", "system"]);
  });
});

describe("event type namespaces + synthetics", () => {
  test("the six namespaces are pinned (agent added in BRO-1756 for agent.said)", () => {
    expect(EVENT_NAMESPACES).toEqual(["run", "tool", "check", "gate", "budget", "agent"]);
  });

  test("agent.* is a real namespace (agent.said), not a synthetic", () => {
    expect(eventNamespace("agent.said")).toBe("agent");
    expect(isSyntheticEventType("agent.said")).toBe(false);
    expect(isWireEventType("agent.said")).toBe(true);
  });

  test("eventNamespace extracts the family prefix", () => {
    expect(eventNamespace("run.started")).toBe("run");
    expect(eventNamespace("check.verdict")).toBe("check");
    expect(eventNamespace("budget.exhausted")).toBe("budget");
    expect(eventNamespace("node.updated")).toBeNull(); // synthetic, not a namespace
    expect(eventNamespace("nonsense")).toBeNull();
  });

  test("synthetic list is closed and excludes node.created (D-DURABILITY)", () => {
    expect(SYNTHETIC_EVENT_TYPES).toEqual([
      "node.updated",
      "gate.opened",
      "gate.decided",
      "schedule.fired",
    ]);
    expect(isSyntheticEventType("node.updated")).toBe(true);
    expect(isSyntheticEventType("node.created")).toBe(false);
  });

  test("isWireEventType accepts namespaced + synthetic, rejects the rest", () => {
    expect(isWireEventType("run.finished")).toBe(true);
    expect(isWireEventType("node.updated")).toBe(true);
    expect(isWireEventType("node.created")).toBe(false);
    expect(isWireEventType("verify.started")).toBe(false); // canon discrepancy: not admitted
  });

  test("the named catalog uses canon names (D-EVENTNAMES)", () => {
    expect(EVENT_TYPES.RUN_FINISHED).toBe("run.finished");
    expect(EVENT_TYPES.CHECK_VERDICT).toBe("check.verdict"); // not bare `verdict`
    expect(EVENT_TYPES.NODE_UPDATED).toBe("node.updated");
    expect(EVENT_TYPES.BUDGET_REFUSED).toBe("budget.refused"); // HARNESS §3 (BRO-1788)
    expect(EVENT_TYPES.BUDGET_METERED).toBe("budget.metered");
    // HARNESS §2 (BRO-1767): the supervisor's liveness escalation event — a real
    // run.* member, so eventNamespace resolves it and the tailer admits it.
    expect(EVENT_TYPES.RUN_HUNG).toBe("run.hung");
    expect(eventNamespace(EVENT_TYPES.RUN_HUNG)).toBe("run");
    // HARNESS §4 (BRO-1779): run.exiting code vs real exit-code mismatch — a real run.* member
    expect(EVENT_TYPES.RUN_EXIT_MISMATCH).toBe("run.exit_mismatch");
    expect(eventNamespace(EVENT_TYPES.RUN_EXIT_MISMATCH)).toBe("run");
    // HARNESS §5 (BRO-1795): the child's fresh-context restart request — a real run.* member
    expect(EVENT_TYPES.RUN_RESTART_REQUESTED).toBe("run.restart_requested");
    expect(eventNamespace(EVENT_TYPES.RUN_RESTART_REQUESTED)).toBe("run");
    // VERIFIER §7 (BRO-1794): verify.started/judge.result/verify.error FOLDED into check.* — not widened.
    expect(EVENT_TYPES.CHECK_STARTED).toBe("check.started");
    expect(EVENT_TYPES.CHECK_RESULT).toBe("check.result");
    expect(EVENT_TYPES.CHECK_JUDGE).toBe("check.judge");
    expect(EVENT_TYPES.CHECK_ERROR).toBe("check.error");
    for (const t of [EVENT_TYPES.CHECK_STARTED, EVENT_TYPES.CHECK_JUDGE, EVENT_TYPES.CHECK_ERROR]) {
      expect(eventNamespace(t)).toBe("check");
    }
    // every catalog value is a valid wire event type
    for (const type of Object.values(EVENT_TYPES)) {
      expect(isWireEventType(type)).toBe(true);
    }
  });
});

describe("errors (API.md §4)", () => {
  test("the error codes are pinned (5 core + 3 intent-surface, BRO-1820)", () => {
    expect(ERROR_CODES).toEqual([
      "budget_exhausted",
      "lease_held",
      "gate_required",
      "not_found",
      "unauthorized",
      "invalid_intent",
      "unsupported_intent",
      "intent_failed",
    ]);
  });

  test("the error response shape round-trips", () => {
    const err: ErrorResponse = {
      error: { code: "budget_exhausted", message: "per_run_usd exceeded", retryable: false },
    };
    expect(JSON.parse(JSON.stringify(err))).toEqual(err);
  });
});

describe("protocol version (D-NAME)", () => {
  test("header name + version constant", () => {
    expect(X_MAESTRO_PROTOCOL).toBe("x-maestro-protocol");
    expect(MAESTRO_PROTOCOL_VERSION).toBe(1);
  });
});
