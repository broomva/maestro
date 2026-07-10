/// <reference types="bun" />
// supervisor.test.ts — BRO-1779 done.check `bun test apps/runtime --filter supervisor`.
//
// The exit-code matrix (HARNESS §4) drives a REAL git-worktree sandbox (no worktree mocks, P11) + a
// real `:memory:` index; only the CHILD PROCESS is a fixture — the injectable `spawnChild` seam returns
// either a fabricated `ChildStdioPort` (deterministic matrix) or a real `Bun.spawn` of a tiny fixture
// script (the dogfood + SIGKILL-containment tests that prove the real exited/stdout/kill wiring, per
// the BRO-1767 Bun-stream lesson). Anti-vacuity [[self-hosting-vacuous-pass]]: every case asserts the
// EXACT (session, node, event) triple, not just "something happened" — swap a mapping and a test fails.

import { afterEach, describe, expect, test } from "bun:test";
import { chmod, mkdtemp, realpath, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { EVENT_TYPES } from "@maestro/protocol";
import { and, eq } from "drizzle-orm";
import { createApp } from "../app";
import { loadConfig } from "../config";
import { type IndexHandle, openIndex } from "../db/client";
import { event, gate, lease, node, runBudget, session } from "../db/schema";
import { git } from "../git/git";
import type { ChildStdioPort } from "../harness/stdio";
import { SessionTokenRegistry } from "../proxy/tokens";
import { createWorktreeSandboxFactory } from "../sandbox/worktree";
import { createSupervisor, type SpawnChild, type SupervisorDeps } from "./supervisor";

// ── Fixtures + harness ───────────────────────────────────────────────────────

const FIXED_MS = 1_700_000_000_000;
const fixedNow = () => FIXED_MS;

const handles: IndexHandle[] = [];
const tmps: string[] = [];
afterEach(async () => {
  for (const h of handles.splice(0)) h.client.close();
  for (const d of tmps.splice(0)) await rm(d, { recursive: true, force: true });
});

/** A canonical (realpath'd) temp git repo — a real runtime workspace is canonical (the prunable-
 *  respawn class of bug only reproduces on one; macOS mkdtemp is a /var→/private/var symlink). */
async function makeWorkspace(): Promise<string> {
  const dir = await realpath(await mkdtemp(join(tmpdir(), "maestro-supervisor-")));
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

/** A ReadableStream of newline-delimited lines (encoded), closing after the last — a fake child stdout. */
function streamOf(lines: string[]): ReadableStream<Uint8Array> {
  const enc = new TextEncoder();
  return new ReadableStream<Uint8Array>({
    start(controller) {
      for (const l of lines) controller.enqueue(enc.encode(l));
      controller.close();
    },
  });
}

/** A fabricated `ChildStdioPort` — scripted stdout lines + a pinned exit code. `signals` records kills. */
function fakePort(script: {
  lines: string[];
  exitCode: number | Promise<number>;
  signals?: string[];
}): ChildStdioPort {
  return {
    stdout: streamOf(script.lines),
    stderr: streamOf([]),
    writeStdin: () => {},
    kill: (s) => {
      script.signals?.push(String(s));
    },
    exited: Promise.resolve(script.exitCode),
  };
}

/** One `run.exiting {code, reason}` NDJSON line (the child's terminal utterance). */
function runExiting(code: number, reason?: string): string {
  const payload = reason === undefined ? { code } : { code, reason };
  return `${JSON.stringify({ actor: "system", type: "run.exiting", payload })}\n`;
}

/** A spawner that yields a pre-scripted fake port per call (matrix + respawn). */
function scriptedSpawner(
  scripts: Array<{ lines: string[]; exitCode: number | Promise<number>; signals?: string[] }>,
): { spawn: SpawnChild; calls: () => number } {
  let i = 0;
  return {
    calls: () => i,
    spawn: () => fakePort(scripts[Math.min(i++, scripts.length - 1)] as (typeof scripts)[number]),
  };
}

/** Seed a dispatchable node row (resolved contract already in the index, as the scanner leaves it). */
async function seedNode(
  h: IndexHandle,
  id: string,
  extra: Partial<typeof node.$inferInsert> = {},
): Promise<void> {
  await h.db.insert(node).values({
    id,
    path: `work/${id}`,
    kind: "task",
    state: "triggered",
    gate: "human",
    createdAt: FIXED_MS,
    updatedAt: FIXED_MS,
    ...extra,
  });
}

/** Build a supervisor over a real worktree factory + `:memory:` index + a fixture spawner. */
function makeSupervisor(
  ws: string,
  h: IndexHandle,
  spawn: SpawnChild,
  over: Partial<SupervisorDeps> = {},
): {
  sup: ReturnType<typeof createSupervisor>;
  tokens: SessionTokenRegistry;
  mintCalls: () => number;
} {
  let mintCalls = 0;
  const tokens = new SessionTokenRegistry(() => `tok-${++mintCalls}`);
  const sup = createSupervisor({
    db: h.db,
    factory: createWorktreeSandboxFactory({ workspace: ws }),
    tokens,
    proxy: { url: "http://127.0.0.1:0" },
    spawnChild: spawn,
    now: fixedNow,
    mintRunId: () => "r1",
    hostEnv: { PATH: process.env.PATH },
    ...over,
  });
  return { sup, tokens, mintCalls: () => mintCalls };
}

async function exists(p: string): Promise<boolean> {
  try {
    await stat(p);
    return true;
  } catch {
    return false;
  }
}

async function openMem(): Promise<IndexHandle> {
  const h = await openIndex(":memory:");
  handles.push(h);
  return h;
}

// ── Exit-code matrix (fabricated child) ──────────────────────────────────────

describe("reap exit-code matrix (HARNESS §4)", () => {
  test("exit 0 (claims complete) → session review + node review + run.finished", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedNode(h, "n0");
    const { sup, tokens } = makeSupervisor(
      ws,
      h,
      scriptedSpawner([
        {
          lines: ['{"actor":"agent","type":"agent.said","payload":{"text":"done"}}\n'],
          exitCode: 0,
        },
      ]).spawn,
    );

    const out = await sup.dispatch("n0");
    expect(out.dispatched).toBe(true);
    if (!out.dispatched) throw new Error("unreachable");
    const res = await out.reaped;

    // the exact terminal triple
    expect(res.sessionStatus).toBe("review");
    expect(res.nodeState).toBe("review");
    expect(res.event).toBe("run.finished");
    expect(res.crash).toBe(false);
    // the DB reflects it
    const [srow] = await h.db.select().from(session).where(eq(session.id, "r1"));
    expect(srow?.status).toBe("review");
    expect(srow?.endedAt).toBe(FIXED_MS);
    const [nrow] = await h.db.select().from(node).where(eq(node.id, "n0"));
    expect(nrow?.state).toBe("review");
    // run.finished landed on the durable log
    const finished = await h.db
      .select()
      .from(event)
      .where(and(eq(event.sessionId, "r1"), eq(event.type, EVENT_TYPES.RUN_FINISHED)));
    expect(finished).toHaveLength(1);
    // token revoked, registry drained, lease released
    expect(tokens.size).toBe(0);
    expect(sup.list()).toHaveLength(0);
    expect(await h.db.select().from(lease).where(eq(lease.key, "n0"))).toHaveLength(0);
  });

  for (const reason of ["budget", "iteration_cap", "no_progress", "user_stop"] as const) {
    test(`exit 10 reason ${reason} → session blocked + node blocked`, async () => {
      const ws = await makeWorkspace();
      const h = await openMem();
      await seedNode(h, "n0");
      const { sup } = makeSupervisor(
        ws,
        h,
        scriptedSpawner([{ lines: [runExiting(10, reason)], exitCode: 10 }]).spawn,
      );
      const out = await sup.dispatch("n0");
      if (!out.dispatched) throw new Error("unreachable");
      const res = await out.reaped;
      expect(res.sessionStatus).toBe("blocked");
      expect(res.nodeState).toBe("blocked");
      expect(res.reason).toBe(reason);
      expect(res.crash).toBe(false);
      expect(res.event).toBe("run.finished"); // a clean stop is finished, NOT failed
      const [srow] = await h.db.select().from(session).where(eq(session.id, "r1"));
      expect(srow?.status).toBe("blocked");
      const [nrow] = await h.db.select().from(node).where(eq(node.id, "n0"));
      expect(nrow?.state).toBe("blocked");
      // the durable terminal event is RUN_FINISHED, not RUN_FAILED
      const finished = await h.db
        .select()
        .from(event)
        .where(and(eq(event.sessionId, "r1"), eq(event.type, EVENT_TYPES.RUN_FINISHED)));
      expect(finished).toHaveLength(1);
    });
  }

  test("exit 10 reason fresh_context → respawn: same run id + worktree, budget NOT re-zeroed, new token", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedNode(h, "n0");
    // Attempt 1 stops fresh_context; attempt 2 completes clean.
    const scripted = scriptedSpawner([
      { lines: [runExiting(10, "fresh_context")], exitCode: 10 },
      { lines: [runExiting(0)], exitCode: 0 },
    ]);
    // Interpose a budget mutation on the respawn's factory.create (which the supervisor AWAITs before
    // re-launch), so we can PROVE the respawn does not reset it — a re-zero anywhere after would clobber
    // this sentinel (positive anti-vacuity, deterministic because create is awaited).
    const base = createWorktreeSandboxFactory({ workspace: ws });
    let createN = 0;
    const factory = {
      async create(runId: string, opts?: Parameters<typeof base.create>[1]) {
        if (++createN === 2) {
          await h.db
            .update(runBudget)
            .set({ spentUsd: 0.42, iterations: 3 })
            .where(eq(runBudget.sessionId, runId));
        }
        return base.create(runId, opts);
      },
    };
    const { sup, mintCalls } = makeSupervisor(ws, h, scripted.spawn, { factory });

    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("unreachable");
    const res = await out.reaped;

    expect(scripted.calls()).toBe(2); // the respawn happened
    expect(res.respawns).toBe(1);
    expect(res.runId).toBe("r1"); // SAME run id across attempts
    expect(res.sessionStatus).toBe("review"); // a clean review — proves NO runBudget re-insert (PK crash)
    // exactly one run_budget row, and it was NOT re-zeroed (the interposed sentinel survives)
    const budgets = await h.db.select().from(runBudget).where(eq(runBudget.sessionId, "r1"));
    expect(budgets).toHaveLength(1);
    expect(budgets[0]?.spentUsd).toBe(0.42);
    expect(budgets[0]?.iterations).toBe(3);
    // the worktree is the SAME and still on disk (re-attached, not re-created)
    expect(await exists(join(ws, ".maestro", "worktrees", "run-r1"))).toBe(true);
    // a NEW token was minted for attempt 2 (mint revokes prior) — two mints total
    expect(mintCalls()).toBe(2);
  });

  test("exit 20 (needs input) → question gate opened + session review + node review", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedNode(h, "n0");
    const { sup } = makeSupervisor(
      ws,
      h,
      scriptedSpawner([{ lines: [runExiting(20)], exitCode: 20 }]).spawn,
    );
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("unreachable");
    const res = await out.reaped;
    expect(res.sessionStatus).toBe("review");
    expect(res.nodeState).toBe("review");
    expect(res.event).toBe("run.finished");
    expect(res.gateId).toBeTruthy();
    // a pending question gate row
    const gates = await h.db.select().from(gate).where(eq(gate.sessionId, "r1"));
    expect(gates).toHaveLength(1);
    expect(gates[0]?.kind).toBe("question");
    expect(gates[0]?.verdict).toBeNull();
    // gate.opened emitted
    const opened = await h.db
      .select()
      .from(event)
      .where(and(eq(event.sessionId, "r1"), eq(event.type, EVENT_TYPES.GATE_OPENED)));
    expect(opened).toHaveLength(1);
  });

  test("crash (unexpected exit code, no run.exiting) → blocked + run.failed + worktree PRESERVED", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedNode(h, "n0");
    const { sup, tokens } = makeSupervisor(
      ws,
      h,
      scriptedSpawner([{ lines: ['{"actor":"system","type":"run.started"}\n'], exitCode: 139 }])
        .spawn,
    );
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("unreachable");
    const res = await out.reaped;
    expect(res.crash).toBe(true);
    expect(res.sessionStatus).toBe("blocked");
    expect(res.nodeState).toBe("blocked");
    expect(res.event).toBe("run.failed");
    const failed = await h.db
      .select()
      .from(event)
      .where(and(eq(event.sessionId, "r1"), eq(event.type, EVENT_TYPES.RUN_FAILED)));
    expect(failed).toHaveLength(1);
    // the worktree is preserved (the crash receipt), and the token/registry are still cleaned up
    expect(await exists(join(ws, ".maestro", "worktrees", "run-r1"))).toBe(true);
    expect(tokens.size).toBe(0);
    expect(sup.list()).toHaveLength(0);
  });

  test("run.exiting code ≠ real exit code → run.exit_mismatch emitted (Loop-4 signal)", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedNode(h, "n0");
    // child declares 20 but the process actually exits 0 → mismatch; the REAL 0 wins the route (review)
    const { sup } = makeSupervisor(
      ws,
      h,
      scriptedSpawner([{ lines: [runExiting(20)], exitCode: 0 }]).spawn,
    );
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("unreachable");
    const res = await out.reaped;
    expect(res.mismatch).toBe(true);
    expect(res.sessionStatus).toBe("review"); // routed on the REAL code (0), not the declared (20)
    const mismatch = await h.db
      .select()
      .from(event)
      .where(and(eq(event.sessionId, "r1"), eq(event.type, EVENT_TYPES.RUN_EXIT_MISMATCH)));
    expect(mismatch).toHaveLength(1);
    expect(JSON.parse(mismatch[0]?.payload ?? "{}")).toMatchObject({ declared: 20, actual: 0 });
  });
});

