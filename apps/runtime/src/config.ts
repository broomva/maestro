/// <reference types="bun" />
// Runtime configuration surface (STACK.md §runtime). BRO-1790 scope: a sketch,
// resolved from the environment with self-host-friendly defaults. The index and
// the singleton lock are only *located* here — the libSQL index opens at the P0
// exit, the `runtime.lock` singleton (D4) is acquired in P2.

import { resolve } from "node:path";
import { VERIFIER_MAX_ATTEMPTS } from "@maestro/protocol";

export interface RuntimeConfig {
  /** TCP port the Hono service binds (MAESTRO_PORT, default DEFAULT_PORT). */
  port: number;
  /** Workspace root — the git repo the runtime owns (MAESTRO_WORKSPACE, default cwd). */
  workspace: string;
  /** libSQL index file path (MAESTRO_INDEX, default <workspace>/.maestro/index.db). Not opened yet. */
  indexPath: string;
  /** Reserved singleton-lock location (D4, lands P2). Not acquired yet. */
  lockPath: string;
  /**
   * SSE tail poll interval in ms (MAESTRO_STREAM_POLL_MS). Optional — the stream
   * routes (BRO-1816) fall back to their own default when unset. Tunable for a
   * calmer/livelier feed; tests drop it low for prompt delivery assertions.
   */
  streamPollMs?: number;
  /** SSE idle-heartbeat interval in ms (MAESTRO_STREAM_HEARTBEAT_MS). Optional; see above. */
  streamHeartbeatMs?: number;
  /**
   * Child liveness (HARNESS §2, BRO-1767) — the supervisor pings a child that has
   * been silent this long (MAESTRO_CHILD_HEARTBEAT_MS). Optional, like the SSE
   * cadences above: loadConfig always fills it, but a hand-built config literal may
   * omit it and the supervisor falls back to DEFAULT_CHILD_HEARTBEAT_MS.
   */
  childHeartbeatMs?: number;
  /** Silent longer than this (ms) then the child is hung; escalate SIGTERM to
   *  SIGKILL (MAESTRO_CHILD_HUNG_MS). Optional; default DEFAULT_CHILD_HUNG_MS. */
  childHungMs?: number;
  /** Grace after SIGTERM before SIGKILL (ms) (MAESTRO_CHILD_GRACE_MS). Optional;
   *  default DEFAULT_CHILD_GRACE_MS. */
  childGraceMs?: number;
  /**
   * Stop-condition guardrails (AUTONOMY §4, HARNESS §5, BRO-1795) — the RUNTIME defaults the
   * child's stop-condition engine (`harness/stop-conditions.ts`) reads. Each is a policy default a
   * per-work contract can still override: `budget.max_iterations` on the contract wins over
   * `maxIterations` here (frontmatter overrides the runtime default). loadConfig always fills all
   * three; a hand-built config literal may omit them and the engine falls back to its own DEFAULT_*.
   */
  /** Iteration-cap default when a contract sets no `budget.max_iterations` (MAESTRO_MAX_ITERATIONS). */
  maxIterations?: number;
  /** Consecutive empty-diffs / identical-errors before the no-progress halt (MAESTRO_NO_PROGRESS_N). */
  noProgressN?: number;
  /** Context-size ceiling (tokens) past which the child restarts fresh (MAESTRO_CONTEXT_CEILING_TOKENS). */
  contextCeilingTokens?: number;
  /**
   * session.jsonl rotation thresholds (DECISIONS §D3, BRO-1811) — the supervisor's rotating journal
   * bounds each run's `session.jsonl` at whichever of these it reaches first, rotating to
   * `session.jsonl.<n>` + digesting into `summary.md`. The index `event` table keeps the full archive;
   * the FS keeps the tail + summaries. loadConfig always fills both; a hand-built literal may omit them
   * and the journal falls back to its DEFAULT_ROTATE_*.
   */
  /** Bytes ceiling for a session.jsonl segment (MAESTRO_ROTATE_MAX_BYTES). */
  rotateMaxBytes?: number;
  /** Line ceiling for a session.jsonl segment (MAESTRO_ROTATE_MAX_LINES). */
  rotateMaxLines?: number;
  /**
   * Verifier consecutive-fail cap (MAESTRO_VERIFIER_MAX_ATTEMPTS, VERIFIER §5) — how many failing
   * verification attempts a run may burn before it parks `blocked` (reason `verifier_exhausted`) instead
   * of respawning the coding agent again. A run-time policy default; a contract's `budget` does not
   * override it (it bounds the verify loop, not the model-call budget). loadConfig always fills it; a
   * hand-built literal may omit it and the supervisor falls back to DEFAULT_VERIFIER_MAX_ATTEMPTS.
   */
  verifierMaxAttempts?: number;
  /**
   * Mock-model mode (MAESTRO_MOCK_MODEL=1) — mount the dispatch loop with the scripted mock upstream
   * (proxy/mock-model.ts) instead of a real Anthropic upstream, so a running runtime can dispatch
   * sessions with ZERO tokens / no API key (BRO-1822). Today this is the ONLY mode that mounts dispatch:
   * no real upstream exists yet (the sole ModelUpstream is the mock), so a false/unset value leaves the
   * runtime read-only (no supervisor mounted, the kill intent reports no live run). A real upstream +
   * the "live locally" path is a follow-up. Default false.
   */
  mockModel?: boolean;
}

