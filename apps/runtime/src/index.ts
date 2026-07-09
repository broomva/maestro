/// <reference types="bun" />
// @maestro/runtime — the 24/7 supervisor (Bun + Hono, `bun build --compile`).
// Owns FS + git + sh; schedules agent sessions; libSQL derived index.
//
// BRO-1790: the skeleton — a Hono service with `/health` and a single-binary
// compile (the self-host deliverable from day one). The four loops, the libSQL
// index, and the guardrails (budget-in-path, kill switch) land later (P0 exit +
// P1 + P2). The wire contract is imported from @maestro/protocol, never
// redeclared (the no-codegen-drift guarantee, PATTERNS §10).

import { MAESTRO_PROTOCOL_VERSION } from "@maestro/protocol";
import { createApp } from "./app";
import { loadConfig } from "./config";

const config = loadConfig();
const app = createApp(config, Date.now());

/** Exported for embedding/tests; the binary serves it when run as the entrypoint. */
export { app, config };

if (import.meta.main) {
  Bun.serve({ port: config.port, fetch: app.fetch });
  console.log(
    `maestro runtime · protocol ${MAESTRO_PROTOCOL_VERSION} · http://localhost:${config.port} · workspace ${config.workspace}`,
  );
}
