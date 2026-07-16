/// <reference types="bun" />
// intents.test.ts — the write surface (BRO-1820, `bun test apps/runtime --filter intents`).
//
// done.check, three halves:
//  1. new_mission (F1) creates folder + `_work.md` + git commit ATOMICALLY (the commit is
//     the transaction), writing frontmatter with gate PINNED to human (a checkless mission
//     can't be gate:auto), owner/budget inherited from the parent at scan time.
//  2. a retried POST with the SAME Idempotency-Key is a NO-OP (one commit, not two).
//  3. a FAILING new_mission leaves the workspace exactly as it was (nothing half-created).
// Plus the refusal contract: missing key / unknown type / malformed body / path traversal.

import { afterAll, expect, test } from "bun:test";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { parseWorkFile, type VerdictReceipt } from "@maestro/protocol";
import { and, eq, like } from "drizzle-orm";
import { createApp } from "../app";
import { DEFAULT_PORT, type RuntimeConfig } from "../config";
import { type IndexDb, openIndex } from "../db/client";
import { event, gate, node, session } from "../db/schema";
import { gitIsClean } from "../git/git";
import { scanIntoIndex } from "../scanner";
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

/** A temp workspace; git-init'd (with a local identity so commits succeed) unless git:false. */
function mkWorkspace(git = true): string {
  const ws = mkdtempSync(join(tmpdir(), "maestro-intents-"));
  tmps.push(ws);
  if (git) {
    gitOk(ws, ["init", "-q"]);
    gitOk(ws, ["config", "user.email", "t@maestro.local"]);
    gitOk(ws, ["config", "user.name", "maestro-test"]);
    gitOk(ws, ["config", "commit.gpgsign", "false"]);
  }
  return ws;
}

async function mkApp(ws: string, reconcile?: () => void) {
  const handle = await openIndex(":memory:");
  return { app: createApp(cfg(ws), Date.now(), handle.db, reconcile), handle };
}

/** Count commits reachable from HEAD (0 on an empty repo). */
function commitCount(ws: string): number {
  const r = Bun.spawnSync(["git", "rev-list", "--count", "HEAD"], { cwd: ws });
  return r.exitCode === 0 ? Number(r.stdout.toString().trim()) : 0;
}

function post(app: Awaited<ReturnType<typeof mkApp>>["app"], body: unknown, key?: string) {
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  if (key !== undefined) headers["Idempotency-Key"] = key;
  return app.request("/api/intents", { method: "POST", headers, body: JSON.stringify(body) });
}

const NEW_MISSION = {
  type: "new_mission",
  parentPath: "",
  title: "Ship the board",
  brief: "make it live",
  kind: "task",
};

// ── 1. F1 create ────────────────────────────────────────────────────────────
test("new_mission creates folder + _work.md + a single commit, minimal frontmatter", async () => {
  const ws = mkWorkspace();
  const { app } = await mkApp(ws);

  const res = await post(app, NEW_MISSION, "key-1");
  expect(res.status).toBe(202);
  expect(await res.json()).toEqual({ accepted: true });

  const dir = join(ws, "ship-the-board");
  expect(existsSync(dir)).toBe(true);
  const content = readFileSync(join(dir, "_work.md"), "utf8");
  const wf = parseWorkFile(content); // parses ⇒ valid contract
  expect(wf.contract.kind).toBe("task");
  expect(wf.contract.state).toBe("proposed");
  expect(wf.brief).toContain("# Ship the board");
  expect(wf.brief).toContain("make it live");
  // gate PINNED to human (a checkless fresh mission can't be auto); owner/budget still inherit.
  expect(content).toContain("gate: human");
  expect(wf.contract.gate).toBe("human");
  expect(content).not.toContain("owner:");
  // The commit IS the transaction.
  expect(commitCount(ws)).toBe(1);
});

// ── 2. idempotency ────────────────────────────────────────────────────────────
test("a retry with the same Idempotency-Key is a no-op (one commit, not two)", async () => {
  const ws = mkWorkspace();
  const { app } = await mkApp(ws);

  const a = await post(app, NEW_MISSION, "same-key");
  const b = await post(app, NEW_MISSION, "same-key");
  expect(a.status).toBe(202);
  expect(b.status).toBe(202);
  expect(commitCount(ws)).toBe(1); // NOT 2
  // exactly one mission folder (plus .git)
  const dirs = readdirSync(ws).filter((n) => n !== ".git");
  expect(dirs).toEqual(["ship-the-board"]);
});

test("a different Idempotency-Key with the same title creates a second (suffixed) folder", async () => {
  const ws = mkWorkspace();
  const { app } = await mkApp(ws);

  await post(app, NEW_MISSION, "key-a");
  await post(app, NEW_MISSION, "key-b");
  expect(commitCount(ws)).toBe(2);
  const dirs = readdirSync(ws).filter((n) => n !== ".git");
  expect(dirs.length).toBe(2);
  expect(dirs).toContain("ship-the-board");
  expect(dirs.some((d) => d.startsWith("ship-the-board-"))).toBe(true); // the suffixed twin
});

// ── 3. failure leaves the workspace clean ──────────────────────────────────────
test("a failing new_mission (non-git workspace) leaves nothing half-created", async () => {
  const ws = mkWorkspace(false); // NOT a git repo → gitCommit throws after the folder is written
  const { app } = await mkApp(ws);

  const res = await post(app, NEW_MISSION, "key-fail");
  expect(res.status).toBe(500);
  expect(((await res.json()) as { error: { code: string } }).error.code).toBe("intent_failed");
  // The folder that was created mid-transaction is rolled back — workspace is empty.
  expect(readdirSync(ws)).toEqual([]);
});

test("a commit that fails AFTER `git add` unstages the index (no phantom staged entry)", async () => {
  // Force `git add` to succeed but `git commit` to fail WITHOUT a hook — the runtime now disables hooks AND
  // neutralizes the config exec channels on every git spawn (BRO-1802 key-confinement), so a rejecting
  // pre-commit hook OR a `commit.gpgsign`+`gpg.program` trick would simply be overridden. A hook-free,
  // config-hardening-proof failure: an EMPTY committer identity — `git add` stages fine (needs no identity),
  // `git commit` fails ("empty ident name not allowed"), and the enumerator never touches `user.*`. The
  // rollback must then clean the INDEX too, not just the working tree (P20 minor: "nothing half-created").
  const ws = mkWorkspace();
  Bun.spawnSync(["git", "config", "user.email", ""], { cwd: ws });
  Bun.spawnSync(["git", "config", "user.name", ""], { cwd: ws });
  const { app } = await mkApp(ws);

  const res = await post(app, NEW_MISSION, "key-signfail");
  expect(res.status).toBe(500);
  expect(((await res.json()) as { error: { code: string } }).error.code).toBe("intent_failed");
  // Working tree: folder gone. Index: nothing staged (the `git add` was rolled back).
  expect(existsSync(join(ws, "ship-the-board"))).toBe(false);
  const staged = Bun.spawnSync(["git", "diff", "--cached", "--name-only"], { cwd: ws })
    .stdout.toString()
    .trim();
  expect(staged).toBe("");
});