/** Default runtime port when MAESTRO_PORT is unset or invalid. */
export const DEFAULT_PORT = 4319;

/** Ping a child idle-silent for this long (HARNESS §2: heartbeat every 60 s). */
export const DEFAULT_CHILD_HEARTBEAT_MS = 60_000;
/** Silent past this then hung (HARNESS §2: silent > 5 min then SIGTERM). */
export const DEFAULT_CHILD_HUNG_MS = 300_000;
/** SIGTERM to SIGKILL grace (HARNESS §2: 15 s grace, then escalate). */
export const DEFAULT_CHILD_GRACE_MS = 15_000;

/** Iteration cap when a contract sets no `budget.max_iterations` (AUTONOMY §4 "start 20–50"; F3 pins 30). */
export const DEFAULT_MAX_ITERATIONS = 30;
/** Consecutive empty diffs / identical errors before the no-progress halt (AUTONOMY §4, F3 §5). */
export const DEFAULT_NO_PROGRESS_N = 3;
/** Context-token ceiling past which the child restarts fresh (HARNESS §5). Conservative default: a
 *  ~200k working window with headroom for the final progress.md write + the restart signal. Tunable
 *  per model/host via MAESTRO_CONTEXT_CEILING_TOKENS. */
export const DEFAULT_CONTEXT_CEILING_TOKENS = 160_000;

/** Verifier consecutive-fail cap when unset (VERIFIER §5) — the protocol's `VERIFIER_MAX_ATTEMPTS`. */
export const DEFAULT_VERIFIER_MAX_ATTEMPTS = VERIFIER_MAX_ATTEMPTS;

/** session.jsonl rotates at 5 MB (DECISIONS §D3). */
export const DEFAULT_ROTATE_MAX_BYTES = 5 * 1024 * 1024;
/** …or 5,000 lines, whichever comes first (DECISIONS §D3). */
export const DEFAULT_ROTATE_MAX_LINES = 5000;

