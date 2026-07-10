/// <reference types="bun" />
// @maestro/runtime — the 24/7 supervisor (Bun + Hono, `bun build --compile`).
// Owns FS + git + sh; schedules agent sessions; libSQL derived index.
//
// BRO-1790: the skeleton — a Hono service with `/health` and a single-binary
// compile (the self-host deliverable from day one). BRO-1812 opens the index here:
// startup applies the embedded schema (compiled-safe), scans the workspace into the
// `node` table (FLOWS §F9 step 1 — populated BEFORE the read API opens), then serves
// the API §1 reads. The wire contract is imported from @maestro/protocol (PATTERNS §10).
//
// COMPILED-BINARY CAVEAT (BRO-1841 follow-up): the libSQL driver is a native addon
// (`@libsql/<platform>`) that `bun build --compile` cannot embed in the single
// binary — a STATIC import of ./db/client would crash the binary at load, before any
// handler runs. So the index is opened behind a DYNAMIC import inside a try/catch:
//   - dev / `bun run` (native addon present in node_modules) → index opens, reads served;
//   - the compiled binary (addon unresolved) → the open throws, is caught, and the
//     runtime degrades to a /health-only stub with a warning, never crashing.
// Resolving the native-driver story (bun:sqlite swap, or shipping the addon) is BRO-1841.

import { mkdir } from "node:fs/promises";
import { dirname } from "node:path";
import { MAESTRO_PROTOCOL_VERSION } from "@maestro/protocol";
import { createApp } from "./app";
import { loadConfig } from "./config";
import type { IndexDb } from "./db/client";

const config = loadConfig();
const startedAt = Date.now();

let index: IndexDb | undefined;
let indexNodes = 0;
let scanErrorCount = 0;
try {
  // Ensure the index's parent dir exists (libSQL creates the file, not the dir).
  await mkdir(dirname(config.indexPath), { recursive: true });
  // Dynamic import so a native-addon load failure in the compiled binary is CATCHABLE
  // (a static import would evaluate — and crash — at binary init, before this try).
  const { indexUrl, openIndex } = await import("./db/client");
  const { scanIntoIndex } = await import("./scanner");
  const opened = await openIndex(indexUrl(config.indexPath));
  index = opened.db;
  // FLOWS §F9 step 1 — reconcile the workspace into the `node` table before the API
  // opens, so a client's first `/api/tree` sees the current work, not an empty index.
  const { summary, errors } = await scanIntoIndex(index, config.workspace);
  indexNodes = summary.inserted + summary.updated + summary.unchanged;
  scanErrorCount = errors.length;
} catch (err) {
  // The index driver is unavailable (compiled binary without the native addon, or a
  // disk failure). Stay alive on /health so liveness never depends on the index.
  console.warn(
    `maestro runtime · index unavailable, serving /health only (reads disabled): ${(err as Error).message}`,
  );
  index = undefined;
}

const app = createApp(config, startedAt, index);

/** Exported for embedding/tests; the binary serves it when run as the entrypoint. */
export { app, config };

if (import.meta.main) {
  Bun.serve({ port: config.port, fetch: app.fetch });
  const indexStatus = index
    ? `index ${indexNodes} nodes${scanErrorCount ? ` (${scanErrorCount} scan errors)` : ""}`
    : "index unavailable (reads disabled)";
  console.log(
    `maestro runtime · protocol ${MAESTRO_PROTOCOL_VERSION} · http://localhost:${config.port} · workspace ${config.workspace} · ${indexStatus}`,
  );
}
