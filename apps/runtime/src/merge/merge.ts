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
  GitError,
  isIndexLockError,
} from "../git/git";

/** Which ladder rung merged. */
export type MergeFreshness = "base_unmoved" | "no_overlap";
/** Why a merge was deferred — the gate stays open and the caller redispatches "Rebase & re-verify". */
export type StaleReason = "overlap" | "conflict";
/** Why the merge was refused outright (a precondition the caller must resolve — never a silent merge).
 *  `workspace_busy` is RETRYABLE: a stuck workspace-index lock (a concurrent new_mission / reap writer that
 *  {@link isIndexLockError} outlasted {@link git}'s retry budget), rolled back to a clean tree — the caller
 *  re-tries the SAME approve later, NOT a rebase. It is never a conflict and never leaves the tree half-merged.
 *  BRO-1881 removes the contention entirely (one per-workspace index-write lock over approve + new_mission + reap). */
export type MergeRefusal = "not_pass" | "dirty_workspace" | "empty_run" | "workspace_busy";

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

const NOOP = (): void => {};

/**
 * Serialize {@link approveMerge} per workspace so the freshness decision + merge + commit + archive run as ONE
 * critical section. Without it a concurrent approve can move the workspace tip between the ladder's HEAD read
 * and the `merge --squash`, silently combining two runs' changes into a single commit that neither verdict was
 * earned against (the D1 TOCTOU: `git merge --squash` of disjoint hunks onto a moved tip reports "went well",
 * exit 0, no conflict). A runtime owns exactly one workspace (D4 runtime-lock), so an in-process promise chain
 * keyed by `cwd` is a sufficient lock; the map holds one entry per distinct workspace (one, in production) and
 * self-prunes when a chain drains. `gate:auto` (D-AUTODONE) shares this path, so the guard covers it too.
 */
const approveChain = new Map<string, Promise<unknown>>();
function serializeApprove<T>(cwd: string, fn: () => Promise<T>): Promise<T> {
  const prev = approveChain.get(cwd) ?? Promise.resolve();
  const next = prev.then(fn, fn); // run after the previous holder regardless of its outcome
  const tail = next.then(NOOP, NOOP); // a never-rejecting tail the next caller chains behind
  approveChain.set(cwd, tail);
  void tail.then(() => {
    if (approveChain.get(cwd) === tail) approveChain.delete(cwd);
  });
  return next;
}

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

/** Strip anything from the first newline on — a trailer VALUE is a single line, so an id carrying an embedded
 *  newline cannot inject a forged trailer (e.g. a second `Verdict:`) into the commit body. System ids never
 *  contain a newline; this guards a future caller (BRO-1805 / gate:auto) passing an untrusted id. */
function oneLine(value: string): string {
  return value.split("\n")[0] ?? "";
}

/** The squash commit message: node title subject + the D1 trailers. Subject + each trailer value are reduced to
 *  a single line so neither the title nor an id can inject extra commit-body lines (D1: the trailers are receipts). */
function commitMessage(title: string, runId: string, nodeId: string, attempt: number): string {
  const subject = oneLine(title).trim() || `run ${oneLine(runId)}`;
  return `${subject}\n\nRun-Id: ${oneLine(runId)}\nNode-Id: ${oneLine(nodeId)}\nVerdict: pass@${attempt}\n`;
}

/**
 * Attempt to merge an approved run onto the workspace branch, applying the D1 verdict-freshness ladder.
 * Returns a `merged` receipt (rung 1/2), a `stale` deferral the caller redispatches (rung 3), or a `refused`
 * precondition failure — it NEVER merges silently past a stale verdict or a dirty tree. Serialized per workspace
 * ({@link serializeApprove}) so the freshness decision and the merge are one critical section (D1 TOCTOU).
 */
export async function approveMerge(deps: ApproveMergeDeps): Promise<MergeOutcome> {
  return serializeApprove(deps.cwd, () => approveMergeCritical(deps));
}

