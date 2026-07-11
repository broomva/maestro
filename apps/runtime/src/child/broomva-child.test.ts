/// <reference types="bun" />
// broomva-child.test.ts — BRO-1854 slice-1 done.check. The REAL child, dispatched through the REAL
// supervisor (1779) + REAL served proxy (1788) with the scripted mock upstream (1806), ZERO tokens / NO
// key. Proves the child↔proxy↔model round-trip: one model turn → the child reads the response body and
// emits `agent.said` carrying the model's ACTUAL text → exit 0 → session reaps `review`. A proxy 402
// (budget refused in-path) → the child halts budget → exit 10 → session blocked. Anti-vacuity
// [[self-hosting-vacuous-pass]]: assert the exact terminal triple + the teed agent.said TEXT + that the
// MOCK (not Anthropic) served exactly the expected calls (mock.calls.length).

import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { EVENT_TYPES } from "@maestro/protocol";
import { and, eq } from "drizzle-orm";
import { loadConfig, type RuntimeConfig } from "../config";
import { type IndexHandle, openIndex } from "../db/client";
import { event, node } from "../db/schema";
import { git } from "../git/git";
import type { ChildStdioPort } from "../harness/stdio";
import { BudgetGuard } from "../proxy/budget";
import { MemoryEventSink } from "../proxy/events";
import { createMockModel, type MockModelOptions } from "../proxy/mock-model";
import { createModelProxy, type ProxyServer, serveProxy } from "../proxy/proxy";
import { SessionTokenRegistry } from "../proxy/tokens";
import { createWorktreeSandboxFactory } from "../sandbox/worktree";
import { createSupervisor } from "../supervisor/supervisor";
// Pure helpers — importable because broomva-child.ts guards `main()` behind `import.meta.main`.
import { promptFor, textOf } from "./broomva-child";

const BROOMVA_CHILD = join(import.meta.dir, "broomva-child.ts");
const FIXED_MS = 1_700_000_000_000;

const handles: IndexHandle[] = [];
const servers: ProxyServer[] = [];
const tmps: string[] = [];
afterEach(async () => {
  for (const s of servers.splice(0)) s.stop();
  for (const h of handles.splice(0)) h.client.close();
  for (const d of tmps.splice(0)) await rm(d, { recursive: true, force: true });
});

async function makeWorkspace(): Promise<string> {
  const dir = await realpath(await mkdtemp(join(tmpdir(), "maestro-child-")));
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

/** Spawn the REAL broomva-child (no --scenario) — the supervisor's env (BROOMVA_MODEL_PROXY/TOKEN/
 *  CONTRACT/RUN_DIR/SESSION) reaches it unchanged, and its stdout NDJSON tees into the index. `extra`
 *  merges test-only env (e.g. BROOMVA_CONTEXT_CEILING to force the restart path) — the supervisor's real
 *  allowlist passes that BROOMVA_* var in production; here the injected spawn adds it directly (2b-ii). */
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

async function seedNode(
  h: IndexHandle,
  id: string,
  opts: { kind?: string; budgetJson?: string } = {},
): Promise<void> {
  await h.db.insert(node).values({
    id,
    path: `work/${id}`,
    kind: (opts.kind ?? "task") as never,
    state: "triggered",
    gate: "human",
    budgetJson: opts.budgetJson ?? null,
    createdAt: FIXED_MS,
    updatedAt: FIXED_MS,
  });
}

async function harness(
  opts: {
    budgetJson?: string;
    mock?: MockModelOptions;
    proxyUrlOverride?: string;
    kind?: string;
    /** Extra env merged into the spawned child (test-only, e.g. BROOMVA_CONTEXT_CEILING). */
    childEnvExtra?: Record<string, string>;
    /** Runtime config for the supervisor — e.g. contextCeilingTokens (passed to the child via the
     *  spawn-contract ALLOWLIST, the production path, not a test-only env). */
    config?: RuntimeConfig;
  } = {},
) {
  const ws = await makeWorkspace();
  const h = await openMem();
  await seedNode(h, "n0", { budgetJson: opts.budgetJson, kind: opts.kind });
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
    // A dead override points the child at an unreachable proxy (the fetch-throw path); default = the
    // real served proxy.
    proxy: { url: opts.proxyUrlOverride ?? server.url },
    spawnChild: makeChildSpawn(opts.childEnvExtra),
    mintRunId: () => "r1",
    hostEnv: { PATH: process.env.PATH },
    config: opts.config,
  });
  return { h, sup, mock };
}

/** An Anthropic Messages response body carrying `text` — what the mock returns as the assistant reply. */
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

/** An Anthropic Messages response body requesting one tool call — the loop parses this, executes the
 *  tool, and sends a tool_result on the next turn. */
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

/** A response TRUNCATED at the token cap: text, NO tool_use, stop_reason "max_tokens" — the model was cut
 *  off mid-thought, so this is NOT a clean completion. */
