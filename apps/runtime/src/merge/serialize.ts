// The per-workspace git critical section (BRO-1802 D1, generalized BRO-1914).
//
// A runtime owns EXACTLY ONE workspace (D4 runtime-lock), so an in-process promise chain keyed by
// `cwd` is a sufficient lock to serialize every mutation of that workspace's git index/worktree into
// ONE critical section:
//   - approve's freshness-decision + `merge --squash` + commit + archive (merge.ts, the D1 TOCTOU
//     guard — without it a concurrent approve can move the tip between the ladder's HEAD read and the
//     squash, silently combining two runs' changes into a commit neither verdict was earned against);
//   - the durable node-state writer's atomic `_work.md` field patch + path-scoped commit
//     (state-writer.ts, BRO-1914).
//
// Sharing the SAME chain across both means they can never contend on `.git/index.lock` or interleave
// a tip move between a read and a write. This is the first step of the BRO-1881 index-lock unify
// (new_mission's commit is still outside this lock — a separate follow-up). The map holds one entry
// per distinct workspace (one, in production) and self-prunes when a chain drains.

const chain = new Map<string, Promise<unknown>>();
const NOOP = (): void => {};

/**
 * Run `fn` after any in-flight workspace-git critical section on `cwd`, and make the next caller
 * chain behind it — a serial, per-`cwd` mutex. Runs `fn` regardless of the previous holder's
 * outcome (a failed approve must not wedge the next writer); the returned promise settles with
 * `fn`'s own result/rejection.
 *
 * NOT REENTRANT — `fn` must NEVER call `serializeWorkspaceGit` for the SAME `cwd` (directly or
 * transitively): the inner acquire chains behind the outer's own tail, which cannot resolve until
 * the outer `fn` (awaiting the inner) returns → self-deadlock. Today's holders (`approveMerge`,
 * `persistNodeState`) are leaves, so this never bites. FUTURE HAZARD (BRO-1805 slice 2b-ii, approve):
 * do NOT persist `state: done` by calling `persistNodeState` from INSIDE `approveMergeCritical`
 * (which already holds this lock) — call it OUTSIDE the merge critical section, or make the lock
 * reentrant first.
 */
export function serializeWorkspaceGit<T>(cwd: string, fn: () => Promise<T>): Promise<T> {
  const prev = chain.get(cwd) ?? Promise.resolve();
  const next = prev.then(fn, fn); // run after the previous holder regardless of its outcome
  const tail = next.then(NOOP, NOOP); // a never-rejecting tail the next caller chains behind
  chain.set(cwd, tail);
  void tail.then(() => {
    if (chain.get(cwd) === tail) chain.delete(cwd);
  });
  return next;
}
