/// <reference types="bun" />
import { describe, expect, test } from "bun:test";
import { effectiveProtect } from "@maestro/protocol";
import type { GitRunner } from "./stage0";
import type { VerifierResult } from "./verifier";
import { runVerifier } from "./verifier";

type Row = [number | "-", number | "-", string];
const numstat = (rows: Row[]): string => rows.map(([a, d, p]) => `${a}\t${d}\t${p}`).join("\n");
const fakeGit =
  (stdout: string, code = 0, stderr = ""): GitRunner =>
  async () => ({ code, stdout, stderr });

const BASE = "3f1c9e0";
const BRANCH = "run/abcd1234";
const protect = effectiveProtect(undefined);

describe("verifier-stage0 — runVerifier pipeline (short-circuit invariant)", () => {
  test("a clean Stage 0 with no later stage passes", async () => {
    const v = await runVerifier({
      stage0: {
        cwd: "/repo",
        base: BASE,
        branch: BRANCH,
        protect,
        git: fakeGit(numstat([[3, 1, "src/a.ts"]])),
      },
    });
    expect(v).toEqual({
      verdict: "pass",
      tampering: [],
      diffstat: { files: 1, plus: 3, minus: 1 },
      base: BASE,
    });
  });

  test("Stage 0 pass hands control to `next`, whose verdict is returned", async () => {
    let called = false;
    const nextResult: VerifierResult = {
      verdict: "fail",
      tampering: [],
      diffstat: { files: 1, plus: 3, minus: 1 },
      base: BASE,
    };
    const v = await runVerifier({
      stage0: {
        cwd: "/repo",
        base: BASE,
        branch: BRANCH,
        protect,
        git: fakeGit(numstat([[3, 1, "src/a.ts"]])),
      },
      next: async (s0) => {
        called = true;
        expect(s0.verdict).toBe("pass"); // next only ever sees a passing Stage 0
        return nextResult;
      },
    });
    expect(called).toBe(true);
    expect(v).toBe(nextResult);
  });

  test("a Stage 0 tampering fail is TERMINAL — `next` never runs, the verdict cannot be overridden", async () => {
    let called = false;
    const v = await runVerifier({
      stage0: {
        cwd: "/repo",
        base: BASE,
        branch: BRANCH,
        protect,
        git: fakeGit(numstat([[8, 0, "src/head.test.tsx"]])),
      },
      // A later stage that WOULD flip the verdict to pass — it must never be reached.
      next: async () => {
        called = true;
        return {
          verdict: "pass",
          tampering: [],
          diffstat: { files: 0, plus: 0, minus: 0 },
          base: BASE,
        };
      },
    });
    expect(called).toBe(false);
    expect(v.verdict).toBe("fail");
    expect(v.reason).toBe("tampering");
    expect(v.tampering).toEqual(["src/head.test.tsx"]);
  });

  test("a Stage 0 diff_too_large fail is likewise terminal", async () => {
    let called = false;
    const rows: Row[] = Array.from({ length: 5 }, (_, i) => [1, 0, `src/f${i}.ts`] as Row);
    const v = await runVerifier({
      stage0: {
        cwd: "/repo",
        base: BASE,
        branch: BRANCH,
        protect,
        maxFiles: 2,
        git: fakeGit(numstat(rows)),
      },
      next: async () => {
        called = true;
        return {
          verdict: "pass",
          tampering: [],
          diffstat: { files: 0, plus: 0, minus: 0 },
          base: BASE,
        };
      },
    });
    expect(called).toBe(false);
    expect(v.verdict).toBe("fail");
    expect(v.reason).toBe("diff_too_large");
  });

  test("a Stage 0 infra error is terminal (parks blocked; never a send-back)", async () => {
    let called = false;
    const v = await runVerifier({
      stage0: {
        cwd: "/repo",
        base: BASE,
        branch: BRANCH,
        protect,
        git: fakeGit("", 128, "fatal: bad revision"),
      },
      next: async () => {
        called = true;
        return {
          verdict: "pass",
          tampering: [],
          diffstat: { files: 0, plus: 0, minus: 0 },
          base: BASE,
        };
      },
    });
    expect(called).toBe(false);
    expect(v.verdict).toBe("error");
    expect(v.message).toContain("bad revision");
  });
});
