// Approve = squash-merge (D1, FLOWS §F5) — the merge MECHANISM shared by the human `approve` verb
// (BRO-1805 gate slice) and the `gate:auto` path (D-AUTODONE: auto-merge is the same ladder, no human verb).
// This module owns ONLY the git mechanics + the verdict-freshness ladder; emitting `gate.approved`/`merge.stale`,
// updating the gate row + node state, and dispatching the "Rebase & re-verify" redispatch belong to the caller.
//
// The ladder (D1): a verdict is valid only for the base commit it was judged against (verdict.base).
//   rung 1 — base unmoved (workspace tip === verdict.base)        → squash-merge
//   rung 2 — base moved, but the run's files don't overlap the    → squash-merge
//            workspace-branch changes since the judged base
//   rung 3 — file overlap (or, defensively, a merge conflict)     → STALE: gate stays open, the caller
//            redispatches with a single fix_plan item ("rebase onto <sha>, resolve, do not change scope")
//            and the verifier re-earns the verdict. No human resolves a conflict inside Maestro v1.
//
// Approve = squash-merge `run/<id>` onto the workspace branch: one commit per approved run, subject from the
// node title, trailers Run-Id/Node-Id/Verdict (D1). Then the worktree is removed and the branch renamed
// `archive/run-<id>` — the branch is the receipt (D1), never deleted. Every git call runs in the workspace
// repo with the scrubbed env (git.ts key-confinement).

import type { VerdictReceipt } from "@maestro/protocol";
import {
  gitChangedFiles as defaultChangedFiles,
  gitCommitAllStaged as defaultCommitAllStaged,
  gitIsClean as defaultIsClean,
  gitMergeSquash as defaultMergeSquash,
  gitRenameBranch as defaultRenameBranch,
  gitResetHard as defaultResetHard,
  gitRevParse as defaultRevParse,
  gitWorktreeList as defaultWorktreeList,
  gitWorktreeRemove as defaultWorktreeRemove,
} from "../git/git";

/** Which ladder rung merged. */
export type MergeFreshness = "base_unmoved" | "no_overlap";
/** Why a merge was deferred — the gate stays open and the caller redispatches "Rebase & re-verify". */
export type StaleReason = "overlap" | "conflict";
/** Why the merge was refused outright (a precondition the caller must resolve — never a silent merge). */
export type MergeRefusal = "not_pass" | "dirty_workspace" | "empty_run";

export type MergeOutcome =
  | { kind: "merged"; sha: string; freshness: MergeFreshness; archivedBranch: string }
  | { kind: "stale"; reason: StaleReason; rebaseOnto: string; rebasePlan: string }
  | { kind: "refused"; reason: MergeRefusal };

/** Injectable git surface — defaults to the real {@link ../git/git} helpers (tests drive a real temp repo). */
export interface MergeGit {
  revParse: typeof defaultRevParse;
  changedFiles: typeof defaultChangedFiles;
  isClean: typeof defaultIsClean;
  mergeSquash: typeof defaultMergeSquash;
  commitAllStaged: typeof defaultCommitAllStaged;
  resetHard: typeof defaultResetHard;
  renameBranch: typeof defaultRenameBranch;
  worktreeList: typeof defaultWorktreeList;
  worktreeRemove: typeof defaultWorktreeRemove;
}

const REAL_GIT: MergeGit = {
  revParse: defaultRevParse,
  changedFiles: defaultChangedFiles,
  isClean: defaultIsClean,
  mergeSquash: defaultMergeSquash,
  commitAllStaged: defaultCommitAllStaged,
  resetHard: defaultResetHard,
  renameBranch: defaultRenameBranch,
  worktreeList: defaultWorktreeList,
  worktreeRemove: defaultWorktreeRemove,
};

export interface ApproveMergeDeps {
  /** The workspace repo root (RuntimeConfig.workspace) — approved work squash-merges onto its checked-out branch. */
  cwd: string;
  runId: string;
  nodeId: string;
  /** The node title — the squash commit's subject (first line only; plain voice). */
  nodeTitle: string;
  /** The run's verdict receipt (from verdict.md). MUST be `pass`; carries `base` (the judged commit) + `attempt`. */
  verdict: VerdictReceipt;
  /** Test seam — the git surface. Defaults to the real helpers against `cwd`. */
  git?: MergeGit;
}

