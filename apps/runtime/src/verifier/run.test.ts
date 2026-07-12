// Verifier orchestration tests — runVerification composes Stage 0→1→2, persists the durable receipt,
// decides the outcome, and projects the attempt as the ordered check.* stream. Every seam is injected
// (mem IO, a recording emit, a fake git runner, a fake check runner, a mock judge caller), so the whole
// surface is deterministic with no supervisor, no real clock, no spawned process, and no model call.

import { describe, expect, test } from "bun:test";
import { type DoneCheck, EVENT_TYPES } from "@maestro/protocol";
import { fixPlanPath } from "../harness/stop-conditions";
import type { SandboxSpawnContext } from "../sandbox/sandbox";
import type { CheckProcResult, CheckRunner } from "./checks";
import type { JudgeCaller } from "./judge";
import { type RunVerificationDeps, runVerification, type VerifyEmit } from "./run";
import type { GitRunner } from "./stage0";
import { type VerdictIo, verdictPath } from "./verdict";

const RUN_DIR = "/runs/run-r1";
const BASE = "base0commit";
const BRANCH = "run/r1";
const TS = "2026-07-12T00:00:00.000Z";

// ── seams ────────────────────────────────────────────────────────────────────────────────────────────

/** An in-memory {@link VerdictIo} that records every write — the whole fs side of a verification. */
function memIo(seed: Record<string, string> = {}) {
  const files = new Map<string, string>(Object.entries(seed));
  const io: VerdictIo = {
    read: async (p) => (files.has(p) ? (files.get(p) as string) : null),
    write: async (p, d) => {
      files.set(p, d);
    },
    mkdirp: async () => {},
  };
  return { io, files };
}

/** A recording emit — captures the ordered (type, payload) stream. */
function recorder() {
  const events: { type: string; payload: Record<string, unknown> }[] = [];
  const emit: VerifyEmit = async (type, payload) => {
    events.push({ type, payload });
  };
  return { events, emit, types: () => events.map((e) => e.type) };
}

/** A `git diff --numstat -z` body — NUL-terminated `<added>\t<deleted>\t<path>` records. */
function numstat(rows: [number | "-", number | "-", string][]): string {
  return rows.map(([a, d, p]) => `${a}\t${d}\t${p}\0`).join("");
}

function fakeGit(stdout: string, code = 0, stderr = ""): GitRunner {
  return async () => ({ code, stdout, stderr });
}

const spawnContext: SandboxSpawnContext = { cwd: "/work", commandPrefix: [], env: {} };

/** A check runner keyed on the shell command (last argv element); records the commands it ran. */
function fakeRunner(
  byCmd: Record<string, Partial<CheckProcResult>>,
  calls?: string[],
): CheckRunner {
  return async (argv) => {
    const cmd = argv[argv.length - 1] ?? "";
    calls?.push(cmd);
    const r = byCmd[cmd] ?? {};
    return {
      code: r.code ?? 0,
      stdout: r.stdout ?? "",
      stderr: r.stderr ?? "",
      timedOut: r.timedOut ?? false,
      ...(r.spawnError !== undefined ? { spawnError: r.spawnError } : {}),
    };
  };
}

const RUBRIC = `---
threshold: 0.5
scale: [0, 1, 2]
criteria:
  - id: correctness
    weight: 1
    ask: Is it correct?
---
Grade the diff.
`;

/** A judge caller returning an Anthropic-shaped reply whose text is `json`. */
function judgeReply(json: string): JudgeCaller {
  return async () => ({ status: 200, body: { content: [{ type: "text", text: json }] } });
}
const PASS_JUDGE = judgeReply('{"criteria":[{"id":"correctness","score":2}]}');
const FAIL_JUDGE = judgeReply(
  '{"criteria":[{"id":"correctness","score":0,"note":"still broken"}]}',
);

// ── base deps ────────────────────────────────────────────────────────────────────────────────────────

