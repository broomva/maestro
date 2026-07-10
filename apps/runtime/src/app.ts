// The runtime's Hono app (BRO-1790 skeleton). A pure factory — no globals — so
// it is unit-testable via `app.request()` without binding a socket. The reads
// (`/api/tree`, `/api/board`, …) and the SSE stream (`/api/stream`,
// `/api/sessions/:id/stream`) land on top of this in P1; intents follow in P2.
// Without an index handle it serves only `/health` (the compiled-binary path).

import { MAESTRO_PROTOCOL_VERSION, X_MAESTRO_PROTOCOL } from "@maestro/protocol";
import { Hono } from "hono";
import pkg from "../package.json";
import { registerReadRoutes } from "./api/reads";
import { registerStreamRoutes } from "./api/stream";
import type { RuntimeConfig } from "./config";
import type { IndexDb } from "./db/client";

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
  /**
   * The derived index — `stub` until a handle is wired in (pure-unit createApp),
   * `open` once the runtime has opened + scanned it (index.ts startup, BRO-1812).
   */
  index: { path: string; status: "stub" | "open" };
  uptime_s: number;
}

/**
 * Build the runtime's Hono app. `startedAt` is an epoch-ms stamp used for the
 * `uptime_s` field. When `index` is supplied (the open libSQL handle), the API §1
 * read routes AND the SSE stream routes are mounted over it and `/health` reports
 * the index as `open`; without it the app serves only `/health` (the pure-unit
 * path). The caller decides whether to bind a socket (see index.ts).
 */
export function createApp(config: RuntimeConfig, startedAt: number, index?: IndexDb) {
  const app = new Hono();

  app.get("/health", (c) => {
    const report: HealthReport = {
      ok: true,
      service: "@maestro/runtime",
      version: RUNTIME_VERSION,
      protocol: MAESTRO_PROTOCOL_VERSION,
      workspace: config.workspace,
      index: { path: config.indexPath, status: index ? "open" : "stub" },
      uptime_s: Math.max(0, Math.round((Date.now() - startedAt) / 1000)),
    };
    c.header(X_MAESTRO_PROTOCOL, String(MAESTRO_PROTOCOL_VERSION));
    return c.json(report);
  });

  if (index) {
    registerReadRoutes(app, { db: index, workspace: config.workspace });
    registerStreamRoutes(app, {
      db: index,
      pollMs: config.streamPollMs,
      heartbeatMs: config.streamHeartbeatMs,
    });
  }

  return app;
}
