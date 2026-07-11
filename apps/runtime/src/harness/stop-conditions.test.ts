/// <reference types="bun" />
// stop-conditions.test.ts — BRO-1795 done.check `bun test apps/runtime --filter stop-conditions`.
//
// The stop-condition ENGINE tested on FIXTURES (like BRO-1779/1801 tested dispatch/kill without the
// SDK): each of the three halt conditions triggers INDEPENDENTLY, the fresh-context path writes
// progress.md + emits run.restart_requested + exits 10 fresh_context, and a respawn reading the disk
// memory skips done work. Anti-vacuity [[self-hosting-vacuous-pass]]: every case asserts the EXACT
// decision / reason / event sequence / parsed value — swap a threshold or a mapping and a test fails.

import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { type Budget, DEFAULT_STOP_ON, EVENT_TYPES, isWireEventType } from "@maestro/protocol";
import { DEFAULT_CONTEXT_CEILING_TOKENS, DEFAULT_MAX_ITERATIONS } from "../config";
import {
  type BeatState,
  beatExitEvents,
  evaluateBeat,
  evaluateStopConditions,
  fixPlanPath,
  needsFreshContext,
  type ProgressDoc,
  parseFixPlan,
  parseProgress,
  pendingItems,
  prepareRestart,
  progressPath,
  readFixPlan,
  readProgress,
  tickFixPlan,
  writeFixPlan,
  writeProgress,
} from "./stop-conditions";

const tmps: string[] = [];
afterEach(async () => {
  for (const d of tmps.splice(0)) await rm(d, { recursive: true, force: true });
});
async function makeRunDir(): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), "maestro-stop-cond-"));
  tmps.push(dir);
  return dir;
}

/** A far-from-any-limit baseline; each test perturbs exactly the field under study. */
function base(over: Partial<BeatState> = {}): BeatState {
  return {
    iterations: 0,
    budget: {},
    spentUsd: 0,
    dayUsd: 0,
    recentDiffs: [],
    recentErrors: [],
    contextTokens: 0,
    ceiling: DEFAULT_CONTEXT_CEILING_TOKENS,
    ...over,
  };
}

// ── Iteration cap ──────────────────────────────────────────────────────────────

describe("stop-conditions — iteration cap", () => {
  test("halts at the runtime default (30) and not one beat before", () => {
    expect(evaluateStopConditions(base({ iterations: 29 }))).toEqual({ halt: false });
    expect(evaluateStopConditions(base({ iterations: 30 }))).toEqual({
      halt: true,
      reason: "iteration_cap",
    });
    expect(evaluateStopConditions(base({ iterations: 31 }))).toEqual({
      halt: true,
      reason: "iteration_cap",
    });
    expect(DEFAULT_MAX_ITERATIONS).toBe(30);
  });

  test("contract budget.max_iterations overrides the default (frontmatter wins)", () => {
    const budget: Budget = { max_iterations: 5 };
    expect(evaluateStopConditions(base({ iterations: 4, budget }))).toEqual({ halt: false });
    expect(evaluateStopConditions(base({ iterations: 5, budget }))).toEqual({
      halt: true,
      reason: "iteration_cap",
    });
  });

  test("runtime maxIterationsDefault wins over the hard default but loses to the contract", () => {
    // No contract cap → the threaded runtime default (10) applies.
    expect(evaluateStopConditions(base({ iterations: 10, maxIterationsDefault: 10 }))).toEqual({
      halt: true,
      reason: "iteration_cap",
    });
    // Contract cap beats the runtime default: max_iterations 50 with runtime default 10 → 50 wins.
    const state = base({
      iterations: 12,
      maxIterationsDefault: 10,
      budget: { max_iterations: 50 },
    });
    expect(evaluateStopConditions(state)).toEqual({ halt: false });
  });
});

// ── No-progress ──────────────────────────────────────────────────────────────

