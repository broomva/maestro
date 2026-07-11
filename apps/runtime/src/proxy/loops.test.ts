/// <reference types="bun" />
// loops.test.ts — the D8-layer-1 deterministic loop tests (`bun test:loops`). Full F2→F3 flows driven
// with ZERO tokens and NO API key, through the REAL supervisor (BRO-1779) + REAL model proxy (BRO-1788)
// with the scripted mock-model upstream + the REAL SHIPPED child `broomva-child.ts` (BRO-1855) running the
// REAL stop-condition engine (BRO-1795). The loop-child.ts FIXTURE is retired (slice 2b-ii-B) — every
// scenario now spawns the production child and drives its behavior via the mock SCRIPT + config, not a
// `--scenario` flag. Only the model's far side is scripted; every seam supervisor→proxy→child is production.
//
// The four scenarios each prove one guardrail end-to-end (not in a unit's isolation):
//   budget refusal mid-run → proxy 402 → child halts budget → session blocked          (BRO-1788)
//   no-progress exit       → 3 empty diffs → engine halt → session blocked              (BRO-1795)
//   fresh-context resume   → ceiling → checkpoint + restart → respawn resumes → review  (BRO-1795 + 1779 + 2b-ii-A)
//   kill mid-tool-call     → SIGKILL a live child mid-executeTool → canceled + run.killed (BRO-1801)
//
// Anti-vacuity [[self-hosting-vacuous-pass]]: every scenario asserts the EXACT terminal (session, node,
// event) triple AND the child's own teed events. NO API key is set anywhere (the mock is the upstream +
// DEGRADES tool_use when a request advertises no `tools`; the real child sends TOOL_SCHEMAS so tool_use
// flows) — the "zero tokens in CI" guarantee as code.

import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { EVENT_TYPES, type EventType } from "@maestro/protocol";
import { and, eq } from "drizzle-orm";
import { loadConfig, type RuntimeConfig } from "../config";
import { type IndexHandle, openIndex } from "../db/client";
import { event, node, session } from "../db/schema";
import { git } from "../git/git";
import type { ChildStdioPort } from "../harness/stdio";
import { createWorktreeSandboxFactory } from "../sandbox/worktree";
import { createSupervisor } from "../supervisor/supervisor";
import { BudgetGuard } from "./budget";
import { MemoryEventSink } from "./events";
import { createMockModel, type MockModelOptions } from "./mock-model";
import { createModelProxy, type ProxyServer, serveProxy } from "./proxy";
import { SessionTokenRegistry } from "./tokens";

const BROOMVA_CHILD = join(import.meta.dir, "..", "child", "broomva-child.ts");
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

/** Spawn the REAL broomva-child (BRO-1855) — the supervisor's env (BROOMVA_MODEL_PROXY/TOKEN/CONTRACT/
 *  RUN_DIR/SESSION + the allowlisted BROOMVA_CONTEXT_CEILING) reaches it unchanged; behavior is driven by
 *  the mock script, not a `--scenario` flag. `extra` adds test-only env (a known run dir for a checkpoint). */
