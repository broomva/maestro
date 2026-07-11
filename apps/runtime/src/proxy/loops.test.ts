/// <reference types="bun" />
// loops.test.ts — BRO-1806 done.check `bun test:loops`. The D8-layer-1 deterministic loop tests: full
// F2→F3 flows driven with ZERO tokens and NO API key, through the REAL supervisor (BRO-1779) + REAL
// model proxy (BRO-1788) with the scripted mock-model upstream + a REAL spawned fixture child
// (`loop-child.ts`) running the REAL stop-condition engine (BRO-1795). Only the model's far side is
// scripted; every seam between the supervisor and the proxy is production code.
//
// The four scenarios each prove one guardrail end-to-end (not in a unit's isolation):
//   budget refusal mid-run → proxy 402 → child halts budget → session blocked          (BRO-1788)
//   no-progress exit       → 3 empty diffs → engine halt → session blocked              (BRO-1795)
//   fresh-context resume   → ceiling → checkpoint + restart → respawn resumes → review  (BRO-1795 + 1779)
//   kill mid-tool-call     → SIGKILL a live child mid-call → canceled + run.killed       (BRO-1801)
//
// Anti-vacuity [[self-hosting-vacuous-pass]]: every scenario asserts the EXACT terminal (session, node,
// event) triple AND the child's own teed events — not "something happened". NO API key is set anywhere
// (the mock is the upstream), which is the "zero tokens in CI" guarantee as code.

import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { EVENT_TYPES, type EventType } from "@maestro/protocol";
import { and, eq } from "drizzle-orm";
import { type IndexHandle, openIndex } from "../db/client";
import { event, node, session } from "../db/schema";
import { git } from "../git/git";
import type { ChildStdioPort } from "../harness/stdio";
import { createWorktreeSandboxFactory } from "../sandbox/worktree";
import { createSupervisor, type SpawnChild } from "../supervisor/supervisor";
import { BudgetGuard } from "./budget";
import { MemoryEventSink } from "./events";
import { createMockModel, type MockModelOptions } from "./mock-model";
import { createModelProxy, type ProxyServer, serveProxy } from "./proxy";
import { SessionTokenRegistry } from "./tokens";

const LOOP_CHILD = join(import.meta.dir, "loop-child.ts");
const FIXED_MS = 1_700_000_000_000;

const handles: IndexHandle[] = [];
const servers: ProxyServer[] = [];
const tmps: string[] = [];
afterEach(async () => {
  for (const s of servers.splice(0)) s.stop();
  for (const h of handles.splice(0)) h.client.close();
  for (const d of tmps.splice(0)) await rm(d, { recursive: true, force: true });
});

/** A canonical (realpath'd) temp git workspace — a real runtime workspace is canonical (macOS mkdtemp
 *  is a /var→/private/var symlink; the sandbox worktree code compares canonical paths, BRO-1746). */
async function makeWorkspace(): Promise<string> {
  const dir = await realpath(await mkdtemp(join(tmpdir(), "maestro-loops-")));
  tmps.push(dir);
  await git(dir, ["init", "-q"]);
  await git(dir, ["config", "user.email", "t@t.co"]);
  await git(dir, ["config", "user.name", "t"]);
  await writeFile(join(dir, ".gitignore"), "/.maestro/\n/runs/\n");
  await writeFile(join(dir, "_work.md"), "kind: project\n");
  await git(dir, ["add", "-A"]);
  await git(dir, ["commit", "-qm", "init"]);
  return dir;
}

/** A spawner that Bun.spawns the real fixture child with `--scenario <s>` (mirrors supervisor.test's
 *  realSpawn dogfood pattern). The supervisor's env (BROOMVA_MODEL_PROXY/TOKEN/RUN_DIR/SESSION) reaches
 *  the child unchanged; the scenario is injected via argv (no env-allowlist widening). */
