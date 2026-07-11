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
// Type-only — the dispatch runtime handle. The value (mountDispatch) is dynamically imported in the
// entrypoint block so a bare `import ./index` (embedding/tests) never mounts a proxy server or supervisor.
import type { DispatchRuntime } from "./dispatch";
// Type-only — the D4 lock handle; the value (acquireRuntimeLock) is dynamically imported in the
// entrypoint block so mere `import ./index` (embedding/tests) never acquires a workspace lock.
import type { RuntimeLock } from "./runtime-lock";
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

// The dispatch loop (supervisor + model proxy) is MOUNTED in the entrypoint block below (BRO-1822), after
// recovery, and only in mock-model mode (the only upstream today). It is forward-declared so `createApp`'s
// F8 kill seam can resolve to it lazily — createApp runs at module load (before the mount), so the kill
// closure reads `dispatch` at call time; until the mount (or in a bare `import ./index`) kill finds no
// live run rather than dispatching over an unmounted supervisor.
let dispatch: DispatchRuntime | undefined;

// The watcher's single-flight `nudge` becomes the intent write path's reconcile trigger
// (BRO-1820) — an intent-driven reconcile and an fs.watch one share one scheduler, never overlap.
const app = createApp(
  config,
  startedAt,
  index,
  watcher?.nudge,
  (runId) => dispatch?.kill(runId) ?? false,
  // F10 chat (BRO-1822) — lazy: `dispatch` is mounted below (after recovery, mock-model only), so the
  // chat route reads it at request time. Until then it returns `unsupported_intent`.
  () => dispatch,
);

/** Exported for embedding/tests; the binary serves it when run as the entrypoint. */
export { app, config };

if (import.meta.main) {
  if (rebuildMode) {
    // Kill + rescan, then exit — the "rebuild command" (BRO-1808). Dynamic import keeps the
    // native libSQL addon out of the compiled binary's static graph (same reason as startup).
    try {
      const { rebuildIndex } = await import("./db/rebuild");
      const result = await rebuildIndex(config.indexPath, config.workspace);
      result.handle.client.close();
      const errNote = result.errors.length ? ` (${result.errors.length} scan errors)` : "";
      console.log(
        `maestro runtime · index rebuilt · ${result.nodeCount} nodes${errNote} · ${config.indexPath}`,
      );
      for (const e of result.errors) console.warn(`  scan error: ${e}`);
      process.exit(result.errors.length ? 1 : 0);
    } catch (err) {
      // Symmetric with the startup catch (BRO-1841): in a compiled binary without the native
      // libSQL addon, the dynamic import / openIndex throws — surface a clean message + nonzero
      // exit, not a raw unhandled rejection. The throw precedes rebuildIndex's rm (rebuild.ts
      // statically imports the driver), so nothing is deleted on this path.
      console.error(
        `maestro runtime · index rebuild failed (index driver unavailable?): ${(err as Error).message}`,
      );
      process.exit(1);
    }
  }

  // D4 (BRO-1814): one runtime per workspace. Acquire the lock BEFORE recovery — a second runtime must
  // refuse before it touches the shared index's authoritative tables (else its orphan-parking would park
  // the LIVE runtime's running sessions). runtime-lock is driver-free but dynamic-imported so a bare
  // `import ./index` never grabs a lock. A fresh lock held by a live runtime → refuse + exit(1).
  let lock: RuntimeLock | undefined;
  let lockHeartbeat: ReturnType<typeof setInterval> | undefined;
  // Forward-declared so the ownership-loss callback (wired into the lock BEFORE `shutdown` is defined
  // below) can stand the runtime down. If a heartbeat finds we were stale-stolen, another runtime owns
  // the workspace — this instance must exit so single-writer (D4) is restored.
  let standDown: (holderId: string) => void = () => {};
  {
    const { acquireRuntimeLock, DEFAULT_LOCK_HEARTBEAT_MS, RuntimeLockedError } = await import(
      "./runtime-lock"
    );
    try {
      lock = await acquireRuntimeLock(config.lockPath, {
        onOwnershipLost: (holderId) => standDown(holderId),
      });
    } catch (err) {
      if (err instanceof RuntimeLockedError) {
        console.error(`maestro runtime · refusing to start: ${err.message}`);
        handle?.client.close();
        watcher?.stop();
        process.exit(1);
      }
      throw err;
    }
    lockHeartbeat = setInterval(() => void lock?.heartbeat(), DEFAULT_LOCK_HEARTBEAT_MS);
  }

  // F9 (BRO-1814): reconcile the persisted index BEFORE serving (F9.4). Replay journal tails the index
  // missed (FS-first crash gap), D5 budget derive-and-max, expire dead leases, park orphaned runs. The
  // top-level node-scan already ran; this reconciles events/budget/lease/session-status. Guarded — a
  // recovery failure logs + serves anyway (degraded but up), never crashes the runtime.
  if (index) {
    try {
      const { recoverOnStartup } = await import("./db/recovery");
      const r = await recoverOnStartup(index, { workspace: config.workspace });
      console.log(
        `maestro runtime · recovery · replayed ${r.replayedEvents} events · reconciled ${r.budgetReconciled} budgets · expired ${r.leasesExpired} leases · parked ${r.orphansParked} orphans`,
      );
    } catch (err) {
      console.warn(
        `maestro runtime · recovery incomplete, serving anyway: ${(err as Error).message}`,
      );
    }
  }

  // F2/F3 (BRO-1822): mount the dispatch loop AFTER recovery (so the supervisor never dispatches over
  // un-reconciled state) and ONLY in mock-model mode — the sole model upstream today is the mock, so
  // without it there is nothing to forward to and the runtime stays read-only (the kill seam then finds no
  // live run). Guarded + dynamic-imported like the index open (a mount failure must degrade to reads-only,
  // never crash a healthy read runtime). Needs the open index; skipped when reads are disabled.
  if (index && config.mockModel) {
    try {
      const { mountDispatch } = await import("./dispatch");
      dispatch = await mountDispatch({ db: index, config, hostEnv: process.env });
      console.log(
        `maestro runtime · dispatch mounted (mock-model) · proxy ${dispatch.proxyServer.url}`,
      );
    } catch (err) {
      console.warn(
        `maestro runtime · dispatch unavailable, serving reads only: ${(err as Error).message}`,
      );
      dispatch = undefined;
    }
  } else if (index && !config.mockModel) {
    console.log(
      "maestro runtime · dispatch not mounted (set MAESTRO_MOCK_MODEL=1 for the token-free mock loop; no real model upstream yet)",
    );
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
  const shutdown = (code = 0) => {
    if (shuttingDown) return;
    shuttingDown = true;
    if (lockHeartbeat) clearInterval(lockHeartbeat);
    void lock?.release(); // best-effort — a stale lock is stealable anyway; don't block exit on the unlink
    dispatch?.shutdown(); // SIGKILL live runs + stop the proxy server (BRO-1822)
    watcher?.stop();
    handle?.client.close();
    server.stop();
    process.exit(code);
  };
  process.on("SIGINT", () => shutdown(0));
  process.on("SIGTERM", () => shutdown(0));
  // Lost the workspace lock (stale-stolen after a pause) → another runtime is live here. Stand down with
  // a nonzero exit so a supervisor sees the abnormal handover; release() no-ops (it never deletes a
  // stealer's lock), the heartbeat has already stopped refreshing.
  standDown = (holderId) => {
    console.error(
      `maestro runtime · lost the workspace lock to runtime ${holderId} — standing down (D4 single-writer)`,
    );
    shutdown(1);
  };
}
