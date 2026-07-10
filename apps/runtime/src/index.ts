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
import type { IndexDb, IndexHandle } from "./db/client";
// Type-only — erased at compile time, so the compiled /health-only stub never references
// the watcher module (which is dynamically imported below, native-addon-safe).
import type { WatcherHandle } from "./watcher";

const config = loadConfig();
const startedAt = Date.now();

// `--rebuild` (BRO-1808): the index is a rebuildable cache (fs-index.md "cache with teeth") —
// this subcommand deletes `index.db` and rescans the workspace from the FS (truth), then exits.
// In rebuild mode the normal startup below is SKIPPED: no long-lived index open, no watcher (the
// rebuild opens its own handle, runs one scan, and exits). Handled in the `import.meta.main` block.
const rebuildMode = import.meta.main && Bun.argv.includes("--rebuild");

let index: IndexDb | undefined;
let handle: IndexHandle | undefined;
let watcher: WatcherHandle | undefined;
let indexNodes = 0;
let scanErrorCount = 0;
if (!rebuildMode) {
  try {
    // Ensure the index's parent dir exists (libSQL creates the file, not the dir).
    await mkdir(dirname(config.indexPath), { recursive: true });
    // Dynamic import so a native-addon load failure in the compiled binary is CATCHABLE
    // (a static import would evaluate — and crash — at binary init, before this try).
    const { indexUrl, openIndex } = await import("./db/client");
    const { scanIntoIndex } = await import("./scanner");
    handle = await openIndex(indexUrl(config.indexPath));
    // FLOWS §F9 step 1 — reconcile the workspace into the `node` table before the API
    // opens, so a client's first `/api/tree` sees the current work, not an empty index.
    const { summary, errors } = await scanIntoIndex(handle.db, config.workspace);
    indexNodes = summary.inserted + summary.updated + summary.unchanged;
    scanErrorCount = errors.length;
    // Publish the handle now that the scan succeeded — reads are live from here. A scan
    // failure (above) still leaves the clean /health-only stub, never a half-populated index.
    index = handle.db;
    // Keep the index live (BRO-1804): an FS `_work.md` edit → reconcile → a `node.updated`
    // synthetic on the SSE change feed (BRO-1816). Dynamic import (the compiled /health-only
    // stub never loads it) INSIDE ITS OWN try — starting the watcher is a LIVENESS init, and a
    // liveness failure must not cost the read API. `fs.watch(root,{recursive:true})` throws
    // synchronously on registration failure (inotify ENOSPC/EMFILE on a large workspace), so
    // without this guard a watcher-start failure would fall to the outer catch and disable ALL
    // reads despite a healthy index. Degrade to "reads work, no live updates" instead. (Bounding
    // the recursive-watch footprint so it does not exhaust inotify in the first place is BRO-1846.)
    try {
      const { startWatcher } = await import("./watcher");
      watcher = startWatcher(handle.db, config.workspace);
    } catch (watchErr) {
      console.warn(
        `maestro runtime · live watcher unavailable, reads stay up (no live updates): ${(watchErr as Error).message}`,
      );
    }
  } catch (err) {
    // The index driver is unavailable (compiled binary without the native addon, or a disk
    // failure) — this fires only BEFORE `index` is published (the watcher has its own guard
    // above), so `index` is still undefined here. Close the handle if openIndex SUCCEEDED but a
    // later step threw, so the failed startup does not leak the libSQL client/fd.
    handle?.client.close();
    console.warn(
      `maestro runtime · index unavailable, serving /health only (reads disabled): ${(err as Error).message}`,
    );
    index = undefined;
  }
}

const app = createApp(config, startedAt, index);

/** Exported for embedding/tests; the binary serves it when run as the entrypoint. */
export { app, config };

if (import.meta.main) {
  if (rebuildMode) {
    // Kill + rescan, then exit — the "rebuild command" (BRO-1808). Dynamic import keeps the
    // native libSQL addon out of the compiled binary's static graph (same reason as startup).
    const { rebuildIndex } = await import("./db/rebuild");
    const result = await rebuildIndex(config.indexPath, config.workspace);
    result.handle.client.close();
    const errNote = result.errors.length ? ` (${result.errors.length} scan errors)` : "";
    console.log(
      `maestro runtime · index rebuilt · ${result.nodeCount} nodes${errNote} · ${config.indexPath}`,
    );
    for (const e of result.errors) console.warn(`  scan error: ${e}`);
    process.exit(result.errors.length ? 1 : 0);
  }

  const server = Bun.serve({ port: config.port, fetch: app.fetch });
  const indexStatus = index
    ? `index ${indexNodes} nodes${scanErrorCount ? ` (${scanErrorCount} scan errors)` : ""}`
    : "index unavailable (reads disabled)";
  console.log(
    `maestro runtime · protocol ${MAESTRO_PROTOCOL_VERSION} · http://localhost:${config.port} · workspace ${config.workspace} · ${indexStatus}`,
  );

  // Graceful shutdown — release the OS file watcher and the libSQL handle, then stop
  // accepting connections. Without this the recursive watcher keeps the process alive.
  let shuttingDown = false;
  const shutdown = () => {
    if (shuttingDown) return;
    shuttingDown = true;
    watcher?.stop();
    handle?.client.close();
    server.stop();
    process.exit(0);
  };
  process.on("SIGINT", shutdown);
  process.on("SIGTERM", shutdown);
}
