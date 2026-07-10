/// <reference types="bun" />
// sandbox.test.ts (BRO-1746 done.check `bun test apps/runtime --filter sandbox`).
// Runs the reusable interface-conformance suite against the phase-1 worktree adapter, then adds the
// phase-1-specific assertions the portable suite deliberately leaves out (the .maestro/ location, the
// branch-is-the-receipt semantics, idempotent respawn, run-id safety). Each test provisions a real
// temp git repo as the workspace and exercises real `git worktree` — no mocks (P11).

import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { git } from "../git/git";
import { registerSandboxConformance } from "./conformance";
import { createWorktreeSandboxFactory } from "./worktree";

async function exists(p: string): Promise<boolean> {
  try {
    await stat(p);
    return true;
  } catch {
    return false;
  }
}

/** A fresh temp git repo (with one commit — `git worktree add` needs a born HEAD). */
async function makeWorkspace(): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), "maestro-sandbox-"));
  await git(dir, ["init", "-q"]);
  await git(dir, ["config", "user.email", "t@t.co"]);
  await git(dir, ["config", "user.name", "t"]);
  await writeFile(join(dir, "_work.md"), "kind: project\n");
  await git(dir, ["add", "-A"]);
  await git(dir, ["commit", "-qm", "init"]);
  return dir;
}

// ── Per-test workspaces for the phase-1 suite (cleaned after each test) ──────────
const tmps: string[] = [];
afterEach(async () => {
  for (const d of tmps.splice(0)) await rm(d, { recursive: true, force: true });
});
async function trackedWorkspace(): Promise<string> {
  const d = await makeWorkspace();
  tmps.push(d);
  return d;
}

// ── The portable conformance suite against the worktree adapter ─────────────────
// Provisions its OWN workspace (not tracked in `tmps`, so the module afterEach never nukes it
// mid-suite) and cleans it in the suite's afterAll.
registerSandboxConformance("worktree", async () => {
  const ws = await makeWorkspace();
  return {
    factory: createWorktreeSandboxFactory({ workspace: ws }),
    cleanup: () => rm(ws, { recursive: true, force: true }),
  };
});

// ── Phase-1 (worktree) specifics ────────────────────────────────────────────────

describe("WorktreeSandbox (phase-1 specifics)", () => {
  test("provisions the worktree under .maestro/worktrees on branch run/<id>", async () => {
    const ws = await trackedWorkspace();
    const sb = await createWorktreeSandboxFactory({ workspace: ws }).create("7f3a");
    expect(sb.workdir).toBe(join(ws, ".maestro", "worktrees", "run-7f3a"));
    expect(sb.runDir).toBe(join(ws, "runs", "run-7f3a"));
    expect(sb.branch).toBe("run/7f3a");
    // exec runs INSIDE the worktree — its HEAD is the run branch
    const head = await sb.exec(["git", "rev-parse", "--abbrev-ref", "HEAD"]);
    expect(head.code).toBe(0);
    expect(head.stdout.trim()).toBe("run/7f3a");
  });

  test("the worktree checkout does NOT dirty the main tree (under gitignored .maestro/)", async () => {
    const ws = await trackedWorkspace();
    // the real repo gitignores /.maestro/ + /runs/; the test repo must too for this assertion
    await writeFile(join(ws, ".gitignore"), "/.maestro/\n/runs/\n");
    await git(ws, ["add", ".gitignore"]);
    await git(ws, ["commit", "-qm", "ignore runtime dirs"]);
    await createWorktreeSandboxFactory({ workspace: ws }).create("clean1");
    const status = await git(ws, ["status", "--porcelain"]);
    expect(status.stdout.trim()).toBe(""); // no worktree/receipt pollution
  });

  test("teardown(preserve=false) removes the worktree dir but KEEPS the branch + runDir (receipts)", async () => {
    const ws = await trackedWorkspace();
    const sb = await createWorktreeSandboxFactory({ workspace: ws }).create("aa");
    expect(await exists(sb.workdir)).toBe(true);
    await sb.teardown({ preserve: false });
    expect(await exists(sb.workdir)).toBe(false); // working dir freed
    const branch = await git(ws, ["branch", "--list", "run/aa"]);
    expect(branch.stdout).toContain("run/aa"); // the branch is the receipt — never deleted
    expect(await exists(sb.runDir)).toBe(true); // receipts dir survives
  });

  test("teardown(preserve=true) and the bare default both LEAVE the worktree in place", async () => {
    const ws = await trackedWorkspace();
    const f = createWorktreeSandboxFactory({ workspace: ws });
    const explicit = await f.create("bb");
    await explicit.teardown({ preserve: true });
    expect(await exists(explicit.workdir)).toBe(true);
    const bare = await f.create("cc");
    await bare.teardown(); // no args → preserve is the safe default
    expect(await exists(bare.workdir)).toBe(true);
  });

  test("respawn reuses the SAME worktree — in-progress work survives (idempotent create)", async () => {
    const ws = await trackedWorkspace();
    const f = createWorktreeSandboxFactory({ workspace: ws });
    const first = await f.create("dd");
    await writeFile(join(first.workdir, "in-progress.txt"), "half-done"); // the agent's partial work
    const respawn = await f.create("dd"); // fresh-context respawn, same run id
    expect(respawn.workdir).toBe(first.workdir);
    expect(respawn.branch).toBe(first.branch);
    expect(await exists(join(respawn.workdir, "in-progress.txt"))).toBe(true); // not re-created
  });

  test("rejects an unsafe run id (path traversal / branch forgery)", async () => {
    const ws = await trackedWorkspace();
    const f = createWorktreeSandboxFactory({ workspace: ws });
    await expect(f.create("../evil")).rejects.toThrow();
    await expect(f.create("a/b")).rejects.toThrow();
    await expect(f.create("")).rejects.toThrow();
    await expect(f.create(".")).rejects.toThrow();
  });

  test("spawnContext (phase-1) has an empty commandPrefix + cwd = workdir", async () => {
    const ws = await trackedWorkspace();
    const sb = await createWorktreeSandboxFactory({ workspace: ws }).create("ee");
    const ctx = sb.spawnContext();
    expect(ctx.cwd).toBe(sb.workdir);
    expect(ctx.commandPrefix).toEqual([]);
    expect(ctx.env).toEqual({});
  });

  test("honors overridden worktreesRoot + runsRoot", async () => {
    const ws = await trackedWorkspace();
    const worktreesRoot = join(ws, "custom-wt");
    const runsRoot = join(ws, "custom-runs");
    const sb = await createWorktreeSandboxFactory({
      workspace: ws,
      worktreesRoot,
      runsRoot,
    }).create("ff");
    expect(sb.workdir).toBe(join(worktreesRoot, "run-ff"));
    expect(sb.runDir).toBe(join(runsRoot, "run-ff"));
    expect(await exists(sb.runDir)).toBe(true);
  });
});