/** Build the runtime config from an environment map (defaults to process.env). */
export function loadConfig(env: Record<string, string | undefined> = process.env): RuntimeConfig {
  const workspace = resolve(env.MAESTRO_WORKSPACE ?? process.cwd());
  // A misconfigured port (non-numeric, ≤0, or above the TCP u16 ceiling) falls
  // back to the default rather than being silently clamped by the socket layer
  // — Bun.serve would bind 65535 while the startup log claimed the larger port.
  const parsedPort = Number(env.MAESTRO_PORT);
  const validPort = Number.isInteger(parsedPort) && parsedPort > 0 && parsedPort <= 65535;
  const port = validPort ? parsedPort : DEFAULT_PORT;
  // Resolve the index override against the workspace for symmetry with lockPath:
  // an absolute MAESTRO_INDEX is unchanged, a relative one anchors to the
  // workspace root (never cwd, which a long-lived supervisor does not pin).
  const indexPath = resolve(workspace, env.MAESTRO_INDEX ?? ".maestro/index.db");
  const lockPath = resolve(workspace, ".maestro/runtime.lock");
  // SSE cadence overrides: a positive integer wins; anything else leaves the field
  // undefined so the stream route applies its own default (never a 0ms busy-loop).
  const streamPollMs = positiveInt(env.MAESTRO_STREAM_POLL_MS);
  const streamHeartbeatMs = positiveInt(env.MAESTRO_STREAM_HEARTBEAT_MS);
  // Child liveness cadences: a positive override wins, else the HARNESS §2 default
  // (unlike the SSE cadences, these always resolve to a concrete number — the
  // liveness monitor has no route-layer default to fall back to).
  const childHeartbeatMs =
    positiveInt(env.MAESTRO_CHILD_HEARTBEAT_MS) ?? DEFAULT_CHILD_HEARTBEAT_MS;
  const childHungMs = positiveInt(env.MAESTRO_CHILD_HUNG_MS) ?? DEFAULT_CHILD_HUNG_MS;
  const childGraceMs = positiveInt(env.MAESTRO_CHILD_GRACE_MS) ?? DEFAULT_CHILD_GRACE_MS;
  // Stop-condition guardrails (BRO-1795): a positive override wins, else the AUTONOMY §4 / HARNESS §5
  // default. Like the child-liveness cadences, these always resolve to a concrete number — the
  // engine has an ultimate fallback but the runtime hands it a resolved value.
  const maxIterations = positiveInt(env.MAESTRO_MAX_ITERATIONS) ?? DEFAULT_MAX_ITERATIONS;
  const noProgressN = positiveInt(env.MAESTRO_NO_PROGRESS_N) ?? DEFAULT_NO_PROGRESS_N;
  const contextCeilingTokens =
    positiveInt(env.MAESTRO_CONTEXT_CEILING_TOKENS) ?? DEFAULT_CONTEXT_CEILING_TOKENS;
  // session.jsonl rotation thresholds (BRO-1811): a positive override wins, else the D3 default.
  const rotateMaxBytes = positiveInt(env.MAESTRO_ROTATE_MAX_BYTES) ?? DEFAULT_ROTATE_MAX_BYTES;
  const rotateMaxLines = positiveInt(env.MAESTRO_ROTATE_MAX_LINES) ?? DEFAULT_ROTATE_MAX_LINES;
  // Verifier consecutive-fail cap (BRO-1794): a positive override wins, else the VERIFIER §5 default.
  const verifierMaxAttempts =
    positiveInt(env.MAESTRO_VERIFIER_MAX_ATTEMPTS) ?? DEFAULT_VERIFIER_MAX_ATTEMPTS;
  // Mock-model mode is an explicit opt-in (only "1" enables it) — the dispatch mount is a spawn-capable
  // surface, so it stays off unless the operator asks for the token-free mock loop.
  const mockModel = env.MAESTRO_MOCK_MODEL === "1";
  return {
    port,
    workspace,
    indexPath,
    lockPath,
    streamPollMs,
    streamHeartbeatMs,
    childHeartbeatMs,
    childHungMs,
    childGraceMs,
    maxIterations,
    noProgressN,
    contextCeilingTokens,
    rotateMaxBytes,
    rotateMaxLines,
    verifierMaxAttempts,
    mockModel,
  };
}

/** A strictly-positive integer from an env string, or undefined (use the default). */
function positiveInt(raw: string | undefined): number | undefined {
  const n = Number(raw);
  return Number.isInteger(n) && n > 0 ? n : undefined;
}
