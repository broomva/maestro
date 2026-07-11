/// <reference types="bun" />
// dispatch.ts (BRO-1822 slice 1) — MOUNT the F2/F3 dispatch loop into the running runtime. The supervisor
// (BRO-1779), model proxy (BRO-1788), and broomva-child (BRO-1855/1862) exist as tested library code but
// have never been assembled into the 24/7 process (index.ts wired only reads/scan/watch/recovery). This is
// that assembly: a loopback model proxy + a supervisor that spawns the real child. The child dials the
// proxy as BROOMVA_MODEL_PROXY with a per-session bearer (it never holds the runtime/Anthropic key); the
// proxy meters through the BudgetGuard (budget events journaled to the DURABLE session.jsonl, D-DURABILITY)
// and forwards to the upstream.
//
// SCOPE (slice 1): MOCK-model mode only. The sole ModelUpstream today is proxy/mock-model.ts — no real
// Anthropic upstream exists — so a mounted runtime dispatches with ZERO tokens / no key. A real upstream +
// the "live locally" path is a follow-up. The child is spawned via `bun run <broomva-child.ts>` (there is
// no compiled `broomva-child` binary yet — CHILD_BIN on PATH would ENOENT; the compiled-child spawn is
// BRO-1841-adjacent). The dispatch TRIGGER (what makes the running runtime call `dispatch`) is slice 2 (the
// F10 chat endpoint's auto-dispatch); slice 1 exposes `supervisor` so that + tests can drive it.

import { fileURLToPath } from "node:url";
import type { RuntimeConfig } from "./config";
import type { IndexDb } from "./db/client";
import { fromBunSubprocess } from "./harness/stdio";
import { BudgetGuard } from "./proxy/budget";
import { fsJournalSink } from "./proxy/events";
import { createMockModel } from "./proxy/mock-model";
import { createModelProxy, type ModelUpstream, type ProxyServer, serveProxy } from "./proxy/proxy";
import { SessionTokenRegistry } from "./proxy/tokens";
import { createWorktreeSandboxFactory } from "./sandbox/worktree";
import { createSupervisor, type SpawnChild, type Supervisor } from "./supervisor/supervisor";

/** The real child entry, resolved from this module — spawned via `bun run` (no compiled bin yet). */
const CHILD_ENTRY = fileURLToPath(new URL("./child/broomva-child.ts", import.meta.url));

/**
 * The dev/CI child spawner: `bun run <broomva-child.ts> <argv>`. The supervisor's `defaultSpawnChild` runs
 * the `broomva-child` binary on PATH (empty phase-1 commandPrefix), which does not exist yet — this runs
 * the source instead. Reuses `fromBunSubprocess` (the canonical port adapter) so the tee/liveness contract
 * is identical to production. `commandPrefix` (a phase-2 container-exec prefix) is honored if present.
 */
export const devSpawnChild: SpawnChild = (args) =>
  fromBunSubprocess(
    Bun.spawn([...args.commandPrefix, "bun", "run", CHILD_ENTRY, ...args.argv], {
      cwd: args.cwd,
      env: args.env,
      stdout: "pipe",
      stderr: "pipe",
      stdin: "pipe",
    }),
  );

/** The assembled dispatch runtime — the supervisor + the served proxy + lifecycle handles. */
export interface DispatchRuntime {
  /** The live supervisor (F2 dispatch / F8 kill / F10 control). The dispatch TRIGGER (slice 2) uses it. */
  supervisor: Supervisor;
  /** The loopback model proxy the child dials. */
  proxyServer: ProxyServer;
  /** The F8 kill seam wired into `createApp` — kill a live run by id (BRO-1801). */
  kill: (runId: string) => boolean;
  /** Stop every live run + the proxy server (called from the runtime's shutdown hook). */
  shutdown: () => void;
}

export interface MountDispatchDeps {
  /** The open index (the supervisor's authoritative store). */
  db: IndexDb;
  config: RuntimeConfig;
  /** Host env `buildChildEnv` filters to the allowlist (default process.env) — never leaks host secrets. */
  hostEnv?: Record<string, string | undefined>;
  /** The model upstream — default the scripted mock (the only upstream today). Tests inject a script. */
  upstream?: ModelUpstream;
  /** The child spawner — default `devSpawnChild`. Tests inject a fixture / a source-spawner. */
  spawnChild?: SpawnChild;
  /** Mint a run id — injected for deterministic tests (default the supervisor's random id). */
  mintRunId?: () => string;
}

/**
 * Assemble the dispatch loop: a per-session token registry + budget guard (durable fsJournalSink) → a
 * loopback-served model proxy → a supervisor that spawns the real child against that proxy. Returns the
 * supervisor (for the dispatch trigger + tests), the `kill` seam (for `createApp`), and a `shutdown`.
 */
export function mountDispatch(deps: MountDispatchDeps): DispatchRuntime {
  const tokens = new SessionTokenRegistry();
  // Budget events are DURABLE — journaled to the run's session.jsonl (the `event` table is a projection),
  // NOT a memory tap. A data-loss advisory-write here is exactly the class of bug P20 caught on BRO-1811.
  const guard = new BudgetGuard(deps.db, fsJournalSink());
  const upstream = deps.upstream ?? createMockModel();
  const proxyApp = createModelProxy({
    guard,
    tokens,
    upstream,
    // Read lazily at forward time. In mock mode this is empty and never used; the child gets a per-session
    // bearer, never this key. serveProxy binds loopback-only, so the key never leaves the host regardless.
    apiKey: () => process.env.ANTHROPIC_API_KEY ?? "",
    env: process.env,
  });
  const proxyServer = serveProxy(proxyApp, { port: 0 }); // loopback, OS-assigned port
  const supervisor = createSupervisor({
    db: deps.db,
    factory: createWorktreeSandboxFactory({ workspace: deps.config.workspace }),
    tokens,
    proxy: { url: proxyServer.url },
    spawnChild: deps.spawnChild ?? devSpawnChild,
    hostEnv: deps.hostEnv ?? process.env,
    config: deps.config,
    ...(deps.mintRunId ? { mintRunId: deps.mintRunId } : {}),
  });
  return {
    supervisor,
    proxyServer,
    kill: (runId) => supervisor.kill(runId),
    shutdown: () => {
      supervisor.killAll();
      proxyServer.stop();
    },
  };
}
