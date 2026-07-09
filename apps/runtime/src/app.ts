// The runtime's Hono app (BRO-1790 skeleton). A pure factory — no globals — so
// it is unit-testable via `app.request()` without binding a socket. The reads
// (`/api/tree`, `/api/board`, …), intents, and the SSE stream (API.md) land on
// top of this in P1; today it serves only `/health`.

import { MAESTRO_PROTOCOL_VERSION, X_MAESTRO_PROTOCOL } from "@maestro/protocol";
import { Hono } from "hono";
import pkg from "../package.json";
import type { RuntimeConfig } from "./config";

/** The runtime's own version — the self-host binary's version, from package.json. */
export const RUNTIME_VERSION = pkg.version;

/** The `/health` payload. `ok` is the liveness contract the P0-exit check greps for. */
export interface HealthReport {
  ok: true;
  service: "@maestro/runtime";
  version: string;
  /** The wire protocol version the runtime speaks (imported from @maestro/protocol). */
  protocol: number;
  workspace: string;
  /** The derived index — located, not yet opened (P0 exit). */
  index: { path: string; status: "stub" };
  uptime_s: number;
}

/**
 * Build the runtime's Hono app. `startedAt` is an epoch-ms stamp used for the
 * `uptime_s` field. Pure and side-effect-free — the caller decides whether to
 * bind a socket (see index.ts).
 */
export function createApp(config: RuntimeConfig, startedAt: number) {
  const app = new Hono();

  app.get("/health", (c) => {
    const report: HealthReport = {
      ok: true,
      service: "@maestro/runtime",
      version: RUNTIME_VERSION,
      protocol: MAESTRO_PROTOCOL_VERSION,
      workspace: config.workspace,
      index: { path: config.indexPath, status: "stub" },
      uptime_s: Math.max(0, Math.round((Date.now() - startedAt) / 1000)),
    };
    c.header(X_MAESTRO_PROTOCOL, String(MAESTRO_PROTOCOL_VERSION));
    return c.json(report);
  });

  return app;
}
