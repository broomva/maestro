// Verifier Stage 1 — the deterministic check runner (VERIFIER §2 Stage 1). The oracle: the contract's
// named checks, run IN ORDER inside the run worktree, each with its own timeout, in an env that carries
// NO host secrets. The judge (Stage 2) only ever SUPPLEMENTS this — a green Stage 1 is the ground truth.
//
// Runs ONLY after a Stage 0 pass (verifier.ts wires this as the `next` continuation). Each check:
//   - exit 0 within timeout            → pass
//   - nonzero exit                     → fail (exit code + last 200 lines of output as evidence)
//   - killed after its timeout         → fail, reason `timeout`
//   - the shell itself cannot spawn    → verdict `error` (infra, not the agent's fault): the run parks
//                                        blocked, an attempt is NEVER burned on a broken harness.
// Ordered: the first REQUIRED failure stops further REQUIRED checks (their verdict is already decided);
// ADVISORY (`required: false`) checks still run — they are cheap signal and never gate the stage.
//
// SECURITY: a check runs agent-influenced code (the diff's own test/build scripts), so it must not see
// the model token or any host secret. We do NOT use `sandbox.exec` (it inherits `process.env`); we spawn
// through `sandbox.spawnContext()` — cwd + the phase-2 `commandPrefix` — with `buildCheckEnv` as the FULL
// env (Bun.spawn with an explicit env replaces, never merges), so the check sees only PATH/HOME/toolchain.

import { mkdir } from "node:fs/promises";
import { dirname, join } from "node:path";
import { DEFAULT_CHECK_TIMEOUT_S, type DoneCheck, MAX_CHECK_TIMEOUT_S } from "@maestro/protocol";
import { buildCheckEnv } from "../harness/spawn-contract";
import type { SandboxSpawnContext } from "../sandbox/sandbox";
import type { DiffStat } from "./stage0";
import type { CheckResult, VerifierResult } from "./verifier";

/** The captured outcome of one check process (mirrors SandboxExecResult + timeout/spawn signals). */
export interface CheckProcResult {
  /** Exit code (meaningless when `timedOut` or `spawnError`). */
  code: number;
  stdout: string;
  stderr: string;
  /** The process was killed after exceeding its timeout. */
  timedOut: boolean;
  /** Set iff the process could not be launched at all (the shell binary missing) — an INFRA error. */
  spawnError?: string;
}

/** Runs one check process to completion (or timeout). Injected in tests; the default spawns via Bun. */
export type CheckRunner = (
  argv: string[],
  opts: { cwd: string; env: Record<string, string>; timeoutMs: number },
) => Promise<CheckProcResult>;

/** Stage 1's overall outcome: `pass` (all required checks passed), `fail` (a required check failed or
 *  timed out), or `error` (the harness could not run a check — park blocked). */
export interface Stage1Result {
  verdict: VerifierResult["verdict"];
  checks: CheckResult[];
  /** Set only on `verdict === "error"` — the infra failure that stopped the stage. */
  error?: string;
}

export interface RunChecksDeps {
  /** The contract's checks, already normalized (`normalizeChecks(done.check)`). */
  checks: readonly DoneCheck[];
  /** How to enter the sandbox — cwd (the worktree), phase-2 commandPrefix, sandbox env additions. */
  spawnContext: SandboxSpawnContext;
  /** The receipts dir `runs/run-<id>/` — evidence logs land under `<runDir>/checks/`. */
  runDir: string;
  /** Host env the allowlist filters (default `process.env`). */
  hostEnv?: Record<string, string | undefined>;
  /** The check-process runner (default {@link defaultCheckRunner}); injected in tests. */
  run?: CheckRunner;
  /** Persist an evidence log at a run-dir-relative path (default writes under `runDir`); injected in tests. */
  writeLog?: (relPath: string, content: string) => Promise<void>;
  /** Epoch-ms clock for durations (default `Date.now`); injected for deterministic tests. */
  now?: () => number;
  /** How to invoke the non-interactive shell (default `["sh", "-c"]`). */
  shell?: readonly string[];
  /** Max lines of output kept per evidence log (default 200, VERIFIER §2). */
  maxLogLines?: number;
}

/** The default runner: spawn `argv` via Bun, capture streams, and enforce `timeoutMs` by SIGKILL. A
 *  spawn THROW (the shell binary missing) becomes `spawnError` — the caller maps it to verdict `error`. */
export const defaultCheckRunner: CheckRunner = async (argv, { cwd, env, timeoutMs }) => {
  let proc: ReturnType<typeof Bun.spawn>;
  try {
    proc = Bun.spawn(argv, { cwd, env, stdout: "pipe", stderr: "pipe", stdin: "ignore" });
  } catch (err) {
    return { code: -1, stdout: "", stderr: "", timedOut: false, spawnError: msg(err) };
  }
  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    proc.kill("SIGKILL");
  }, timeoutMs);
  try {
    const [stdout, stderr, code] = await Promise.all([
      new Response(proc.stdout as ReadableStream).text(),
      new Response(proc.stderr as ReadableStream).text(),
      proc.exited,
    ]);
    return { code, stdout, stderr, timedOut };
  } finally {
    clearTimeout(timer);
  }
};

