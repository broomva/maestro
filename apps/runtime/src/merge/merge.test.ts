/// <reference types="bun" />
// merge.test.ts (BRO-1802, D1) — the approve = squash-merge freshness ladder, over REAL temp git repos
// (the P11-faithful way: real worktrees, real commits, real merges — no git mock). Proves the three rungs
// (base unmoved / no overlap / overlap→stale), the D1 commit trailers, the branch→archive rename, the
// worktree removal, and every refusal precondition (not-pass / dirty tree / empty run).

import { afterEach, describe, expect, test } from "bun:test";
import { existsSync, writeFileSync } from "node:fs";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { VerdictReceipt } from "@maestro/protocol";
import { git } from "../git/git";
import { approveMerge, rebaseFixPlanItem } from "./merge";

const tmps: string[] = [];
afterEach(async () => {
  for (const d of tmps.splice(0)) await rm(d, { recursive: true, force: true });
});

/** A temp workspace repo with one commit (the base). */
async function makeRepo(): Promise<{ dir: string; base: string }> {
  const dir = await realpath(await mkdtemp(join(tmpdir(), "maestro-merge-")));
  tmps.push(dir);
  await git(dir, ["init", "-q"]);
  await git(dir, ["config", "user.email", "t@t.co"]);
  await git(dir, ["config", "user.name", "t"]);
  writeFileSync(join(dir, "README.md"), "base\n");
  await git(dir, ["add", "-A"]);
  await git(dir, ["commit", "-qm", "init"]);
  return { dir, base: (await git(dir, ["rev-parse", "HEAD"])).stdout.trim() };
}

/** Create `run/<runId>` off `base`, commit `files` on it in a worktree. `keepWorktree` leaves the worktree
 *  registered (to exercise removal); otherwise it is freed here so the run branch is not checked out. */
async function seedRun(
  dir: string,
  runId: string,
  base: string,
  files: Record<string, string>,
  keepWorktree = false,
): Promise<void> {
  const runBranch = `run/${runId}`;
  await git(dir, ["branch", runBranch, base]);
  const wt = await mkdtemp(join(tmpdir(), `maestro-wt-${runId}-`));
  tmps.push(wt);
  await git(dir, ["worktree", "add", "-q", wt, runBranch]);
  for (const [f, content] of Object.entries(files)) writeFileSync(join(wt, f), content);
  await git(wt, ["add", "-A"]);
  await git(wt, ["commit", "-qm", `run ${runId}`]);
  if (!keepWorktree) await git(dir, ["worktree", "remove", "--force", wt]);
}

/** Advance the workspace branch tip with a commit touching `files` (moves the base off the judged one). */
async function advanceMain(dir: string, files: Record<string, string>): Promise<void> {
  for (const [f, content] of Object.entries(files)) writeFileSync(join(dir, f), content);
  await git(dir, ["add", "-A"]);
  await git(dir, ["commit", "-qm", "workspace change"]);
}

function passVerdict(base: string, attempt = 1): VerdictReceipt {
  return {
    verdict: "pass",
    attempt,
    base,
    diffstat: { files: 1, plus: 1, minus: 0 },
    tampering: [],
    checks: [],
    judge: { score: 1 },
  };
}

async function commitBody(dir: string, ref: string): Promise<string> {
  return (await git(dir, ["show", "-s", "--format=%B", ref])).stdout;
}

