/// <reference types="bun" />
// gate-slice.test.ts — BRO-1805 F5 gate-slice E2E, the integration half of the done.check
// (`bun test apps/runtime --filter gate`).
//
// The done.check also names `playwright test gate-slice.spec` (a browser E2E). A faithful all-four-
// verdicts BROWSER path is disproportionate to this slice, for two structural reasons this test
// documents rather than fights:
//   1. The SPA's gate-queue learns a gate's id ONLY from a live SSE `gate.opened` — the boot hydration
//      fetches `/api/tree` (nodes-only) and no view polls `/api/node/:id` for gates (store/stream.ts,
//      store/project.ts §gateId). A hand-seeded gate therefore never surfaces a gateId to drive a verb.
//   2. approve needs a run that produced a REAL mergeable diff + a passing `verdict.md`; the mock model
//      (the only dispatch mode with no upstream) produces neither. So the browser approve path would need
//      new mock-dispatch-to-gate + mergeable-mock infrastructure — out of proportion to the gate slice.
//
// This runtime integration test stands in for the browser spec (the loop directive's escape hatch) and
// adds what no per-verdict unit test covers: the WHOLE gate-slice narrative through the LIVE read + write
// API the SPA actually consumes —
//   ① a seeded run reaches "Needs you": `/api/board` groups it review-first AND `/api/node/:id` joins its
//      OPEN gate (verdict null) — the exact projection `selectGateQueue` + the WorkItem `gateId` depend on;
//   ② each of the four verdict intents lands its state over the live `/api/intents` route
//      (approve → done + real squash-merge, revise → triggered, block → canceled, escalate → review-stays);
//   ③ an auto-done attempt throws: there is NO wire path review→done but `approve` (`set_state` 501s), and
//      the transition module guards the edge (GateRequiredError) — "no auto-done when gate:human".
//
// Where the block/revise/escalate/approve UNIT tests (intents.test.ts) assert DB rows directly, this
// asserts the read API the board renders from, in one coherent run — the seam between the two.

import { afterAll, describe, expect, test } from "bun:test";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { GateRequiredError, transition, type VerdictReceipt } from "@maestro/protocol";
import { eq } from "drizzle-orm";
import { createApp } from "../app";
import { DEFAULT_PORT, type RuntimeConfig } from "../config";
import { type IndexDb, openIndex } from "../db/client";
import { gate, node, session } from "../db/schema";
import { renderVerdictMd } from "../verifier/verdict";

const tmps: string[] = [];
afterAll(() => {
  for (const t of tmps) rmSync(t, { recursive: true, force: true });
});

function cfg(workspace: string): RuntimeConfig {
  return {
    port: DEFAULT_PORT,
    workspace,
    indexPath: ":memory:",
    lockPath: join(workspace, ".maestro/lock"),
  };
}

function gitOk(cwd: string, args: string[]): void {
  const r = Bun.spawnSync(["git", ...args], { cwd });
  if (r.exitCode !== 0) throw new Error(`git ${args.join(" ")}: ${r.stderr.toString()}`);
}

function gitOut(cwd: string, args: string[]): string {
  const r = Bun.spawnSync(["git", ...args], { cwd });
  if (r.exitCode !== 0) throw new Error(`git ${args.join(" ")}: ${r.stderr.toString()}`);
  return r.stdout.toString().trim();
}

/** A temp git workspace (with a local identity so commits succeed). */
function mkWorkspace(): string {
  const ws = mkdtempSync(join(tmpdir(), "maestro-gate-slice-"));
  tmps.push(ws);
  gitOk(ws, ["init", "-q"]);
  gitOk(ws, ["config", "user.email", "t@maestro.local"]);
  gitOk(ws, ["config", "user.name", "maestro-test"]);
  gitOk(ws, ["config", "commit.gpgsign", "false"]);
  return ws;
}