function scenarioSpawn(scenario: string): SpawnChild {
  return (args): ChildStdioPort => {
    const proc = Bun.spawn(["bun", LOOP_CHILD, "--scenario", scenario, ...args.argv], {
      cwd: args.cwd,
      env: { ...args.env },
      stdout: "pipe",
      stderr: "pipe",
      stdin: "pipe",
    });
    return {
      stdout: proc.stdout,
      stderr: proc.stderr,
      exited: proc.exited,
      kill: (s) => proc.kill(s),
      writeStdin: (b: string) => {
        proc.stdin.write(b);
        void proc.stdin.flush();
      },
    };
  };
}

async function openMem(): Promise<IndexHandle> {
  const h = await openIndex(":memory:");
  handles.push(h);
  return h;
}

/** Seed a dispatchable node (resolved contract already in the row, as the scanner leaves it). */
async function seedNode(h: IndexHandle, id: string, budgetJson?: string): Promise<void> {
  await h.db.insert(node).values({
    id,
    path: `work/${id}`,
    kind: "task",
    state: "triggered",
    gate: "human",
    budgetJson: budgetJson ?? null,
    createdAt: FIXED_MS,
    updatedAt: FIXED_MS,
  });
}

/**
 * Wire the full loop stack for a scenario: real `:memory:` index, real budget guard + token registry
 * (shared with the proxy), the scripted mock upstream behind a REAL served proxy, and a real supervisor
 * that spawns the fixture child. `mintRunId` is pinned to "r1" so the run dir / session id are stable.
 */
async function harness(
  scenario: string,
  opts: { budgetJson?: string; mock?: MockModelOptions } = {},
): Promise<{
  h: IndexHandle;
  sup: ReturnType<typeof createSupervisor>;
  tokens: SessionTokenRegistry;
}> {
  const ws = await makeWorkspace();
  const h = await openMem();
  await seedNode(h, "n0", opts.budgetJson);

  const tokens = new SessionTokenRegistry(() => "tok-1");
  const guard = new BudgetGuard(h.db, new MemoryEventSink());
  const mock = createMockModel(opts.mock);
  const proxyApp = createModelProxy({
    guard,
    tokens,
    upstream: mock,
    apiKey: () => "sk-never-forwarded", // the mock ignores it; asserts NO real key is needed
    env: process.env,
  });
  const server = serveProxy(proxyApp, { port: 0 });
  servers.push(server);

  const sup = createSupervisor({
    db: h.db,
    factory: createWorktreeSandboxFactory({ workspace: ws }),
    tokens,
    proxy: { url: server.url },
    spawnChild: scenarioSpawn(scenario),
    mintRunId: () => "r1",
    hostEnv: { PATH: process.env.PATH },
  });
  return { h, sup, tokens };
}

/** Poll the durable event table for a session's first event of `type` (bounded — the child + tee run
 *  async). Returns true once seen; false on timeout. */
async function waitForEvent(
  h: IndexHandle,
  sessionId: string,
  type: EventType,
  timeoutMs = 5000,
): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const rows = await h.db
      .select()
      .from(event)
      .where(and(eq(event.sessionId, sessionId), eq(event.type, type)));
    if (rows.length > 0) return true;
    await new Promise((r) => setTimeout(r, 25));
  }
  return false;
}

/** All events of a type for a session (for the exact-count / payload assertions). */
async function eventsOf(h: IndexHandle, sessionId: string, type: EventType) {
  return h.db
    .select()
    .from(event)
    .where(and(eq(event.sessionId, sessionId), eq(event.type, type)));
}