function anthropicTruncated(text: string): unknown {
  return {
    id: "msg_trunc",
    type: "message",
    role: "assistant",
    content: [{ type: "text", text }],
    stop_reason: "max_tokens",
    usage: { input_tokens: 8, output_tokens: 8192 },
  };
}

async function eventsOf(h: IndexHandle, sessionId: string, type: string) {
  return h.db
    .select()
    .from(event)
    .where(and(eq(event.sessionId, sessionId), eq(event.type, type as never)));
}

describe("broomva-child slice 1 — one model turn through the proxy (zero tokens)", () => {
  test("dispatch → child asks the model once → agent.said carries the reply → session review", async () => {
    const REPLY = "first step: scaffold the runner package";
    const { h, sup, mock } = await harness({ mock: { script: [{ body: anthropicBody(REPLY) }] } });

    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    // Exit 0 → the supervisor reaps `review` (the human gate holds it; verifier is P3).
    expect(reaped.exitCode).toBe(0);
    expect(reaped.sessionStatus).toBe("review");
    expect(reaped.nodeState).toBe("review");

    // Exactly ONE model call reached the upstream, and it was the MOCK (not Anthropic) — zero tokens.
    expect(mock.calls).toHaveLength(1);

    // The child READ the contract and folded it into the OUTBOUND prompt (proves it did NOT ignore the
    // contract — a regression returning a constant prompt would fail here). The proxy forwarded the
    // child's request body verbatim; it references the seeded contract (kind "task", id "n0").
    const sentPrompt = JSON.stringify(mock.calls[0]?.payload ?? {});
    expect(sentPrompt).toContain("task");
    expect(sentPrompt).toContain("n0");

    // The child READ the response and teed the model's ACTUAL text as agent.said (not a fabricated line).
    const said = await eventsOf(h, "r1", EVENT_TYPES.AGENT_SAID);
    expect(said).toHaveLength(1);
    expect(JSON.parse(said[0]?.payload ?? "{}")).toEqual({ text: REPLY });

    // Bookends carry the right code + order, not just counts: run.exiting {code:0}, the child-declared
    // code matches the real exit (no mismatch), and the seq order is run.started < agent.said < exiting.
    const started = await eventsOf(h, "r1", EVENT_TYPES.RUN_STARTED);
    const exiting = await eventsOf(h, "r1", EVENT_TYPES.RUN_EXITING);
    expect(started).toHaveLength(1);
    expect(exiting).toHaveLength(1);
    expect(JSON.parse(exiting[0]?.payload ?? "{}")).toEqual({ code: 0 });
    expect(reaped.mismatch).toBe(false);
    expect((started[0]?.seq ?? -1) < (said[0]?.seq ?? -1)).toBe(true);
    expect((said[0]?.seq ?? -1) < (exiting[0]?.seq ?? -1)).toBe(true);
  });

  test("proxy 402 (budget refused in-path) → child halts budget → exit 10 → session blocked", async () => {
    // A tiny per_run cap below ANY single-call reservation ceiling (the proxy reserves
    // estimateCallCeilingUsd(model, max_tokens:1024) BEFORE forwarding) → the FIRST call refuses 402,
    // never forwarding. (The refusal is the pre-forward RESERVATION, not the mock's metered cost.)
    const { h, sup, mock } = await harness({
      budgetJson: JSON.stringify({ per_run_usd: 0.000001 }),
    });

    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.exitCode).toBe(10);
    expect(reaped.sessionStatus).toBe("blocked");
    // The refused call NEVER forwarded to the upstream (the guard 402'd before forward) — zero tokens.
    expect(mock.calls).toHaveLength(0);
    // The child emitted the budget halt, not an agent.said (it never got a model reply).
    expect(await eventsOf(h, "r1", EVENT_TYPES.BUDGET_EXHAUSTED)).toHaveLength(1);
    expect(await eventsOf(h, "r1", EVENT_TYPES.AGENT_SAID)).toHaveLength(0);
  });

  test("non-2xx (502 upstream) → child exits 1 with a run.exiting receipt → crash-contained blocked", async () => {
    // The mock returns 502; the proxy forwards it (mock.calls===1) → the child can't proceed → exit 1
    // WITH a run.exiting {code:1} receipt (not a bare crash) → supervisor crash-routes blocked + run.failed.
    const { h, sup, mock } = await harness({ mock: { script: [{ status: 502 }] } });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.exitCode).toBe(1);
    expect(reaped.crash).toBe(true);
    expect(reaped.sessionStatus).toBe("blocked");
    expect(mock.calls).toHaveLength(1);
    expect(await eventsOf(h, "r1", EVENT_TYPES.RUN_FAILED)).toHaveLength(1); // supervisor crash event
    const exiting = await eventsOf(h, "r1", EVENT_TYPES.RUN_EXITING); // the child's own receipt
    expect(exiting).toHaveLength(1);
    expect(JSON.parse(exiting[0]?.payload ?? "{}").code).toBe(1);
    expect(await eventsOf(h, "r1", EVENT_TYPES.AGENT_SAID)).toHaveLength(0);
  });

  test("proxy unreachable (fetch throws) → child exits 1 with a receipt, not a bare crash", async () => {
    // Point the child at a dead proxy so fetch rejects (ECONNREFUSED). The guard must emit a run.exiting
    // {code:1, reason: model unreachable} receipt (HARNESS §6) before exiting — never a receiptless crash.
    const { h, sup, mock } = await harness({ proxyUrlOverride: "http://127.0.0.1:1" });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.exitCode).toBe(1);
    expect(reaped.sessionStatus).toBe("blocked");
    expect(mock.calls).toHaveLength(0); // never reached the served proxy/mock
    const exiting = await eventsOf(h, "r1", EVENT_TYPES.RUN_EXITING);
    expect(exiting).toHaveLength(1); // the receipt landed despite the transport crash
    const payload = JSON.parse(exiting[0]?.payload ?? "{}");
    expect(payload.code).toBe(1);
    expect(String(payload.reason)).toContain("unreachable");
    expect(await eventsOf(h, "r1", EVENT_TYPES.AGENT_SAID)).toHaveLength(0);
  });

  test("a non-object (string) 200 body → no text, no tool_use → clean exit 0 → review", async () => {
    // textOf/parseToolUses must NOT throw on a non-object body (their no-throw contract). A string body →
    // text "" (no agent.said — the loop only speaks when there IS text) AND no tool_use → a valid-but-
    // empty turn completes the loop (exit 0 / review), not a receiptless crash on `body.content`.
    const { h, sup, mock } = await harness({
      mock: { script: [{ body: "unexpected string body" }] },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.exitCode).toBe(0);
    expect(reaped.sessionStatus).toBe("review");
    expect(mock.calls).toHaveLength(1);
    // An empty turn says nothing — no agent.said noise — but still exits cleanly with a run.exiting {0}.
    expect(await eventsOf(h, "r1", EVENT_TYPES.AGENT_SAID)).toHaveLength(0);
    const exiting = await eventsOf(h, "r1", EVENT_TYPES.RUN_EXITING);
    expect(exiting).toHaveLength(1);
    expect(JSON.parse(exiting[0]?.payload ?? "{}")).toEqual({ code: 0 });
  });

  test("the outbound prompt reflects the contract — a different kind yields a different prompt", async () => {
    // Discriminates "child read the contract" from "child sends a constant prompt": a `project` contract
    // must flow into the forwarded prompt, NOT the default "task"/bare wording.
    const { sup, mock } = await harness({
      kind: "project",
      mock: { script: [{ body: anthropicBody("ok") }] },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    await out.reaped;
    expect(mock.calls).toHaveLength(1);
    const sent = JSON.stringify(mock.calls[0]?.payload ?? {});
    expect(sent).toContain("project");
    expect(sent).not.toContain("Work on this task");
  });
});

describe("broomva-child slice 2b-i — the F3 beat loop + tool execution (zero tokens)", () => {
  test("tool_use turn → executes the tool in the worktree → tees tool.call/tool.result → next text turn → exit 0", async () => {
    // Beat 1: the model asks for a shell tool that WRITES a file; the child executes it in the run
    // worktree and sends the result back. Beat 2: the model replies with text (no tool) → clean exit 0.
    const { h, sup, mock } = await harness({
      mock: {
        script: [
          { body: anthropicToolUse("tu1", "shell", { command: "echo hi > out.txt" }) },
          { body: anthropicBody("done") },
        ],
      },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.exitCode).toBe(0);
    expect(reaped.sessionStatus).toBe("review");
    expect(mock.calls).toHaveLength(2); // two turns reached the mock (tool turn + finishing text turn)

    // The tool was announced then executed successfully (tool.call → tool.result {ok:true}).
    const calls = await eventsOf(h, "r1", EVENT_TYPES.TOOL_CALL);
    expect(calls).toHaveLength(1);
    expect(JSON.parse(calls[0]?.payload ?? "{}")).toMatchObject({ tool: "shell" });
    const toolResults = await eventsOf(h, "r1", EVENT_TYPES.TOOL_RESULT);
    expect(toolResults).toHaveLength(1);
    const tr = JSON.parse(toolResults[0]?.payload ?? "{}");
    expect(tr).toMatchObject({ tool: "shell", ok: true });
    expect(tr.summary).toContain("out.txt"); // the EXACT command ran (summary carries it)

    // The beat's effect was REAL: the run.beat diffstat is not "(no change)" — the content-sensitive
    // signal saw the worktree move, proving executeTool ran the write IN cwd (not a no-op).
    const beats = await eventsOf(h, "r1", EVENT_TYPES.RUN_BEAT);
    expect(beats).toHaveLength(1);
    expect(JSON.parse(beats[0]?.payload ?? "{}").diffstat).not.toBe("(no change)");

    // The finishing turn's text landed as agent.said and the run completed.
    const said = await eventsOf(h, "r1", EVENT_TYPES.AGENT_SAID);
    expect(said).toHaveLength(1);
    expect(JSON.parse(said[0]?.payload ?? "{}")).toEqual({ text: "done" });
  });

  test("no-progress → beat 1 changes the tree, then 3 no-op beats → engine halts no_progress → blocked", async () => {
    // Beat 1 WRITES a file (a real change); beats 2-4 run a no-op tool (`echo hi` → stdout only, the
    // worktree is UNCHANGED from beat 1). The per-beat signature is `porcelain === previous ? "" : cur`,
    // so beats 2-4 are empty (the cumulative diff didn't move) → 3 consecutive empty → the BRO-1795 engine
    // halts no_progress. This exercises the cumulative-but-no-new-change path: a regression using the raw
    // porcelain (not the delta-vs-previous) would see a constant non-empty diff and never halt.
    const { h, sup, mock } = await harness({
      mock: {
        script: [{ body: anthropicToolUse("tu1", "shell", { command: "echo hi > a.txt" }) }],
        fallback: { body: anthropicToolUse("tuN", "shell", { command: "echo hi" }) },
      },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.exitCode).toBe(10);
    expect(reaped.reason).toBe("no_progress");
    expect(reaped.sessionStatus).toBe("blocked");
    expect(reaped.crash).toBe(false);
    // Exactly 4 beats: beat 1 moved the tree, beats 2-4 (DEFAULT_NO_PROGRESS_N) added nothing → halt. A
    // raw-porcelain regression (no delta-vs-previous) never halts → runs to the iteration cap (30 calls).
    expect(mock.calls).toHaveLength(4);
    const exiting = await eventsOf(h, "r1", EVENT_TYPES.RUN_EXITING);
    expect(exiting).toHaveLength(1);
    expect(JSON.parse(exiting[0]?.payload ?? "{}")).toMatchObject({
      code: 10,
      reason: "no_progress",
    });
    // The halt is the ENGINE's, not the proxy's — no budget.exhausted on this path.
    expect(await eventsOf(h, "r1", EVENT_TYPES.BUDGET_EXHAUSTED)).toHaveLength(0);
    // It DID execute a tool each beat (4 tool.calls) — the no-progress is about the EFFECT, not inaction.
    expect(await eventsOf(h, "r1", EVENT_TYPES.TOOL_CALL)).toHaveLength(4);
  });

  test("iterative CONTENT edits to the same file are progress (content-sensitive), not a false no_progress", async () => {
    // The common refine pattern: the model edits ONE file's content across many beats. `git status
    // --porcelain` (file STATUS) reads these as no-change → a FALSE no_progress halt; the CONTENT
    // signature sees each edit, so the loop keeps making progress and completes. MUTATION-PROOF: revert
    // the signal to porcelain and this halts no_progress at beat 4 (exit 10) instead of completing.
    const { h, sup, mock } = await harness({
      mock: {
        script: [
          { body: anthropicToolUse("t1", "edit", { path: "a.txt", content: "v1" }) },
          { body: anthropicToolUse("t2", "edit", { path: "a.txt", content: "v2" }) },
          { body: anthropicToolUse("t3", "edit", { path: "a.txt", content: "v3" }) },
          { body: anthropicToolUse("t4", "edit", { path: "a.txt", content: "v4" }) },
          { body: anthropicBody("done") },
        ],
      },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.exitCode).toBe(0); // completed — NOT a no_progress halt
    expect(reaped.sessionStatus).toBe("review");
    expect(mock.calls).toHaveLength(5); // 4 content edits + the finishing text (no early halt)
    const exiting = await eventsOf(h, "r1", EVENT_TYPES.RUN_EXITING);
    expect(exiting).toHaveLength(1);
    expect(JSON.parse(exiting[0]?.payload ?? "{}")).toEqual({ code: 0 });
  });

  test("context ceiling → child checkpoints + requests a fresh-context restart → supervisor respawns", async () => {
    // A tiny ceiling (via the injected env — 2b-ii wires it through the supervisor's allowlist) forces the
    // restart branch on beat 1 (the accumulated conversation exceeds 50 tokens after one tool turn). The
    // child writes progress.md + emits run.restart_requested + exits 10 fresh_context; the supervisor
    // respawns, and the script has exhausted to a text fallback so attempt 2 completes (exit 0 → review).
    // (The respawn RESUMING from progress.md is slice 2b-ii; here we prove the restart DECISION + respawn.)
    // A known run dir so we can read the checkpoint the child writes (the supervisor would set this via
    // its allowlist — 2b-ii).
    const runDir = await mkdtemp(join(tmpdir(), "maestro-run-"));
    tmps.push(runDir);
    const { h, sup } = await harness({
      childEnvExtra: { BROOMVA_CONTEXT_CEILING: "50", BROOMVA_RUN_DIR: runDir },
      mock: {
        script: [{ body: anthropicToolUse("tu1", "shell", { command: "echo hi > grew.txt" }) }],
        fallback: { body: anthropicBody("done") },
      },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.respawns).toBe(1); // exactly one fresh-context respawn fired
    expect(reaped.exitCode).toBe(0); // attempt 2 (text fallback) completed
    expect(reaped.sessionStatus).toBe("review");
    expect(reaped.crash).toBe(false);
    // attempt 1 asked for the restart → prepareRestart wrote progress.md THEN emitted this.
    expect(await eventsOf(h, "r1", EVENT_TYPES.RUN_RESTART_REQUESTED)).toHaveLength(1);
    // The checkpoint is REAL and lossless: progress.md carries the run state a 2b-ii respawn resumes from.
    const progress = await Bun.file(join(runDir, "progress.md")).text();
    expect(progress).toContain("context ceiling");
    expect(progress).toContain("r1"); // the session id
  });

  test("a respawned child RESUMES from progress.md — folds the checkpoint into the prompt (ceiling via config allowlist)", async () => {
    // The full lossless-restart cycle through the PRODUCTION path: the ceiling comes from the runtime
    // config → the supervisor's spawn-contract allowlist → BROOMVA_CONTEXT_CEILING (NOT a test-only env).
    // Attempt 1 hits the ceiling → checkpoints → the supervisor respawns → attempt 2 READS progress.md and
    // folds its state into the opening prompt (resume, not restart-from-scratch). A known run dir lets the
    // checkpoint round-trip; the script exhausts to a text fallback so attempt 2 completes.
    const runDir = await mkdtemp(join(tmpdir(), "maestro-resume-"));
    tmps.push(runDir);
    const { sup, mock } = await harness({
      config: { ...loadConfig({}), contextCeilingTokens: 50 }, // ceiling via the config→allowlist path
      childEnvExtra: { BROOMVA_RUN_DIR: runDir }, // known run dir so the checkpoint round-trips
      mock: {
        script: [{ body: anthropicToolUse("tu1", "shell", { command: "echo hi > grew.txt" }) }],
        fallback: { body: anthropicBody("done") },
      },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.respawns).toBe(1);
    expect(reaped.exitCode).toBe(0);
    // Attempt 2's FIRST request (mock.calls[1]) folded the checkpoint into the prompt → the child READ
    // progress.md and RESUMED (a from-scratch restart would omit it). A regression that skips readProgress
    // leaves the resume text absent → this fails.
    const resumeCall = mock.calls[1];
    if (!resumeCall) throw new Error("no respawn request");
    const prompt = JSON.stringify(resumeCall.payload ?? {});
    expect(prompt).toContain("RESUMING");
    expect(prompt).toContain("context ceiling"); // the checkpoint's state-of-the-world, folded in
    expect(prompt).toContain("from beat 1"); // the checkpoint's iteration NUMBER round-tripped, not just prose
  });

  test("iteration_cap accumulates ACROSS respawns — the run's cap spans attempts, not processes", async () => {
    // max_iterations=2 + a tiny ceiling + a tool that makes progress every beat: attempt 1 beat 1 (iter 1)
    // hits the ceiling → restart (iter 1 < cap); the respawn RESUMES seeding iterations=1, so attempt 2
    // beat 2 is iter 2 >= cap → iteration_cap halt. If iterations did NOT accumulate, attempt 2 would
    // restart from iter 1 again → an endless respawn chain (respawns ≫ 1), never the cap. So respawns===1
    // + reason iteration_cap mutation-proves the accumulation.
    const { sup } = await harness({
      budgetJson: JSON.stringify({ max_iterations: 2 }),
      config: { ...loadConfig({}), contextCeilingTokens: 50 },
      mock: { fallback: { body: anthropicToolUse("acc", "shell", { command: "echo x > f.txt" }) } },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.respawns).toBe(1); // exactly one restart, then the cap halted attempt 2 (accumulated)
    expect(reaped.exitCode).toBe(10);
    expect(reaped.reason).toBe("iteration_cap");
  });

  test("a malformed / stale progress.md is ignored — the child starts fresh, does not crash or resume", async () => {
    // A garbage checkpoint (no machine block) → parseProgress → null → readProgress → null → no resume.
    const runDir = await mkdtemp(join(tmpdir(), "maestro-bad-"));
    tmps.push(runDir);
    await writeFile(join(runDir, "progress.md"), "just some notes, no machine block\n");
    const { sup, mock } = await harness({
      childEnvExtra: { BROOMVA_RUN_DIR: runDir },
      mock: { script: [{ body: anthropicBody("first step") }] },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.exitCode).toBe(0); // fresh completion, no crash
    // The prompt did NOT fold in a resume (the malformed checkpoint was treated as absent).
    const prompt = JSON.stringify(mock.calls[0]?.payload ?? {});
    expect(prompt).not.toContain("RESUMING");
  });

  test("a checkpoint-write failure on restart still lands a receipt (exit 1), not a receiptless crash", async () => {
    // Point the run dir at an unwritable path (`/dev/null/nope` → mkdir ENOTDIR) so prepareRestart's
    // writeProgress throws. The GUARDED restart branch must emit run.exiting{code:1} rather than crash.
    const { h, sup } = await harness({
      childEnvExtra: { BROOMVA_CONTEXT_CEILING: "50", BROOMVA_RUN_DIR: "/dev/null/nope" },
      mock: {
        script: [{ body: anthropicToolUse("tu1", "shell", { command: "echo hi > grew.txt" }) }],
        fallback: { body: anthropicBody("done") },
      },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.exitCode).toBe(1); // the checkpoint write failed → exit 1, NOT a receiptless crash
    const exiting = await eventsOf(h, "r1", EVENT_TYPES.RUN_EXITING);
    expect(exiting).toHaveLength(1);
    expect(String(JSON.parse(exiting[0]?.payload ?? "{}").reason)).toContain("checkpoint failed");
  });

  test("a turn with 2 tool_use blocks → BOTH execute → 2 tool.call + 2 tool.result → one combined reply", async () => {
    // The Anthropic contract: a turn can request multiple tools; every tool_use MUST get a tool_result in
    // ONE following user turn. The fan-out loop executes both and returns both results before the next turn.
    const { h, sup, mock } = await harness({
      mock: {
        script: [
          {
            body: {
              content: [
                {
                  type: "tool_use",
                  id: "tu1",
                  name: "shell",
                  input: { command: "echo a > a.txt" },
                },
                {
                  type: "tool_use",
                  id: "tu2",
                  name: "shell",
                  input: { command: "echo b > b.txt" },
                },
              ],
              stop_reason: "tool_use",
            },
          },
          { body: anthropicBody("both done") },
        ],
      },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.exitCode).toBe(0);
    expect(mock.calls).toHaveLength(2); // beat 1 (two tools) + beat 2 (finishing text)
    // Both tools ran, in order, each with its own tool.call + tool.result.
    expect(await eventsOf(h, "r1", EVENT_TYPES.TOOL_CALL)).toHaveLength(2);
    const results = await eventsOf(h, "r1", EVENT_TYPES.TOOL_RESULT);
    expect(results).toHaveLength(2);
    expect(results.every((r) => JSON.parse(r.payload ?? "{}").ok === true)).toBe(true);
    // The FORWARDED beat-2 request must carry a tool_result for BOTH tool_use ids (the named regression:
    // a dropped/mis-keyed result passes the mock but a real Anthropic endpoint 400s on the unpaired turn).
    const call2 = mock.calls[1];
    if (!call2) throw new Error("no second request");
    const msgs = (call2.payload as { messages?: Array<{ role: string; content: unknown }> })
      .messages;
    const toolResultTurn = [...(msgs ?? [])]
      .reverse()
      .find(
        (m) =>
          m.role === "user" &&
          Array.isArray(m.content) &&
          (m.content as Array<{ type?: string }>).some((b) => b.type === "tool_result"),
      );
    const ids = ((toolResultTurn?.content as Array<{ type: string; tool_use_id?: string }>) ?? [])
      .filter((b) => b.type === "tool_result")
      .map((b) => b.tool_use_id);
    expect(ids.sort()).toEqual(["tu1", "tu2"]); // BOTH paired, keyed by their tool_use ids
  });

  test("a tool that FAILS (ok:false) → is_error propagates, worstError feeds the engine, loop continues", async () => {
    // A failing shell command → tool.result {ok:false}; the child sends the error result back (it does NOT
    // wedge) and the model finishes on the next turn → clean exit 0. Proves the failure-path wiring.
    const { h, sup, mock } = await harness({
      mock: {
        script: [
          { body: anthropicToolUse("tu1", "shell", { command: "exit 7" }) },
          { body: anthropicBody("recovered") },
        ],
      },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.exitCode).toBe(0); // the loop CONTINUED past the failing tool and completed
    expect(mock.calls).toHaveLength(2);
    const results = await eventsOf(h, "r1", EVENT_TYPES.TOOL_RESULT);
    expect(results).toHaveLength(1);
    expect(JSON.parse(results[0]?.payload ?? "{}")).toMatchObject({ tool: "shell", ok: false });
    // The FORWARDED tool_result the model actually sees carries is_error:true + the error content — not
    // just the teed event (a regression mapping the failure to is_error:false would mislead the model).
    const call2 = mock.calls[1];
    if (!call2) throw new Error("no second request");
    const msgs = (call2.payload as { messages?: Array<{ role: string; content: unknown }> })
      .messages;
    const tr = [...(msgs ?? [])]
      .flatMap((m) =>
        Array.isArray(m.content) ? (m.content as Array<Record<string, unknown>>) : [],
      )
      .find((b) => b.type === "tool_result" && b.tool_use_id === "tu1");
    expect(tr?.is_error).toBe(true);
    expect(String(tr?.content)).toContain("exit 7");
  });

  test("a max_tokens-TRUNCATED tool-less turn is NOT a completion — the loop continues, not exit 0", async () => {
    // The model narrates past the token cap before it can act → content=[text], stop_reason "max_tokens",
    // no tool_use. Exiting 0 here would certify a cut-off run as done. The loop must read stop_reason and
    // CONTINUE (nudge). MUTATION-PROOF: ignore stop_reason (exit 0 on any tool-less turn) → mock.calls=1.
    const { h, sup, mock } = await harness({
      mock: {
        script: [
          { body: anthropicTruncated("here is my long plan…") },
          { body: anthropicBody("done") },
        ],
      },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.exitCode).toBe(0); // completed on beat 2 (the end_turn reply), NOT beat 1
    expect(mock.calls).toHaveLength(2); // it did NOT stop at the truncated beat 1
    // the truncated narration was still surfaced; the run completed only after a real end_turn.
    const said = await eventsOf(h, "r1", EVENT_TYPES.AGENT_SAID);
    expect(said.some((r) => (JSON.parse(r.payload ?? "{}").text ?? "").includes("long plan"))).toBe(
      true,
    );
  });

  test("a beat that COMMITS its work counts as progress (diff vs run base), not a false no_progress", async () => {
    // The model commits each beat (the branch is the receipt). Diffing vs HEAD would empty after a commit
    // → false no_progress; diffing vs the run BASE keeps committed work visible. 4 committing beats make
    // progress and the run completes. MUTATION-PROOF: diff vs HEAD → halts no_progress at beat ~3-4.
    const commit = (n: number) =>
      anthropicToolUse(`c${n}`, "shell", {
        command: `echo v${n} > f${n}.txt && git add -A && git commit -qm w${n}`,
      });
    const { sup, mock } = await harness({
      mock: {
        script: [
          { body: commit(1) },
          { body: commit(2) },
          { body: commit(3) },
          { body: commit(4) },
        ],
        fallback: { body: anthropicBody("done") },
      },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.exitCode).toBe(0); // completed — committing is progress, no false no_progress halt
    expect(reaped.sessionStatus).toBe("review");
    expect(mock.calls).toHaveLength(5); // 4 committing beats + the finishing text
  });

  test("the SECOND request pairs the assistant tool_use with a matching tool_result (Anthropic contract)", async () => {
    // A dropped / wrong-id tool_result passes the mock but a real Anthropic endpoint rejects it. Inspect
    // the forwarded messages of beat 2: [user prompt, assistant tool_use, user tool_result], and the
    // tool_result's tool_use_id must equal the assistant tool_use's id.
    const { sup, mock } = await harness({
      mock: {
        script: [
          { body: anthropicToolUse("tu-abc", "shell", { command: "echo hi" }) },
          { body: anthropicBody("done") },
        ],
      },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    await out.reaped;

    expect(mock.calls).toHaveLength(2);
    const call2 = mock.calls[1];
    if (!call2) throw new Error("no second request");
    const msgs = (call2.payload as { messages?: Array<{ role: string; content: unknown }> })
      .messages;
    if (!msgs) throw new Error("no messages in the second request");
    // The assistant turn carries the tool_use; the following user turn carries the matching tool_result.
    const assistant = msgs.find((m) => m.role === "assistant");
    const toolUse = (assistant?.content as Array<{ type: string; id?: string }>)?.find(
      (b) => b.type === "tool_use",
    );
    expect(toolUse?.id).toBe("tu-abc");
    const lastUser = [...msgs].reverse().find((m) => m.role === "user");
    const toolResult = (lastUser?.content as Array<{ type: string; tool_use_id?: string }>)?.find(
      (b) => b.type === "tool_result",
    );
    expect(toolResult?.tool_use_id).toBe("tu-abc"); // paired by id — the loop did not drop/mis-key it
  });

  test("iteration cap → the engine halts after budget.max_iterations beats → blocked, receipt code 10", async () => {
    // The contract caps iterations at 3; each beat makes progress (distinct file) so no_progress never
    // fires — the cap is what stops it. Proves the iteration_cap → exit-10 receipt path.
    const { h, sup, mock } = await harness({
      budgetJson: JSON.stringify({ max_iterations: 3 }),
      mock: {
        script: [
          { body: anthropicToolUse("i1", "shell", { command: "echo a > a.txt" }) },
          { body: anthropicToolUse("i2", "shell", { command: "echo b > b.txt" }) },
          { body: anthropicToolUse("i3", "shell", { command: "echo c > c.txt" }) },
        ],
      },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.exitCode).toBe(10);
    expect(reaped.reason).toBe("iteration_cap");
    expect(reaped.sessionStatus).toBe("blocked");
    expect(mock.calls).toHaveLength(3);
    const exiting = await eventsOf(h, "r1", EVENT_TYPES.RUN_EXITING);
    expect(JSON.parse(exiting[0]?.payload ?? "{}")).toMatchObject({
      code: 10,
      reason: "iteration_cap",
    });
  });

  test("no-progress via IDENTICAL errors: a tool failing the same way each beat halts (stalledOnErrors)", async () => {
    // Each beat appends to a log (so the DIFF grows — no stalledOnDiffs) but exits non-zero with the SAME
    // command → the same terminal-error signature → the engine's stalledOnErrors ("agreeing with itself")
    // halts no_progress. Isolates the error-based trigger from the diff-based one.
    const { h, sup, mock } = await harness({
      mock: {
        fallback: {
          body: anthropicToolUse("e", "shell", { command: "echo x >> log.txt && exit 1" }),
        },
      },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.exitCode).toBe(10);
    expect(reaped.reason).toBe("no_progress");
    expect(mock.calls).toHaveLength(3); // DEFAULT_NO_PROGRESS_N identical errors → halt
    const results = await eventsOf(h, "r1", EVENT_TYPES.TOOL_RESULT);
    expect(results.every((r) => JSON.parse(r.payload ?? "{}").ok === false)).toBe(true);
  });

  test("REPEATED truncation is bounded — a model that keeps hitting max_tokens without acting halts", async () => {
    // The nudge-and-continue on max_tokens must not loop forever: each truncated beat runs no tool → the
    // worktree is unchanged → empty beat effect → after DEFAULT_NO_PROGRESS_N empty beats, no_progress halts.
    const { sup, mock } = await harness({
      mock: { fallback: { body: anthropicTruncated("still thinking…") } },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.exitCode).toBe(10);
    expect(reaped.reason).toBe("no_progress"); // bounded, not an infinite nudge loop
    expect(mock.calls).toHaveLength(3);
  });

  test("a turn carrying BOTH text and a tool_use → the text is said AND the tool runs", async () => {
    // The common real-model shape: the assistant narrates ("let me check …") AND calls a tool in one turn.
    // Both must surface — agent.said for the text, tool.call/tool.result for the tool.
    const { h, sup } = await harness({
      mock: {
        script: [
          {
            body: {
              content: [
                { type: "text", text: "let me check the tree" },
                {
                  type: "tool_use",
                  id: "tu1",
                  name: "shell",
                  input: { command: "echo x > x.txt" },
                },
              ],
              stop_reason: "tool_use",
            },
          },
          { body: anthropicBody("done") },
        ],
      },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.exitCode).toBe(0);
    // The combined turn's text landed (agent.said "let me check…") AND the tool ran (tool.call).
    const said = await eventsOf(h, "r1", EVENT_TYPES.AGENT_SAID);
    expect(
      said.some((r) => (JSON.parse(r.payload ?? "{}").text ?? "").includes("let me check")),
    ).toBe(true);
    expect(await eventsOf(h, "r1", EVENT_TYPES.TOOL_CALL)).toHaveLength(1);
  });
});

describe("broomva-child pure helpers", () => {
  test("textOf joins content[] text blocks and guards null/non-object/non-text without throwing", () => {
    expect(
      textOf({
        content: [
          { type: "text", text: "a" },
          { type: "text", text: "b" },
        ],
      }),
    ).toBe("ab");
    expect(
      textOf({
        content: [
          { type: "text", text: "x" },
          { type: "tool_use", id: "t" },
        ],
      }),
    ).toBe("x");
    // Load-bearing: `typeof null === "object"` in JS, so the explicit `=== null` guard is what stops
    // `null.content` from throwing — a body the mock can't produce, so this direct unit test is its cover.
    expect(textOf(null)).toBe("");
    expect(textOf("a string body")).toBe("");
    expect(textOf(42)).toBe("");
    expect(textOf({ content: "not an array" })).toBe("");
    expect(textOf({})).toBe("");
  });

  test("promptFor folds the contract kind + id; a null contract degrades to a bare prompt", () => {
    const p = promptFor(
      {
        id: "n7",
        kind: "project",
        state: "triggered",
        gate: "human",
        created: "2026-01-01",
        updated: "2026-01-01",
      },
      "sess-1",
    );
    expect(p).toContain("project");
    expect(p).toContain("n7");
    expect(promptFor(null, "sess-1")).toContain("sess-1");
  });
});