describe("stop-conditions — no-progress halt", () => {
  test("N=3 consecutive EMPTY diffs halt; 2 empty + a real diff does not", () => {
    expect(evaluateStopConditions(base({ recentDiffs: ["", "", ""] }))).toEqual({
      halt: true,
      reason: "no_progress",
    });
    // A real change in the window breaks the stall.
    expect(evaluateStopConditions(base({ recentDiffs: ["", "3 files +9-1", ""] }))).toEqual({
      halt: false,
    });
    // Only the LAST N matter: an old empty run followed by progress is fine.
    expect(evaluateStopConditions(base({ recentDiffs: ["", "", "", "a", "b", "c"] }))).toEqual({
      halt: false,
    });
  });

  test("fewer than N beats never trip no-progress", () => {
    expect(evaluateStopConditions(base({ recentDiffs: ["", ""] }))).toEqual({ halt: false });
    expect(evaluateStopConditions(base({ recentErrors: ["boom", "boom"] }))).toEqual({
      halt: false,
    });
  });

  test("N identical NON-EMPTY errors halt (agreeing with itself); differing errors do not", () => {
    expect(
      evaluateStopConditions(base({ recentErrors: ["ETIMEDOUT", "ETIMEDOUT", "ETIMEDOUT"] })),
    ).toEqual({ halt: true, reason: "no_progress" });
    expect(evaluateStopConditions(base({ recentErrors: ["a", "b", "c"] }))).toEqual({
      halt: false,
    });
    // Empty entries are "no error" — three of them is NOT an identical-error stall (diffs own emptiness).
    expect(evaluateStopConditions(base({ recentErrors: ["", "", ""] }))).toEqual({ halt: false });
  });

  test("a custom noProgressN window is honored", () => {
    expect(evaluateStopConditions(base({ recentDiffs: ["", ""], noProgressN: 2 }))).toEqual({
      halt: true,
      reason: "no_progress",
    });
    expect(evaluateStopConditions(base({ recentDiffs: ["", ""], noProgressN: 5 }))).toEqual({
      halt: false,
    });
  });
});

// ── Budget ──────────────────────────────────────────────────────────────────

describe("stop-conditions — budget exhausted", () => {
  test("per_run dollar cap halts at/over the cap, not under", () => {
    const budget: Budget = { per_run_usd: 1.0 };
    expect(evaluateStopConditions(base({ budget, spentUsd: 0.99 }))).toEqual({ halt: false });
    expect(evaluateStopConditions(base({ budget, spentUsd: 1.0 }))).toEqual({
      halt: true,
      reason: "budget",
    });
    expect(evaluateStopConditions(base({ budget, spentUsd: 1.5 }))).toEqual({
      halt: true,
      reason: "budget",
    });
  });

  test("per_day dollar cap halts independently of per_run", () => {
    const budget: Budget = { per_day_usd: 20 };
    expect(evaluateStopConditions(base({ budget, dayUsd: 19.99 }))).toEqual({ halt: false });
    expect(evaluateStopConditions(base({ budget, dayUsd: 20 }))).toEqual({
      halt: true,
      reason: "budget",
    });
  });

  test("no dollar caps set → budget never fires", () => {
    expect(evaluateStopConditions(base({ spentUsd: 999, dayUsd: 999 }))).toEqual({ halt: false });
  });
});

// ── stop_on narrowing + precedence ───────────────────────────────────────────

describe("stop-conditions — stop_on + precedence", () => {
  test("stop_on narrows the active set: only no_progress → an at-cap over-budget beat does NOT halt", () => {
    const state = base({
      iterations: 99,
      budget: { max_iterations: 30, per_run_usd: 1 },
      spentUsd: 5,
      stopOn: ["no_progress"],
    });
    expect(evaluateStopConditions(state)).toEqual({ halt: false });
    // …and no_progress still fires when its own condition is met under the same narrowing.
    expect(evaluateStopConditions({ ...state, recentDiffs: ["", "", ""] })).toEqual({
      halt: true,
      reason: "no_progress",
    });
  });

  test("canon order (cap → no_progress → budget) resolves a beat that trips all three", () => {
    const all = base({
      iterations: 30, // cap
      recentDiffs: ["", "", ""], // no_progress
      budget: { per_run_usd: 1 },
      spentUsd: 2, // budget
    });
    expect(evaluateStopConditions(all)).toEqual({ halt: true, reason: "iteration_cap" });
    // Drop cap → no_progress wins next.
    expect(evaluateStopConditions({ ...all, iterations: 0 })).toEqual({
      halt: true,
      reason: "no_progress",
    });
    // Drop cap + no_progress → budget is last.
    expect(evaluateStopConditions({ ...all, iterations: 0, recentDiffs: [] })).toEqual({
      halt: true,
      reason: "budget",
    });
    expect(DEFAULT_STOP_ON).toEqual(["cap", "no_progress", "budget"]);
  });
});

// ── Fresh-context restart ─────────────────────────────────────────────────────

describe("stop-conditions — fresh-context ceiling", () => {
  test("needsFreshContext fires at/over the ceiling, is disabled at ceiling <= 0", () => {
    expect(needsFreshContext(base({ contextTokens: 159_999 }))).toBe(false);
    expect(needsFreshContext(base({ contextTokens: DEFAULT_CONTEXT_CEILING_TOKENS }))).toBe(true);
    expect(needsFreshContext(base({ contextTokens: 999_999, ceiling: 0 }))).toBe(false);
  });
});

