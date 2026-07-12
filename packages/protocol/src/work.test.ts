/// <reference types="bun" />
import { describe, expect, test } from "bun:test";
import {
  assertContractGate,
  DEFAULT_CHECK_TIMEOUT_S,
  DEFAULT_DIFF_MAX_FILES,
  DEFAULT_DIFF_MAX_LINES,
  DEFAULT_PROTECT_GLOBS,
  type Done,
  EXIT_REASONS,
  effectiveProtect,
  hasCheck,
  InvalidContractError,
  isValidRubricRef,
  MAX_CHECK_TIMEOUT_S,
  normalizeChecks,
  SESSION_STATUSES,
  STOP_CONDITIONS,
  VERIFIER_MAX_ATTEMPTS,
  type VerdictReceipt,
  type WorkContract,
} from "./work";

describe("done schema (VERIFIER §1)", () => {
  test("a bare string check is sugar for one named check", () => {
    expect(normalizeChecks("pnpm test")).toEqual([{ name: "check", run: "pnpm test" }]);
  });
  test("a named-check array passes through", () => {
    const checks = [
      { name: "tests", run: "bun test", timeout_s: 600 },
      { name: "types", run: "tsc", required: false },
    ];
    expect(normalizeChecks(checks)).toEqual(checks);
  });
  test("hasCheck reflects a runnable check", () => {
    expect(hasCheck(undefined)).toBe(false);
    expect(hasCheck({ check: "bun test" })).toBe(true);
    expect(hasCheck({ check: [] })).toBe(false);
  });
  test("the numeric defaults match canon", () => {
    expect(DEFAULT_CHECK_TIMEOUT_S).toBe(600);
    expect(MAX_CHECK_TIMEOUT_S).toBe(1800);
    expect(DEFAULT_DIFF_MAX_FILES).toBe(30);
    expect(DEFAULT_DIFF_MAX_LINES).toBe(2000);
    expect(VERIFIER_MAX_ATTEMPTS).toBe(5);
  });
});

describe("gate-pairing rule (VERIFIER §1)", () => {
  test("gate:auto with no check is rejected", () => {
    expect(() => assertContractGate({ gate: "auto" })).toThrow(InvalidContractError);
    expect(() => assertContractGate({ gate: "auto", done: { check: [] } })).toThrow(
      InvalidContractError,
    );
  });
  test("gate:auto with a check is allowed", () => {
    expect(() => assertContractGate({ gate: "auto", done: { check: "bun test" } })).not.toThrow();
  });
  test("gate:human is always allowed (judge-only ok)", () => {
    expect(() => assertContractGate({ gate: "human" })).not.toThrow();
    expect(() =>
      assertContractGate({ gate: "human", done: { check: [], judge: "r.md" } }),
    ).not.toThrow();
  });
});

describe("protect floor (VERIFIER §1)", () => {
  test("defaults are the three-glob floor", () => {
    expect(DEFAULT_PROTECT_GLOBS).toEqual(["**/*.test.*", "**/rubric.md", "**/_work.md"]);
  });
  test("author additions union with the floor, never shrink it", () => {
    const done: Done = { check: "x", protect: [".github/**", "package.json"] };
    const eff = effectiveProtect(done);
    for (const g of DEFAULT_PROTECT_GLOBS) {
      expect(eff).toContain(g);
    }
    expect(eff).toContain(".github/**");
    expect(eff).toContain("package.json");
  });
  test("dedupes overlap with the floor", () => {
    const eff = effectiveProtect({ check: "x", protect: ["**/_work.md"] });
    expect(eff.filter((g) => g === "**/_work.md")).toHaveLength(1);
  });
});

describe("done-schema — rubric ref (isValidRubricRef, VERIFIER §1/§3)", () => {
  test("a relative .md path within the tree is valid", () => {
    expect(isValidRubricRef("rubric.md")).toBe(true);
    expect(isValidRubricRef("checks/rubric.md")).toBe(true);
    expect(isValidRubricRef("r.md")).toBe(true);
  });
  test("empty or whitespace-wrapped refs are malformed", () => {
    expect(isValidRubricRef("")).toBe(false);
    expect(isValidRubricRef("   ")).toBe(false);
    expect(isValidRubricRef(" rubric.md")).toBe(false);
    expect(isValidRubricRef("rubric.md ")).toBe(false);
  });
  test("a non-.md file is malformed", () => {
    expect(isValidRubricRef("rubric.yaml")).toBe(false);
    expect(isValidRubricRef("rubric")).toBe(false);
    expect(isValidRubricRef("rubric.md.txt")).toBe(false);
  });
  test("absolute paths are malformed (posix + windows)", () => {
    expect(isValidRubricRef("/etc/rubric.md")).toBe(false);
    expect(isValidRubricRef("C:\\rubric.md")).toBe(false);
  });
  test("parent-traversal is malformed (escapes the worktree — a tamper vector)", () => {
    expect(isValidRubricRef("../rubric.md")).toBe(false);
    expect(isValidRubricRef("checks/../../rubric.md")).toBe(false);
    expect(isValidRubricRef("a/../b/rubric.md")).toBe(false);
  });
});

describe("session + exit enums", () => {
  test("session status is the five-value set (DATA-MODEL §B.3)", () => {
    expect(SESSION_STATUSES).toEqual(["running", "blocked", "review", "done", "canceled"]);
  });
  test("exit reasons include fresh_context (D-EVENTNAMES)", () => {
    expect(EXIT_REASONS).toEqual([
      "budget",
      "iteration_cap",
      "no_progress",
      "user_stop",
      "fresh_context",
    ]);
  });
  test("stop conditions are cap/no_progress/budget", () => {
    expect(STOP_CONDITIONS).toEqual(["cap", "no_progress", "budget"]);
  });
});

describe("wire-shaped shapes round-trip", () => {
  test("a full work contract", () => {
    const contract: WorkContract = {
      id: "7f3a9c",
      kind: "task",
      state: "running",
      owner: "@alex",
      gate: "human",
      budget: { per_run_usd: 5, per_day_usd: 20, max_iterations: 40 },
      done: {
        check: [{ name: "tests", run: "bun test", timeout_s: 600 }],
        judge: "rubric.md",
        protect: [".github/**"],
        diff: { max_files: 30, max_lines: 2000 },
        stop_on: ["cap", "no_progress", "budget"],
      },
      created: "2026-06-25",
      updated: "2026-06-25",
    };
    expect(JSON.parse(JSON.stringify(contract))).toEqual(contract);
  });

  test("a verdict receipt (check.verdict payload)", () => {
    const receipt: VerdictReceipt = {
      verdict: "fail",
      attempt: 2,
      base: "3f1c9e0",
      diffstat: { files: 4, plus: 122, minus: 8 },
      tampering: [],
      checks: [{ name: "tests", ok: false, exit: 1, duration_s: 41, log: "checks/tests.log" }],
      judge: { score: null },
    };
    expect(JSON.parse(JSON.stringify(receipt))).toEqual(receipt);
  });
});