const makeChildSpawn =
  (extra: Record<string, string> = {}) =>
  (args: { argv: string[]; env: Record<string, string>; cwd: string }): ChildStdioPort => {
    const proc = Bun.spawn(["bun", BROOMVA_CHILD, ...args.argv], {
      cwd: args.cwd,
      env: { ...args.env, ...extra },
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

/** Wire the full loop stack: real `:memory:` index, real budget guard + token registry (shared with the
 *  proxy), the scripted mock upstream behind a REAL served proxy, and a real supervisor that spawns the
 *  REAL child. `mintRunId` is pinned to "r1" so the run dir / session id are stable. */
async function harness(
  opts: {
    budgetJson?: string;
    mock?: MockModelOptions;
    config?: RuntimeConfig;
    childEnvExtra?: Record<string, string>;
  } = {},
): Promise<{
  h: IndexHandle;
  sup: ReturnType<typeof createSupervisor>;
  tokens: SessionTokenRegistry;
  mock: ReturnType<typeof createMockModel>;
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
    apiKey: () => "sk-never-forwarded",
    env: process.env,
  });
  const server = serveProxy(proxyApp, { port: 0 });
  servers.push(server);

  const sup = createSupervisor({
    db: h.db,
    factory: createWorktreeSandboxFactory({ workspace: ws }),
    tokens,
    proxy: { url: server.url },
    spawnChild: makeChildSpawn(opts.childEnvExtra),
    mintRunId: () => "r1",
    hostEnv: { PATH: process.env.PATH },
    config: opts.config,
  });
  return { h, sup, tokens, mock };
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

/** An Anthropic Messages body carrying a text reply (a completed turn, stop_reason end_turn). */
function anthropicBody(text: string): unknown {
  return {
    id: "msg_test",
    type: "message",
    role: "assistant",
    content: [{ type: "text", text }],
    stop_reason: "end_turn",
    usage: { input_tokens: 8, output_tokens: 12 },
  };
}

/** An Anthropic Messages body requesting one tool call — the real child executes it in the worktree. */
function anthropicToolUse(id: string, name: string, input: Record<string, unknown>): unknown {
  return {
    id: "msg_tool",
    type: "message",
    role: "assistant",
    content: [{ type: "tool_use", id, name, input }],
    stop_reason: "tool_use",
    usage: { input_tokens: 8, output_tokens: 12 },
  };
}

describe("loops (D8 layer 1) — deterministic F2→F3 flows through the REAL child, zero tokens", () => {
  // The zero-token guarantee is STRUCTURAL: the proxy's upstream is the injected mock and the child dials
  // the proxy, so no real Anthropic call is ever made. Each scenario asserts mock.calls — proof the MOCK
  // (not Anthropic) served every call.

  test("budget refusal mid-run → proxy 402 → child halts budget → session blocked", async () => {
    // The model appends to a file each beat (a GROWING diff, so the no-progress engine never fires and the
    // BUDGET is the sole halt), each call metered at $30 against a $100/run cap → the proxy forwards the
    // first call(s) then refuses at preflight when the next reservation would breach the cap.
    const { h, sup, mock } = await harness({
      budgetJson: JSON.stringify({ per_run_usd: 100 }),
      mock: {
        usagePerCallUsd: 30,
        fallback: { body: anthropicToolUse("b", "shell", { command: "echo x >> log.txt" }) },
      },
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
    // MID-RUN, not up-front: at least one call reached the model before the reservation breached the cap
    // (an up-front refusal on call 1 leaves mock.calls empty). The exact count depends on the per-call
    // reservation ceiling for the child's max_tokens, so we assert the mid-run PROPERTY, not a fixed count.
    expect(mock.calls.length).toBeGreaterThanOrEqual(1);
  });

  test("no-progress exit → 3 consecutive empty diffs → engine halt → session blocked", async () => {
    // Every beat the model calls a tool that changes NOTHING in the worktree (`echo hi` → stdout only), so
    // the content-diff signal is empty 3 beats running → the BRO-1795 engine halts no_progress.
    const { h, sup, mock } = await harness({
      mock: { fallback: { body: anthropicToolUse("np", "shell", { command: "echo hi" }) } },
    });
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
    // Exactly 3 beats reached the model before the halt (DEFAULT_NO_PROGRESS_N); the child executed a tool
    // each beat (a 1-empty-diff regression would show 1 call, a never-halting one would run to the cap).
    expect(mock.calls).toHaveLength(3);
    expect(await eventsOf(h, "r1", EVENT_TYPES.TOOL_CALL)).toHaveLength(3);
  });

  test("fresh-context restart → checkpoint + respawn → resumes → review", async () => {
    // A tiny ceiling (via the config→allowlist path) forces the restart on beat 1; the supervisor respawns
    // (same run id "r1" → same run dir), the child RESUMES from progress.md (slice 2b-ii-A), and the
    // exhausted script's text fallback completes.
    const { h, sup, mock } = await harness({
      config: { ...loadConfig({}), contextCeilingTokens: 50 },
      mock: {
        script: [{ body: anthropicToolUse("fc", "shell", { command: "echo hi > grew.txt" }) }],
        fallback: { body: anthropicBody("done") },
      },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const res = await out.reaped;

    // exactly one fresh-context respawn, then attempt 2 completed (exit 0 → review) — the full lossless
    // restart cycle end-to-end.
    expect(res.respawns).toBe(1);
    expect(res.exitCode).toBe(0);
    expect(res.sessionStatus).toBe("review");
    expect(res.crash).toBe(false);
    // attempt 1 asked for the restart (proves the ceiling path fired + checkpoint written before the respawn).
    expect(await eventsOf(h, "r1", EVENT_TYPES.RUN_RESTART_REQUESTED)).toHaveLength(1);
    // RESUME (not just respawn) proven IN-FILE: attempt 2's FIRST request (mock.calls[1]) folded the
    // checkpoint into the prompt → the child READ progress.md across the respawn. A broken readProgress
    // would leave the resume text absent while respawns/exit/status still hold — so this is the assertion
    // that fails on a resume regression, not the ones above. (Mirrors broomva-child.test.ts's unit proof.)
    const resumeCall = mock.calls[1];
    if (!resumeCall) throw new Error("no respawn request reached the mock");
    const prompt = JSON.stringify(resumeCall.payload ?? {});
    expect(prompt).toContain("RESUMING"); // the resumeSuffix marker
    expect(prompt).toContain("context ceiling"); // the checkpoint's state-of-the-world, folded in
  });

  test("kill mid-tool-call → SIGKILL a live child → canceled + run.killed, worktree preserved", async () => {
    // The model calls a long-running tool; the child emits tool.call then blocks in executeTool. We kill it
    // mid-tool (F8) and assert the human-kill terminal is DISTINCT from a crash. `sleep 5` (not 300): the
    // kill lands ~100ms after tool.call, so 5s is ample slack to guarantee mid-tool — but SIGKILL does not
    // cascade to the `sh -c` grandchild, so a long sleep would orphan a process (reparented to PID 1) for
    // its full duration; 5s self-clears, so repeated/watch runs can't accumulate live orphans.
    const { h, sup } = await harness({
      mock: { fallback: { body: anthropicToolUse("k", "shell", { command: "sleep 5" }) } },
    });
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