// ── evaluateBeat composition (halt beats restart) ────────────────────────────

describe("stop-conditions — evaluateBeat", () => {
  test("continue when nothing trips", () => {
    expect(evaluateBeat(base({ iterations: 3, contextTokens: 100 }))).toEqual({
      action: "continue",
    });
  });

  test("restart when only the ceiling is reached", () => {
    expect(evaluateBeat(base({ contextTokens: DEFAULT_CONTEXT_CEILING_TOKENS }))).toEqual({
      action: "restart",
      reason: "fresh_context",
    });
  });

  test("HALT wins over restart when both a stop condition AND the ceiling trip", () => {
    const state = base({ iterations: 30, contextTokens: DEFAULT_CONTEXT_CEILING_TOKENS });
    expect(evaluateBeat(state)).toEqual({ action: "halt", reason: "iteration_cap" });
  });
});

// ── The child's terminal event sequence ──────────────────────────────────────

describe("stop-conditions — beatExitEvents", () => {
  test("continue emits nothing", () => {
    expect(beatExitEvents({ action: "continue" }, { iteration: 4 })).toEqual([]);
  });

  test("budget halt emits budget.exhausted THEN run.exiting {code:10, reason:budget}", () => {
    expect(beatExitEvents({ action: "halt", reason: "budget" }, { iteration: 7 })).toEqual([
      { actor: "system", type: EVENT_TYPES.BUDGET_EXHAUSTED, payload: { iteration: 7 } },
      { actor: "system", type: EVENT_TYPES.RUN_EXITING, payload: { code: 10, reason: "budget" } },
    ]);
  });

  test("cap / no_progress halts emit ONLY run.exiting with their reason", () => {
    expect(beatExitEvents({ action: "halt", reason: "iteration_cap" }, { iteration: 30 })).toEqual([
      {
        actor: "system",
        type: EVENT_TYPES.RUN_EXITING,
        payload: { code: 10, reason: "iteration_cap" },
      },
    ]);
    expect(beatExitEvents({ action: "halt", reason: "no_progress" }, { iteration: 3 })).toEqual([
      {
        actor: "system",
        type: EVENT_TYPES.RUN_EXITING,
        payload: { code: 10, reason: "no_progress" },
      },
    ]);
  });

  test("restart emits run.restart_requested THEN run.exiting fresh_context", () => {
    expect(
      beatExitEvents({ action: "restart", reason: "fresh_context" }, { iteration: 12 }),
    ).toEqual([
      {
        actor: "system",
        type: EVENT_TYPES.RUN_RESTART_REQUESTED,
        payload: { iteration: 12, reason: "context_ceiling" },
      },
      {
        actor: "system",
        type: EVENT_TYPES.RUN_EXITING,
        payload: { code: 10, reason: "fresh_context" },
      },
    ]);
  });

  test("every produced event.type is a valid wire event type (guards the new run.restart_requested)", () => {
    const decisions = [
      { action: "halt", reason: "budget" },
      { action: "halt", reason: "iteration_cap" },
      { action: "halt", reason: "no_progress" },
      { action: "restart", reason: "fresh_context" },
    ] as const;
    for (const d of decisions) {
      for (const ev of beatExitEvents(d, { iteration: 1 })) {
        expect(isWireEventType(ev.type)).toBe(true);
      }
    }
  });
});

// ── Disk memory — progress.md round-trip ─────────────────────────────────────

describe("stop-conditions — progress.md", () => {
  const doc: ProgressDoc = {
    session: "7f3a",
    iteration: 12,
    updated: "2026-07-11T02:00:00.000Z",
    stateOfTheWorld: "Migrated the auth module; 2 endpoints left to port.",
    whatsLeft: ["port /login", "port /logout"],
  };

  test("write → read round-trips exactly, and lands at runs/<dir>/progress.md", async () => {
    const dir = await makeRunDir();
    await writeProgress(dir, doc);
    expect(progressPath(dir)).toBe(join(dir, "progress.md"));
    expect(await readProgress(dir)).toEqual(doc);
  });

  test("the passed `updated` is honored verbatim (no ambient clock)", async () => {
    const dir = await makeRunDir();
    await writeProgress(dir, doc);
    const back = await readProgress(dir);
    expect(back?.updated).toBe("2026-07-11T02:00:00.000Z");
  });

  test("an empty whatsLeft round-trips to []", async () => {
    const dir = await makeRunDir();
    await writeProgress(dir, { ...doc, whatsLeft: [] });
    expect((await readProgress(dir))?.whatsLeft).toEqual([]);
  });

  test("absent file → null; a file with no machine block → null", async () => {
    const dir = await makeRunDir();
    expect(await readProgress(dir)).toBeNull();
    await writeFile(progressPath(dir), "# just prose, no meta block\n", "utf8");
    expect(await readProgress(dir)).toBeNull();
    expect(parseProgress("nonsense")).toBeNull();
  });
});