// ── Dispatch idempotency + guards ────────────────────────────────────────────

describe("dispatch guards", () => {
  test("a held node lease makes dispatch a silent no-op (no session created)", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedNode(h, "n0");
    // someone else already holds the node's dispatch lease
    await h.db
      .insert(lease)
      .values({ key: "n0", holder: "other", acquiredAt: FIXED_MS, expiresAt: FIXED_MS + 1000 });
    const { sup } = makeSupervisor(
      ws,
      h,
      scriptedSpawner([{ lines: [runExiting(0)], exitCode: 0 }]).spawn,
    );
    const out = await sup.dispatch("n0");
    expect(out).toEqual({ dispatched: false, reason: "lease_held" });
    // no run started
    expect(await h.db.select().from(session)).toHaveLength(0);
    expect(sup.list()).toHaveLength(0);
  });

  test("an unknown node → dispatched:false node_not_found (no lease consumed)", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    const { sup } = makeSupervisor(
      ws,
      h,
      scriptedSpawner([{ lines: [runExiting(0)], exitCode: 0 }]).spawn,
    );
    const out = await sup.dispatch("ghost");
    expect(out).toEqual({ dispatched: false, reason: "node_not_found" });
    expect(await h.db.select().from(lease)).toHaveLength(0);
  });

  test("a spawn throw (broomva-child missing) is contained: blocked + run.failed, worktree preserved", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedNode(h, "n0");
    const throwingSpawn: SpawnChild = () => {
      throw new Error("ENOENT: broomva-child not found");
    };
    const { sup, tokens } = makeSupervisor(ws, h, throwingSpawn);
    const out = await sup.dispatch("n0");
    expect(out.dispatched).toBe(true); // the session was committed; the crash is reflected in reaped
    if (!out.dispatched) throw new Error("unreachable");
    const res = await out.reaped;
    expect(res.crash).toBe(true);
    expect(res.sessionStatus).toBe("blocked");
    // the session is not orphaned as running-with-no-process
    const [srow] = await h.db.select().from(session).where(eq(session.id, "r1"));
    expect(srow?.status).toBe("blocked");
    // token minted-then-revoked → none live; worktree preserved for the crash receipt
    expect(tokens.size).toBe(0);
    expect(await exists(join(ws, ".maestro", "worktrees", "run-r1"))).toBe(true);
  });
});

