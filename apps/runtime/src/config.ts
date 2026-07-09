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
}

/** Default runtime port when MAESTRO_PORT is unset or invalid. */
export const DEFAULT_PORT = 4319;

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
  return { port, workspace, indexPath, lockPath };
}