// ── the P20 MAJOR: a mission under a gate:auto parent must still materialize ────
test("new_mission under a gate:auto parent is NOT silently dropped by the scanner", async () => {
  // The parent is legitimately gate:auto (it has a done.check). A child that INHERITED gate:auto
  // would be invalid (auto needs a check) and the scanner would drop it into a discarded errors
  // list — 202 accepted but no card ever appears. Pinning the child to gate:human fixes it.
  const ws = mkWorkspace();
  mkdirSync(join(ws, "growth"));
  writeFileSync(
    join(ws, "growth", "_work.md"),
    [
      "---",
      "id: growth",
      "kind: project",
      "state: proposed",
      "gate: auto",
      "done:",
      "  check: bun test",
      "created: 2026-06-25",
      "updated: 2026-06-25",
      "---",
      "# Growth",
      "",
    ].join("\n"),
  );
  const { app } = await mkApp(ws);

  const res = await post(
    app,
    { type: "new_mission", parentPath: "growth", title: "SEO refresh", brief: "", kind: "task" },
    "key-auto",
  );
  expect(res.status).toBe(202);

  // Scan the workspace into a fresh index — the child must appear (not dropped), gate resolved
  // to human (it did not inherit the parent's auto), state proposed.
  const idx = await openIndex(":memory:");
  const { errors } = await scanIntoIndex(idx.db, ws, 1000);
  const child = (await idx.db.select().from(node).where(like(node.path, "growth/seo-refresh%")))[0];
  idx.client.close();
  expect(child).toBeDefined();
  expect(child?.gate).toBe("human");
  expect(child?.state).toBe("proposed");
  // and no scan error swallowed the child
  expect(errors.some((e) => e.path.includes("growth/seo-refresh"))).toBe(false);
});

// ── reconcile wiring (intents in, events out — F1 step 4) ──────────────────────
test("a successful new_mission nudges the reconcile; a no-op retry / refusal does not", async () => {
  const ws = mkWorkspace();
  let calls = 0;
  const { app } = await mkApp(ws, () => {
    calls += 1;
  });

  await post(app, NEW_MISSION, "rk-1");
  expect(calls).toBe(1); // a real create reconciles → node.updated on the stream

  await post(app, NEW_MISSION, "rk-1"); // idempotent retry (same key)
  expect(calls).toBe(1); // no re-dispatch ⇒ no second reconcile

  await post(app, { type: "approve", gateId: "x" }, "rk-2"); // unsupported refusal
  expect(calls).toBe(1); // a refusal never reconciles
});

// ── refusal contract ──────────────────────────────────────────────────────────
test("missing Idempotency-Key is refused invalid_intent (400), nothing created", async () => {
  const ws = mkWorkspace();
  const { app } = await mkApp(ws);
  const res = await post(app, NEW_MISSION); // no key
  expect(res.status).toBe(400);
  expect(((await res.json()) as { error: { code: string } }).error.code).toBe("invalid_intent");
  expect(commitCount(ws)).toBe(0);
});

test("a valid-but-unimplemented intent is refused unsupported_intent (501)", async () => {
  const ws = mkWorkspace();
  const { app } = await mkApp(ws);
  const res = await post(app, { type: "dispatch", nodeId: "abc" }, "key-d");
  expect(res.status).toBe(501);
  expect(((await res.json()) as { error: { code: string } }).error.code).toBe("unsupported_intent");
});

test("a malformed new_mission (missing title) is refused invalid_intent (400)", async () => {
  const ws = mkWorkspace();
  const { app } = await mkApp(ws);
  const res = await post(
    app,
    { type: "new_mission", parentPath: "", brief: "x", kind: "task" },
    "key-m",
  );
  expect(res.status).toBe(400);
  expect(((await res.json()) as { error: { code: string } }).error.code).toBe("invalid_intent");
});

test("parentPath escaping the workspace is refused unauthorized (403)", async () => {
  const ws = mkWorkspace();
  const { app } = await mkApp(ws);
  const res = await post(app, { ...NEW_MISSION, parentPath: "../../etc" }, "key-esc");
  expect(res.status).toBe(403);
  expect(((await res.json()) as { error: { code: string } }).error.code).toBe("unauthorized");
  expect(commitCount(ws)).toBe(0);
});

test("a missing parentPath is refused not_found (404)", async () => {
  const ws = mkWorkspace();
  const { app } = await mkApp(ws);
  const res = await post(app, { ...NEW_MISSION, parentPath: "nope/gone" }, "key-nf");
  expect(res.status).toBe(404);
  expect(((await res.json()) as { error: { code: string } }).error.code).toBe("not_found");
  expect(commitCount(ws)).toBe(0);
});

test("new_mission nests under an existing parent folder", async () => {
  const ws = mkWorkspace();
  const { app } = await mkApp(ws);
  // Create a parent mission, then a child under it.
  await post(
    app,
    { type: "new_mission", parentPath: "", title: "Growth", brief: "", kind: "initiative" },
    "k-parent",
  );
  const res = await post(
    app,
    { type: "new_mission", parentPath: "growth", title: "SEO refresh", brief: "", kind: "project" },
    "k-child",
  );
  expect(res.status).toBe(202);
  expect(existsSync(join(ws, "growth", "seo-refresh", "_work.md"))).toBe(true);
  expect(commitCount(ws)).toBe(2);
});

// ── kill intent (F8 / BRO-1801) ───────────────────────────────────────────────

test("kill intent → 202 and invokes the kill seam with the session id", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const killed: string[] = [];
  const app = createApp(cfg(ws), Date.now(), handle.db, undefined, (sid) => {
    killed.push(sid);
    return true;
  });
  const res = await app.request("/api/intents", {
    method: "POST",
    headers: { "Content-Type": "application/json", "Idempotency-Key": "k-kill-1" },
    body: JSON.stringify({ type: "kill", sessionId: "r1" }),
  });
  expect(res.status).toBe(202);
  expect(killed).toEqual(["r1"]); // the run.killed reaches the client on the stream, not this body
  handle.client.close();
});

test("kill intent for a run with no live process → not_found 404 (lease released)", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db, undefined, () => false);
  const res = await app.request("/api/intents", {
    method: "POST",
    headers: { "Content-Type": "application/json", "Idempotency-Key": "k-kill-2" },
    body: JSON.stringify({ type: "kill", sessionId: "ghost" }),
  });
  expect(res.status).toBe(404);
  expect(((await res.json()) as { error: { code: string } }).error.code).toBe("not_found");
  handle.client.close();
});

