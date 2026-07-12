/// <reference types="bun" />
import { describe, expect, test } from "bun:test";
import type { DoneCheck } from "@maestro/protocol";
import type { SandboxSpawnContext } from "../sandbox/sandbox";
import type { CheckProcResult, CheckRunner } from "./checks";
import { defaultCheckRunner, liftStage1, MAX_EVIDENCE_BYTES, runChecks } from "./checks";

const PHASE1: SandboxSpawnContext = { cwd: "/wt", commandPrefix: [], env: {} };

// A scripted runner: returns the given outcomes in order, recording every call it received.
function scripted(outcomes: CheckProcResult[]) {
  const calls: { argv: string[]; cwd: string; env: Record<string, string>; timeoutMs: number }[] =
    [];
  let i = 0;
  const run: CheckRunner = async (argv, opts) => {
    calls.push({ argv, ...opts });
    return outcomes[i++] ?? pass();
  };
  return { run, calls };
}
// noUncheckedIndexedAccess-safe accessors for assertions.
function only<T>(arr: T[]): T {
  const v = arr[0];
  if (v === undefined) throw new Error("expected at least one element");
  return v;
}
function need(logs: Record<string, string>, key: string): string {
  const v = logs[key];
  if (v === undefined) throw new Error(`no log at ${key}`);
  return v;
}
const pass = (stdout = "ok"): CheckProcResult => ({ code: 0, stdout, stderr: "", timedOut: false });
const fail = (code = 1, stderr = "boom"): CheckProcResult => ({
  code,
  stdout: "",
  stderr,
  timedOut: false,
});
const timeout = (): CheckProcResult => ({ code: -1, stdout: "", stderr: "", timedOut: true });
const spawnErr = (m = "sh: ENOENT"): CheckProcResult => ({
  code: -1,
  stdout: "",
  stderr: "",
  timedOut: false,
  spawnError: m,
});

// A capturing writeLog + a monotonic clock (2s per check) for deterministic durations.
function harness() {
  const logs: Record<string, string> = {};
  const writeLog = async (relPath: string, content: string) => {
    logs[relPath] = content;
  };
  let t = 0;
  const now = () => {
    const v = t;
    t += 2000;
    return v;
  };
  return { logs, writeLog, now };
}

