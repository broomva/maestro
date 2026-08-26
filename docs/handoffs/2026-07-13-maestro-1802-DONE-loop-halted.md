# Maestro handoff — BRO-1802 DONE, autonomous loop HALTED (2026-07-13)

## TL;DR

**BRO-1802 (Approve = squash-merge, D1) is DONE** — merged to main via PR #67, squash `71eecad`, main tip `71eecad`. **The autonomous self-firing build loop was HALTED here** per the user directive of 2026-07-13 ("rate limit raised, please continue and stop on the next arc that closes, lets halt the loop"): stopped via `ScheduleWakeup({stop:true})` after this merge, with **no re-fire to BRO-1805**. To resume, re-issue the `/loop` AUTONOMOUS MAESTRO BUILD GOVERNOR directive; the next ticket is BRO-1805.

## What shipped

`apps/runtime/src/merge/merge.ts` (+ new git primitives in `git/git.ts`): `approveMerge(deps)` squash-merges `run/<id>` onto the workspace branch under the D1 3-rung verdict-freshness ladder.

- **rung 1** — base unmoved (`workspaceTip === verdict.base`) → merge (`base_unmoved`).
- **rung 2** — base moved, run files disjoint from workspace-branch changes since the judged base → merge (`no_overlap`).
- **rung 3** — file overlap (or a defensive conflict) → `{kind:"stale", reason:"overlap"|"conflict", rebaseOnto, rebasePlan}`; the caller redispatches the single `rebaseFixPlanItem` and the verifier re-earns the verdict.
- Squash commit subject = node title (`oneLine`'d); trailers `Run-Id` / `Node-Id` / `Verdict: pass@N` (each `oneLine`'d — forged-trailer-proof). Branch renamed `archive/run-<id>` (the branch is the receipt, never deleted). Run worktree removed post-merge (best-effort; a durable merge never becomes a thrown "approve failed").
- Preconditions refuse rather than merge unsafe/unearned: `not_pass` / `dirty_workspace` / `empty_run`.
- `serializeApprove(cwd, fn)` per-workspace in-process mutex + a `tipBeforeMerge` re-read guard make the freshness decision + merge + commit + archive ONE critical section (closes the D1 TOCTOU). `gate:auto` (D-AUTODONE) shares this exact path.
- New git primitives (all under the `gitEnv()` key-confinement scrub): `gitRevParse`, `gitChangedFiles`, `gitIsClean`, `gitMergeSquash`, `gitCommitAllStaged`, `gitResetHard`, `gitRenameBranch`.

done.check `bun test apps/runtime --filter merge` green (569 pass); tsc + biome clean.

## P20 (R2→R5, terminal PASS)

- **R2/R3** — git config-exec RCE hardening: `filter.*`/`merge.*`/`diff.*`/hooks/`core.fsmonitor`/gpg neutralized via the `GIT_CONFIG_*` env form (immune to the `-c` `=`-in-subsection bypass R3 found); bounded (1 MiB) + timed (5s) + fail-closed enumeration read. R3 also fixed a confirmed TOCTOU blocker (the mutex).
- **R4 MAJOR** — approve was serialized only vs other approves, not vs `new_mission`/reap → `.git/index.lock` contention misread as a conflict, or a triple-window dirty-wedge. **Fixed:** `git()` retries a transient index.lock (~1.4s bounded backoff, `isIndexLockError`); `approveMerge` classifies a stuck lock as a retryable `refused: workspace_busy` with the tree rolled back; `subcommandOf()` keys the exec-channel enumeration off the real subcommand, not `args[0]`.
- **R5 terminal PASS** (`wf_7b73f04b`, min 7/10, **0 surviving blocker/major**): 9 confirmed findings all minor/nit (no live caller until BRO-1805; loud/recoverable; or D7 defense-in-depth). Two test-integrity findings fixed as test-only strengthening (production byte-identical to the R5-verified `882eac3`): a deterministic `serializeApprove` mutex proof + a real `gitResetHard` dirty→clean test, both mutation-proven.

P11 dogfood (real git, real lock): transient → merges with D1 trailers + `archive/run-dog`, tree clean; stuck → clean `workspace_busy`, tree clean.

## If the loop is resumed — BRO-1805 (F5 gate slice) picks up here

BRO-1805 wires `approveMerge` to a LIVE caller (the human `approve` verb + `gate:auto`). It is where the two R4/R5 residuals must land, because they only become reachable once a concurrent live caller exists:

1. **BRO-1881** (Med) — unify approve + `new_mission` + reap under ONE per-workspace index-write lock (extract `serializeApprove` → `withWorkspaceIndexLock`). Removes the contention entirely; the `git()` retry becomes a pure backstop. Also collapses the disjoint-move `stale:overlap` mislabel.
2. **Bound `gitChangedFiles`** — it reads agent-controlled `--name-only` output through the unbounded `git()` runner (the load-bearing OOM class). Route it through a bounded read before BRO-1805 wires a live caller. (Tracked in BRO-1881.)

Other open follow-ups: **BRO-1877** (Low — phase-2 container isolation for the residual config-exec TOCTOU + FIFO), **BRO-1878** (Low — deterministic `.git/hooks` FILE-hook regression test via a hermetic git-config env).

P3 chain remaining: **1805** → 1809 (M5 inspector) → 1813 (adversarial suite) → 1818 (autonomy ledger) → 1821 (P3-exit), then P4/P5.

## State at handoff

- main tip `71eecad`, clean tree, no leftover worktrees, feature branch deleted.
- Linear: BRO-1802 → Done (completion comment posted). BRO-1881/1877/1878 in Backlog.
- Docs: DECISIONS.md §D1, FLOWS.md §F5.