test("kill intent without a wired supervisor → unsupported_intent 501", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db); // no kill seam
  const res = await app.request("/api/intents", {
    method: "POST",
    headers: { "Content-Type": "application/json", "Idempotency-Key": "k-kill-3" },
    body: JSON.stringify({ type: "kill", sessionId: "r1" }),
  });
  expect(res.status).toBe(501);
  handle.client.close();
});

test("kill intent missing sessionId → invalid_intent 400", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db, undefined, () => true);
  const res = await app.request("/api/intents", {
    method: "POST",
    headers: { "Content-Type": "application/json", "Idempotency-Key": "k-kill-4" },
    body: JSON.stringify({ type: "kill" }),
  });
  expect(res.status).toBe(400);
  handle.client.close();
});

test("kill intent is idempotent per key — a same-key retry is a no-op, the seam fires once", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const killed: string[] = [];
  const app = createApp(cfg(ws), Date.now(), handle.db, undefined, (sid) => {
    killed.push(sid);
    return true;
  });
  const headers = { "Content-Type": "application/json", "Idempotency-Key": "k-kill-idem" };
  const body = JSON.stringify({ type: "kill", sessionId: "r1" });
  const r1 = await app.request("/api/intents", { method: "POST", headers, body });
  const r2 = await app.request("/api/intents", { method: "POST", headers, body });
  expect(r1.status).toBe(202);
  expect(r2.status).toBe(202);
  expect(killed).toEqual(["r1"]); // the second (same-key) POST did NOT re-invoke the kill seam
  handle.client.close();
});

test("a kill 404 (no live run) releases the lease so a same-key retry once live succeeds", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  let live = false;
  const killed: string[] = [];
  const app = createApp(cfg(ws), Date.now(), handle.db, undefined, (sid) => {
    if (!live) return false;
    killed.push(sid);
    return true;
  });
  const headers = { "Content-Type": "application/json", "Idempotency-Key": "k-kill-retry" };
  const body = JSON.stringify({ type: "kill", sessionId: "r1" });
  const first = await app.request("/api/intents", { method: "POST", headers, body });
  expect(first.status).toBe(404); // no live run → lease released
  live = true;
  const second = await app.request("/api/intents", { method: "POST", headers, body });
  expect(second.status).toBe(202); // the released lease lets the SAME key re-attempt
  expect(killed).toEqual(["r1"]);
  handle.client.close();
});

test("a kill seam that throws → intent_failed 500 and the lease is released (retryable)", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  let attempts = 0;
  const app = createApp(cfg(ws), Date.now(), handle.db, undefined, () => {
    attempts++;
    if (attempts === 1) throw new Error("SIGKILL on an already-exited pid");
    return true;
  });
  const headers = { "Content-Type": "application/json", "Idempotency-Key": "k-kill-throw" };
  const body = JSON.stringify({ type: "kill", sessionId: "r1" });
  const first = await app.request("/api/intents", { method: "POST", headers, body });
  expect(first.status).toBe(500);
  expect(((await first.json()) as { error: { code: string } }).error.code).toBe("intent_failed");
  // the lease was released → a same-key retry re-attempts (now succeeds)
  const second = await app.request("/api/intents", { method: "POST", headers, body });
  expect(second.status).toBe(202);
  handle.client.close();
});

// ── F5 gate verdicts (BRO-1805 slice 2a — block) ─────────────────────────────
// The write path that decides a review node's open gate. Slice 2a wires `block` (→ canceled); the
// gate-decision spine (`decideGateVerdict`) is shared by the other three verdicts in later sub-slices.

/** Seed a node@review + its session + an OPEN completion gate (verdict null) — the F5 decision setup. */
async function seedOpenGate(
  db: IndexDb,
  ids: { nodeId: string; sessionId: string; gateId: string } = {
    nodeId: "n1",
    sessionId: "r1",
    gateId: "g1",
  },
): Promise<{ nodeId: string; sessionId: string; gateId: string }> {
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
  return ids;
}

const jsonErr = async (r: Response): Promise<string> =>
  ((await r.json()) as { error: { code: string } }).error.code;

test("BRO-1805 slice 2a: block { gateId } decides the gate + cancels the node (gate.decided + node.updated)", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { nodeId, sessionId, gateId } = await seedOpenGate(handle.db);

  const res = await post(app, { type: "block", gateId, reason: "not this way" }, "k-block-1");
  expect(res.status).toBe(202);
  expect(await res.json()).toEqual({ accepted: true });

  // the gate is decided `block`, by the human, with a timestamp (verdict was pending → now set)
  const [g] = await handle.db.select().from(gate).where(eq(gate.id, gateId));
  expect(g?.verdict).toBe("block");
  expect(g?.decidedBy).toBe("human");
  expect(g?.decidedAt ?? 0).toBeGreaterThan(0);

  // the node transitioned review → canceled (D-GATE)
  const [n] = await handle.db.select().from(node).where(eq(node.id, nodeId));
  expect(n?.state).toBe("canceled");

  // gate.decided journaled session-scoped, carrying the widened payload (BRO-1915's rebuild projector)
  const decided = await handle.db.select().from(event).where(eq(event.type, "gate.decided"));
  expect(decided).toHaveLength(1);
  expect(decided[0]?.sessionId).toBe(sessionId);
  const dp = JSON.parse(decided[0]?.payload ?? "{}") as {
    gateId?: string;
    verdict?: string;
    kind?: string;
    decidedBy?: string;
    reason?: string;
  };
  expect(dp.gateId).toBe(gateId);
  expect(dp.verdict).toBe("block");
  expect(dp.kind).toBe("completion");
  expect(dp.decidedBy).toBe("human");
  expect(dp.reason).toBe("not this way");

  // node.updated on the GLOBAL stream (sessionId null) carries the canceled projection (live board)
  const updated = await handle.db.select().from(event).where(eq(event.type, "node.updated"));
  expect(updated).toHaveLength(1);
  expect(updated[0]?.sessionId).toBeNull();
  expect(JSON.parse(updated[0]?.payload ?? "{}").state).toBe("canceled");
  handle.client.close();
});

test("BRO-1805 slice 2a: a same-key block retry is a no-op (one gate.decided, node stays canceled once)", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { gateId } = await seedOpenGate(handle.db);

  const r1 = await post(app, { type: "block", gateId }, "k-block-idem");
  const r2 = await post(app, { type: "block", gateId }, "k-block-idem");
  expect(r1.status).toBe(202);
  expect(r2.status).toBe(202);
  // the second (same-key) POST is guarded by the idempotency lease → NOT re-decided
  const decided = await handle.db.select().from(event).where(eq(event.type, "gate.decided"));
  expect(decided).toHaveLength(1);
  handle.client.close();
});

test("BRO-1805 slice 2a: a DIFFERENT-key block on an already-blocked gate is an idempotent no-op", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { gateId } = await seedOpenGate(handle.db);

  const r1 = await post(app, { type: "block", gateId }, "k-block-a");
  const r2 = await post(app, { type: "block", gateId }, "k-block-b"); // new key, gate already block
  expect(r1.status).toBe(202);
  expect(r2.status).toBe(202);
  // the spine's same-verdict short-circuit fires (past the lease) → still exactly ONE decide
  const decided = await handle.db.select().from(event).where(eq(event.type, "gate.decided"));
  expect(decided).toHaveLength(1);
  handle.client.close();
});