// ── Disk memory — fix_plan.md tick + "skip done work" ─────────────────────────

describe("stop-conditions — fix_plan.md skip-done-work", () => {
  test("parse reads checkbox state; pendingItems returns only the undone", () => {
    const items = parseFixPlan(
      ["# Fix plan", "", "- [x] set up schema", "- [ ] wire the route", "- [ ] add tests"].join(
        "\n",
      ),
    );
    expect(items).toEqual([
      { text: "set up schema", done: true },
      { text: "wire the route", done: false },
      { text: "add tests", done: false },
    ]);
    expect(pendingItems(items).map((i) => i.text)).toEqual(["wire the route", "add tests"]);
  });

  test("write → read round-trips; a respawn sees the pending set shrink after a tick", async () => {
    const dir = await makeRunDir();
    await writeFixPlan(dir, [
      { text: "wire the route", done: false },
      { text: "add tests", done: false },
    ]);
    // Respawn #1 sees both pending.
    expect(pendingItems(await readFixPlan(dir)).map((i) => i.text)).toEqual([
      "wire the route",
      "add tests",
    ]);
    // The child finishes one item and ticks it.
    expect(await tickFixPlan(dir, ["wire the route"])).toBe(1);
    // Respawn #2 skips the done work — only the untouched item remains.
    expect(pendingItems(await readFixPlan(dir)).map((i) => i.text)).toEqual(["add tests"]);
  });

  test("tickFixPlan is idempotent, matches by exact text, preserves append-only history", async () => {
    const dir = await makeRunDir();
    // A file with append-only verifier history (## attempt sections) + prose the parser must keep.
    const original = [
      "# Fix plan",
      "",
      "- [ ] port /login",
      "",
      "## Verifier — attempt 2 failed (2026-07-07T06:14Z)",
      "- [ ] tests: 3 failures in head.test.tsx (see runs/run-7f3a/checks/tests.log)",
    ].join("\n");
    await writeFile(fixPlanPath(dir), original, "utf8");
    // Tick one; a non-matching text ticks nothing (returns the count of FLIPS = 1).
    expect(await tickFixPlan(dir, ["port /login", "not a real item"])).toBe(1);
    // Idempotent: the item is already done, so a second tick flips nothing (0).
    expect(await tickFixPlan(dir, ["port /login"])).toBe(0);
    const after = await readFile(fixPlanPath(dir), "utf8");
    // The heading + evidence line survive byte-for-byte; only the checkbox flipped.
    expect(after).toContain("## Verifier — attempt 2 failed (2026-07-07T06:14Z)");
    expect(after).toContain("- [x] port /login");
    expect(after).toContain("- [ ] tests: 3 failures in head.test.tsx");
  });

  test("tickFixPlan on a missing file / empty request is a 0 no-op", async () => {
    const dir = await makeRunDir();
    expect(await tickFixPlan(dir, ["anything"])).toBe(0);
    await writeFixPlan(dir, [{ text: "x", done: false }]);
    expect(await tickFixPlan(dir, [])).toBe(0);
  });
});

// ── prepareRestart — the lossless restart composition ─────────────────────────

describe("stop-conditions — prepareRestart", () => {
  test("writes progress.md, ticks the finished item, returns the restart signal events", async () => {
    const dir = await makeRunDir();
    await writeFixPlan(dir, [
      { text: "port /login", done: false },
      { text: "port /logout", done: false },
    ]);
    const progress: ProgressDoc = {
      session: "7f3a",
      iteration: 8,
      updated: "2026-07-11T02:30:00.000Z",
      stateOfTheWorld: "Ported /login; hit the context ceiling before /logout.",
      whatsLeft: ["port /logout"],
    };
    const events = await prepareRestart(dir, { progress, doneTexts: ["port /login"] });

    // The signal events (the exact fresh-context sequence).
    expect(events).toEqual([
      {
        actor: "system",
        type: EVENT_TYPES.RUN_RESTART_REQUESTED,
        payload: { iteration: 8, reason: "context_ceiling" },
      },
      {
        actor: "system",
        type: EVENT_TYPES.RUN_EXITING,
        payload: { code: 10, reason: "fresh_context" },
      },
    ]);
    // The respawn (a fresh read of disk memory) resumes exactly where this left off + skips done work.
    expect(await readProgress(dir)).toEqual(progress);
    expect(pendingItems(await readFixPlan(dir)).map((i) => i.text)).toEqual(["port /logout"]);
  });
});
