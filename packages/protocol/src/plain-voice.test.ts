/// <reference types="bun" />
import { describe, expect, test } from "bun:test";
import {
  compareByAttention,
  PLAIN_VOICE,
  type PlainVoiceEntry,
  plainVoice,
  plainVoiceForNode,
  STANDING,
  WK_GROUP_ORDER,
} from "./plain-voice";
import { ORCH_STATES, type OrchState } from "./state";

describe("plain-voice mapping (DATA-MODEL §B.2, data-contract.md, D-ENUM)", () => {
  test("every OrchState has a mapping", () => {
    for (const s of ORCH_STATES) {
      expect(PLAIN_VOICE[s]).toBeDefined();
    }
  });

  test("the three queued states collapse to Queued", () => {
    expect(plainVoice("proposed")).toBe("Queued");
    expect(plainVoice("reviewing")).toBe("Queued");
    expect(plainVoice("triggered")).toBe("Queued");
  });

  test("the exact canon labels", () => {
    expect(plainVoice("running")).toBe("Running");
    expect(plainVoice("blocked")).toBe("Stuck");
    expect(plainVoice("review")).toBe("Needs you");
    expect(plainVoice("done")).toBe("Done");
    // canceled renders under the Done group (D-ENUM)
    expect(plainVoice("canceled")).toBe("Done");
  });

  test("Needs you is accent-blue, never red (D-COLOR)", () => {
    expect(PLAIN_VOICE.review.dot).toBe("accent-blue");
    expect(PLAIN_VOICE.review.tone).toBe("accent");
    // no state uses a red/danger dot
    for (const s of ORCH_STATES) {
      expect(["gray", "info", "warning", "accent-blue", "success", "pulse"]).toContain(
        PLAIN_VOICE[s].dot,
      );
    }
  });

  test("the full label/tone/dot table matches canon for every state (DATA-MODEL §B.2)", () => {
    // The complete mapping — every color-carrying field pinned, not spot-checked.
    const canon = {
      proposed: { label: "Queued", tone: "muted", dot: "gray" },
      reviewing: { label: "Queued", tone: "muted", dot: "gray" },
      triggered: { label: "Queued", tone: "muted", dot: "gray" },
      running: { label: "Running", tone: "active", dot: "info" },
      blocked: { label: "Stuck", tone: "warn", dot: "warning" },
      review: { label: "Needs you", tone: "accent", dot: "accent-blue" },
      done: { label: "Done", tone: "success", dot: "success" },
      canceled: { label: "Done", tone: "muted", dot: "gray" },
    } satisfies Record<OrchState, PlainVoiceEntry>;
    for (const s of ORCH_STATES) {
      expect(PLAIN_VOICE[s]).toEqual(canon[s]);
    }
  });
});

describe("Standing overlay (routine between fires)", () => {
  test("a non-running, non-terminal routine reads Standing", () => {
    expect(plainVoiceForNode("proposed", "routine")).toEqual(STANDING);
    expect(plainVoiceForNode("triggered", "routine")).toEqual(STANDING);
    expect(STANDING.label).toBe("Standing");
    expect(STANDING.dot).toBe("pulse");
  });
  test("a running routine reads Running, not Standing", () => {
    expect(plainVoiceForNode("running", "routine")).toEqual(PLAIN_VOICE.running);
  });
  test("an attention-state routine surfaces attention, never Standing (P20 regression)", () => {
    // A routine parked at the gate or stuck must NOT be masked as calm Standing —
    // "Needs you"/"Stuck" are the load-bearing signals. No `isRunning` hack needed:
    // at review/blocked the run has settled (FLOWS F5/F8/F2/F3), so isRunning is
    // honestly false, yet the node must still read its attention voice.
    expect(plainVoiceForNode("review", "routine")).toEqual(PLAIN_VOICE.review);
    expect(plainVoiceForNode("blocked", "routine")).toEqual(PLAIN_VOICE.blocked);
    expect(plainVoiceForNode("review", "routine").label).toBe("Needs you");
    expect(plainVoiceForNode("blocked", "routine").label).toBe("Stuck");
  });
  test("a terminal routine reads its terminal voice, not Standing", () => {
    expect(plainVoiceForNode("done", "routine")).toEqual(PLAIN_VOICE.done);
    expect(plainVoiceForNode("canceled", "routine")).toEqual(PLAIN_VOICE.canceled);
  });
  test("non-routine kinds never get the overlay", () => {
    expect(plainVoiceForNode("proposed", "task")).toEqual(PLAIN_VOICE.proposed);
  });
});

describe("attention order (D-ORDER, WK_GROUP_ORDER)", () => {
  test("review-first, then blocked", () => {
    expect(WK_GROUP_ORDER[0]).toBe("review");
    expect(WK_GROUP_ORDER[1]).toBe("blocked");
    expect(WK_GROUP_ORDER).toEqual([
      "review",
      "blocked",
      "running",
      "triggered",
      "reviewing",
      "proposed",
      "done",
      "canceled",
    ]);
  });

  test("every OrchState appears exactly once", () => {
    const set = new Set<OrchState>(WK_GROUP_ORDER);
    expect(set.size).toBe(ORCH_STATES.length);
    for (const s of ORCH_STATES) {
      expect(set.has(s)).toBe(true);
    }
  });

  test("comparator sorts review ahead of proposed and done", () => {
    const board: OrchState[] = ["done", "proposed", "review", "running"];
    expect([...board].sort(compareByAttention)).toEqual(["review", "running", "proposed", "done"]);
  });
});