test("BRO-1805 slice 2a: block refuses — missing gateId (400), unknown gate (404), decided-differently (409)", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);

  const bad = await post(app, { type: "block" }, "k-b-missing");
  expect(bad.status).toBe(400);
  expect(await jsonErr(bad)).toBe("invalid_intent");

  const missing = await post(app, { type: "block", gateId: "nope" }, "k-b-unknown");
  expect(missing.status).toBe(404);
  expect(await jsonErr(missing)).toBe("not_found");

  // a non-string reason is rejected before any lookup
  const badReason = await post(app, { type: "block", gateId: "g", reason: 42 }, "k-b-reason");
  expect(badReason.status).toBe(400);
  expect(await jsonErr(badReason)).toBe("invalid_intent");

  // a gate already decided with a DIFFERENT verdict → 409 (never silently re-decided)
  const { gateId } = await seedOpenGate(handle.db, { nodeId: "n2", sessionId: "r2", gateId: "g2" });
  await handle.db
    .update(gate)
    .set({ verdict: "approve", decidedBy: "human", decidedAt: Date.now() })
    .where(eq(gate.id, gateId));
  const conflict = await post(app, { type: "block", gateId }, "k-b-conflict");
  expect(conflict.status).toBe(409);
  expect(await jsonErr(conflict)).toBe("invalid_intent");
  // the conflicting POST did NOT overwrite the prior approve verdict
  const [g] = await handle.db.select().from(gate).where(eq(gate.id, gateId));
  expect(g?.verdict).toBe("approve");
  handle.client.close();
});

test("BRO-1805 slice 2a: concurrent different-key blocks decide each gate exactly ONCE (atomic verdict CAS)", async () => {
  // Two DIFFERENT-key blocks on the SAME gate race across TWO connections to one file db. The lease only
  // dedupes SAME-key, so both pass the read-side pending check; the atomic `UPDATE ... WHERE verdict IS
  // NULL` is what makes exactly one win. Looped (the BRO-1814/1912 race-harness discipline — a single shot
  // may not interleave). Under the fix: exactly one gate.decided + one node.updated per gate, every round.
  const ws = mkWorkspace(false);
  const dbPath = join(ws, "race.db");
  const h1 = await openIndex(`file:${dbPath}`);
  const h2 = await openIndex(`file:${dbPath}`);
  // busy_timeout so the two connections WAIT on each other's write lock instead of erroring SQLITE_BUSY
  // under parallel-suite contention — the CAS still elects the winner; this only removes the lock-contention
  // flake (the race being tested is the read/CAS interleave, not lock acquisition).
  await h1.client.execute("PRAGMA busy_timeout = 5000");
  await h2.client.execute("PRAGMA busy_timeout = 5000");
  const app1 = createApp(cfg(ws), Date.now(), h1.db);
  const app2 = createApp(cfg(ws), Date.now(), h2.db);

  const ROUNDS = 30;
  for (let i = 0; i < ROUNDS; i++) {
    const ids = { nodeId: `n${i}`, sessionId: `r${i}`, gateId: `g${i}` };
    await seedOpenGate(h1.db, ids);
    await Promise.all([
      post(app1, { type: "block", gateId: ids.gateId }, `race-${i}-a`),
      post(app2, { type: "block", gateId: ids.gateId }, `race-${i}-b`),
    ]);
    // exactly ONE decide for this gate despite the concurrent pair, and (cumulatively) exactly one
    // node.updated per round — the conditional node transition means the race loser emits nothing.
    const decided = await h1.db
      .select()
      .from(event)
      .where(and(eq(event.type, "gate.decided"), eq(event.sessionId, ids.sessionId)));
    expect(decided).toHaveLength(1);
    const updated = await h1.db.select().from(event).where(eq(event.type, "node.updated"));
    expect(updated).toHaveLength(i + 1);
    const [gRow] = await h1.db.select().from(gate).where(eq(gate.id, ids.gateId));
    expect(gRow?.verdict).toBe("block");
    const [nRow] = await h1.db.select().from(node).where(eq(node.id, ids.nodeId));
    expect(nRow?.state).toBe("canceled");
  }
  h1.client.close();
  h2.client.close();
});

test("BRO-1805 slice 2a: block on a gate whose SESSION is tombstoned refuses (404) and cancels nothing", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { nodeId, sessionId, gateId } = await seedOpenGate(handle.db);
  // a superseded/old run: tombstone the session that owns the gate. A newer session may now hold n1's
  // review, so the stale gate must NOT decide it (Codex finding — the wrong-node cancel).
  await handle.db.update(session).set({ deletedAt: Date.now() }).where(eq(session.id, sessionId));

  const res = await post(app, { type: "block", gateId }, "k-tomb");
  expect(res.status).toBe(404);
  expect(await jsonErr(res)).toBe("not_found");
  // the node was NOT canceled, and nothing was journaled
  const [n] = await handle.db.select().from(node).where(eq(node.id, nodeId));
  expect(n?.state).toBe("review");
  const decided = await handle.db.select().from(event).where(eq(event.type, "gate.decided"));
  expect(decided).toHaveLength(0);
  handle.client.close();
});

test("BRO-1805 slice 2a: block on a gate superseded by a NEWER review session refuses (409), cancels nothing", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { nodeId, gateId } = await seedOpenGate(handle.db); // gate on session r1
  // a NEWER live session re-reviews the same node (the BRO-1914 rescan-revert epoch) — later startedAt.
  await handle.db.insert(session).values({
    id: "r-new",
    nodeId,
    branch: "run/r-new",
    status: "review",
    startedAt: Date.now() + 10_000,
    updatedAt: Date.now(),
  });

  const res = await post(app, { type: "block", gateId }, "k-superseded");
  expect(res.status).toBe(409);
  expect(await jsonErr(res)).toBe("invalid_intent");
  // the OLD gate did NOT cancel the review the newer session now owns
  const [n] = await handle.db.select().from(node).where(eq(node.id, nodeId));
  expect(n?.state).toBe("review");
  handle.client.close();
});

test("BRO-1805 slice 2a: a decided gate stranded on a still-review node (partial write) is repaired by a retry", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { nodeId, gateId } = await seedOpenGate(handle.db);
  // Simulate a partial write: the gate is decided `block`, but the node transition never landed (a
  // crash/SQLITE_FULL between the CAS and the node update). The node is stranded at `review`.
  await handle.db
    .update(gate)
    .set({ verdict: "block", decidedBy: "human", decidedAt: Date.now() })
    .where(eq(gate.id, gateId));

  const res = await post(app, { type: "block", gateId }, "k-repair");
  expect(res.status).toBe(202);
  // the idempotent completion REPAIRS the transition (→ canceled) rather than stranding the decided gate
  const [n] = await handle.db.select().from(node).where(eq(node.id, nodeId));
  expect(n?.state).toBe("canceled");
  // and emits exactly one node.updated for the repair
  const updated = await handle.db.select().from(event).where(eq(event.type, "node.updated"));
  expect(updated).toHaveLength(1);
  handle.client.close();
});

