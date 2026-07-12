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

/** After the check process ends (or is killed) never wait longer than this for its stdout/stderr to
 *  reach EOF: a backgrounded grandchild can inherit and HOLD the pipe's write end open, so the stream
 *  never EOFs. Without this bound the runner would hang forever, defeating the very timeout it enforces
 *  (the repo's own `escalateHung` races exit against a delay for the same reason). */
const DRAIN_GRACE_MS = 2000;

/** Cap the in-memory evidence buffer PER STREAM. A check runs agent-influenced code; a hot-loop
 *  `console.log`, `yes`, or `cat /dev/zero` emits output without bound, and Bun.spawn has no `maxBuffer`.
 *  Accumulating it all would balloon the long-lived supervisor heap and OOM-crash the 24/7 engine (and
 *  every concurrent run with it). We keep a rolling TAIL of the last {@link MAX_EVIDENCE_BYTES} — evidence
 *  is the last-200-lines tail anyway, where failures surface — and drain continuously so the child never
 *  blocks on a full pipe. 1 MiB per stream holds 200 lines of ordinary output with huge margin. */
export const MAX_EVIDENCE_BYTES = 1024 * 1024;

/** A cancellable delay for `Promise.race`. The LOSING branch's timer MUST be cleared: a bare
 *  `setTimeout` left armed keeps the event loop alive for up to `timeout_s` (max 1800s) after a fast
 *  check resolves, blocking clean process exit in any short-lived reuse of the runner (the `verify` CLI,
 *  the `--role verifier` child, a `bun test` that drives the real runner). */
function deadline(ms: number): { promise: Promise<void>; cancel: () => void } {
  let id: ReturnType<typeof setTimeout> | undefined;
  const promise = new Promise<void>((resolve) => {
    id = setTimeout(resolve, ms);
  });
  return { promise, cancel: () => clearTimeout(id) };
}

/** Concatenate byte chunks into one buffer. */
function concatChunks(chunks: Uint8Array[]): Uint8Array {
  let total = 0;
  for (const c of chunks) total += c.length;
  const out = new Uint8Array(total);
  let off = 0;
  for (const c of chunks) {
    out.set(c, off);
    off += c.length;
  }
  return out;
}

/** Read a process stream into text via a reader we OWN (not a `Response`, which locks the stream), so a
 *  hung pipe can be `cancel()`led to release its fd. Memory is BOUNDED: only the trailing
 *  {@link MAX_EVIDENCE_BYTES} are retained (oldest whole chunks dropped first; a single oversized chunk
 *  copied down to its tail so the big source buffer is released), so an unbounded-output check cannot grow
 *  the supervisor heap. We keep draining rather than stop reading, both so the child never blocks on a
 *  full pipe AND so we capture the TAIL (where failures surface), not the head. `text` always resolves —
 *  with whatever was captured when the stream EOFs, errors, or is cancelled — so it can never wedge the
 *  runner. A truncation marker is prepended iff bytes were dropped. */
function pipeReader(stream: ReadableStream<Uint8Array> | null): {
  text: Promise<string>;
  cancel: () => void;
} {
  if (stream === null) return { text: Promise.resolve(""), cancel: () => {} };
  const reader = stream.getReader();
  const chunks: Uint8Array[] = [];
  let bytes = 0;
  let dropped = false;
  const text = (async () => {
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        if (!value || value.byteLength === 0) continue;
        chunks.push(value);
        bytes += value.byteLength;
        // Keep only the trailing cap: drop whole leading chunks first (never the newest)...
        while (bytes > MAX_EVIDENCE_BYTES && chunks.length > 1) {
          const front = chunks.shift();
          if (front) bytes -= front.byteLength;
          dropped = true;
        }
        // ...then, if one chunk alone still exceeds the cap, keep a COPY of its tail (`subarray` would
        // retain the oversized backing buffer, defeating the bound).
        if (bytes > MAX_EVIDENCE_BYTES && chunks.length === 1) {
          const only = chunks[0];
          if (only) {
            const tail = only.slice(only.byteLength - MAX_EVIDENCE_BYTES);
            chunks[0] = tail;
            bytes = tail.byteLength;
            dropped = true;
          }
        }
      }
    } catch {
      // cancelled or errored mid-read — return what we captured
    }
    const body = new TextDecoder().decode(concatChunks(chunks));
    return dropped ? `[output truncated to last ${MAX_EVIDENCE_BYTES} bytes]\n${body}` : body;
  })();
  return {
    text,
    cancel: () => {
      reader.cancel().catch(() => {});
    },
  };
}