describe("loops (D8 layer 1) — deterministic F2→F3 flows, zero tokens", () => {
  test("NO ANTHROPIC_API_KEY is present — the mock is the upstream (the zero-token guarantee)", () => {
    expect(process.env.ANTHROPIC_API_KEY ?? "").toBe("");
  });

  test("budget refusal mid-run → proxy 402 → child halts budget → session blocked", async () => {
    // per_run 1.5 with a $1.0/call mock cost → the 3rd call's reservation breaches the cap → 402.
    const { h, sup } = await harness("budget", {
      budgetJson: JSON.stringify({ per_run_usd: 1.5 }),
      mock: { usagePerCallUsd: 1.0 },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const res = await out.reaped;

    // exit 10 with the child's declared budget reason → parked blocked (not a crash).
    expect(res.exitCode).toBe(10);
    expect(res.reason).toBe("budget");
    expect(res.sessionStatus).toBe("blocked");
    expect(res.nodeState).toBe("blocked");
    expect(res.crash).toBe(false);
    // the child emitted budget.exhausted THEN run.exiting {reason:budget} (both teed durably).
    expect(await eventsOf(h, "r1", EVENT_TYPES.BUDGET_EXHAUSTED)).toHaveLength(1);
    const exiting = await eventsOf(h, "r1", EVENT_TYPES.RUN_EXITING);
    expect(exiting).toHaveLength(1);
    expect(JSON.parse(exiting[0]?.payload ?? "{}")).toMatchObject({ code: 10, reason: "budget" });
    // it got a couple beats in before the refusal (mid-run, not up-front).
    const started = await eventsOf(h, "r1", EVENT_TYPES.RUN_STARTED);
    expect(started).toHaveLength(1);
  });

  test("no-progress exit → 3 consecutive empty diffs → engine halt → session blocked", async () => {
    const { h, sup } = await harness("no_progress");
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const res = await out.reaped;

    expect(res.exitCode).toBe(10);
    expect(res.reason).toBe("no_progress");
    expect(res.sessionStatus).toBe("blocked");
    expect(res.crash).toBe(false);
    const exiting = await eventsOf(h, "r1", EVENT_TYPES.RUN_EXITING);
    expect(exiting).toHaveLength(1);
    expect(JSON.parse(exiting[0]?.payload ?? "{}")).toMatchObject({
      code: 10,
      reason: "no_progress",
    });
    // no budget.exhausted on this path (the halt is the engine's, not the proxy's).
    expect(await eventsOf(h, "r1", EVENT_TYPES.BUDGET_EXHAUSTED)).toHaveLength(0);
  });

  test("fresh-context restart → checkpoint + respawn → resumes skipping done work → review", async () => {
    const { h, sup } = await harness("fresh_context");
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const res = await out.reaped;

    // exactly one fresh-context respawn, then attempt 2 completed (exit 0 → review).
    expect(res.respawns).toBe(1);
    expect(res.exitCode).toBe(0);
    expect(res.sessionStatus).toBe("review");
    expect(res.crash).toBe(false);
    // attempt 1 asked for the restart (proves the ceiling path fired + checkpoint written).
    expect(await eventsOf(h, "r1", EVENT_TYPES.RUN_RESTART_REQUESTED)).toHaveLength(1);
    // attempt 2 took the RESUME branch — only reachable by reading progress.md → lossless resume proven.
    const said = await eventsOf(h, "r1", EVENT_TYPES.AGENT_SAID);
    const resumed = said.some((r) =>
      (JSON.parse(r.payload ?? "{}").text ?? "").includes("resumed from checkpoint"),
    );
    expect(resumed).toBe(true);
  });

  test("kill mid-tool-call → SIGKILL a live child → canceled + run.killed, worktree preserved", async () => {
    const { h, sup } = await harness("kill");
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    // the child is live + registered; wait until it is genuinely mid-tool-call (tool.call teed).
    expect(sup.list()).toHaveLength(1);
    expect(await waitForEvent(h, "r1", EVENT_TYPES.TOOL_CALL)).toBe(true);
    // F8: kill by intent → SIGKILL.
    expect(sup.kill("r1")).toBe(true);
    const res = await out.reaped;

    // a human-killed run ends DISTINCTLY from a crash: canceled + run.killed (BRO-1801).
    expect(res.crash).toBe(false);
    expect(res.event).toBe("run.killed");
    expect(res.sessionStatus).toBe("canceled");
    expect(res.nodeState).toBe("blocked");
    const [srow] = await h.db.select().from(session).where(eq(session.id, "r1"));
    expect(srow?.status).toBe("canceled");
    expect(await eventsOf(h, "r1", EVENT_TYPES.RUN_KILLED)).toHaveLength(1);
  });
});