// ── P20 fixes: reap-fault containment · attempt-scoping · kill-race · dispatch guards ──

describe("reap containment + attempt-scoping + kill race (P20 fixes)", () => {
  test("BLOCKER: an index fault during reap still cleans up (token revoked, registry drained, reaped resolves)", async () => {
    const ws = await makeWorkspace();
    // NOT tracked in `handles` — this test closes the client itself to simulate the index fault.
    const h = await openIndex(":memory:");
    await seedNode(h, "n0");
    // A child that emits nothing and whose exit we control — so the db is closed BEFORE reap reads it.
    let resolveExit: (code: number) => void = () => {};
    const exited = new Promise<number>((r) => {
      resolveExit = r;
    });
    const { sup, tokens } = makeSupervisor(ws, h, () => fakePort({ lines: [], exitCode: exited }));
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("unreachable");
    // Simulate the exact index-closed/busy fault the module guards against, THEN let the child exit so
    // reap runs every db read/write against a closed handle.
    h.client.close();
    resolveExit(0);
    // The reap must RESOLVE (no unhandled rejection) and still free the in-memory resources it owns.
    const res = await out.reaped;
    expect(res).toBeTruthy();
    expect(tokens.size).toBe(0); // token revoked despite the db fault (the blast-radius invariant)
    expect(sup.list()).toHaveLength(0); // registry entry dropped
  });

  test("MAJOR: a respawned attempt that does not re-declare run.exiting parks blocked (attempt-scoped read)", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedNode(h, "n0");
    // Attempt 1 stops fresh_context (respawn); attempt 2 exits 10 emitting NO run.exiting of its own.
    // Without per-attempt scoping, attempt 2 would read attempt 1's fresh_context row and respawn-loop.
    const scripted = scriptedSpawner([
      { lines: [runExiting(10, "fresh_context")], exitCode: 10 },
      { lines: [], exitCode: 10 },
    ]);
    const { sup } = makeSupervisor(ws, h, scripted.spawn, { maxRespawns: 3 });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("unreachable");
    const res = await out.reaped;
    // fixed: exactly ONE respawn, then park blocked (attempt 2's absent run.exiting → reason undefined)
    expect(scripted.calls()).toBe(2); // broken (stale read) would loop to maxRespawns+1 calls
    expect(res.respawns).toBe(1);
    expect(res.sessionStatus).toBe("blocked");
    expect(res.crash).toBe(false);
  });

  test("MAJOR: kill during a fresh_context respawn WINS — no resurrection (no new child / token)", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedNode(h, "n0");
    const scripted = scriptedSpawner([
      { lines: [runExiting(10, "fresh_context")], exitCode: 10 }, // attempt 1 → respawn
      { lines: [runExiting(0)], exitCode: 0 }, // attempt 2 (must NEVER spawn — kill wins)
    ]);
    // Interpose on the respawn's factory.create so the test can fire kill() mid-await.
    const base = createWorktreeSandboxFactory({ workspace: ws });
    let createN = 0;
    let signalCreate2 = () => {};
    const create2Started = new Promise<void>((r) => {
      signalCreate2 = r;
    });
    let proceed = () => {};
    const create2Proceed = new Promise<void>((r) => {
      proceed = r;
    });
    const factory = {
      async create(runId: string, opts?: Parameters<typeof base.create>[1]) {
        if (++createN === 2) {
          signalCreate2();
          await create2Proceed;
        }
        return base.create(runId, opts);
      },
    };
    const { sup, tokens } = makeSupervisor(ws, h, scripted.spawn, { factory });
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("unreachable");
    await create2Started; // attempt 1 reaped fresh_context; respawn is now awaiting factory.create
    expect(sup.kill("r1")).toBe(true); // kill lands mid-respawn
    proceed(); // create resolves; respawn re-checks cancelled → contain, NOT re-launch
    const res = await out.reaped;
    expect(scripted.calls()).toBe(1); // attempt-2 child was NEVER spawned — kill won the race
    expect(res.crash).toBe(true);
    expect(res.sessionStatus).toBe("blocked");
    expect(tokens.size).toBe(0);
  });

  test("provision failure (factory.create throws) is contained: blocked + run.failed, lease released", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedNode(h, "n0");
    const factory = {
      create(): Promise<never> {
        throw new Error("worktree provision failed");
      },
    };
    const { sup, tokens } = makeSupervisor(
      ws,
      h,
      scriptedSpawner([{ lines: [runExiting(0)], exitCode: 0 }]).spawn,
      { factory },
    );
    const out = await sup.dispatch("n0");
    expect(out.dispatched).toBe(true);
    if (!out.dispatched) throw new Error("unreachable");
    const res = await out.reaped;
    expect(res.crash).toBe(true);
    expect(res.sessionStatus).toBe("blocked");
    // run.failed emitted (index-only path, no runDir), session parked, lease released, no live token
    const failed = await h.db
      .select()
      .from(event)
      .where(and(eq(event.sessionId, "r1"), eq(event.type, EVENT_TYPES.RUN_FAILED)));
    expect(failed).toHaveLength(1);
    const [srow] = await h.db.select().from(session).where(eq(session.id, "r1"));
    expect(srow?.status).toBe("blocked");
    expect(await h.db.select().from(lease).where(eq(lease.key, "n0"))).toHaveLength(0);
    expect(tokens.size).toBe(0);
  });

  test("a tombstoned node → node_not_found (no lease, no session)", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedNode(h, "n0", { deletedAt: FIXED_MS }); // soft-deleted
    const { sup } = makeSupervisor(
      ws,
      h,
      scriptedSpawner([{ lines: [runExiting(0)], exitCode: 0 }]).spawn,
    );
    const out = await sup.dispatch("n0");
    expect(out).toEqual({ dispatched: false, reason: "node_not_found" });
    expect(await h.db.select().from(lease)).toHaveLength(0);
    expect(await h.db.select().from(session)).toHaveLength(0);
  });
});

