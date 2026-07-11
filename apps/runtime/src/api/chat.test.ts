/// <reference types="bun" />
// chat.test.ts (BRO-1822 slice 2) — the F10 chat endpoint, END TO END through the REAL mounted dispatch
// (real supervisor + served loopback proxy + REAL spawned child + scripted mock upstream, ZERO tokens).
// The done.check scenarios (API §Chat / FLOWS F10): a mock-model chat ROUND-TRIP, idle-node chat
// AUTO-DISPATCHES, and a mid-chat child death surfaces an ERROR part.
//
// Anti-vacuity [[self-hosting-vacuous-pass]] + [[mock-fidelity-gap-false-green]]: the round-trip asserts
// the chat text actually REACHED the model call (the mock records the forwarded payload) — proving the
// full route HTTP → control.chat → child stdin → user-turn fold → proxy → mock, not just that a stream
// came back. The mid-death test kills a genuinely-live child parked mid-model-call (no shell grandchild
// orphaned) and asserts the distinct `error` chunk.

import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { EVENT_TYPES } from "@maestro/protocol";
import { and, eq } from "drizzle-orm";
import type { Hono } from "hono";
import { createApp } from "../app";
import { loadConfig } from "../config";
import { type IndexHandle, openIndex } from "../db/client";
import { event, node } from "../db/schema";
import { type DispatchRuntime, deriveDayTotalUsdFromIndex, mountDispatch } from "../dispatch";
import { git } from "../git/git";
import { createMockModel, type MockModelOptions } from "../proxy/mock-model";
import { extractLatestUserMessage } from "./chat";

const FIXED_MS = 1_700_000_000_000;

const handles: IndexHandle[] = [];
const mounts: DispatchRuntime[] = [];
const tmps: string[] = [];
afterEach(async () => {
  for (const m of mounts.splice(0)) m.shutdown();
  for (const h of handles.splice(0)) h.client.close();
  for (const d of tmps.splice(0)) await rm(d, { recursive: true, force: true });
});

async function makeWorkspace(): Promise<string> {
  const dir = await realpath(await mkdtemp(join(tmpdir(), "maestro-chat-")));
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

async function openMem(): Promise<IndexHandle> {
  const h = await openIndex(":memory:");
  handles.push(h);
  return h;
}

async function seedNode(h: IndexHandle, id: string): Promise<void> {
  await h.db.insert(node).values({
    id,
    path: `work/${id}`,
    kind: "task",
    state: "triggered",
    gate: "human",
    budgetJson: null,
    createdAt: FIXED_MS,
    updatedAt: FIXED_MS,
  });
}

async function eventsOf(h: IndexHandle, sessionId: string, type: string) {
  return h.db
    .select()
    .from(event)
    .where(and(eq(event.sessionId, sessionId), eq(event.type, type as never)));
}

async function waitForEvent(h: IndexHandle, sessionId: string, type: string, timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if ((await eventsOf(h, sessionId, type)).length > 0) return true;
    await new Promise((r) => setTimeout(r, 25));
  }
  return false;
}

async function waitFor(pred: () => boolean, timeoutMs = 5000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pred()) return true;
    await new Promise((r) => setTimeout(r, 25));
  }
  return false;
}

function anthropicText(text: string): unknown {
  return {
    id: "msg_text",
    type: "message",
    role: "assistant",
    content: [{ type: "text", text }],
    stop_reason: "end_turn",
    usage: { input_tokens: 8, output_tokens: 6 },
  };
}

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

/** Mount the dispatch loop + build the app with the F10 chat route wired to it (fast tail for tests). */
async function harness(opts: { mock?: MockModelOptions } = {}) {
  const ws = await makeWorkspace();
  const h = await openMem();
  await seedNode(h, "n0");
  const config = { ...loadConfig({}), workspace: ws, mockModel: true, streamPollMs: 10 };
  const mock = createMockModel(opts.mock);
  const dispatch = await mountDispatch({ db: h.db, config, upstream: mock, mintRunId: () => "r1" });
  mounts.push(dispatch);
  const app: Hono = createApp(
    config,
    FIXED_MS,
    h.db,
    undefined,
    (id) => dispatch.kill(id),
    () => dispatch,
  );
  return { h, dispatch, mock, app, ws };
}