test("BRO-1805 slice 2a: a block FS-journals gate.decided to the run journal (durable, replayable)", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { sessionId, gateId } = await seedOpenGate(handle.db);

  const res = await post(app, { type: "block", gateId, reason: "durable" }, "k-journal");
  expect(res.status).toBe(202);
  // the decision is durably journaled FS-first to <ws>/runs/run-<sessionId>/session.jsonl — symmetric with
  // slice-1's gate.opened, so it survives beyond the index row (BRO-1915 replay can reconstruct the gate).
  const journalPath = join(ws, "runs", `run-${sessionId}`, "session.jsonl");
  expect(existsSync(journalPath)).toBe(true);
  const lines = readFileSync(journalPath, "utf8")
    .trim()
    .split("\n")
    .map((l) => JSON.parse(l) as { type?: string; verdict?: string; gateId?: string });
  const decided = lines.filter((l) => l.type === "gate.decided");
  expect(decided).toHaveLength(1);
  expect(decided[0]?.verdict).toBe("block");
  expect(decided[0]?.gateId).toBe(gateId);
  handle.client.close();
});

// ── BRO-1805 slice 2b-i: revise (→ triggered, feedback) + escalate (→ owner reassigned, re-decidable) ──────

test("BRO-1805 slice 2b-i: revise { gateId, feedback } decides `revise` + triggers the node + carries feedback", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { nodeId, sessionId, gateId } = await seedOpenGate(handle.db);

  const res = await post(
    app,
    { type: "revise", gateId, feedback: "tighten the error path" },
    "k-rev-1",
  );
  expect(res.status).toBe(202);
  expect(await res.json()).toEqual({ accepted: true });

  // the gate is decided `revise` (terminating), by the human
  const [g] = await handle.db.select().from(gate).where(eq(gate.id, gateId));
  expect(g?.verdict).toBe("revise");
  expect(g?.decidedBy).toBe("human");

  // the node transitioned review → triggered (send back for a fresh dispatch)
  const [n] = await handle.db.select().from(node).where(eq(node.id, nodeId));
  expect(n?.state).toBe("triggered");

  // gate.decided carries the FEEDBACK (the send-back note the redispatched run picks up), session-scoped
  const decided = await handle.db.select().from(event).where(eq(event.type, "gate.decided"));
  expect(decided).toHaveLength(1);
  expect(decided[0]?.sessionId).toBe(sessionId);
  const dp = JSON.parse(decided[0]?.payload ?? "{}") as { verdict?: string; feedback?: string };
  expect(dp.verdict).toBe("revise");
  expect(dp.feedback).toBe("tighten the error path");

  // node.updated on the global stream carries the triggered projection
  const updated = await handle.db.select().from(event).where(eq(event.type, "node.updated"));
  expect(updated).toHaveLength(1);
  expect(updated[0]?.sessionId).toBeNull();
  expect(JSON.parse(updated[0]?.payload ?? "{}").state).toBe("triggered");
  handle.client.close();
});

test("BRO-1805 slice 2b-i: a same-key revise retry is a no-op (one gate.decided)", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { gateId } = await seedOpenGate(handle.db);

  const r1 = await post(app, { type: "revise", gateId, feedback: "again" }, "k-rev-idem");
  const r2 = await post(app, { type: "revise", gateId, feedback: "again" }, "k-rev-idem");
  expect(r1.status).toBe(202);
  expect(r2.status).toBe(202);
  const decided = await handle.db.select().from(event).where(eq(event.type, "gate.decided"));
  expect(decided).toHaveLength(1);
  handle.client.close();
});

test("BRO-1805 slice 2b-i: revise refuses a missing/empty feedback (400) + unknown gate (404) + conflicting verdict (409)", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { gateId } = await seedOpenGate(handle.db);

  // missing feedback
  const noFb = await post(app, { type: "revise", gateId }, "k-rev-nofb");
  expect(noFb.status).toBe(400);
  expect(await jsonErr(noFb)).toBe("invalid_intent");
  // empty/whitespace feedback
  const emptyFb = await post(app, { type: "revise", gateId, feedback: "   " }, "k-rev-emptyfb");
  expect(emptyFb.status).toBe(400);
  // non-string feedback
  const badFb = await post(app, { type: "revise", gateId, feedback: 7 }, "k-rev-badfb");
  expect(badFb.status).toBe(400);
  // missing gateId
  const noGate = await post(app, { type: "revise", feedback: "x" }, "k-rev-nogate");
  expect(noGate.status).toBe(400);
  // unknown gate
  const unknown = await post(
    app,
    { type: "revise", gateId: "nope", feedback: "x" },
    "k-rev-unknown",
  );
  expect(unknown.status).toBe(404);
  expect(await jsonErr(unknown)).toBe("not_found");
  // gate already decided differently (block) → 409 on a revise
  await post(app, { type: "block", gateId }, "k-rev-block-first");
  const conflict = await post(app, { type: "revise", gateId, feedback: "x" }, "k-rev-conflict");
  expect(conflict.status).toBe(409);
  expect(await jsonErr(conflict)).toBe("invalid_intent");
  handle.client.close();
});

test("BRO-1805 slice 2b-i: escalate { gateId, to } reassigns owner, node STAYS review, gate STAYS open", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { nodeId, sessionId, gateId } = await seedOpenGate(handle.db);

  const res = await post(app, { type: "escalate", gateId, to: "alice" }, "k-esc-1");
  expect(res.status).toBe(202);

  // owner reassigned; node still at the gate (review); gate NOT decided (verdict null — re-decidable §4)
  const [n] = await handle.db.select().from(node).where(eq(node.id, nodeId));
  expect(n?.owner).toBe("alice");
  expect(n?.state).toBe("review");
  const [g] = await handle.db.select().from(gate).where(eq(gate.id, gateId));
  expect(g?.verdict).toBeNull();

  // NO gate.decided (escalate does not decide); a gate.escalated is journaled session-scoped instead
  const decided = await handle.db.select().from(event).where(eq(event.type, "gate.decided"));
  expect(decided).toHaveLength(0);
  const escalated = await handle.db.select().from(event).where(eq(event.type, "gate.escalated"));
  expect(escalated).toHaveLength(1);
  expect(escalated[0]?.sessionId).toBe(sessionId);
  const ep = JSON.parse(escalated[0]?.payload ?? "{}") as { to?: string; escalatedBy?: string };
  expect(ep.to).toBe("alice");
  expect(ep.escalatedBy).toBe("human");

  // node.updated on the global stream carries the new owner, still review
  const updated = await handle.db.select().from(event).where(eq(event.type, "node.updated"));
  expect(updated).toHaveLength(1);
  const up = JSON.parse(updated[0]?.payload ?? "{}");
  expect(up.owner).toBe("alice");
  expect(up.state).toBe("review");
  handle.client.close();
});