describe("approve = squash-merge (D1 freshness ladder)", () => {
  test("rung 1 — base unmoved → squash-merge with D1 trailers, branch archived", async () => {
    const { dir, base } = await makeRepo();
    await seedRun(dir, "r1", base, { "feature.ts": "export const A = 1;\n" });

    const out = await approveMerge({
      cwd: dir,
      runId: "r1",
      nodeId: "n1",
      nodeTitle: "add the greeting feature",
      verdict: passVerdict(base, 2),
    });

    expect(out.kind).toBe("merged");
    if (out.kind !== "merged") throw new Error("unreachable");
    expect(out.freshness).toBe("base_unmoved");
    expect(out.archivedBranch).toBe("archive/run-r1");

    // ONE squash commit on the workspace branch, subject = node title, D1 trailers verbatim.
    const body = await commitBody(dir, "HEAD");
    expect(body.split("\n")[0]).toBe("add the greeting feature");
    expect(body).toContain("Run-Id: r1");
    expect(body).toContain("Node-Id: n1");
    expect(body).toContain("Verdict: pass@2");
    // The run's change landed.
    expect((await git(dir, ["cat-file", "-p", "HEAD:feature.ts"])).stdout).toBe(
      "export const A = 1;\n",
    );
    // The branch is the receipt — renamed, never deleted; run/<id> gone.
    const branches = (await git(dir, ["branch", "--list"])).stdout;
    expect(branches).toContain("archive/run-r1");
    expect(branches).not.toContain("run/r1");
    // It really is a squash (single parent), not a merge commit.
    expect((await git(dir, ["rev-list", "--count", "HEAD"])).stdout.trim()).toBe("2");
  });

  test("rung 2 — base moved, no file overlap → merge (no_overlap)", async () => {
    const { dir, base } = await makeRepo();
    await seedRun(dir, "r2", base, { "feature.ts": "export const A = 1;\n" });
    await advanceMain(dir, { "unrelated.ts": "export const B = 2;\n" }); // workspace tip moved, disjoint files

    const out = await approveMerge({
      cwd: dir,
      runId: "r2",
      nodeId: "n2",
      nodeTitle: "add feature beside unrelated work",
      verdict: passVerdict(base),
    });

    expect(out.kind).toBe("merged");
    if (out.kind !== "merged") throw new Error("unreachable");
    expect(out.freshness).toBe("no_overlap");
    // Both the run's file AND the workspace's later file are present.
    expect(existsSync(join(dir, "feature.ts"))).toBe(true);
    expect(existsSync(join(dir, "unrelated.ts"))).toBe(true);
    expect((await git(dir, ["branch", "--list"])).stdout).toContain("archive/run-r2");
  });

  test("rung 3 — file overlap → stale, gate stays open, rebase plan carries the workspace tip", async () => {
    const { dir, base } = await makeRepo();
    await seedRun(dir, "r3", base, { "shared.ts": "run version\n" });
    await advanceMain(dir, { "shared.ts": "workspace version\n" }); // SAME file → overlap
    const tip = (await git(dir, ["rev-parse", "HEAD"])).stdout.trim();

    const out = await approveMerge({
      cwd: dir,
      runId: "r3",
      nodeId: "n3",
      nodeTitle: "edit the shared file",
      verdict: passVerdict(base),
    });

    expect(out.kind).toBe("stale");
    if (out.kind !== "stale") throw new Error("unreachable");
    expect(out.reason).toBe("overlap");
    expect(out.rebaseOnto).toBe(tip);
    expect(out.rebasePlan).toBe(rebaseFixPlanItem(tip));
    expect(out.rebasePlan).toContain("rebase onto");
    // NO merge happened: run branch intact, workspace tip unchanged, shared.ts still the workspace version.
    expect((await git(dir, ["branch", "--list"])).stdout).toContain("run/r3");
    expect((await git(dir, ["rev-parse", "HEAD"])).stdout.trim()).toBe(tip);
    expect((await git(dir, ["cat-file", "-p", "HEAD:shared.ts"])).stdout).toBe(
      "workspace version\n",
    );
  });

  test("removes the run worktree before renaming the branch", async () => {
    const { dir, base } = await makeRepo();
    // Keep the worktree registered — approveMerge must remove it (a branch checked out in a worktree can't rename).
    await seedRun(dir, "r4", base, { "feature.ts": "export const A = 1;\n" }, true);
    const before = (await git(dir, ["worktree", "list", "--porcelain"])).stdout;
    expect(before).toContain("run/r4");

    const out = await approveMerge({
      cwd: dir,
      runId: "r4",
      nodeId: "n4",
      nodeTitle: "run in a live worktree",
      verdict: passVerdict(base),
    });

    expect(out.kind).toBe("merged");
    const after = (await git(dir, ["worktree", "list", "--porcelain"])).stdout;
    expect(after).not.toContain("run/r4"); // worktree gone
    expect((await git(dir, ["branch", "--list"])).stdout).toContain("archive/run-r4");
  });

  test("refuses a non-pass verdict (never merges unearned work)", async () => {
    const { dir, base } = await makeRepo();
    await seedRun(dir, "r5", base, { "feature.ts": "x\n" });
    const before = (await git(dir, ["rev-parse", "HEAD"])).stdout.trim();

    const out = await approveMerge({
      cwd: dir,
      runId: "r5",
      nodeId: "n5",
      nodeTitle: "should not merge",
      verdict: { ...passVerdict(base), verdict: "fail" },
    });

    expect(out).toEqual({ kind: "refused", reason: "not_pass" });
    expect((await git(dir, ["rev-parse", "HEAD"])).stdout.trim()).toBe(before); // no mutation
    expect((await git(dir, ["branch", "--list"])).stdout).toContain("run/r5"); // branch untouched
  });

  test("refuses a dirty workspace (never clobbers uncommitted human edits)", async () => {
    const { dir, base } = await makeRepo();
    await seedRun(dir, "r6", base, { "feature.ts": "x\n" });
    writeFileSync(join(dir, "README.md"), "uncommitted human edit\n"); // dirty tree

    const out = await approveMerge({
      cwd: dir,
      runId: "r6",
      nodeId: "n6",
      nodeTitle: "should refuse",
      verdict: passVerdict(base),
    });

    expect(out).toEqual({ kind: "refused", reason: "dirty_workspace" });
    // The human's uncommitted edit is untouched.
    expect((await git(dir, ["status", "--porcelain"])).stdout).toContain("README.md");
  });

  test("refuses an empty run (nothing committed vs the judged base)", async () => {
    const { dir, base } = await makeRepo();
    await git(dir, ["branch", "run/r7", base]); // branch at base, no commits

    const out = await approveMerge({
      cwd: dir,
      runId: "r7",
      nodeId: "n7",
      nodeTitle: "empty",
      verdict: passVerdict(base),
    });

    expect(out).toEqual({ kind: "refused", reason: "empty_run" });
  });
});
