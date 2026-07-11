/// <reference types="bun" />
// dispatch.test.ts (BRO-1822 slice 1) — the F2/F3 dispatch loop MOUNTED into the runtime, end to end, with
// ZERO tokens / no key. `mountDispatch` builds the REAL served model proxy + REAL supervisor; a dispatch
// spawns the REAL child (`bun run broomva-child.ts`, the `devSpawnChild` default) which dials the proxy →
// the scripted mock upstream. This is the P11 dogfood of the mount: real child process, real loopback HTTP,
// real worktree. Proves (1) a mounted dispatch runs a real child to completion with session events teed to
// the index, and (2) the F8 kill seam (the one `createApp` wires) kills a live child mid-run → canceled.
// Anti-vacuity [[self-hosting-vacuous-pass]]: assert the exact terminal + the child's teed events + that the
// MOCK (not Anthropic) served every call.

import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { EVENT_TYPES } from "@maestro/protocol";
import { and, eq } from "drizzle-orm";
import { loadConfig } from "./config";
import { type IndexHandle, openIndex } from "./db/client";
import { event, node } from "./db/schema";
import { type DispatchRuntime, mountDispatch } from "./dispatch";
import { git } from "./git/git";
import { createMockModel, type MockModelOptions } from "./proxy/mock-model";

const FIXED_MS = 1_700_000_000_000;

const handles: IndexHandle[] = [];
const mounts: DispatchRuntime[] = [];
const tmps: string[] = [];
afterEach(async () => {
  for (const m of mounts.splice(0)) m.shutdown();
  for (const h of handles.splice(0)) h.client.close();
  for (const d of tmps.splice(0)) await rm(d, { recursive: true, force: true });
});

/** A canonical (realpath'd) temp git workspace — the worktree factory branches off it (BRO-1746). */
async function makeWorkspace(): Promise<string> {
  const dir = await realpath(await mkdtemp(join(tmpdir(), "maestro-dispatch-")));
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

/** Seed a dispatchable node (resolved contract in the row, as the scanner leaves it). */
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

async function waitForEvent(
  h: IndexHandle,
  sessionId: string,
  type: string,
  timeoutMs = 5000,
): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if ((await eventsOf(h, sessionId, type)).length > 0) return true;
    await new Promise((r) => setTimeout(r, 25));
  }
  return false;
}

/** Poll a predicate until true or the deadline — for non-index signals (e.g. mock.calls). */
async function waitFor(pred: () => boolean, timeoutMs = 5000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pred()) return true;
    await new Promise((r) => setTimeout(r, 25));
  }
  return false;
}

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

/** Mount the dispatch loop over a fresh temp workspace + `:memory:` index with a scripted mock upstream.
 *  Uses the DEFAULT `devSpawnChild` — so the run spawns the REAL child via `bun run`. */
async function harness(opts: { mock?: MockModelOptions } = {}) {
  const ws = await makeWorkspace();
  const h = await openMem();
  await seedNode(h, "n0");
  const config = { ...loadConfig({}), workspace: ws, mockModel: true };
  const mock = createMockModel(opts.mock);
  const dispatch = mountDispatch({ db: h.db, config, upstream: mock, mintRunId: () => "r1" });
  mounts.push(dispatch);
  return { h, dispatch, mock };
}

describe("dispatch mount (BRO-1822 slice 1) — the loop assembled into the runtime, zero tokens", () => {
  test("a mounted dispatch runs a REAL child to completion → session events teed to the index", async () => {
    const { h, dispatch, mock } = await harness({
      mock: { fallback: { body: anthropicBody("done") } },
    });
    const out = await dispatch.supervisor.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    const res = await out.reaped;

    // A real child spawned (bun run), dialed the MOUNTED loopback proxy, which forwarded to the mock →
    // clean completion (exit 0 → review), not a crash.
    expect(res.exitCode).toBe(0);
    expect(res.sessionStatus).toBe("review");
    expect(res.crash).toBe(false);
    // Its teed events are durable in the index: run.started + the actual model text + run.exiting {0}.
    expect(await eventsOf(h, "r1", EVENT_TYPES.RUN_STARTED)).toHaveLength(1);
    const said = await eventsOf(h, "r1", EVENT_TYPES.AGENT_SAID);
    expect(said.some((r) => JSON.parse(r.payload ?? "{}").text === "done")).toBe(true);
    const exiting = await eventsOf(h, "r1", EVENT_TYPES.RUN_EXITING);
    expect(exiting).toHaveLength(1);
    expect(JSON.parse(exiting[0]?.payload ?? "{}")).toMatchObject({ code: 0 });
    // The MOCK served the call (not Anthropic) — the zero-token/no-key guarantee, structurally.
    expect(mock.calls.length).toBeGreaterThanOrEqual(1);
  });

  test("the F8 kill seam kills a live mounted child mid-run → canceled + run.killed", async () => {
    // Park the child mid-model-call so the F8 kill lands on a genuinely live child: the mock records the
    // forwarded request then hangs (delayMs). We hang it with a NEVER-RESOLVING promise, not a real timer,
    // so nothing out-lives the killed run (a pending setTimeout would keep Bun's event loop reffed after
    // the test). Killing mid-model-call (not mid-shell-tool) spawns NO `sh -c` grandchild, so the SIGKILL
    // leaves nothing orphaned — phase 1's SIGKILL does not reap a shell tool's grandchild (reaping the
    // whole process group is the phase-2 container jail's job, tracked in BRO-1860; out of scope here).
    const { h, dispatch, mock } = await harness({
      mock: {
        fallback: { body: anthropicBody("slow"), delayMs: 1 },
        sleep: () => new Promise<void>(() => {}),
      },
    });
    const out = await dispatch.supervisor.dispatch("n0");
    if (!out.dispatched) throw new Error("dispatch failed");
    // The child booted (run.started teed) and dialed the proxy — the mock records the forwarded request
    // before parking on delayMs, so mock.calls >= 1 proves the child is live + parked mid-call. Then kill
    // via the SAME seam createApp wires.
    expect(await waitForEvent(h, "r1", EVENT_TYPES.RUN_STARTED)).toBe(true);
    expect(await waitFor(() => mock.calls.length >= 1)).toBe(true);
    expect(dispatch.kill("r1")).toBe(true);
    const res = await out.reaped;

    // A human-killed run ends DISTINCTLY from a crash: canceled + run.killed (BRO-1801).
    expect(res.crash).toBe(false);
    expect(res.event).toBe("run.killed");
    expect(res.sessionStatus).toBe("canceled");
    expect(await eventsOf(h, "r1", EVENT_TYPES.RUN_KILLED)).toHaveLength(1);
  });
});