test("BRO-1805 slice 2b-i: an escalated gate is RE-DECIDABLE — a later block still cancels it", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { nodeId, gateId } = await seedOpenGate(handle.db);

  const esc = await post(app, { type: "escalate", gateId, to: "bob" }, "k-esc-then-block-1");
  expect(esc.status).toBe(202);
  // the gate stayed open, so a block afterward still decides + cancels (escalate did NOT terminate it)
  const blk = await post(
    app,
    { type: "block", gateId, reason: "changed my mind" },
    "k-esc-then-block-2",
  );
  expect(blk.status).toBe(202);
  const [g] = await handle.db.select().from(gate).where(eq(gate.id, gateId));
  expect(g?.verdict).toBe("block");
  const [n] = await handle.db.select().from(node).where(eq(node.id, nodeId));
  expect(n?.state).toBe("canceled");
  handle.client.close();
});

test("BRO-1805 slice 2b-i: a re-escalate to the SAME owner is an idempotent no-op; a DIFFERENT owner reassigns", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { nodeId, gateId } = await seedOpenGate(handle.db);

  await post(app, { type: "escalate", gateId, to: "alice" }, "k-esc-same-1");
  await post(app, { type: "escalate", gateId, to: "alice" }, "k-esc-same-2"); // DIFFERENT key, SAME owner
  // same-owner re-escalate changes nothing → still exactly ONE gate.escalated + ONE node.updated
  let escalated = await handle.db.select().from(event).where(eq(event.type, "gate.escalated"));
  expect(escalated).toHaveLength(1);
  let updated = await handle.db.select().from(event).where(eq(event.type, "node.updated"));
  expect(updated).toHaveLength(1);

  // a different owner DOES reassign → a second gate.escalated + node.updated
  await post(app, { type: "escalate", gateId, to: "carol" }, "k-esc-diff");
  const [n] = await handle.db.select().from(node).where(eq(node.id, nodeId));
  expect(n?.owner).toBe("carol");
  escalated = await handle.db.select().from(event).where(eq(event.type, "gate.escalated"));
  expect(escalated).toHaveLength(2);
  updated = await handle.db.select().from(event).where(eq(event.type, "node.updated"));
  expect(updated).toHaveLength(2);
  handle.client.close();
});

test("BRO-1805 slice 2b-i: escalate refuses missing `to` (400), unknown gate (404), an already-decided gate (409)", async () => {
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { gateId } = await seedOpenGate(handle.db);

  const noTo = await post(app, { type: "escalate", gateId }, "k-esc-noto");
  expect(noTo.status).toBe(400);
  expect(await jsonErr(noTo)).toBe("invalid_intent");
  const emptyTo = await post(app, { type: "escalate", gateId, to: "  " }, "k-esc-emptyto");
  expect(emptyTo.status).toBe(400);
  const unknown = await post(
    app,
    { type: "escalate", gateId: "nope", to: "alice" },
    "k-esc-unknown",
  );
  expect(unknown.status).toBe(404);
  // a decided gate cannot be escalated
  await post(app, { type: "block", gateId }, "k-esc-block-first");
  const decided = await post(app, { type: "escalate", gateId, to: "alice" }, "k-esc-on-decided");
  expect(decided.status).toBe(409);
  expect(await jsonErr(decided)).toBe("invalid_intent");
  handle.client.close();
});

test("BRO-1805 slice 2b-i: escalate refuses (409, no phantom) when the gate is open but the node left review", async () => {
  // The REACHABLE sibling of the concurrent-decide race: a BRO-1914 rescan-revert can leave an OPEN gate
  // (verdict null) on a node that reverted OUT of `review`. escalate must refuse — reassigning the owner of a
  // node no longer at the gate is meaningless, and a phantom 202 would hide it. (The same protection for the
  // intra-request timing — node decided between the resolveGateChain read and the owner-CAS — is the re-read.)
  const ws = mkWorkspace(false);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { nodeId, gateId } = await seedOpenGate(handle.db);
  // the node reverted to `triggered` while the gate row stays open (verdict still null) — the degraded state
  await handle.db.update(node).set({ state: "triggered" }).where(eq(node.id, nodeId));

  const res = await post(app, { type: "escalate", gateId, to: "alice" }, "k-esc-nonreview");
  expect(res.status).toBe(409);
  expect(await jsonErr(res)).toBe("invalid_intent");
  // owner untouched, node stays triggered, NO gate.escalated (no phantom success)
  const [n] = await handle.db.select().from(node).where(eq(node.id, nodeId));
  expect(n?.owner).toBeNull();
  expect(n?.state).toBe("triggered");
  const escalated = await handle.db.select().from(event).where(eq(event.type, "gate.escalated"));
  expect(escalated).toHaveLength(0);
  handle.client.close();
});

test("BRO-1805 slice 2b-i: under concurrent escalate + block, escalate never phantom-202s", async () => {
  // MAJOR (Codex Strata-A): an escalate that reads an open `review` gate then loses the owner-CAS to a
  // concurrent block (node → canceled) must NOT return a phantom 202 — a zero-row owner-CAS is AMBIGUOUS
  // (same-owner no-op vs the node left review). The re-read disambiguates it to 409 when the node was decided
  // out from under the escalate. Race harness (two connections, many rounds — a single shot may not interleave
  // the read→CAS window; the BRO-1814 discipline). Invariant, EVERY ordering: escalate 202 ⟹ exactly one
  // gate.escalated for its session; escalate 409 ⟹ the gate was decided (node canceled), never a phantom.
  const ws = mkWorkspace(false);
  const dbPath = join(ws, "esc-race.db");
  const h1 = await openIndex(`file:${dbPath}`);
  const h2 = await openIndex(`file:${dbPath}`);
  await h1.client.execute("PRAGMA busy_timeout = 5000");
  await h2.client.execute("PRAGMA busy_timeout = 5000");
  const app1 = createApp(cfg(ws), Date.now(), h1.db);
  const app2 = createApp(cfg(ws), Date.now(), h2.db);

  const ROUNDS = 40;
  for (let i = 0; i < ROUNDS; i++) {
    const ids = { nodeId: `n${i}`, sessionId: `r${i}`, gateId: `g${i}` };
    await seedOpenGate(h1.db, ids);
    const [escRes] = await Promise.all([
      post(app1, { type: "escalate", gateId: ids.gateId, to: "alice" }, `esc-${i}`),
      post(app2, { type: "block", gateId: ids.gateId }, `blk-${i}`),
    ]);
    const escalated = await h1.db
      .select()
      .from(event)
      .where(and(eq(event.type, "gate.escalated"), eq(event.sessionId, ids.sessionId)));
    if (escRes.status === 202) {
      // escalate won the CAS before block canceled the node → it really applied (exactly one gate.escalated)
      expect(escalated).toHaveLength(1);
    } else {
      // escalate refused because block decided the gate first → 409, NO phantom escalation, node canceled
      expect(escRes.status).toBe(409);
      expect(escalated).toHaveLength(0);
      const [nRow] = await h1.db.select().from(node).where(eq(node.id, ids.nodeId));
      expect(nRow?.state).toBe("canceled");
    }
    // block always decides the open gate, whether escalate ran first or not
    const [gRow] = await h1.db.select().from(gate).where(eq(gate.id, ids.gateId));
    expect(gRow?.verdict).toBe("block");
  }
  h1.client.close();
  h2.client.close();
});