describe("verifier-checks — runChecks (Stage 1 deterministic oracle)", () => {
  test("all required checks passing → verdict pass, ok results, evidence logged", async () => {
    const { run, calls } = scripted([pass("tests ok"), pass("lint ok")]);
    const { logs, writeLog, now } = harness();
    const checks: DoneCheck[] = [
      { name: "tests", run: "bun test" },
      { name: "lint", run: "biome ci" },
    ];
    const r = await runChecks({ checks, spawnContext: PHASE1, runDir: "/rd", run, writeLog, now });
    expect(r.verdict).toBe("pass");
    expect(r.checks.map((c) => [c.name, c.ok, c.exit, c.duration_s, c.log])).toEqual([
      ["tests", true, 0, 2, "checks/tests.log"],
      ["lint", true, 0, 2, "checks/lint.log"],
    ]);
    expect(logs["checks/tests.log"]).toBe("tests ok");
    // the check ran via the shell in the worktree cwd, no phase-2 prefix
    expect(only(calls).argv).toEqual(["sh", "-c", "bun test"]);
    expect(only(calls).cwd).toBe("/wt");
  });

  test("a required nonzero exit → verdict fail, reason fail, exit + stderr evidence", async () => {
    const { run } = scripted([fail(2, "3 failures")]);
    const { logs, writeLog, now } = harness();
    const r = await runChecks({
      checks: [{ name: "tests", run: "bun test" }],
      spawnContext: PHASE1,
      runDir: "/rd",
      run,
      writeLog,
      now,
    });
    expect(r.verdict).toBe("fail");
    expect(r.checks[0]).toMatchObject({ name: "tests", ok: false, exit: 2, reason: "fail" });
    expect(logs["checks/tests.log"]).toBe("3 failures");
  });

  test("a check killed on timeout → verdict fail, reason timeout", async () => {
    const { run } = scripted([timeout()]);
    const { writeLog, now } = harness();
    const r = await runChecks({
      checks: [{ name: "slow", run: "sleep 999" }],
      spawnContext: PHASE1,
      runDir: "/rd",
      run,
      writeLog,
      now,
    });
    expect(r.verdict).toBe("fail");
    expect(r.checks[0]).toMatchObject({ ok: false, reason: "timeout" });
  });

  test("a spawn failure → verdict error (infra: parks blocked, never a fail/send-back)", async () => {
    const { run } = scripted([spawnErr("sh: not found")]);
    const { writeLog, now } = harness();
    const r = await runChecks({
      checks: [{ name: "tests", run: "bun test" }],
      spawnContext: PHASE1,
      runDir: "/rd",
      run,
      writeLog,
      now,
    });
    expect(r.verdict).toBe("error");
    expect(r.error).toContain("not found");
    expect(r.checks).toEqual([]); // the erroring check produced no result
  });

  test("ordered: first REQUIRED failure stops later REQUIRED checks, ADVISORY still runs", async () => {
    // tests(required, FAIL) → lint(required, SKIPPED, no runner call) → types(advisory, RUNS).
    // Only 2 runner calls happen (lint is skipped), so the runner sees exactly these 2 outcomes in order.
    const { run, calls } = scripted([fail(1), pass("advisory ran")]);
    const { logs, writeLog, now } = harness();
    const checks: DoneCheck[] = [
      { name: "tests", run: "bun test" },
      { name: "lint", run: "biome ci" }, // required (default), must be SKIPPED
      { name: "types", run: "tsc", required: false }, // advisory, must RUN
    ];
    const r = await runChecks({ checks, spawnContext: PHASE1, runDir: "/rd", run, writeLog, now });
    expect(r.verdict).toBe("fail");
    // only tests + types ran (lint skipped); 2 runner calls, not 3
    expect(calls).toHaveLength(2);
    expect(r.checks.map((c) => c.name)).toEqual(["tests", "types"]);
    expect(logs["checks/lint.log"]).toBeUndefined();
    expect(logs["checks/types.log"]).toBe("advisory ran");
  });

  test("an ADVISORY failure does NOT fail the stage", async () => {
    const { run } = scripted([pass(), fail(1)]);
    const { writeLog, now } = harness();
    const checks: DoneCheck[] = [
      { name: "tests", run: "bun test" },
      { name: "types", run: "tsc", required: false },
    ];
    const r = await runChecks({ checks, spawnContext: PHASE1, runDir: "/rd", run, writeLog, now });
    expect(r.verdict).toBe("pass");
    expect(r.checks[1]).toMatchObject({ name: "types", ok: false, required: false });
  });

  test("the check env carries NO secrets — only the allowlist floor (env-leak guard)", async () => {
    const { run, calls } = scripted([pass()]);
    const { writeLog, now } = harness();
    const hostEnv = {
      PATH: "/usr/bin",
      HOME: "/home/agent",
      ANTHROPIC_API_KEY: "sk-secret",
      BROOMVA_MODEL_TOKEN: "bearer-secret",
      AWS_SECRET_ACCESS_KEY: "leak",
      RANDOM_HOST_VAR: "nope",
    };
    await runChecks({
      checks: [{ name: "tests", run: "bun test" }],
      spawnContext: PHASE1,
      runDir: "/rd",
      run,
      writeLog,
      now,
      hostEnv,
    });
    const env = only(calls).env;
    expect(env.PATH).toBe("/usr/bin");
    expect(env.HOME).toBe("/home/agent");
    expect(env.ANTHROPIC_API_KEY).toBeUndefined();
    expect(env.BROOMVA_MODEL_TOKEN).toBeUndefined();
    expect(env.AWS_SECRET_ACCESS_KEY).toBeUndefined();
    expect(env.RANDOM_HOST_VAR).toBeUndefined();
  });

  test("phase-2 commandPrefix wraps the argv + sandbox env merges under the allowlist", async () => {
    const { run, calls } = scripted([pass()]);
    const { writeLog, now } = harness();
    const phase2: SandboxSpawnContext = {
      cwd: "/container/wt",
      commandPrefix: ["docker", "exec", "-i", "c1"],
      env: { IN_CONTAINER: "1" },
    };
    await runChecks({
      checks: [{ name: "tests", run: "bun test" }],
      spawnContext: phase2,
      runDir: "/rd",
      run,
      writeLog,
      now,
      hostEnv: { PATH: "/usr/bin" },
    });
    expect(only(calls).argv).toEqual(["docker", "exec", "-i", "c1", "sh", "-c", "bun test"]);
    expect(only(calls).cwd).toBe("/container/wt");
    expect(only(calls).env.IN_CONTAINER).toBe("1");
    expect(only(calls).env.PATH).toBe("/usr/bin");
  });

  test("evidence is truncated to the last 200 lines", async () => {
    const big = Array.from({ length: 300 }, (_, i) => `line ${i}`).join("\n");
    const { run } = scripted([pass(big)]);
    const { logs, writeLog, now } = harness();
    await runChecks({
      checks: [{ name: "tests", run: "bun test" }],
      spawnContext: PHASE1,
      runDir: "/rd",
      run,
      writeLog,
      now,
    });
    const log = need(logs, "checks/tests.log").split("\n");
    expect(log).toHaveLength(200);
    expect(log[0]).toBe("line 100"); // first 100 dropped
    expect(log.at(-1)).toBe("line 299");
  });

  test("evidence with exactly maxLogLines content lines + a trailing newline keeps ALL of them", async () => {
    // 3 content lines + trailing NL. The trailing empty split element must NOT consume a slot.
    const { run } = scripted([pass("a\nb\nc\n")]);
    const { logs, writeLog, now } = harness();
    await runChecks({
      checks: [{ name: "tests", run: "x" }],
      spawnContext: PHASE1,
      runDir: "/rd",
      run,
      writeLog,
      now,
      maxLogLines: 3,
    });
    expect(need(logs, "checks/tests.log")).toBe("a\nb\nc\n"); // "a" kept (not dropped for a blank trailer)
  });

  test("evidence over maxLogLines truncates to the last N content lines (trailing NL preserved)", async () => {
    const { run } = scripted([pass("a\nb\nc\n")]);
    const { logs, writeLog, now } = harness();
    await runChecks({
      checks: [{ name: "tests", run: "x" }],
      spawnContext: PHASE1,
      runDir: "/rd",
      run,
      writeLog,
      now,
      maxLogLines: 2,
    });
    expect(need(logs, "checks/tests.log")).toBe("b\nc\n"); // oldest content line dropped
  });

  test("colliding check names get distinct log files (no evidence overwrite)", async () => {
    const { run } = scripted([pass("a"), pass("b")]);
    const { logs, writeLog, now } = harness();
    const checks: DoneCheck[] = [
      { name: "test/unit", run: "x" }, // sanitizes to test_unit
      { name: "test unit", run: "y" }, // also sanitizes to test_unit → -2
    ];
    const r = await runChecks({ checks, spawnContext: PHASE1, runDir: "/rd", run, writeLog, now });
    expect(r.checks.map((c) => c.log)).toEqual(["checks/test_unit.log", "checks/test_unit-2.log"]);
    expect(logs["checks/test_unit.log"]).toBe("a");
    expect(logs["checks/test_unit-2.log"]).toBe("b");
  });

  test("a literal name equal to a generated -N suffix does NOT overwrite (seen-set keys on emitted base)", async () => {
    // check-2 (literal) → check-2.log; check → check.log; check (dup) → check-2 taken → check-3.log.
    const { run } = scripted([pass("first"), pass("second"), pass("third")]);
    const { logs, writeLog, now } = harness();
    const checks: DoneCheck[] = [
      { name: "check-2", run: "a" },
      { name: "check", run: "b" },
      { name: "check", run: "c" },
    ];
    const r = await runChecks({ checks, spawnContext: PHASE1, runDir: "/rd", run, writeLog, now });
    expect(r.checks.map((c) => c.log)).toEqual([
      "checks/check-2.log",
      "checks/check.log",
      "checks/check-3.log",
    ]);
    // no file was written twice — every check's evidence survives
    expect(logs["checks/check-2.log"]).toBe("first");
    expect(logs["checks/check.log"]).toBe("second");
    expect(logs["checks/check-3.log"]).toBe("third");
  });

  test("timeout is derived per-check (default 600s; explicit honored; clamped to the 1800s cap)", async () => {
    const { run, calls } = scripted([pass(), pass(), pass()]);
    const { writeLog, now } = harness();
    const checks: DoneCheck[] = [
      { name: "a", run: "x" }, // no timeout → 600s
      { name: "b", run: "y", timeout_s: 120 }, // explicit
      { name: "c", run: "z", timeout_s: 9999 }, // over cap → clamped to 1800
    ];
    await runChecks({ checks, spawnContext: PHASE1, runDir: "/rd", run, writeLog, now });
    expect(calls.map((c) => c.timeoutMs)).toEqual([600_000, 120_000, 1_800_000]);
  });

  test("an evidence-write failure → verdict error (infra: parks blocked, never rejects)", async () => {
    // A full/read-only runs/ dir makes writeLog throw. That is infra, exactly like a spawn error — it
    // must map to verdict `error` (park blocked, never burn an attempt), NOT escape as a rejected promise.
    const { run } = scripted([pass("ok")]);
    const now = harness().now;
    const writeLog = async () => {
      throw new Error("ENOSPC: no space left on device");
    };
    const r = await runChecks({
      checks: [{ name: "tests", run: "bun test" }],
      spawnContext: PHASE1,
      runDir: "/rd",
      run,
      writeLog,
      now,
    });
    expect(r.verdict).toBe("error");
    expect(r.error).toContain("evidence log write failed");
    expect(r.error).toContain("ENOSPC");
  });
});

