// The runtime's Hono app (BRO-1790 skeleton). A pure factory — no globals — so
// it is unit-testable via `app.request()` without binding a socket. The reads
// (`/api/tree`, `/api/board`, …), the SSE stream (`/api/stream`,
// `/api/sessions/:id/stream`), and the write surface (`POST /api/intents`, BRO-1820)
// all land on top of this in P1. Without an index handle it serves only `/health`
// (the compiled-binary path — reads/stream/intents all need the open index).

import { MAESTRO_PROTOCOL_VERSION, X_MAESTRO_PROTOCOL } from "@maestro/protocol";
import { Hono } from "hono";
import pkg from "../package.json";
import { registerChatRoutes } from "./api/chat";
import { registerIntentRoutes } from "./api/intents";
import { registerReadRoutes } from "./api/reads";
import { registerStreamRoutes } from "./api/stream";
import type { RuntimeConfig } from "./config";
import type { IndexDb } from "./db/client";
import type { DispatchRuntime } from "./dispatch";

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
 * read routes, the SSE stream routes, AND the write surface (`POST /api/intents`)
 * are mounted over it and `/health` reports the index as `open`; without it the app
 * serves only `/health` (the pure-unit path). The caller decides whether to bind a
 * socket (see index.ts).
 */
export function createApp(
  config: RuntimeConfig,
  startedAt: number,
  index?: IndexDb,
  /** Reconcile trigger for the write path — the watcher's single-flight `nudge` (BRO-1820). */
  reconcile?: () => void,
  /** Kill seam for the F8 kill intent — the supervisor's `kill` (BRO-1801). Absent → kill intent 501s. */
  kill?: (sessionId: string) => boolean,
  /**
   * F10 chat seam (BRO-1822) — a LAZY accessor for the mounted dispatch runtime. Lazy because the
   * supervisor is mounted AFTER createApp runs (index.ts), and only in mock-model mode; the chat route
   * reads it at request time. Absent / returning undefined → chat returns `unsupported_intent` 501.
   */
  dispatch?: () => DispatchRuntime | undefined,
) {
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
    registerIntentRoutes(app, { db: index, workspace: config.workspace, reconcile, kill });
    registerChatRoutes(app, {
      db: index,
      dispatch: dispatch ?? (() => undefined),
      pollMs: config.streamPollMs,
    });
  }

  return app;
}