// ── BRO-1914: the COORDINATED writer — a gate verdict must durably write `_work.md`, or the
//    FS-authoritative reconcile resurrects the DECIDED node as a stranded `review`. ──────────────

/** Seed n1's `_work.md` on disk at `state`, committed clean — simulates park's durable review write.
 *  Also ignores the runtime's transient dirs (`/runs/`, `/.maestro/`) as a real workspace does, so the
 *  gate journal's `runs/` write doesn't show as a dirty tree (only TRACKED content gates cleanliness). */
function seedWorkFileAtReview(ws: string, nodeId: string): void {
  const wm = `---\nid: ${nodeId}\nkind: task\nstate: review\ngate: human\ncreated: 2026-06-25\nupdated: 2026-06-25\n---\n\n# ${nodeId}\n`;
  writeFileSync(join(ws, ".gitignore"), "/.maestro/\n/runs/\n");
  mkdirSync(join(ws, "work", nodeId), { recursive: true });
  writeFileSync(join(ws, "work", nodeId, "_work.md"), wm);
  gitOk(ws, ["add", "-A"]);
  gitOk(ws, ["commit", "-qm", "seed review"]);
}

test("BRO-1914: revise durably writes _work.md=triggered, so a live reconcile does NOT revert the decided node to review", async () => {
  const ws = mkWorkspace(true); // git workspace — the verdict's coordinated write commits _work.md
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { nodeId, gateId } = await seedOpenGate(handle.db); // DB node n1 @ review + open gate
  seedWorkFileAtReview(ws, nodeId); // FS _work.md @ review (as park left it, BRO-1914 park slice)

  const res = await post(
    app,
    { type: "revise", gateId, feedback: "try again" },
    "k-revise-durable",
  );
  expect(res.status).toBe(202);

  // DB advanced review → triggered AND the FS was advanced too (the coordinated write), tree stays clean.
  const [n] = await handle.db.select().from(node).where(eq(node.id, nodeId));
  expect(n?.state).toBe("triggered");
  expect(
    parseWorkFile(readFileSync(join(ws, "work", nodeId, "_work.md"), "utf8")).contract.state,
  ).toBe("triggered");
  expect(await gitIsClean(ws)).toBe(true);

  // THE REGRESSION GUARD: a live reconcile (the fs.watch / boot path) re-derives state from _work.md.
  // Without the coordinated write the FS would still say `review` → syncNodes would clobber the DB back to
  // `review` → a stranded, already-decided gate. With it, FS=triggered=DB → the decision STANDS.
  await scanIntoIndex(handle.db, ws, Date.now());
  const [after] = await handle.db.select().from(node).where(eq(node.id, nodeId));
  expect(after?.state).toBe("triggered"); // NOT resurrected to review — the MAJOR is closed
  handle.client.close();
});

test("BRO-1914: block durably writes _work.md=canceled, so a live reconcile keeps the node canceled", async () => {
  const ws = mkWorkspace(true);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { nodeId, gateId } = await seedOpenGate(handle.db);
  seedWorkFileAtReview(ws, nodeId);

  expect((await post(app, { type: "block", gateId }, "k-block-durable")).status).toBe(202);
  expect(
    parseWorkFile(readFileSync(join(ws, "work", nodeId, "_work.md"), "utf8")).contract.state,
  ).toBe("canceled");
  expect(await gitIsClean(ws)).toBe(true);

  await scanIntoIndex(handle.db, ws, Date.now());
  const [after] = await handle.db.select().from(node).where(eq(node.id, nodeId));
  expect(after?.state).toBe("canceled"); // reconcile keeps it — not reverted to review
  handle.client.close();
});

test("BRO-1914: escalate durably writes _work.md owner, so a reconcile keeps the reassignment (node stays review)", async () => {
  const ws = mkWorkspace(true);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { nodeId, gateId } = await seedOpenGate(handle.db);
  seedWorkFileAtReview(ws, nodeId);

  expect((await post(app, { type: "escalate", gateId, to: "@lead" }, "k-esc-durable")).status).toBe(
    202,
  );
  const onDisk = parseWorkFile(readFileSync(join(ws, "work", nodeId, "_work.md"), "utf8")).contract;
  expect(onDisk.owner).toBe("@lead");
  expect(onDisk.state).toBe("review"); // escalate is non-terminal: node STAYS at review
  expect(await gitIsClean(ws)).toBe(true);

  await scanIntoIndex(handle.db, ws, Date.now());
  const [after] = await handle.db.select().from(node).where(eq(node.id, nodeId));
  expect(after?.owner).toBe("@lead"); // reconcile keeps the reassignment
  expect(after?.state).toBe("review");
  handle.client.close();
});

// ── BRO-1805 slice 2b-ii: the approve verb → squash-merge (D1) → node done. The ONE verdict with an
//    irreversible side effect BEFORE it commits: CLAIM the gate, merge, KEEP the verdict only on a landed
//    merge (stale / refused reopen the gate — never a silent merge past a stale verdict). Real git repos. ──

/** Set up a real approvable run: n1's `_work.md` @ review (committed base), a `run/<sessionId>` branch with
 *  one committed change off that base, and a passing `runs/run-<id>/verdict.md` whose `base` is the workspace
 *  tip (rung 1). Returns the base sha. `runs/` is gitignored (seedWorkFileAtReview) so the receipt file and
 *  the gate journal don't dirty the tree. */
function seedApprovableRun(ws: string, nodeId: string, sessionId: string): string {
  seedWorkFileAtReview(ws, nodeId); // base commit: _work.md @ review + .gitignore(/runs/ /.maestro/)
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
  return base;
}