// ── Real Bun.spawn (dogfood): real exited/stdout/kill wiring + SIGKILL containment ──

describe("real child process (dogfood, P11)", () => {
  /** Write an executable bun fixture-child script; return its absolute path. */
  async function writeFixtureChild(ws: string, name: string, body: string): Promise<string> {
    const p = join(ws, name);
    // `Bun.write(Bun.stdout, …)` (not process.stdout.write) so a line FLUSHES before exit — a child
    // killed before an OS flush loses un-flushed events (the BRO-1767 Bun block-buffer lesson).
    await writeFile(p, `${body}\n`);
    await chmod(p, 0o755);
    return p;
  }

  /** A spawner that Bun.spawns a real fixture script inside the sandbox. */
  function realSpawn(scriptPath: string): SpawnChild {
    return (args) => {
      const proc = Bun.spawn(["bun", scriptPath, ...args.argv], {
        cwd: args.cwd,
        env: { ...args.env },
        stdout: "pipe",
        stderr: "pipe",
        stdin: "pipe",
      });
      // fromBunSubprocess is what dispatch's default uses; import indirectly via the port shape.
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

  test("clean exit 0 through a real process → review, and the child's stdout event is teed", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedNode(h, "n0");
    const scriptPath = await writeFixtureChild(
      ws,
      "clean-child.ts",
      `await Bun.write(Bun.stdout, JSON.stringify({actor:"agent",type:"agent.said",payload:{text:"hi from real child"}}) + "\\n");
process.exit(0);`,
    );
    const { sup } = makeSupervisor(ws, h, realSpawn(scriptPath));
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("unreachable");
    const res = await out.reaped;
    expect(res.exitCode).toBe(0);
    expect(res.sessionStatus).toBe("review");
    // the real child's stdout line was split, classified, and teed to the event table
    const said = await h.db
      .select()
      .from(event)
      .where(and(eq(event.sessionId, "r1"), eq(event.type, EVENT_TYPES.AGENT_SAID)));
    expect(said).toHaveLength(1);
    expect(JSON.parse(said[0]?.payload ?? "{}")).toMatchObject({ text: "hi from real child" });
  });

  test("SIGKILL a live real child → blocked + run.failed, and the runtime KEEPS serving /health", async () => {
    const ws = await makeWorkspace();
    const h = await openMem();
    await seedNode(h, "n0");
    // stand up the REAL runtime app over the same index — its /health is the "runtime serving" proof
    const config = loadConfig({ MAESTRO_WORKSPACE: ws });
    const app = createApp(config, FIXED_MS, h.db);
    const before = await app.request("/health");
    expect(before.status).toBe(200);

    // a real child that announces itself then hangs forever (so it is LIVE when we SIGKILL it)
    const scriptPath = await writeFixtureChild(
      ws,
      "hang-child.ts",
      `await Bun.write(Bun.stdout, JSON.stringify({actor:"system",type:"run.started"}) + "\\n");
await new Promise(() => {});`,
    );
    const { sup, tokens } = makeSupervisor(ws, h, realSpawn(scriptPath));
    const out = await sup.dispatch("n0");
    if (!out.dispatched) throw new Error("unreachable");
    // the run is live and registered
    expect(sup.list()).toHaveLength(1);
    // F8 seam: SIGKILL it
    expect(sup.kill("r1")).toBe(true);
    const res = await out.reaped;

    // the child crash is CONTAINED — parked, not propagated
    expect(res.crash).toBe(true);
    expect(res.sessionStatus).toBe("blocked");
    const failed = await h.db
      .select()
      .from(event)
      .where(and(eq(event.sessionId, "r1"), eq(event.type, EVENT_TYPES.RUN_FAILED)));
    expect(failed).toHaveLength(1);
    // the worktree receipt is preserved, token cleaned, registry drained
    expect(await exists(join(ws, ".maestro", "worktrees", "run-r1"))).toBe(true);
    expect(tokens.size).toBe(0);
    expect(sup.list()).toHaveLength(0);
    // THE INVARIANT: the runtime still serves after the child died
    const after = await app.request("/health");
    expect(after.status).toBe(200);
    expect(await after.json()).toMatchObject({ ok: true });
  });
});
