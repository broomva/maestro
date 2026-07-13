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
import { GitError, git } from "../git/git";
import { approveMerge, type MergeGit, rebaseFixPlanItem } from "./merge";

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

  test("concurrent approves on the SAME file → exactly one merged, one deferred (never two silent merges)", async () => {
    const { dir, base } = await makeRepo();
    // Two runs off the SAME base both adding the SAME file. This is the END-TO-END outcome invariant: however the
    // two concurrent approves interleave, the workspace never ends with BOTH runs silently merged. The first to
    // land commits shared.txt; the second is deferred — its files overlap the workspace change since its judged
    // base, so the ladder's overlap pre-check (or the re-read guard) returns stale:overlap. (This proves the
    // OUTCOME, not serializeApprove in isolation — same-file runs also collide via the overlap check + re-read
    // guard, so removing the mutex alone need not RED this. The next test mutation-proves the mutex deterministically.)
    await seedRun(dir, "r1", base, { "shared.txt": "from-r1\n" });
    await seedRun(dir, "r2", base, { "shared.txt": "from-r2\n" });

    // Fire both without awaiting between them (Promise.all) — the per-cwd chain serializes them in call order.
    const [o1, o2] = await Promise.all([
      approveMerge({
        cwd: dir,
        runId: "r1",
        nodeId: "n1",
        nodeTitle: "r1",
        verdict: passVerdict(base),
      }),
      approveMerge({
        cwd: dir,
        runId: "r2",
        nodeId: "n2",
        nodeTitle: "r2",
        verdict: passVerdict(base),
      }),
    ]);

    // Exactly one merged and one deferred — NEVER two silent merges.
    expect([o1.kind, o2.kind].sort()).toEqual(["merged", "stale"]);
    const stale = [o1, o2].find((o) => o.kind === "stale");
    expect(stale?.kind === "stale" && stale.reason).toBe("overlap");
    // The workspace holds exactly ONE run's content, never a combine.
    const shared = (await git(dir, ["show", "HEAD:shared.txt"])).stdout;
    expect(shared === "from-r1\n" || shared === "from-r2\n").toBe(true);
  });

  test("serializeApprove makes concurrent approves NON-INTERLEAVED critical sections (deterministic mutex proof)", async () => {
    // The guarantee serializeApprove uniquely provides — beyond the ladder's overlap pre-check + re-read guard — is
    // that two approves on the SAME workspace run as fully-ordered critical sections: the first's read→stage→commit
    // completes ENTIRELY before the second reads any state. That is what prevents the D1 stage-level combine (both
    // `merge --squash` staging disjoint files into one index before either commits). Proven deterministically with an
    // instrumented stub that YIELDS the event loop inside its section: with the mutex the second cannot enter during
    // the first's yield; without it, the two interleave. Uses a distinct cwd so it can't disturb other tests' chains.
    const events: string[] = [];
    const yieldTick = () => new Promise((r) => setTimeout(r, 5));
    const stub = (id: string): MergeGit => ({
      revParse: async () => "SAME", // rung 1 base_unmoved + the re-read guard passes → reaches merge+commit
      changedFiles: async () => ["f.txt"],
      isClean: async () => {
        events.push(`${id}:enter`);
        await yieldTick(); // a real event-loop yield INSIDE the critical section
        return true;
      },
      mergeSquash: async () => ({ code: 0, stdout: "", stderr: "" }),
      commitAllStaged: async () => {
        events.push(`${id}:commit`);
        return `sha-${id}`;
      },
      resetHard: async () => {},
      renameBranch: async () => {},
      worktreeList: async () => [],
      worktreeRemove: async () => {},
    });

    await Promise.all([
      approveMerge({
        cwd: "/mutex",
        runId: "a",
        nodeId: "na",
        nodeTitle: "a",
        verdict: passVerdict("BASE"),
        git: stub("a"),
      }),
      approveMerge({
        cwd: "/mutex",
        runId: "b",
        nodeId: "nb",
        nodeTitle: "b",
        verdict: passVerdict("BASE"),
        git: stub("b"),
      }),
    ]);

    // The first approve (call order → "a") fully finishes (enter → commit) before the second enters — no interleave.
    // MUTATION: make approveMerge call approveMergeCritical(deps) directly (drop the serializeApprove wrapper) → both
    // isClean run during each other's yield → events become [a:enter, b:enter, a:commit, b:commit] → this REDs.
    expect(events).toEqual(["a:enter", "a:commit", "b:enter", "b:commit"]);
  });

  test("a conflicting squash resets the tree and defers to stale — the mutating rollback path (via the injectable git seam)", async () => {
    // Drive approveMerge with a MergeGit stub whose mergeSquash reports a conflict, so the otherwise-hard-to-reach
    // resetHard rollback branch (merge.ts) is exercised without a synthetic gitattributes-driver conflict. Also
    // exercises the `git?: MergeGit` injection seam end to end.
    const seen: string[] = [];
    const stub: MergeGit = {
      revParse: async () => "SAME", // judgedBase === HEAD → rung 1 base_unmoved, and the re-read guard passes
      changedFiles: async () => ["f.txt"], // a non-empty run
      isClean: async () => true,
      mergeSquash: async () => {
        seen.push("merge");
        return { code: 1, stdout: "", stderr: "CONFLICT" };
      },
      resetHard: async () => {
        seen.push("reset");
      },
      commitAllStaged: async () => {
        seen.push("commit");
        return "SHOULD-NOT-COMMIT";
      },
      renameBranch: async () => {
        seen.push("rename");
      },
      worktreeList: async () => [],
      worktreeRemove: async () => {},
    };

    const out = await approveMerge({
      cwd: "/nonexistent-stub-cwd",
      runId: "rc",
      nodeId: "nc",
      nodeTitle: "conflict",
      verdict: passVerdict("BASE"),
      git: stub,
    });

    expect(out).toEqual({
      kind: "stale",
      reason: "conflict",
      rebaseOnto: "SAME",
      rebasePlan: rebaseFixPlanItem("SAME"),
    });
    expect(seen).toEqual(["merge", "reset"]); // the tree was reset; the commit was NEVER made
  });

  // ── BRO-1802 P20 R4 MAJOR: a stuck workspace-index lock is retryable, NOT a conflict or a dirty-wedge ─────────
  // git() rides out a TRANSIENT index.lock (new_mission's brief add+commit); these prove that a lock which OUTLASTS
  // that retry budget resolves to a clean `workspace_busy` refusal with the tree reset — never a bogus
  // "rebase & resolve" redispatch (merge path) and never a thrown crash leaving a half-merged tree (commit path).
  const lockStub = (over: Partial<MergeGit>, seen: string[]): MergeGit => ({
    revParse: async () => "SAME", // rung 1 base_unmoved + the re-read guard passes
    changedFiles: async () => ["f.txt"], // a non-empty run
    isClean: async () => true,
    mergeSquash: async () => {
      seen.push("merge");
      return { code: 0, stdout: "", stderr: "" };
    },
    resetHard: async () => {
      seen.push("reset");
    },
    commitAllStaged: async () => {
      seen.push("commit");
      return "SHA";
    },
    renameBranch: async () => {
      seen.push("rename");
    },
    worktreeList: async () => [],
    worktreeRemove: async () => {},
    ...over,
  });

  const INDEX_LOCK_STDERR =
    "fatal: Unable to create '/ws/.git/index.lock': File exists.\n\nAnother git process seems to be running in this repository";

  test("a stuck index-lock on merge --squash → workspace_busy (retryable), tree reset — NOT misread as conflict", async () => {
    const seen: string[] = [];
    const stub = lockStub(
      {
        mergeSquash: async () => {
          seen.push("merge");
          return { code: 128, stdout: "", stderr: INDEX_LOCK_STDERR };
        },
      },
      seen,
    );

    const out = await approveMerge({
      cwd: "/nonexistent-stub-cwd",
      runId: "rl",
      nodeId: "nl",
      nodeTitle: "locked merge",
      verdict: passVerdict("BASE"),
      git: stub,
    });

    // MUTATION: drop the `isIndexLockError(merge.stderr)` branch in merge.ts → returns `stale:conflict` (a bogus
    // rebase redispatch) → this REDs.
    expect(out).toEqual({ kind: "refused", reason: "workspace_busy" });
    expect(seen).toEqual(["merge", "reset"]); // rolled back to the clean tree; no commit
  });

  test("a stuck index-lock on commit → workspace_busy (retryable), tree reset — NOT a thrown crash", async () => {
    const seen: string[] = [];
    const stub = lockStub(
      {
        commitAllStaged: async () => {
          seen.push("commit");
          throw new GitError(["commit", "--no-verify"], 128, INDEX_LOCK_STDERR);
        },
      },
      seen,
    );

    const out = await approveMerge({
      cwd: "/nonexistent-stub-cwd",
      runId: "rc2",
      nodeId: "nc2",
      nodeTitle: "locked commit",
      verdict: passVerdict("BASE"),
      git: stub,
    });

    // MUTATION: drop the `err instanceof GitError && isIndexLockError(err.stderr)` branch → approveMerge REJECTS
    // (the GitError propagates) → this test throws instead of asserting → REDs.
    expect(out).toEqual({ kind: "refused", reason: "workspace_busy" });
    expect(seen).toEqual(["merge", "commit", "reset"]); // squash staged, commit lost the lock, tree reset
  });

  test("a NON-lock commit failure still throws (empty-identity etc. are not swallowed as workspace_busy)", async () => {
    const seen: string[] = [];
    const stub = lockStub(
      {
        commitAllStaged: async () => {
          seen.push("commit");
          throw new GitError(["commit", "--no-verify"], 128, "fatal: empty ident name not allowed");
        },
      },
      seen,
    );

    // MUTATION: widen isIndexLockError (or drop the GitError-narrowing) so a plain failure returns workspace_busy →
    // this REDs (it must still reject, never a silent refusal that hides a real crash).
    await expect(
      approveMerge({
        cwd: "/nonexistent-stub-cwd",
        runId: "rc3",
        nodeId: "nc3",
        nodeTitle: "empty identity",
        verdict: passVerdict("BASE"),
        git: stub,
      }),
    ).rejects.toThrow(/empty ident/);
    expect(seen).toEqual(["merge", "commit", "reset"]); // best-effort reset still ran before the rethrow
  });
});