test("BRO-1805 slice 2b-ii: approve { gateId } squash-merges the run and moves the node review → done", async () => {
  const ws = mkWorkspace(true);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { nodeId, sessionId, gateId } = await seedOpenGate(handle.db);
  seedApprovableRun(ws, nodeId, sessionId);

  const res = await post(app, { type: "approve", gateId }, "k-approve-1");
  expect(res.status).toBe(202);
  expect(await res.json()).toEqual({ accepted: true });

  // verdict committed = approve (KEPT — the merge landed), decided by the human
  const [g] = await handle.db.select().from(gate).where(eq(gate.id, gateId));
  expect(g?.verdict).toBe("approve");
  expect(g?.decidedBy).toBe("human");
  expect(g?.decidedAt ?? 0).toBeGreaterThan(0);

  // node review → done, durable on BOTH sides (DB + FS), tree clean (coordinated write committed)
  const [n] = await handle.db.select().from(node).where(eq(node.id, nodeId));
  expect(n?.state).toBe("done");
  expect(
    parseWorkFile(readFileSync(join(ws, "work", nodeId, "_work.md"), "utf8")).contract.state,
  ).toBe("done");
  expect(await gitIsClean(ws)).toBe(true);

  // the run really merged: its file landed on the workspace branch; run/<id> archived (the branch is the receipt)
  expect(existsSync(join(ws, "feature.ts"))).toBe(true);
  const branches = gitOut(ws, ["branch", "--list"]);
  expect(branches).toContain("archive/run-r1");
  expect(branches).not.toContain("run/r1");

  // events: gate.approved carries the merge receipt (sha + freshness), gate.decided records the verdict
  const approved = await handle.db.select().from(event).where(eq(event.type, "gate.approved"));
  expect(approved).toHaveLength(1);
  const ap = JSON.parse(approved[0]?.payload ?? "{}") as {
    gateId?: string;
    sha?: string;
    freshness?: string;
  };
  expect(ap.gateId).toBe(gateId);
  expect(ap.sha).toMatch(/^[0-9a-f]{7,}$/);
  expect(ap.freshness).toBe("base_unmoved");
  const decided = await handle.db.select().from(event).where(eq(event.type, "gate.decided"));
  expect(JSON.parse(decided[0]?.payload ?? "{}").verdict).toBe("approve");

  // THE REGRESSION GUARD (BRO-1914): a live reconcile keeps the node done, not resurrected to review
  await scanIntoIndex(handle.db, ws, Date.now());
  const [after] = await handle.db.select().from(node).where(eq(node.id, nodeId));
  expect(after?.state).toBe("done");
  handle.client.close();
});

test("BRO-1805 slice 2b-ii: approve on a run stale vs the workspace tip is refused 409 and leaves the gate OPEN", async () => {
  const ws = mkWorkspace(true);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { nodeId, sessionId, gateId } = await seedOpenGate(handle.db);
  seedApprovableRun(ws, nodeId, sessionId);
  // advance the workspace tip touching the SAME file the run changed → rung 3 overlap → stale
  writeFileSync(join(ws, "feature.ts"), "export const conflicting = 2;\n");
  gitOk(ws, ["add", "-A"]);
  gitOk(ws, ["commit", "-qm", "workspace moved onto feature.ts"]);

  const res = await post(app, { type: "approve", gateId }, "k-approve-stale");
  expect(res.status).toBe(409);
  expect(await jsonErr(res)).toBe("invalid_intent");

  // THE INVARIANT: no silent merge past a stale verdict — the gate stays OPEN, re-decidable (the claim is RELEASED).
  const [g] = await handle.db.select().from(gate).where(eq(gate.id, gateId));
  expect(g?.verdict).toBeNull(); // MUTATION: drop releaseApproveClaim on the stale path → this reads "approve" → RED
  const [n] = await handle.db.select().from(node).where(eq(node.id, nodeId));
  expect(n?.state).toBe("review");
  expect(gitOut(ws, ["branch", "--list"])).toContain("run/r1"); // NOT archived — no merge happened
  handle.client.close();
});

test("BRO-1805 slice 2b-ii: approve with no passing verdict.md is refused 409, gate stays open", async () => {
  const ws = mkWorkspace(true);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { nodeId, gateId } = await seedOpenGate(handle.db);
  seedWorkFileAtReview(ws, nodeId); // node @ review but NO run branch / verdict.md

  const res = await post(app, { type: "approve", gateId }, "k-approve-noverdict");
  expect(res.status).toBe(409);
  expect(await jsonErr(res)).toBe("invalid_intent");
  const [g] = await handle.db.select().from(gate).where(eq(gate.id, gateId));
  expect(g?.verdict).toBeNull(); // refused before the claim → no verdict to release
  handle.client.close();
});

test("BRO-1805 slice 2b-ii: approve refuses (retryable) when the workspace tree is dirty, gate stays open", async () => {
  const ws = mkWorkspace(true);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { nodeId, sessionId, gateId } = await seedOpenGate(handle.db);
  seedApprovableRun(ws, nodeId, sessionId);
  writeFileSync(join(ws, "work", nodeId, "_work.md"), "dirty uncommitted edit\n"); // dirty a TRACKED file

  const res = await post(app, { type: "approve", gateId }, "k-approve-dirty");
  expect(res.status).toBe(503);
  const body = (await res.json()) as { error: { code: string; retryable: boolean } };
  expect(body.error.code).toBe("intent_failed");
  expect(body.error.retryable).toBe(true);
  // claim RELEASED on refuse → gate open, re-decidable after the tree is cleaned
  const [g] = await handle.db.select().from(gate).where(eq(gate.id, gateId));
  expect(g?.verdict).toBeNull();
  handle.client.close();
});

test("BRO-1805 slice 2b-ii: a same-key approve retry is a no-op (the run merges exactly once)", async () => {
  const ws = mkWorkspace(true);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { nodeId, sessionId, gateId } = await seedOpenGate(handle.db);
  seedApprovableRun(ws, nodeId, sessionId);
  const before = commitCount(ws);

  const a = await post(app, { type: "approve", gateId }, "k-approve-idem");
  const b = await post(app, { type: "approve", gateId }, "k-approve-idem");
  expect(a.status).toBe(202);
  expect(b.status).toBe(202);
  // merged exactly once: the squash commit + the state:done commit = +2, never +4
  expect(commitCount(ws) - before).toBe(2);
  const [n] = await handle.db.select().from(node).where(eq(node.id, nodeId));
  expect(n?.state).toBe("done");
  handle.client.close();
});

test("BRO-1805 slice 2b-ii: approve on an already-decided (block) gate is refused 409, no merge", async () => {
  const ws = mkWorkspace(true);
  const handle = await openIndex(":memory:");
  const app = createApp(cfg(ws), Date.now(), handle.db);
  const { nodeId, sessionId, gateId } = await seedOpenGate(handle.db);
  seedApprovableRun(ws, nodeId, sessionId);
  await post(app, { type: "block", gateId }, "k-pre-block"); // decide it block first (→ canceled)
  const before = commitCount(ws);

  const res = await post(app, { type: "approve", gateId }, "k-approve-afterblock");
  expect(res.status).toBe(409);
  expect(await jsonErr(res)).toBe("invalid_intent");
  expect(commitCount(ws)).toBe(before); // no merge — the run branch is untouched
  expect(gitOut(ws, ["branch", "--list"])).toContain("run/r1");
  handle.client.close();
});