async function mkApp(ws: string) {
  const handle = await openIndex(":memory:");
  return { app: createApp(cfg(ws), Date.now(), handle.db), handle };
}

function post(app: Awaited<ReturnType<typeof mkApp>>["app"], body: unknown, key: string) {
  return app.request("/api/intents", {
    method: "POST",
    headers: { "Content-Type": "application/json", "Idempotency-Key": key },
    body: JSON.stringify(body),
  });
}

/** Seed a node@review + its session + an OPEN completion gate (verdict null) — the F5 decision setup. */
async function seedOpenGate(
  db: IndexDb,
  ids: { nodeId: string; sessionId: string; gateId: string },
): Promise<void> {
  const now = Date.now();
  await db.insert(node).values({
    id: ids.nodeId,
    path: `work/${ids.nodeId}`,
    kind: "task",
    state: "review",
    gate: "human",
    createdAt: now,
    updatedAt: now,
  });
  await db.insert(session).values({
    id: ids.sessionId,
    nodeId: ids.nodeId,
    branch: `run/${ids.sessionId}`,
    status: "review",
    startedAt: now,
    updatedAt: now,
  });
  await db.insert(gate).values({
    id: ids.gateId,
    sessionId: ids.sessionId,
    kind: "completion",
    proposalJson: null,
    verdict: null,
    decidedBy: null,
    openedAt: now,
    decidedAt: null,
    updatedAt: now,
    deletedAt: null,
  });
}

/** Seed n's `_work.md` on disk at review, committed clean (the runtime's transient dirs gitignored). */
function seedWorkFileAtReview(ws: string, nodeId: string): void {
  const wm = `---\nid: ${nodeId}\nkind: task\nstate: review\ngate: human\ncreated: 2026-06-25\nupdated: 2026-06-25\n---\n\n# ${nodeId}\n`;
  writeFileSync(join(ws, ".gitignore"), "/.maestro/\n/runs/\n");
  mkdirSync(join(ws, "work", nodeId), { recursive: true });
  writeFileSync(join(ws, "work", nodeId, "_work.md"), wm);
  gitOk(ws, ["add", "-A"]);
  gitOk(ws, ["commit", "-qm", "seed review"]);
}

/** A real approvable run: base commit @ review, a `run/<id>` branch with a committed change off that base,
 *  and a passing `runs/run-<id>/verdict.md` whose `base` is the workspace tip (rung 1 — base unmoved). */
function seedApprovableRun(ws: string, nodeId: string, sessionId: string): void {
  seedWorkFileAtReview(ws, nodeId);
  const base = gitOut(ws, ["rev-parse", "HEAD"]);
  const runBranch = `run/${sessionId}`;
  gitOk(ws, ["branch", runBranch, base]);
  const wt = mkdtempSync(join(tmpdir(), `maestro-wt-${sessionId}-`));
  tmps.push(wt);
  gitOk(ws, ["worktree", "add", "-q", wt, runBranch]);
  writeFileSync(join(wt, "feature.ts"), "export const A = 1;\n");
  gitOk(wt, ["add", "-A"]);
  gitOk(wt, ["commit", "-qm", `run ${sessionId}`]);
  gitOk(ws, ["worktree", "remove", "--force", wt]);
  const runDir = join(ws, "runs", `run-${sessionId}`);
  mkdirSync(runDir, { recursive: true });
  const receipt: VerdictReceipt = {
    verdict: "pass",
    attempt: 2,
    base,
    diffstat: { files: 1, plus: 1, minus: 0 },
    tampering: [],
    checks: [],
    judge: { score: 1 },
  };
  writeFileSync(join(runDir, "verdict.md"), renderVerdictMd(receipt, "looks good"));
}