async function approveMergeCritical(deps: ApproveMergeDeps): Promise<MergeOutcome> {
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
      return staleOverlap(workspaceTip);
    }
    freshness = "no_overlap"; // disjoint files → the check ran on the same files it judged (D1)
  }

  // TOCTOU guard: serializeApprove keeps concurrent APPROVES out, but a concurrent new_mission commit could
  // still advance HEAD during the read-only analysis above. Re-read HEAD immediately before the merge; if it
  // moved, the ladder judged a tip we are no longer merging onto → defer to stale rather than combine onto a
  // tip no verdict covers (nothing is staged yet, so there is nothing to roll back).
  const tipBeforeMerge = await g.revParse(cwd, "HEAD");
  if (tipBeforeMerge !== workspaceTip) return staleOverlap(tipBeforeMerge);

  // Squash-merge + commit — the irreversible step. `merge --squash` stages WITHOUT a MERGE_HEAD, so any failure
  // before the commit lands (a reported conflict, or a throw from the merge/commit — e.g. empty identity, or
  // the >500 exec-key fail-closed during commit's config enumeration) must reset the clean tree back, or a
  // failed approve would leave the live workspace checkout dirty and wedge every later approve on `isClean`
  // (mirrors the conflict path + new_mission's commit-is-the-transaction rollback in intents.ts).
  let sha: string;
  try {
    const merge = await g.mergeSquash(cwd, runBranch);
    if (merge.code !== 0) {
      await g.resetHard(cwd, "HEAD"); // roll the unfinished squash back to the clean tree (may throw → outer catch)
      // A stuck workspace-index lock is NOT a conflict: report it retryable so the caller re-tries the same approve,
      // never a bogus "rebase & resolve" redispatch (git.ts already rode out the transient hold — see workspace_busy).
      if (isIndexLockError(merge.stderr)) return { kind: "refused", reason: "workspace_busy" };
      return {
        kind: "stale",
        reason: "conflict",
        rebaseOnto: workspaceTip,
        rebasePlan: rebaseFixPlanItem(workspaceTip),
      };
    }
    sha = await g.commitAllStaged(cwd, commitMessage(nodeTitle, runId, nodeId, verdict.attempt));
  } catch (err) {
    await g.resetHard(cwd, "HEAD").catch(NOOP); // best-effort — the throw, not the reset outcome, is the signal
    // commit lost the index-lock race after the squash staged → the reset above restored the clean tree → report it
    // retryable, not a crash. Any other failure (empty identity, >500 exec-key fail-closed) still throws.
    if (err instanceof GitError && isIndexLockError(err.stderr)) {
      return { kind: "refused", reason: "workspace_busy" };
    }
    throw err;
  }

  // The commit is DURABLE — the merge has succeeded. Archival (worktree removal + run/<id> → archive/run-<id>)
  // is post-durable CLEANUP: a failure here must NOT turn a landed merge into a thrown "approve failed" (D1: the
  // branch is the receipt — run/<id> is itself a valid receipt if the rename cannot complete). Best-effort.
  const archivedBranch = `archive/run-${runId}`;
  const archived = await archiveRun(g, cwd, runBranch, archivedBranch);

  return { kind: "merged", sha, freshness, archivedBranch: archived ? archivedBranch : runBranch };
}

/** The rung-3 / moved-tip deferral: the gate stays open and the caller re-earns the verdict on a rebase. */
function staleOverlap(tip: string): MergeOutcome {
  return { kind: "stale", reason: "overlap", rebaseOnto: tip, rebasePlan: rebaseFixPlanItem(tip) };
}

/** Post-merge cleanup: free the run worktree (if still checked out) then rename run/<id> → archive/run-<id>.
 *  BEST-EFFORT — the squash commit is already durable, so a cleanup failure (a locked worktree, a pre-existing
 *  archive branch on a re-dispatched id, the >500 exec-key fail-closed on `worktree list`) must not discard the
 *  merge. Returns whether the branch reached its archived name; the caller reports run/<id> as the receipt if not. */
async function archiveRun(
  g: MergeGit,
  cwd: string,
  runBranch: string,
  archivedBranch: string,
): Promise<boolean> {
  try {
    await removeRunWorktree(g, cwd, runBranch);
    await g.renameBranch(cwd, runBranch, archivedBranch);
    return true;
  } catch {
    return false;
  }
}

/** Remove the worktree checked out to `runBranch`, if one is still registered. Idempotent — a run whose
 *  worktree was already freed (e.g. at reap) is a no-op. `force` because a completed run's uncommitted delta
 *  is not a receipt (the branch commits + runDir are). */
async function removeRunWorktree(g: MergeGit, cwd: string, runBranch: string): Promise<void> {
  const worktrees = await g.worktreeList(cwd);
  const wt = worktrees.find((w) => w.branch === runBranch);
  if (wt) await g.worktreeRemove(cwd, wt.path, { force: true });
}
