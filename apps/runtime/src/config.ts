/// <reference types="bun" />
// Runtime configuration surface (STACK.md §runtime). BRO-1790 scope: a sketch,
// resolved from the environment with self-host-friendly defaults. The index and
// the singleton lock are only *located* here — the libSQL index opens at the P0
// exit, the `runtime.lock` singleton (D4) is acquired in P2.

import { resolve } from "node:path";

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
}

/** Default runtime port when MAESTRO_PORT is unset or invalid. */
export const DEFAULT_PORT = 4319;

/** Ping a child idle-silent for this long (HARNESS §2: heartbeat every 60 s). */
export const DEFAULT_CHILD_HEARTBEAT_MS = 60_000;
/** Silent past this then hung (HARNESS §2: silent > 5 min then SIGTERM). */
export const DEFAULT_CHILD_HUNG_MS = 300_000;
/** SIGTERM to SIGKILL grace (HARNESS §2: 15 s grace, then escalate). */
export const DEFAULT_CHILD_GRACE_MS = 15_000;

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
  };
}

/** A strictly-positive integer from an env string, or undefined (use the default). */
function positiveInt(raw: string | undefined): number | undefined {
  const n = Number(raw);
  return Number.isInteger(n) && n > 0 ? n : undefined;
}