describe("verifier-checks — defaultCheckRunner (real subprocess, bounded capture)", () => {
  // A REAL check that floods stdout must not grow the runner heap without bound: only the trailing
  // MAX_EVIDENCE_BYTES are retained, so a hot-loop logger / `yes` / `cat /dev/zero` can't OOM the 24/7
  // supervisor. The long (30s) timeout also proves the fix for the leaked kill-timer: if the losing
  // Promise.race timer were not cleared, this suite would hang ~30s after the check resolves.
  test("floods stdout but the runner keeps only the bounded tail (no unbounded heap growth)", async () => {
    const floodBytes = MAX_EVIDENCE_BYTES * 4;
    const r = await defaultCheckRunner(
      ["sh", "-c", `yes AAAAAAAA | head -c ${floodBytes}; printf '\\nTAIL_MARKER\\n'`],
      { cwd: process.cwd(), env: { PATH: process.env.PATH ?? "" }, timeoutMs: 30_000 },
    );
    expect(r.spawnError).toBeUndefined();
    expect(r.timedOut).toBe(false);
    expect(r.code).toBe(0);
    // captured output is bounded (tail + a short truncation marker), NOT the full 4 MiB flood
    expect(r.stdout.length).toBeLessThanOrEqual(MAX_EVIDENCE_BYTES + 4096);
    // the TAIL survives (the flood's head is dropped) and truncation is signalled
    expect(r.stdout).toContain("TAIL_MARKER");
    expect(r.stdout).toContain("output truncated");
  }, 60_000);
});

describe("verifier-checks — liftStage1 (into a VerifierResult)", () => {
  test("carries Stage 0's diffstat + base forward and the checks[]", () => {
    const diffstat = { files: 3, plus: 10, minus: 2 };
    const v = liftStage1(
      {
        verdict: "fail",
        checks: [
          {
            name: "t",
            ok: false,
            exit: 1,
            duration_s: 4,
            log: "checks/t.log",
            required: true,
            reason: "fail",
          },
        ],
      },
      diffstat,
      "3f1c9e0",
    );
    expect(v).toEqual({
      verdict: "fail",
      tampering: [],
      diffstat,
      base: "3f1c9e0",
      checks: [
        {
          name: "t",
          ok: false,
          exit: 1,
          duration_s: 4,
          log: "checks/t.log",
          required: true,
          reason: "fail",
        },
      ],
    });
  });
  test("an error stage propagates its message", () => {
    const v = liftStage1(
      { verdict: "error", checks: [], error: "sh: not found" },
      { files: 0, plus: 0, minus: 0 },
      "b",
    );
    expect(v.verdict).toBe("error");
    expect(v.message).toBe("sh: not found");
  });
});