/**
 * The default runner: spawn `argv` via Bun, capture streams, and enforce `timeoutMs`. A spawn THROW (the
 * shell binary missing) becomes `spawnError` — the caller maps it to verdict `error`. The timeout is
 * enforced on the PROCESS (race `exited` against the deadline, then SIGKILL), and the stream drain is
 * BOUNDED by {@link DRAIN_GRACE_MS}: a check that orphans a pipe-holding grandchild (jest/vitest workers,
 * `dev-server &`, an E2E fixture) can hold stdout open after the shell dies — so we never await EOF
 * unconditionally. On the grace path the readers are cancelled to release the fds. The runner ALWAYS
 * resolves; it never throws post-spawn.
 */
export const defaultCheckRunner: CheckRunner = async (argv, { cwd, env, timeoutMs }) => {
  let proc: ReturnType<typeof Bun.spawn>;
  try {
    proc = Bun.spawn(argv, { cwd, env, stdout: "pipe", stderr: "pipe", stdin: "ignore" });
  } catch (err) {
    return { code: -1, stdout: "", stderr: "", timedOut: false, spawnError: msg(err) };
  }
  const out = pipeReader(proc.stdout as ReadableStream<Uint8Array>);
  const err = pipeReader(proc.stderr as ReadableStream<Uint8Array>);

  // Enforce the timeout on the process, not the pipe: kill the shell if it outlives the deadline. The
  // losing timer is cancelled so a fast check leaves no armed setTimeout keeping the event loop alive.
  let timedOut = false;
  const killAt = deadline(timeoutMs);
  const outcome = await Promise.race([
    proc.exited.then(() => "exited" as const),
    killAt.promise.then(() => "timeout" as const),
  ]);
  killAt.cancel();
  if (outcome === "timeout") {
    timedOut = true;
    proc.kill("SIGKILL");
  }

  // Drain the captured output, but never block past the grace — an orphan may hold the pipe open.
  const graceAt = deadline(DRAIN_GRACE_MS);
  const drained = await Promise.race([
    Promise.all([out.text, err.text]).then((v) => v),
    graceAt.promise.then(() => null),
  ]);
  graceAt.cancel();
  if (drained === null) {
    out.cancel();
    err.cancel();
  }
  const [stdout, stderr] = drained ?? (await Promise.all([out.text, err.text]));
  const code = timedOut ? -1 : await proc.exited;
  return { code, stdout, stderr, timedOut };
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
 *  control char can escape `<runDir>/checks/`. A collision gets a `-2`/`-3` suffix so no evidence is
 *  silently overwritten. The seen-set keys on the EMITTED base (not the sanitized input), so a generated
 *  `-N` suffix that equals a later literal check name (e.g. a check literally named `foo-2`) still gets
 *  its own distinct file rather than clobbering the suffix. */
function logNamer(): (name: string) => string {
  const used = new Set<string>();
  return (name) => {
    const base = name.replace(/[^A-Za-z0-9_-]/g, "_") || "check";
    let candidate = base;
    let n = 1;
    while (used.has(candidate)) {
      n += 1;
      candidate = `${base}-${n}`;
    }
    used.add(candidate);
    return join("checks", `${candidate}.log`);
  };
}

/** Keep the last `max` lines of `text` (the evidence tail, VERIFIER §2 "last 200 lines"). A single
 *  trailing newline (the normal shape of process output) is NOT counted as a line, so exactly `max`
 *  content lines followed by a newline are all kept — not `max-1` content lines plus a blank trailer. */
function lastLines(text: string, max: number): string {
  const trailingNl = text.endsWith("\n");
  const body = trailingNl ? text.slice(0, -1) : text;
  const lines = body.split("\n");
  const kept = lines.length <= max ? lines : lines.slice(-max);
  return kept.join("\n") + (trailingNl ? "\n" : "");
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
    // A failed evidence write is infra (ENOSPC/EACCES/EROFS), not the agent's fault — map it to verdict
    // `error` so the run parks blocked, matching the spawn-error path. runChecks must never reject.
    try {
      await writeLog(logRel, lastLines(combined, maxLogLines));
    } catch (e) {
      return {
        verdict: "error",
        checks: results,
        error: `check "${check.name}": evidence log write failed: ${msg(e)}`,
      };
    }

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