// ── the read-API projection the SPA renders ────────────────────────────────────
interface BoardResp {
  groups: { state: string; nodes: { id: string; state: string }[] }[];
}
interface NodeDetailResp {
  node: { id: string; state: string; owner?: string | null };
  gates: { id: string; verdict: string | null; sessionId: string }[];
}
const getJson = async <T>(
  app: Awaited<ReturnType<typeof mkApp>>["app"],
  path: string,
): Promise<T> => {
  const r = await app.request(path);
  if (!r.ok) throw new Error(`GET ${path} → ${r.status}`);
  return (await r.json()) as T;
};

describe("BRO-1805 gate-slice E2E — the live read+write API the board renders from", () => {
  // ① Needs you: the read projection selectGateQueue + the WorkItem gateId depend on.
  test("① a seeded run reaches Needs you — /api/board groups it review-first, /api/node/:id joins its OPEN gate", async () => {
    const ws = mkWorkspace();
    const { app, handle } = await mkApp(ws);
    await seedOpenGate(handle.db, { nodeId: "n1", sessionId: "r1", gateId: "g1" });

    // /api/board: the sole seeded node is a review card, and review is the FIRST group (D-ORDER,
    // review-first) — exactly how the board surfaces "Needs you".
    const board = await getJson<BoardResp>(app, "/api/board");
    expect(board.groups[0]?.state).toBe("review");
    expect(board.groups[0]?.nodes.map((n) => n.id)).toContain("n1");

    // /api/node/:id: the open gate (verdict null) is joined through the session — its id is the gateId the
    // verbs dispatch off (store/project.ts picks the most-recently-opened open gate as the WorkItem key).
    const detail = await getJson<NodeDetailResp>(app, "/api/node/n1");
    expect(detail.node.state).toBe("review");
    const open = detail.gates.filter((g) => g.verdict === null);
    expect(open).toHaveLength(1);
    expect(open[0]?.id).toBe("g1");
    handle.client.close();
  });

  // ② block → canceled, observed through the read API (not the DB row).
  test("② block lands the node canceled (read via /api/node/:id)", async () => {
    const ws = mkWorkspace();
    const { app, handle } = await mkApp(ws);
    await seedOpenGate(handle.db, { nodeId: "n1", sessionId: "r1", gateId: "g1" });

    expect((await post(app, { type: "block", gateId: "g1", reason: "no" }, "k-block")).status).toBe(
      202,
    );

    const detail = await getJson<NodeDetailResp>(app, "/api/node/n1");
    expect(detail.node.state).toBe("canceled");
    expect(detail.gates.find((g) => g.id === "g1")?.verdict).toBe("block");
    // and it left the review group on the board (no longer "Needs you")
    const board = await getJson<BoardResp>(app, "/api/board");
    expect(board.groups.find((g) => g.state === "review")).toBeUndefined();
    handle.client.close();
  });

  // ② revise → triggered.
  test("② revise lands the node triggered (send back), read via /api/node/:id", async () => {
    const ws = mkWorkspace();
    const { app, handle } = await mkApp(ws);
    await seedOpenGate(handle.db, { nodeId: "n1", sessionId: "r1", gateId: "g1" });

    expect(
      (await post(app, { type: "revise", gateId: "g1", feedback: "tighten it" }, "k-rev")).status,
    ).toBe(202);

    const detail = await getJson<NodeDetailResp>(app, "/api/node/n1");
    expect(detail.node.state).toBe("triggered");
    expect(detail.gates.find((g) => g.id === "g1")?.verdict).toBe("revise");
    handle.client.close();
  });

  // ② escalate → node STAYS review, owner reassigned, gate STAYS open (re-decidable — a later block still lands).
  test("② escalate keeps the node at review (reassigned, gate re-decidable), read via /api/node/:id", async () => {
    const ws = mkWorkspace();
    const { app, handle } = await mkApp(ws);
    await seedOpenGate(handle.db, { nodeId: "n1", sessionId: "r1", gateId: "g1" });

    expect((await post(app, { type: "escalate", gateId: "g1", to: "@lead" }, "k-esc")).status).toBe(
      202,
    );

    const afterEsc = await getJson<NodeDetailResp>(app, "/api/node/n1");
    expect(afterEsc.node.state).toBe("review"); // non-terminal: still Needs you
    expect(afterEsc.node.owner).toBe("@lead"); // reassigned
    expect(afterEsc.gates.find((g) => g.id === "g1")?.verdict).toBeNull(); // gate still OPEN

    // re-decidable: the still-open gate can now be blocked → the escalate did NOT terminate it.
    expect((await post(app, { type: "block", gateId: "g1" }, "k-esc-then-block")).status).toBe(202);
    const afterBlock = await getJson<NodeDetailResp>(app, "/api/node/n1");
    expect(afterBlock.node.state).toBe("canceled");
    expect(afterBlock.gates.find((g) => g.id === "g1")?.verdict).toBe("block");
    handle.client.close();
  });

  // ② approve → real squash-merge (D1) → node done, observed through the read API + the git receipt.
  test("② approve squash-merges the run and lands the node done (the branch is the receipt)", async () => {
    const ws = mkWorkspace();
    const { app, handle } = await mkApp(ws);
    await seedOpenGate(handle.db, { nodeId: "n1", sessionId: "r1", gateId: "g1" });
    seedApprovableRun(ws, "n1", "r1");

    expect((await post(app, { type: "approve", gateId: "g1" }, "k-approve")).status).toBe(202);

    const detail = await getJson<NodeDetailResp>(app, "/api/node/n1");
    expect(detail.node.state).toBe("done");
    expect(detail.gates.find((g) => g.id === "g1")?.verdict).toBe("approve");
    // the run REALLY merged: its file landed on the workspace branch, run/<id> archived (the receipt).
    expect(existsSync(join(ws, "feature.ts"))).toBe(true);
    const branches = gitOut(ws, ["branch", "--list"]);
    expect(branches).toContain("archive/run-r1");
    expect(branches).not.toContain("run/r1");
    // left the review group — no longer Needs you.
    const board = await getJson<BoardResp>(app, "/api/board");
    expect(board.groups.find((g) => g.state === "review")).toBeUndefined();
    handle.client.close();
  });

  // ③ no auto-done under gate:human — the transition module throws AND no wire path bypasses it.
  test("③ auto-done throws: transition module guards review→done, and no intent bypasses it (set_state 501s)", async () => {
    // The state machine: the ONLY legal path to `done` from `review` is an approve verdict; a bare
    // transition throws GateRequiredError (packages/protocol/src/state.ts — "no auto-done when gate:human").
    expect(() => transition("review", "done")).toThrow(GateRequiredError);
    expect(() => transition("review", "done", { verdict: "revise" })).toThrow(GateRequiredError);
    expect(() => transition("running", "done", { gate: "human" })).toThrow(GateRequiredError);
    // and `review → done` under an approve verdict is the one legal edge.
    expect(transition("review", "done", { verdict: "approve" })).toBe("done");

    // The write surface offers NO auto-done bypass: `set_state` (the audited human override) is not wired,
    // so a direct review→done override is refused `unsupported_intent` (501) — the gate is the only door.
    const ws = mkWorkspace();
    const { app, handle } = await mkApp(ws);
    await seedOpenGate(handle.db, { nodeId: "n1", sessionId: "r1", gateId: "g1" });
    const res = await post(app, { type: "set_state", nodeId: "n1", state: "done" }, "k-setstate");
    expect(res.status).toBe(501);
    expect(((await res.json()) as { error: { code: string } }).error.code).toBe(
      "unsupported_intent",
    );
    // the node did NOT move to done off the refused override — still at the gate.
    const [n] = await handle.db.select().from(node).where(eq(node.id, "n1"));
    expect(n?.state).toBe("review");
    // the gate is untouched (still open)
    const [g] = await handle.db.select().from(gate).where(eq(gate.id, "g1"));
    expect(g?.verdict).toBeNull();
    handle.client.close();
  });
});