/** Clamp a check's `timeout_s` to the contract-legal window (defensive — the parser already enforces
 *  `(0, 1800]`, but a check reconstructed from an un-validated source must never spawn an unbounded kill
 *  timer nor a non-positive one). Missing → the 600s default. */
function timeoutMsFor(check: DoneCheck): number {
  const s = check.timeout_s ?? DEFAULT_CHECK_TIMEOUT_S;
  const clamped = Math.min(Math.max(s, 1), MAX_CHECK_TIMEOUT_S);
  return clamped * 1000;
}

/** Sanitize a check name into a safe log-file base — `[A-Za-z0-9_-]` only, so no `/`, no `..`, no
 *  control char can escape `<runDir>/checks/`. A collision (two checks whose names sanitize alike) gets
 *  a `-2`/`-3` suffix so no evidence is silently overwritten. */
function logNamer(): (name: string) => string {
  const used = new Map<string, number>();
  return (name) => {
    const base = name.replace(/[^A-Za-z0-9_-]/g, "_") || "check";
    const n = (used.get(base) ?? 0) + 1;
    used.set(base, n);
    return join("checks", n === 1 ? `${base}.log` : `${base}-${n}.log`);
  };
}

/** Keep the last `max` lines of `text` (the evidence tail, VERIFIER §2 "last 200 lines"). */
function lastLines(text: string, max: number): string {
  const lines = text.split("\n");
  return lines.length <= max ? text : lines.slice(-max).join("\n");
}

/**
 * Run the contract's deterministic checks in order (VERIFIER §2 Stage 1). See the module header for the
 * classification + ordering rules. Never throws: a runner spawn failure resolves to verdict `error`.
 */
export async function runChecks(deps: RunChecksDeps): Promise<Stage1Result> {
  const {
    checks,
    spawnContext,
    runDir,
    hostEnv = process.env,
    run = defaultCheckRunner,
    writeLog = defaultWriteLog(runDir),
    now = () => Date.now(),
    shell = ["sh", "-c"],
    maxLogLines = 200,
  } = deps;

  // The FULL env for every check: the allowlist floor (no secrets) plus any phase-2 sandbox additions.
  const env = { ...spawnContext.env, ...buildCheckEnv(hostEnv) };
  const nameLog = logNamer();
  const results: CheckResult[] = [];
  let requiredFailed = false;

  for (const check of checks) {
    const required = check.required !== false;
    // First required failure stops further REQUIRED checks; advisory checks still run (cheap signal).
    if (requiredFailed && required) continue;

    const argv = [...spawnContext.commandPrefix, ...shell, check.run];
    const start = now();
    const r = await run(argv, { cwd: spawnContext.cwd, env, timeoutMs: timeoutMsFor(check) });
    const durationS = Math.max(0, Math.round((now() - start) / 1000));

    // The shell itself could not launch → infra error. Park blocked; never burn an attempt (VERIFIER §2).
    if (r.spawnError !== undefined) {
      return { verdict: "error", checks: results, error: `check "${check.name}": ${r.spawnError}` };
    }

    const logRel = nameLog(check.name);
    const combined = r.stderr ? (r.stdout ? `${r.stdout}\n${r.stderr}` : r.stderr) : r.stdout;
    await writeLog(logRel, lastLines(combined, maxLogLines));

    const ok = !r.timedOut && r.code === 0;
    results.push({
      name: check.name,
      ok,
      exit: r.code,
      duration_s: durationS,
      log: logRel,
      required,
      ...(ok ? {} : { reason: r.timedOut ? "timeout" : "fail" }),
    });
    if (required && !ok) requiredFailed = true;
  }

  return { verdict: requiredFailed ? "fail" : "pass", checks: results };
}

/** Lift a Stage 1 result into a {@link VerifierResult}, carrying Stage 0's diffstat + base forward. */
export function liftStage1(stage1: Stage1Result, diffstat: DiffStat, base: string): VerifierResult {
  return {
    verdict: stage1.verdict,
    tampering: [],
    diffstat,
    base,
    checks: stage1.checks,
    ...(stage1.error !== undefined ? { message: stage1.error } : {}),
  };
}

/** Default evidence-log writer — persists under `<runDir>/<relPath>`, creating parent dirs. */
function defaultWriteLog(runDir: string): (relPath: string, content: string) => Promise<void> {
  return async (relPath, content) => {
    const abs = join(runDir, relPath);
    await mkdir(dirname(abs), { recursive: true });
    await Bun.write(abs, content);
  };
}

function msg(err: unknown): string {
  return String((err as Error)?.message ?? err);
}