/** The single fix_plan item a stale approve redispatches with (D1). The verifier re-runs on the rebased branch. */
export function rebaseFixPlanItem(sha: string): string {
  return `- [ ] rebase onto ${sha}, resolve conflicts, do not change scope`;
}

/** The squash commit message: node title subject + the D1 trailers. */
function commitMessage(title: string, runId: string, nodeId: string, attempt: number): string {
  const subject = title.split("\n")[0]?.trim() || `run ${runId}`;
  return `${subject}\n\nRun-Id: ${runId}\nNode-Id: ${nodeId}\nVerdict: pass@${attempt}\n`;
}

/**
 * Attempt to merge an approved run onto the workspace branch, applying the D1 verdict-freshness ladder.
 * Returns a `merged` receipt (rung 1/2), a `stale` deferral the caller redispatches (rung 3), or a `refused`
 * precondition failure — it NEVER merges silently past a stale verdict or a dirty tree.
 */
export async function approveMerge(deps: ApproveMergeDeps): Promise<MergeOutcome> {
  const { cwd, runId, nodeId, nodeTitle, verdict } = deps;
  const g = deps.git ?? REAL_GIT;
  const runBranch = `run/${runId}`;

  // Preconditions — refuse rather than merge into an unsafe or unearned state.
  if (verdict.verdict !== "pass") return { kind: "refused", reason: "not_pass" };
  if (!(await g.isClean(cwd))) return { kind: "refused", reason: "dirty_workspace" };

  const judgedBase = await g.revParse(cwd, verdict.base);
  const workspaceTip = await g.revParse(cwd, "HEAD");

  // An empty run (no committed change vs its judged base) has nothing to merge — refuse (never an empty commit).
  const runFiles = await g.changedFiles(cwd, judgedBase, runBranch);
  if (runFiles.length === 0) return { kind: "refused", reason: "empty_run" };

  // Freshness ladder.
  let freshness: MergeFreshness;
  if (workspaceTip === judgedBase) {
    freshness = "base_unmoved"; // rung 1 — the verdict is valid for exactly this tip
  } else {
    // rung 2 vs 3 — did the workspace branch touch any file the run changed since the judged base?
    const baseChanged = new Set(await g.changedFiles(cwd, judgedBase, workspaceTip));
    if (runFiles.some((f) => baseChanged.has(f))) {
      // rung 3 — overlap: the verdict no longer covers the merge; defer, the caller re-earns it on a rebase.
      return {
        kind: "stale",
        reason: "overlap",
        rebaseOnto: workspaceTip,
        rebasePlan: rebaseFixPlanItem(workspaceTip),
      };
    }
    freshness = "no_overlap"; // disjoint files → the check ran on the same files it judged (D1)
  }

  // Squash-merge onto the current workspace checkout. Disjoint files never conflict, but if git reports one
  // (defensive: mode changes, gitattributes merge drivers), roll the clean tree back and defer as stale.
  const merge = await g.mergeSquash(cwd, runBranch);
  if (merge.code !== 0) {
    await g.resetHard(cwd, "HEAD");
    return {
      kind: "stale",
      reason: "conflict",
      rebaseOnto: workspaceTip,
      rebasePlan: rebaseFixPlanItem(workspaceTip),
    };
  }
  const sha = await g.commitAllStaged(
    cwd,
    commitMessage(nodeTitle, runId, nodeId, verdict.attempt),
  );

  // The branch is the receipt (D1): remove the worktree (if still checked out), then rename run/<id> →
  // archive/run-<id>. The rename requires the branch not be checked out in any worktree.
  await removeRunWorktree(g, cwd, runBranch);
  const archivedBranch = `archive/run-${runId}`;
  await g.renameBranch(cwd, runBranch, archivedBranch);

  return { kind: "merged", sha, freshness, archivedBranch };
}

/** Remove the worktree checked out to `runBranch`, if one is still registered. Idempotent — a run whose
 *  worktree was already freed (e.g. at reap) is a no-op. `force` because a completed run's uncommitted delta
 *  is not a receipt (the branch commits + runDir are). */
async function removeRunWorktree(g: MergeGit, cwd: string, runBranch: string): Promise<void> {
  const worktrees = await g.worktreeList(cwd);
  const wt = worktrees.find((w) => w.branch === runBranch);
  if (wt) await g.worktreeRemove(cwd, wt.path, { force: true });
}
