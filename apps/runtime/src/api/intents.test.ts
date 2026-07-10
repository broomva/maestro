/// <reference types="bun" />
// intents.test.ts — the write surface (BRO-1820, `bun test apps/runtime --filter intents`).
//
// done.check, three halves:
//  1. new_mission (F1) creates folder + `_work.md` + git commit ATOMICALLY (the commit is
//     the transaction), writing MINIMAL frontmatter (no gate — the scanner inherits it).
//  2. a retried POST with the SAME Idempotency-Key is a NO-OP (one commit, not two).
//  3. a FAILING new_mission leaves the workspace exactly as it was (nothing half-created).
// Plus the refusal contract: missing key / unknown type / malformed body / path traversal.

import { afterAll, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { parseWorkFile } from "@maestro/protocol";
import { createApp } from "../app";
import { DEFAULT_PORT, type RuntimeConfig } from "../config";
import { openIndex } from "../db/client";

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
  // MINIMAL: no gate/owner/budget written — the scanner resolves those from the parent.
  expect(content).not.toContain("gate:");
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
