/// <reference types="bun" />
import { afterAll, describe, expect, test } from "bun:test";
import { mkdtemp, readdir, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { parseWorkFile } from "@maestro/protocol";
import { git, gitCommitPaths, gitIsClean } from "../git/git";
import { scanWorkspace } from "../scanner/scanner";
import { persistNodeState } from "./state-writer";

const tmps: string[] = [];
afterAll(async () => {
  await Promise.all(tmps.map((d) => rm(d, { recursive: true, force: true }).catch(() => {})));
});

/** A valid `_work.md` at `state` (matches the scanner's minimal contract). */
function wm(id: string, kind: string, state: string): string {
  return `---\nid: ${id}\nkind: ${kind}\nstate: ${state}\ngate: human\ncreated: 2026-06-25\nupdated: 2026-06-25\n---\n\n# ${id}\n`;
}

/** A canonical (realpath'd) temp git repo with a valid root + one node `_work.md`, all committed clean. */
async function makeRepo(): Promise<string> {
  const dir = await realpath(await mkdtemp(join(tmpdir(), "maestro-statewriter-")));
  tmps.push(dir);
  await git(dir, ["init", "-q", "-b", "main"]);
  await git(dir, ["config", "user.email", "t@t.co"]);
  await git(dir, ["config", "user.name", "t"]);
  await git(dir, ["config", "core.autocrlf", "false"]);
  await Bun.write(join(dir, "_work.md"), wm("root", "project", "proposed"));
  await Bun.write(join(dir, "work", "n0", "_work.md"), wm("n0", "task", "triggered"));
  await git(dir, ["add", "-A"]);
  await git(dir, ["commit", "-qm", "base"]);
  return dir;
}

async function head(dir: string): Promise<string> {
  return (await git(dir, ["rev-parse", "HEAD"])).stdout.trim();
}
async function porcelain(dir: string): Promise<string> {
  return (await git(dir, ["status", "--porcelain"])).stdout.trim();
}
async function tmpLeftovers(dir: string): Promise<string[]> {
  return (await readdir(join(dir, "work", "n0"))).filter((n) => n.endsWith(".tmp"));
}

describe("persistNodeState — durable node-state writer (BRO-1914)", () => {
  test("written: patches the field, commits path-scoped, leaves the tree CLEAN, no leftover temp", async () => {
    const dir = await makeRepo();
    const before = await head(dir);

    const out = await persistNodeState(dir, "work/n0", { state: "review" });
    expect(out).toEqual({ kind: "written" });

    // the FS is durably at review — exactly what the scanner re-derives on --rebuild
    const onDisk = await readFile(join(dir, "work", "n0", "_work.md"), "utf8");
    expect(parseWorkFile(onDisk).contract.state).toBe("review");

    // committed (HEAD moved) AND the tree is clean → the clean-tree-gated approveMerge's isClean passes
    expect(await head(dir)).not.toBe(before);
    expect(await porcelain(dir)).toBe("");
    expect(await gitIsClean(dir)).toBe(true);
    expect(await tmpLeftovers(dir)).toEqual([]);
  });

  test("F9 rebuild: a fresh scanWorkspace re-derives the persisted review (the durability done.check)", async () => {
    const dir = await makeRepo();
    await persistNodeState(dir, "work/n0", { state: "review" });

    // scanWorkspace is the pure derivation the --rebuild path runs — the DB is irrelevant; the FS is truth.
    const { nodes } = await scanWorkspace(dir);
    const n0 = nodes.find((n) => n.id === "n0");
    expect(n0?.state).toBe("review"); // survives the rebuild — the bug BRO-1914 closes
  });

  test("unchanged: already at the target value → no write, no empty commit (idempotent)", async () => {
    const dir = await makeRepo();
    const first = await persistNodeState(dir, "work/n0", { state: "review" });
    expect(first).toEqual({ kind: "written" });
    const afterFirst = await head(dir);

    const second = await persistNodeState(dir, "work/n0", { state: "review" });
    expect(second).toEqual({ kind: "unchanged" });
    expect(await head(dir)).toBe(afterFirst); // NO new commit
    expect(await porcelain(dir)).toBe("");
  });

  test("gitCommitPaths contract: no pre-`add`, so a FAILED commit leaves the index UNTOUCHED (no staged residual)", async () => {
    // The load-bearing property that lets the rollback skip an index reset. The runtime disables git hooks
    // (core.hooksPath=/dev/null key-confinement), so force a deterministic, hook-independent real failure:
    // `git commit -- <unchanged-path>` = "nothing to commit" → non-zero. A pre-`add` would have staged it;
    // gitCommitPaths does not, so `git diff --cached` stays empty and the worktree is untouched.
    const dir = await makeRepo();
    const before = await head(dir);
    await expect(gitCommitPaths(dir, ["work/n0/_work.md"], "no change")).rejects.toThrow(); // unchanged → fails
    expect((await git(dir, ["diff", "--cached", "--name-only"])).stdout.trim()).toBe(""); // index untouched
    expect(await head(dir)).toBe(before);
    expect(await gitIsClean(dir)).toBe(true);
  });

  test("rollback (injected commit throw): worktree restored to exact pre-call bytes, tree clean", async () => {
    const dir = await makeRepo();
    const before = await head(dir);
    const origBytes = await readFile(join(dir, "work", "n0", "_work.md"), "utf8");
    const out = await persistNodeState(
      dir,
      "work/n0",
      { state: "review" },
      {
        git: {
          commit: async () => {
            // gitCommitPaths does NO pre-`add`, so a throw leaves the index untouched — rollback is
            // worktree-only. (Deterministic seam alongside the real-hook test above.)
            throw new Error("simulated commit fault");
          },
        },
      },
    );
    expect(out.kind).toBe("failed");
    if (out.kind === "failed") expect(out.reason).toContain("simulated commit fault");
    expect(await readFile(join(dir, "work", "n0", "_work.md"), "utf8")).toBe(origBytes);
    expect(await head(dir)).toBe(before);
    expect(await porcelain(dir)).toBe("");
    expect(await gitIsClean(dir)).toBe(true);
    expect(await tmpLeftovers(dir)).toEqual([]);
  });

  test("MUTATION GUARD: without the atomic write + commit, the tree would be dirty (approve wedge)", async () => {
    // This asserts the observable property the design hinges on: after a written persist the tree is CLEAN.
    // A bare `writeFile` without the commit (the reverted BRO-1913 naive approach) would leave the tree
    // dirty → approveMerge refuses `dirty_workspace`. The `written` path's clean-tree assertion above is
    // the mutation-proof; here we prove the negative directly by dirtying then persisting.
    const dir = await makeRepo();
    await writeFile(join(dir, "work", "n0", "_work.md"), wm("n0", "task", "running")); // uncommitted dirty edit
    expect(await gitIsClean(dir)).toBe(false);

    const out = await persistNodeState(dir, "work/n0", { state: "review" });
    expect(out).toEqual({ kind: "written" });
    expect(await gitIsClean(dir)).toBe(true); // the writer COMMITS, so it re-cleans a tree it found dirty at its path
    const onDisk = await readFile(join(dir, "work", "n0", "_work.md"), "utf8");
    expect(parseWorkFile(onDisk).contract.state).toBe("review");
  });

  test("absent (missing file): a node with no _work.md is benign (not a fault), leaves the tree untouched", async () => {
    const dir = await makeRepo();
    const before = await head(dir);
    const out = await persistNodeState(dir, "work/does-not-exist", { state: "review" });
    expect(out).toEqual({ kind: "absent" }); // not FS-backed → nothing to persist, reconcile tombstones it
    expect(await head(dir)).toBe(before);
    expect(await porcelain(dir)).toBe("");
  });

  test("concurrency: two concurrent persists on the same workspace both succeed (no index.lock failure)", async () => {
    const dir = await makeRepo();
    // Fire two real git-committing persists at once. Without serializeWorkspaceGit they would collide on
    // `.git/index.lock` and one would fault; the shared lock serializes them so BOTH land.
    const [a, b] = await Promise.all([
      persistNodeState(dir, "work/n0", { state: "review" }),
      persistNodeState(dir, "work/n0", { state: "blocked" }),
    ]);
    // one is `written` (the first to run) and the other is `written` too (it sees a different value) —
    // neither is `failed`. The final committed state is deterministic-consistent (one of the two).
    expect(a.kind).not.toBe("failed");
    expect(b.kind).not.toBe("failed");
    expect(await porcelain(dir)).toBe(""); // tree clean after both
    const onDisk = await readFile(join(dir, "work", "n0", "_work.md"), "utf8");
    expect(["review", "blocked"]).toContain(parseWorkFile(onDisk).contract.state);
  });
});