/** POST a chat with the SDK-shaped body (`{ messages: [UIMessage] }`) carrying one user text part. */
async function chatRequest(app: Hono, id: string, text: string): Promise<Response> {
  return app.request(`/api/sessions/${id}/chat`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      id: "chat-1",
      messages: [{ id: "u1", role: "user", parts: [{ type: "text", text }] }],
    }),
  });
}

/** Read a UI Message Stream response to completion, parsing each `data:` SSE frame into a chunk object. */
async function collectChunks(res: Response): Promise<Array<Record<string, unknown>>> {
  const text = await res.text(); // resolves when the stream closes (execute() returns on a terminal event)
  const chunks: Array<Record<string, unknown>> = [];
  for (const line of text.split("\n")) {
    const t = line.trim();
    if (!t.startsWith("data:")) continue;
    const data = t.slice(5).trim();
    if (data === "" || data === "[DONE]") continue;
    chunks.push(JSON.parse(data));
  }
  return chunks;
}

const CHAT_TEXT = "PING_FROM_CHAT_TEST_7f3a";

describe("F10 chat endpoint (BRO-1822 slice 2) — UIMessage in, UI Message Stream out, routed to child stdin", () => {
  test("mock-model chat round-trip: the message routes into the child and the session streams back", async () => {
    // The child loops a few GROWING-diff tool beats (append to a file — so no_progress never halts and the
    // run stays alive) which gives the concurrent stdin reader time to deliver the chat; it is drained into
    // a beat's user turn (the mock then records CHAT_TEXT), and the fallback text completes the run →
    // run.finished → finish. A single-tool scenario races the reader (the chat can arrive after the child's
    // last beat drain), so the loop is what makes the routing assertion deterministic, not wall-clock luck.
    const append = (f: string) => ({
      body: anthropicToolUse("t", "shell", { command: `echo x >> ${f}` }),
    });
    const { app, mock } = await harness({
      mock: {
        script: [append("a.txt"), append("b.txt"), append("c.txt")],
        fallback: { body: anthropicText("ok") },
      },
    });

    const chunks = await collectChunks(await chatRequest(app, "n0", CHAT_TEXT));
    const types = chunks.map((c) => c.type);

    // A well-formed UI Message Stream: opens, carries the assistant's text, and finishes.
    expect(types).toContain("start");
    expect(types).toContain("finish");
    const textDeltas = chunks.filter((c) => c.type === "text-delta").map((c) => c.delta);
    expect(textDeltas.some((d) => d === "ok")).toBe(true);
    // ROUTING PROOF (anti-vacuity): the chat text actually reached a model call — HTTP → control.chat →
    // child stdin → user-turn fold → proxy → mock. A dropped route would leave CHAT_TEXT in no payload.
    expect(mock.calls.length).toBeGreaterThanOrEqual(2);
    expect(mock.calls.some((c) => JSON.stringify(c.payload).includes(CHAT_TEXT))).toBe(true);
  });

  test("idle-node chat auto-dispatches: chatting an idle node spawns a session (FLOWS F10.2)", async () => {
    const { h, app } = await harness({
      mock: { fallback: { body: anthropicText("done") } },
    });
    // No live run exists for n0 before the chat.
    expect((await eventsOf(h, "r1", EVENT_TYPES.RUN_STARTED)).length).toBe(0);

    const chunks = await collectChunks(await chatRequest(app, "n0", "hello"));

    // The endpoint dispatched a session for the idle node — a real child ran (run.started teed) and the
    // stream finished cleanly.
    expect((await eventsOf(h, "r1", EVENT_TYPES.RUN_STARTED)).length).toBe(1);
    expect(chunks.map((c) => c.type)).toContain("finish");
  });

  test("mid-chat child death surfaces an error part (child killed while streaming)", async () => {
    // Park the child mid-model-call (delayMs hung with a never-resolving promise — no shell grandchild to
    // orphan, no leaked timer, exactly the slice-1 kill pattern). Then kill it mid-stream.
    const { h, dispatch, app } = await harness({
      mock: {
        fallback: { body: anthropicText("never"), delayMs: 1 },
        sleep: () => new Promise<void>(() => {}),
      },
    });

    const res = await chatRequest(app, "n0", "start work");
    const chunksP = collectChunks(res); // read the stream concurrently while we kill the run
    // The child is live + parked mid-model-call (it dialed the proxy, which is hung).
    expect(await waitForEvent(h, "r1", EVENT_TYPES.RUN_STARTED)).toBe(true);
    expect(await waitFor(() => dispatch.supervisor.get("r1") !== null)).toBe(true);
    expect(dispatch.kill("r1")).toBe(true);

    const chunks = await chunksP;
    const types = chunks.map((c) => c.type);
    // The death is FIRST-CLASS: a stream-level error part, not a silent hang (contract §4 / §7).
    expect(types).toContain("error");
    const errored = chunks.find((c) => c.type === "error");
    expect(typeof errored?.errorText).toBe("string");
    // And the run ended DISTINCTLY as killed (canceled + run.killed), session state honest.
    expect((await eventsOf(h, "r1", EVENT_TYPES.RUN_KILLED)).length).toBe(1);
  });
});

