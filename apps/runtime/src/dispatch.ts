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
import { EVENT_TYPES } from "@maestro/protocol";
import { eq } from "drizzle-orm";
import { parsePayload } from "./api/event-projection";
import type { RuntimeConfig } from "./config";
import type { IndexDb } from "./db/client";
import { event } from "./db/schema";
import { buildClaudeProviderEnv } from "./harness/spawn-contract";
import { fromBunSubprocess } from "./harness/stdio";
import { BudgetGuard, deriveDayTotal } from "./proxy/budget";
import { fsJournalSink } from "./proxy/events";
import { createMockModel, loadMockScriptFromEnv } from "./proxy/mock-model";
import { createModelProxy, type ModelUpstream, type ProxyServer, serveProxy } from "./proxy/proxy";
import { SessionTokenRegistry } from "./proxy/tokens";
import { createWorktreeSandboxFactory } from "./sandbox/worktree";
import { createSupervisor, type SpawnChild, type Supervisor } from "./supervisor/supervisor";

/** UTC day length in ms — the BudgetGuard's day bucket (proxy/budget.ts `dayBucket`). */
const DAY_MS = 86_400_000;

/**
 * Seed today's metered spend from the DURABLE budget events so the per-day cap is NOT reset to zero on
 * every runtime restart (BRO-1822 latent gap; `deriveDayTotal` was documented as this seed but never
 * wired). By mount time, F9 recovery (index.ts, before the mount) has replayed every journal-only
 * `budget.metered` into the index, so reading them here is sound (they are absent from the index before
 * recovery — the BRO-1814 replay is what makes this correct). Best-effort: a read failure just starts the
 * day total at 0 (a fresh, never-blocking cap), never throws into the mount.
 */
export async function deriveDayTotalUsdFromIndex(db: IndexDb, nowMs: number): Promise<number> {
  const dayStartMs = Math.floor(nowMs / DAY_MS) * DAY_MS;
  try {
    const rows = await db.select().from(event).where(eq(event.type, EVENT_TYPES.BUDGET_METERED));
    const metered = rows.map((r) => {
      const p = parsePayload(r.payload) as { session?: unknown; usd?: unknown } | undefined;
      return {
        session: typeof p?.session === "string" ? p.session : "",
        usd: typeof p?.usd === "number" ? p.usd : 0,
        ts: r.ts,
      };
    });
    return deriveDayTotal(metered, dayStartMs);
  } catch {
    return 0;
  }
}

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

/** The Claude Code CLI runner entry (BRO-1912) — the subscription provider's child. */
const CLAUDE_RUNNER_ENTRY = fileURLToPath(new URL("./child/claude-runner.ts", import.meta.url));

/**
 * The `claude` subscription spawner: `bun run <claude-runner.ts> <argv>`. Identical stdio/tee contract to
 * {@link devSpawnChild} (same `fromBunSubprocess` port), so the supervisor wraps it unchanged — the only
 * difference is the child entry: this one spawns the Claude Code CLI internally and translates its stream.
 *
 * TRUST MODEL — still DENY-BY-DEFAULT, not `{...process.env}`. The subscription CLI authenticates on the
 * operator's OWN channel (macOS Keychain via USER/LOGNAME, or CLAUDE_CODE_OAUTH_TOKEN on Linux/CI), so the
 * env is the deny-by-default floor PLUS that NARROW named auth channel — never the full host env. Leaking
 * every host secret (`ANTHROPIC_API_KEY`, `CLOUDFLARE_API_TOKEN`, …) into a `bypassPermissions` agent is
 * the R5 KEY-EXFIL class the harness exists to prevent; `ANTHROPIC_API_KEY` is force-deleted so the CLI
 * bills the subscription, not the API. {@link buildClaudeProviderEnv} owns that policy; `args.env` layers
 * the supervisor's BROOMVA_* contract (BROOMVA_RUN_DIR) so the runner finds its worktree. Confinement is
 * the WORKTREE (cwd) + `--permission-mode` + this env floor — the runner may spend the subscription, but
 * it cannot touch an unrelated host credential.
 */
export const claudeSpawnChild: SpawnChild = (args) =>
  fromBunSubprocess(
    Bun.spawn([...args.commandPrefix, "bun", "run", CLAUDE_RUNNER_ENTRY, ...args.argv], {
      cwd: args.cwd,
      env: buildClaudeProviderEnv(process.env, args.env),
      stdout: "pipe",
      stderr: "pipe",
      stdin: "pipe",
    }),
  );

/** Select the child spawner for a provider: the CLI runner for `claude`, else the proxy-driven
 *  broomva-child (`mock`, or unset when a test drives the mock loop directly). `codex` is declared but
 *  NOT wired yet, so it FAILS CLOSED — a `codex` mount must never silently run the mock/broomva-child
 *  spawner under a provider the operator did not actually get. (index.ts already gates the mount to
 *  claude|mock, so this throw is defense-in-depth against a future caller, not a live path.) */
function spawnerForProvider(provider: RuntimeConfig["provider"]): SpawnChild {
  if (provider === "claude") return claudeSpawnChild;
  if (provider === "codex") {
    throw new Error("MAESTRO_PROVIDER=codex is not wired yet — use claude or mock");
  }
  return devSpawnChild;
}

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
export async function mountDispatch(deps: MountDispatchDeps): Promise<DispatchRuntime> {
  const tokens = new SessionTokenRegistry();
  // Seed today's metered spend so the per-day cap survives a restart (BRO-1822) — before any proxy is
  // listening, so a slow/failed read never leaks a socket. Best-effort (returns 0 on failure).
  const dayTotalUsd = await deriveDayTotalUsdFromIndex(deps.db, Date.now());
  // Budget events are DURABLE — journaled to the run's session.jsonl (the `event` table is a projection),
  // NOT a memory tap. A data-loss advisory-write here is exactly the class of bug P20 caught on BRO-1811.
  const guard = new BudgetGuard(deps.db, fsJournalSink(), { dayTotalUsd });
  // Default to the scripted mock; when MAESTRO_MOCK_SCRIPT names a JSON script file (the P3-exit E2E's
  // mock-dispatch-to-gate seam, BRO-1821), the spawned runtime's mock drives a real run to a completion
  // gate. Unset → the bare "ok" mock, unchanged. Never consulted for a claude/codex provider.
  const upstream = deps.upstream ?? createMockModel(loadMockScriptFromEnv());
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
  // The proxy is already listening on its port. If assembling the supervisor throws (a bad workspace, a
  // sandbox-factory error), that port would leak: index.ts's mount catch clears `dispatch` to undefined,
  // so its shutdown hook can no longer reach this server to stop it. Tear down what we started, then
  // rethrow so the caller's degrade-to-reads path still runs.
  try {
    const supervisor = createSupervisor({
      db: deps.db,
      factory: createWorktreeSandboxFactory({ workspace: deps.config.workspace }),
      tokens,
      proxy: { url: proxyServer.url },
      spawnChild: deps.spawnChild ?? spawnerForProvider(deps.config.provider),
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
  } catch (err) {
    proxyServer.stop();
    throw err;
  }
}