/** A clean, passing baseline (Stage 0 pass, one green check, no rubric). Override any field per test. */
function baseDeps(
  io: VerdictIo,
  emit: VerifyEmit,
  over: Partial<RunVerificationDeps> = {},
): RunVerificationDeps {
  return {
    stage0: {
      cwd: "/work",
      base: BASE,
      branch: BRANCH,
      protect: ["**/*.test.ts", "rubric.md"],
      git: fakeGit(numstat([[3, 1, "src/a.ts"]])),
    },
    checks: {
      checks: [{ name: "unit", run: "bun test", required: true }] as DoneCheck[],
      spawnContext,
      run: fakeRunner({ "bun test": { code: 0 } }),
      now: () => 0,
    },
    judge: { rubricText: null, diff: "diff", brief: "do the thing", call: PASS_JUDGE, env: {} },
    runDir: RUN_DIR,
    attempt: 1,
    maxAttempts: 5,
    iterations: 2,
    maxIterations: 100,
    emit,
    now: () => TS,
    io,
    ...over,
  };
}

// ── tests ────────────────────────────────────────────────────────────────────────────────────────────

describe("verifier-run — runVerification (verify runner)", () => {
  test("a clean run with no rubric passes → park_review, verdict.md written, no fix_plan", async () => {
    const { io, files } = memIo();
    const { emit, types } = recorder();
    const r = await runVerification(baseDeps(io, emit));

    expect(r.outcome).toEqual({ action: "park_review", burnsAttempt: true });
    expect(r.receipt.verdict).toBe("pass");
    expect(r.receipt.base).toBe(BASE);
    expect(r.receipt.judge).toEqual({ score: null }); // no rubric → judge did not run
    expect(files.has(verdictPath(RUN_DIR))).toBe(true);
    expect(files.has(fixPlanPath(RUN_DIR))).toBe(false); // a pass writes no feedback
    // no rubric → no check.judge event
    expect(types()).toEqual([
      EVENT_TYPES.CHECK_STARTED,
      EVENT_TYPES.CHECK_RESULT,
      EVENT_TYPES.CHECK_VERDICT,
    ]);
  });

  test("a clean run with a passing rubric runs the judge → judge.json + check.judge, park_review", async () => {
    const { io, files } = memIo();
    const { emit, events, types } = recorder();
    const r = await runVerification(
      baseDeps(io, emit, {
        judge: { rubricText: RUBRIC, diff: "diff", brief: "b", call: PASS_JUDGE, env: {} },
      }),
    );

    expect(r.outcome.action).toBe("park_review");
    expect(r.receipt.verdict).toBe("pass");
    expect(r.receipt.judge.score).toBe(1); // all-max score on a 0..2 scale → 1.0
    expect(r.receipt.judge.detail).toBe("judge.json");
    expect(files.has(`${RUN_DIR}/judge.json`)).toBe(true); // writeJudge routed through io
    expect(types()).toEqual([
      EVENT_TYPES.CHECK_STARTED,
      EVENT_TYPES.CHECK_RESULT,
      EVENT_TYPES.CHECK_JUDGE,
      EVENT_TYPES.CHECK_VERDICT,
    ]);
    const judgeEv = events.find((e) => e.type === EVENT_TYPES.CHECK_JUDGE);
    expect(judgeEv?.payload).toMatchObject({ score: 1, detail: "judge.json" });
    expect(typeof judgeEv?.payload.model).toBe("string");
  });

  test("a failing judge fails the verification → respawn + fix_plan feedback appended", async () => {
    const { io, files } = memIo();
    const { emit, types } = recorder();
    const r = await runVerification(
      baseDeps(io, emit, {
        judge: { rubricText: RUBRIC, diff: "diff", brief: "b", call: FAIL_JUDGE, env: {} },
      }),
    );

    expect(r.receipt.verdict).toBe("fail");
    expect(r.outcome).toEqual({ action: "respawn", burnsAttempt: true });
    const plan = files.get(fixPlanPath(RUN_DIR));
    expect(plan).toContain("## Verifier — attempt 1 failed (2026-07-12T00:00:00.000Z)");
    expect(plan).toContain("judge:");
    expect(types()).toContain(EVENT_TYPES.CHECK_JUDGE);
    expect(types().at(-1)).toBe(EVENT_TYPES.CHECK_VERDICT);
  });

  test("a required-check failure → fail, respawn, the failing check surfaces in check.result", async () => {
    const { io, files } = memIo();
    const { emit, events } = recorder();
    const r = await runVerification(
      baseDeps(io, emit, {
        checks: {
          checks: [{ name: "unit", run: "bun test", required: true }] as DoneCheck[],
          spawnContext,
          run: fakeRunner({ "bun test": { code: 1, stdout: "1 failing" } }),
          now: () => 0,
        },
      }),
    );

    expect(r.receipt.verdict).toBe("fail");
    expect(r.outcome.action).toBe("respawn");
    const result = events.find((e) => e.type === EVENT_TYPES.CHECK_RESULT);
    expect(result?.payload).toMatchObject({ name: "unit", ok: false, exit: 1 });
    expect(files.get(fixPlanPath(RUN_DIR))).toContain("unit:");
  });

  test("Stage 0 tampering short-circuits → no check ran, no check.result, fail", async () => {
    const { io, files } = memIo();
    const { emit, types } = recorder();
    const calls: string[] = [];
    const r = await runVerification(
      baseDeps(io, emit, {
        stage0: {
          cwd: "/work",
          base: BASE,
          branch: BRANCH,
          protect: ["**/*.test.ts"],
          git: fakeGit(numstat([[2, 0, "src/a.test.ts"]])),
        },
        checks: {
          checks: [{ name: "unit", run: "bun test", required: true }] as DoneCheck[],
          spawnContext,
          run: fakeRunner({ "bun test": { code: 0 } }, calls),
          now: () => 0,
        },
      }),
    );

    expect(r.receipt.verdict).toBe("fail");
    expect(r.receipt.tampering).toEqual(["src/a.test.ts"]);
    expect(calls).toHaveLength(0); // the `next` continuation never ran — Stage 0 is terminal
    expect(types()).not.toContain(EVENT_TYPES.CHECK_RESULT);
    expect(types()).toEqual([EVENT_TYPES.CHECK_STARTED, EVENT_TYPES.CHECK_VERDICT]);
    expect(files.get(fixPlanPath(RUN_DIR))).toContain("tamper:");
  });

  test("Stage 0 diff_too_large → fail, scope feedback, no check ran", async () => {
    const { io, files } = memIo();
    const { emit } = recorder();
    const calls: string[] = [];
    const r = await runVerification(
      baseDeps(io, emit, {
        stage0: {
          cwd: "/work",
          base: BASE,
          branch: BRANCH,
          protect: [],
          maxFiles: 1,
          git: fakeGit(
            numstat([
              [1, 0, "a.ts"],
              [1, 0, "b.ts"],
            ]),
          ),
        },
        checks: {
          checks: [{ name: "unit", run: "bun test", required: true }] as DoneCheck[],
          spawnContext,
          run: fakeRunner({ "bun test": { code: 0 } }, calls),
          now: () => 0,
        },
      }),
    );

    expect(r.receipt.verdict).toBe("fail");
    expect(r.receipt.diffstat.files).toBe(2);
    expect(calls).toHaveLength(0);
    expect(files.get(fixPlanPath(RUN_DIR))).toContain("scope:");
  });

  test("Stage 0 infra error (git fails) → verify_error park, NO attempt burned, no fix_plan", async () => {
    const { io, files } = memIo();
    const { emit, events, types } = recorder();
    const r = await runVerification(
      baseDeps(io, emit, {
        stage0: {
          cwd: "/work",
          base: BASE,
          branch: BRANCH,
          protect: [],
          git: fakeGit("", 128, "fatal: bad revision"),
        },
      }),
    );

    expect(r.receipt.verdict).toBe("error");
    expect(r.outcome).toEqual({
      action: "park_blocked",
      reason: "verify_error",
      burnsAttempt: false,
    });
    expect(files.has(verdictPath(RUN_DIR))).toBe(true); // the error receipt is still durable
    expect(files.has(fixPlanPath(RUN_DIR))).toBe(false); // an error is not feedback
    const err = events.find((e) => e.type === EVENT_TYPES.CHECK_ERROR);
    expect(String(err?.payload.message)).toContain("bad revision");
    expect(types()).toEqual([
      EVENT_TYPES.CHECK_STARTED,
      EVENT_TYPES.CHECK_ERROR,
      EVENT_TYPES.CHECK_VERDICT,
    ]);
  });

  test("Stage 1 infra error (shell cannot spawn) → verify_error park, no attempt burned", async () => {
    const { io } = memIo();
    const { emit, types } = recorder();
    const r = await runVerification(
      baseDeps(io, emit, {
        checks: {
          checks: [{ name: "unit", run: "bun test", required: true }] as DoneCheck[],
          spawnContext,
          run: fakeRunner({ "bun test": { spawnError: "sh: not found" } }),
          now: () => 0,
        },
      }),
    );

    expect(r.receipt.verdict).toBe("error");
    expect(r.outcome.burnsAttempt).toBe(false);
    expect(types()).toContain(EVENT_TYPES.CHECK_ERROR);
  });

  test("no_progress: an identical fail signature parks blocked", async () => {
    const { io } = memIo();
    const { emit } = recorder();
    // First attempt: derive the signature.
    const first = await runVerification(
      baseDeps(io, emit, {
        checks: {
          checks: [{ name: "unit", run: "bun test", required: true }] as DoneCheck[],
          spawnContext,
          run: fakeRunner({ "bun test": { code: 1 } }),
          now: () => 0,
        },
      }),
    );
    expect(first.outcome.action).toBe("respawn");

    // Second attempt: same failing signature → no_progress.
    const io2 = memIo();
    const rec2 = recorder();
    const second = await runVerification(
      baseDeps(io2.io, rec2.emit, {
        attempt: 2,
        priorSignature: first.signature,
        checks: {
          checks: [{ name: "unit", run: "bun test", required: true }] as DoneCheck[],
          spawnContext,
          run: fakeRunner({ "bun test": { code: 1 } }),
          now: () => 0,
        },
      }),
    );
    expect(second.signature).toBe(first.signature);
    expect(second.outcome).toEqual({
      action: "park_blocked",
      reason: "no_progress",
      burnsAttempt: true,
    });
  });

  test("verifier_exhausted: a fresh fail on the last attempt parks blocked", async () => {
    const { io } = memIo();
    const { emit } = recorder();
    const r = await runVerification(
      baseDeps(io, emit, {
        attempt: 5,
        maxAttempts: 5,
        priorSignature: "something-else", // not no_progress
        checks: {
          checks: [{ name: "unit", run: "bun test", required: true }] as DoneCheck[],
          spawnContext,
          run: fakeRunner({ "bun test": { code: 1 } }),
          now: () => 0,
        },
      }),
    );
    expect(r.outcome).toEqual({
      action: "park_blocked",
      reason: "verifier_exhausted",
      burnsAttempt: true,
    });
  });

  test("iteration_cap: a fresh fail with the iteration budget spent parks blocked", async () => {
    const { io } = memIo();
    const { emit } = recorder();
    const r = await runVerification(
      baseDeps(io, emit, {
        attempt: 2,
        maxAttempts: 5,
        iterations: 100,
        maxIterations: 100,
        priorSignature: "different",
        checks: {
          checks: [{ name: "unit", run: "bun test", required: true }] as DoneCheck[],
          spawnContext,
          run: fakeRunner({ "bun test": { code: 1 } }),
          now: () => 0,
        },
      }),
    );
    expect(r.outcome).toEqual({
      action: "park_blocked",
      reason: "iteration_cap",
      burnsAttempt: true,
    });
  });

  test("the check.verdict payload IS the receipt, verbatim", async () => {
    const { io } = memIo();
    const { emit, events } = recorder();
    const r = await runVerification(baseDeps(io, emit));
    const verdictEv = events.find((e) => e.type === EVENT_TYPES.CHECK_VERDICT);
    expect(verdictEv?.payload).toEqual({ ...r.receipt });
  });

  test("started is always the first event; verdict is always the last", async () => {
    const { io } = memIo();
    const { emit, types } = recorder();
    await runVerification(baseDeps(io, emit));
    expect(types()[0]).toBe(EVENT_TYPES.CHECK_STARTED);
    expect(types().at(-1)).toBe(EVENT_TYPES.CHECK_VERDICT);
  });

  test("durability-first: verdict.md is written even when the check.verdict emit rejects", async () => {
    const { io, files } = memIo();
    // An emit that fails ONLY on the terminal verdict event — the receipt must already be on disk.
    const emit: VerifyEmit = async (type) => {
      if (type === EVENT_TYPES.CHECK_VERDICT) throw new Error("index write failed");
    };
    await expect(runVerification(baseDeps(io, emit))).rejects.toThrow("index write failed");
    // The durable receipt was persisted BEFORE the projection emit — a reorder-to-emit-first reds this.
    expect(files.has(verdictPath(RUN_DIR))).toBe(true);
  });

  test("attempt number flows into the receipt and the started event", async () => {
    const { io } = memIo();
    const { emit, events } = recorder();
    const r = await runVerification(baseDeps(io, emit, { attempt: 3 }));
    expect(r.receipt.attempt).toBe(3);
    const started = events.find((e) => e.type === EVENT_TYPES.CHECK_STARTED);
    expect(started?.payload).toEqual({ attempt: 3 });
  });
});
