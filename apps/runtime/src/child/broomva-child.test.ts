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
 *  CONTRACT/RUN_DIR/SESSION) reaches it unchanged, and its stdout NDJSON tees into the index. */
const realChildSpawn = (args: {
  argv: string[];
  env: Record<string, string>;
  cwd: string;
}): ChildStdioPort => {
  const proc = Bun.spawn(["bun", BROOMVA_CHILD, ...args.argv], {
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

async function openMem(): Promise<IndexHandle> {
  const h = await openIndex(":memory:");
  handles.push(h);
  return h;
}

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

async function harness(
  opts: { budgetJson?: string; mock?: MockModelOptions; proxyUrlOverride?: string } = {},
) {
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
    // A dead override points the child at an unreachable proxy (the fetch-throw path); default = the
    // real served proxy.
    proxy: { url: opts.proxyUrlOverride ?? server.url },
    spawnChild: realChildSpawn,
    mintRunId: () => "r1",
    hostEnv: { PATH: process.env.PATH },
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

  test("a non-object (string) 200 body → empty agent.said, no crash → session review", async () => {
    // textOf must NOT throw on a non-object body (its no-throw contract). A string body → text "" → a
    // valid-but-empty turn (exit 0 / review), not a receiptless crash on `body.content`.
    const { h, sup, mock } = await harness({
      mock: { script: [{ body: "unexpected string body" }] },
    });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const reaped = await out.reaped;

    expect(reaped.exitCode).toBe(0);
    expect(reaped.sessionStatus).toBe("review");
    expect(mock.calls).toHaveLength(1);
    const said = await eventsOf(h, "r1", EVENT_TYPES.AGENT_SAID);
    expect(said).toHaveLength(1);
    expect(JSON.parse(said[0]?.payload ?? "{}")).toEqual({ text: "" });
  });
});