describe("extractLatestUserMessage — the SDK body shapes (chat-transport §2/§9)", () => {
  test("pulls the newest user message from a { messages: [...] } body", () => {
    const m = extractLatestUserMessage({
      messages: [
        { id: "a", role: "user", parts: [{ type: "text", text: "first" }] },
        { id: "b", role: "assistant", parts: [{ type: "text", text: "reply" }] },
        { id: "c", role: "user", parts: [{ type: "text", text: "second" }] },
      ],
    });
    expect(m?.id).toBe("c");
    expect(m?.role).toBe("user");
  });

  test("accepts a single bare UIMessage body (API.md §Chat)", () => {
    const m = extractLatestUserMessage({
      id: "x",
      role: "user",
      parts: [{ type: "text", text: "hi" }],
    });
    expect(m?.id).toBe("x");
  });

  test("returns undefined when there is no user message or the body is malformed", () => {
    expect(
      extractLatestUserMessage({ messages: [{ id: "a", role: "assistant", parts: [] }] }),
    ).toBeUndefined();
    expect(extractLatestUserMessage({ messages: [] })).toBeUndefined();
    expect(extractLatestUserMessage(null)).toBeUndefined();
    expect(extractLatestUserMessage("nope")).toBeUndefined();
    expect(extractLatestUserMessage({ role: "user" })).toBeUndefined(); // no parts array
  });

  test("defaults a missing id (the child only reads parts)", () => {
    const m = extractLatestUserMessage({ messages: [{ role: "user", parts: [] }] });
    expect(m?.id).toBe("user");
  });
});

describe("deriveDayTotalUsdFromIndex — the per-day budget seed (BRO-1822 latent gap)", () => {
  const DAY_MS = 86_400_000;

  test("sums today's budget.metered spend and excludes prior days", async () => {
    const h = await openMem();
    const now = 5 * DAY_MS + 12 * 3_600_000; // midday of day 5
    const todayStart = 5 * DAY_MS;
    // Two metered events today (0.5 + 0.75) and one yesterday (9.0, must be excluded).
    const rows = [
      { session: "r1", usd: 0.5, ts: todayStart + 1000 },
      { session: "r2", usd: 0.75, ts: todayStart + 2000 },
      { session: "r0", usd: 9.0, ts: todayStart - 1000 },
    ];
    let seq = 1;
    for (const r of rows) {
      await h.db.insert(event).values({
        seq: seq++,
        sessionId: r.session,
        ts: r.ts,
        actor: "system",
        type: EVENT_TYPES.BUDGET_METERED,
        payload: JSON.stringify({ session: r.session, usd: r.usd, tokens: 10 }),
      });
    }
    expect(await deriveDayTotalUsdFromIndex(h.db, now)).toBeCloseTo(1.25, 6);
  });

  test("returns 0 when there are no metered events (a fresh cap, never blocking)", async () => {
    const h = await openMem();
    expect(await deriveDayTotalUsdFromIndex(h.db, 5 * DAY_MS)).toBe(0);
  });
});
