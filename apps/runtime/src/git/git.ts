// Minimal git surface for the runtime (BRO-1820). The runtime owns the workspace
// git repo — "the branch is the receipt" (ARCHITECTURE §3a). new_mission (FLOWS §F1)
// makes the commit the transaction: folder + `_work.md` are created, then committed;
// if the commit fails the caller removes what it created, so nothing is half-landed.
//
// argv form ONLY (Bun.spawn with an array) — never a shell string. The commit message
// and paths derive from user input (a mission title / parentPath), so shelling out
// would be an injection surface. spawn's array form passes them as literal argv.
//
// KEY-CONFINEMENT (BRO-1794 P20): EVERY git spawn here runs with a SCRUBBED env — the
// same deny-by-default allowlist ({@link filterPassthroughEnv}) the runtime gives an
// agent-influenced Stage-1 check (buildCheckEnv). Reason: these commands run in the
// AGENT-CONTROLLED worktree, where git executes agent-configured drivers — a diff
// `diff.external`/`textconv`, a `filter.*.smudge`, a hook — and a driver inheriting the
// supervisor's process.env would read ANTHROPIC_API_KEY and exfiltrate it. Scrubbing the
// env means no host secret is ever in scope for any git subprocess. Defense-in-depth on
// the one PATCH-producing diff ({@link gitDiffBounded}): `--no-ext-diff --no-textconv`
// so an agent-planted external-diff/textconv driver never even runs.

import { filterPassthroughEnv } from "../harness/spawn-contract";

/** The scrubbed env EVERY runtime git spawn runs under — PATH/HOME/toolchain only, NEVER a host secret
 *  (see the key-confinement note at the top of this file). Recomputed per call from the live process.env
 *  (cheap; the process env is small) so a late-set toolchain var is still seen. Passed as the FULL env to
 *  the spawn (Bun's `env` REPLACES process.env for the child), so git sees only the allowlist. */
function gitEnv(): Record<string, string> {
  return filterPassthroughEnv(process.env);
}

/** Result of a git invocation — exit code plus captured streams. */
export interface GitResult {
  code: number;
  stdout: string;
  stderr: string;
}

/** A non-zero git exit — carries the subcommand + stderr for a typed failure. */
export class GitError extends Error {
  readonly args: readonly string[];
  readonly code: number;
  readonly stderr: string;
  constructor(args: readonly string[], code: number, stderr: string) {
    super(`git ${args.join(" ")} failed (exit ${code}): ${stderr.trim() || "(no stderr)"}`);
    this.name = "GitError";
    this.args = args;
    this.code = code;
    this.stderr = stderr;
  }
}

/** Run `git <args>` in `cwd`, capturing streams. Resolves even on non-zero exit. */
export async function git(cwd: string, args: string[]): Promise<GitResult> {
  const proc = Bun.spawn(["git", ...args], { cwd, env: gitEnv(), stdout: "pipe", stderr: "pipe" });
  const [stdout, stderr, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ]);
  return { code, stdout, stderr };
}

/** True if `cwd` is inside a git work tree. */
export async function isGitRepo(cwd: string): Promise<boolean> {
  const r = await git(cwd, ["rev-parse", "--is-inside-work-tree"]);
  return r.code === 0 && r.stdout.trim() === "true";
}

/**
 * The current HEAD commit sha of `cwd` — the base a run branches from (VERIFIER §4 `base`). Captured at
 * dispatch from the freshly-created worktree (whose HEAD is the workspace branch point) and threaded on
 * the run context, so the verifier can diff `base..run/<id>` at reap. Throws {@link GitError} on failure
 * (an unborn/bare repo, git missing) so the caller contains it as a crash rather than diffing against "".
 */
export async function gitHead(cwd: string): Promise<string> {
  const r = await git(cwd, ["rev-parse", "HEAD"]);
  if (r.code !== 0) throw new GitError(["rev-parse", "HEAD"], r.code, r.stderr);
  return r.stdout.trim();
}

/**
 * Spawn `git <argv>` in `cwd` and read its stdout BOUNDED to `maxBytes` — streams and STOPS the moment the
 * cap is exceeded (cancelling the read + killing git), so an adversarial huge output can never buffer into
 * the caller (the 24/7 supervisor). Reads only from git (the object store / index / a commit-to-commit
 * diff), NEVER from a filesystem path the caller opens — so it cannot block on an agent-planted FIFO.
 * Returns the captured text (≤ maxBytes + one final chunk) and whether it was truncated. Never throws on a
 * git error (a failed command yields empty text, not truncated) — a memory guard, not a validity gate.
 */
async function gitBoundedStdout(
  cwd: string,
  argv: string[],
  maxBytes: number,
): Promise<{ text: string; truncated: boolean }> {
  const proc = Bun.spawn(["git", ...argv], {
    cwd,
    env: gitEnv(),
    stdout: "pipe",
    stderr: "ignore",
    stdin: "ignore",
  });
  const reader = proc.stdout.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  let truncated = false;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      if (value && value.byteLength > 0) {
        chunks.push(value);
        total += value.byteLength;
        if (total > maxBytes) {
          truncated = true;
          break; // stop reading — never buffer past the cap (+ at most the chunk that tripped it)
        }
      }
    }
  } finally {
    await reader.cancel().catch(() => {});
    proc.kill();
    await proc.exited.catch(() => {});
  }
  const buf = new Uint8Array(total);
  let off = 0;
  for (const c of chunks) {
    buf.set(c, off);
    off += c.byteLength;
  }
  return { text: new TextDecoder().decode(buf), truncated };
}

/**
 * Read `git diff <base> <branch>` in `cwd`, BOUNDED to `maxBytes` — the verifier judge's run diff. It is
 * COMMIT-to-commit (never the working tree), so it reflects exactly what the agent COMMITTED (tamper-safe)
 * and cannot block on a worktree FIFO. Fails the verification CLOSED on `truncated` (see {@link gitBoundedStdout}).
 */
export async function gitDiffBounded(
  cwd: string,
  base: string,
  branch: string,
  maxBytes: number,
): Promise<{ text: string; truncated: boolean }> {
  // `--no-ext-diff --no-textconv`: this is the one PATCH-producing diff run in the agent worktree, so it
  // would otherwise invoke an agent-configured `diff.external`/`textconv` driver. Suppressing them (belt to
  // the scrubbed-env suspenders) means such a driver never runs at all. See the key-confinement note above.
  return gitBoundedStdout(cwd, ["diff", "--no-ext-diff", "--no-textconv", base, branch], maxBytes);
}

/**
 * Read the blob at `<ref>:<pathspec>` from git's object store in `cwd`, BOUNDED to `maxBytes`. `pathspec` is
 * repo-root-relative (POSIX `/`). This reads the COMMITTED content at `ref` (the agent-immutable base
 * captured at dispatch) — NEVER the working tree — so the verifier's rubric + brief inputs cannot be tampered
 * by an uncommitted worktree edit (a committed edit to a protected path is caught by Stage 0), and the read
 * cannot block on an agent-planted worktree FIFO. A missing path / bad ref yields empty text (git show exits
 * non-zero, stderr ignored, empty stdout) — the caller treats that as fail-closed for the rubric.
 */
export async function gitShowBounded(
  cwd: string,
  ref: string,
  pathspec: string,
  maxBytes: number,
): Promise<{ text: string; truncated: boolean }> {
  return gitBoundedStdout(cwd, ["show", `${ref}:${pathspec}`], maxBytes);
}

/**
 * Stage `paths` (repo-relative) and commit them with `message`. Pathspec-limited on
 * BOTH add and commit, so it never sweeps unrelated staged or working-tree changes —
 * only the given paths land in the commit. Throws {@link GitError} on any failure so
 * the caller can roll back the FS (the commit is the transaction, FLOWS §F1).
 */
export async function gitCommit(cwd: string, paths: string[], message: string): Promise<void> {
  const add = await git(cwd, ["add", "--", ...paths]);
  if (add.code !== 0) throw new GitError(["add", "--", ...paths], add.code, add.stderr);
  const commit = await git(cwd, ["commit", "-m", message, "--", ...paths]);
  if (commit.code !== 0) throw new GitError(["commit", "-m", message], commit.code, commit.stderr);
}

/**
 * Best-effort unstage of `paths` (repo-relative) — rolls back a partial `git add` whose commit
 * then failed, so a failed intent leaves the git INDEX clean too, not just the working tree
 * ("nothing half-created", FLOWS §F1). `git rm --cached` works on an unborn branch (no HEAD yet)
 * where `git reset -- <path>` would error; `-r` handles the mission directory, `--ignore-unmatch`
 * makes it a no-op when nothing was staged (e.g. the `git add` itself failed). Never throws.
 */
export async function gitUnstage(cwd: string, paths: string[]): Promise<void> {
  await git(cwd, ["rm", "--cached", "-r", "-q", "--ignore-unmatch", "--", ...paths]);
}

// ── Approve/merge primitives (BRO-1802, D1) ─────────────────────────────────────
// Approve = squash-merge `run/<id>` onto the workspace branch with the verdict-freshness ladder.
// These run in the WORKSPACE repo (the runtime owns it), so like every git spawn here they inherit the
// scrubbed env (key-confinement) — a `merge`/`textconv`/hook driver an agent planted in the shared config
// can neither read a host secret nor (for the merge below) run an external diff.

/** Resolve `ref` (a branch, tag, or sha) to its full 40-char commit sha (`rev-parse <ref>^{commit}`). Used to
 *  normalize the verdict's `base` and compare it to the workspace branch tip. Throws {@link GitError} on a bad ref. */
export async function gitRevParse(cwd: string, ref: string): Promise<string> {
  const r = await git(cwd, ["rev-parse", `${ref}^{commit}`]);
  if (r.code !== 0) throw new GitError(["rev-parse", `${ref}^{commit}`], r.code, r.stderr);
  return r.stdout.trim();
}

/** The set of paths that differ between commits `a` and `b` (`git diff --name-only <a> <b>`). Read with
 *  `-z` (NUL-terminated, raw paths — never C-quoted) + `--no-renames` (a rename is a delete+add, so both
 *  paths surface) + `--no-ext-diff --no-textconv` (an agent-configured driver never runs — a `--name-only`
 *  diff already avoids content drivers, but stay uniform with {@link gitDiffBounded}). Used for the freshness
 *  ladder's file-overlap test. Throws {@link GitError} on a git failure (a bad ref must not read as "no changes"). */
export async function gitChangedFiles(cwd: string, a: string, b: string): Promise<string[]> {
  const r = await git(cwd, [
    "diff",
    "--no-ext-diff",
    "--no-textconv",
    "--name-only",
    "-z",
    "--no-renames",
    a,
    b,
  ]);
  if (r.code !== 0) throw new GitError(["diff", "--name-only", "-z", a, b], r.code, r.stderr);
  return r.stdout.split("\0").filter((p) => p.length > 0);
}

/** True when `cwd`'s working tree + index are clean (`git status --porcelain` empty). The squash-merge lands
 *  in the live workspace checkout, so a dirty tree (uncommitted human edits) makes the merge unsafe — the
 *  approve refuses rather than clobber. Throws {@link GitError} on a git failure. */
export async function gitIsClean(cwd: string): Promise<boolean> {
  const r = await git(cwd, ["status", "--porcelain"]);
  if (r.code !== 0) throw new GitError(["status", "--porcelain"], r.code, r.stderr);
  return r.stdout.trim().length === 0;
}

/** Stage a squash of `branch` onto the current checkout WITHOUT committing (`git merge --squash`). Resolves
 *  even on a conflicting merge (nonzero code) so the caller can roll back + report `stale`; disjoint file
 *  sets (the only case the ladder reaches here) never conflict. Does NOT create a merge commit — the caller
 *  commits the staged squash as one commit ("one commit per approved run", D1). */
export async function gitMergeSquash(cwd: string, branch: string): Promise<GitResult> {
  return git(cwd, ["merge", "--squash", branch]);
}

/** Commit everything currently staged with `message`, returning the new HEAD sha. `--no-verify` skips
 *  commit/pre-commit hooks (an agent-planted hook in the shared repo never runs on the runtime's merge
 *  commit). Throws {@link GitError} on failure (e.g. nothing staged). */
export async function gitCommitAllStaged(cwd: string, message: string): Promise<string> {
  const commit = await git(cwd, ["commit", "--no-verify", "-m", message]);
  if (commit.code !== 0) throw new GitError(["commit", "--no-verify"], commit.code, commit.stderr);
  return gitRevParse(cwd, "HEAD");
}

/** Hard-reset `cwd` to `ref` (default HEAD) — rolls back a conflicted `git merge --squash` (which leaves no
 *  MERGE_HEAD, so `git merge --abort` cannot). Safe only because approve requires a clean tree first, so the
 *  reset restores exactly that clean state. Throws {@link GitError} on failure. */
export async function gitResetHard(cwd: string, ref = "HEAD"): Promise<void> {
  const r = await git(cwd, ["reset", "--hard", ref]);
  if (r.code !== 0) throw new GitError(["reset", "--hard", ref], r.code, r.stderr);
}

/** Rename branch `from` to `to` (`git branch -m`). On merge the run branch `run/<id>` becomes
 *  `archive/run-<id>` — the branch is the receipt (D1), never deleted. The branch must not be checked out in
 *  any worktree (remove the run worktree first). Throws {@link GitError} on failure. */
export async function gitRenameBranch(cwd: string, from: string, to: string): Promise<void> {
  const r = await git(cwd, ["branch", "-m", from, to]);
  if (r.code !== 0) throw new GitError(["branch", "-m", from, to], r.code, r.stderr);
}

// ── Worktree primitives (BRO-1746) ─────────────────────────────────────────────
// The runtime runs each agent in a `git worktree` on a `run/<id>` branch — phase-1 isolation
// (ARCHITECTURE §5). These are the raw git ops the sandbox adapter (sandbox/worktree.ts) composes;
// BRO-1779 (reap) and eventual cleanup reuse them, so they live here in the git surface.

/** One entry of `git worktree list --porcelain` — path, branch, and whether git deems it prunable
 *  (its checkout dir has vanished — the admin entry under `.git/worktrees/` outlived the working tree,
 *  e.g. after `rm -rf .maestro/`; such an entry looks "registered" but points at nothing). */
export interface Worktree {
  /** Absolute worktree path (as git recorded it — may no longer exist on disk if `prunable`). */
  path: string;
  /** Short branch name (refs/heads/ stripped), or null for a detached-HEAD worktree. */
  branch: string | null;
  /** True when git flagged the entry `prunable` (working dir gone) — a stale, not-live registration. */
  prunable: boolean;
}

/**
 * List the repo's worktrees. Parses the stable `--porcelain` format (blank-line-separated blocks,
 * each starting `worktree <path>`), so it never breaks on the human format's alignment. Captures the
 * `prunable` annotation — a registered-but-missing worktree is NOT a live one, and a caller that
 * treats it as live (skipping a needed re-add) hands back a Sandbox whose cwd does not exist.
 */
export async function gitWorktreeList(cwd: string): Promise<Worktree[]> {
  const r = await git(cwd, ["worktree", "list", "--porcelain"]);
  if (r.code !== 0) throw new GitError(["worktree", "list", "--porcelain"], r.code, r.stderr);
  const out: Worktree[] = [];
  let path: string | null = null;
  let branch: string | null = null;
  let prunable = false;
  const flush = () => {
    if (path !== null) out.push({ path, branch, prunable });
    path = null;
    branch = null;
    prunable = false;
  };
  for (const line of r.stdout.split("\n")) {
    if (line.startsWith("worktree ")) {
      flush();
      path = line.slice("worktree ".length);
    } else if (line.startsWith("branch ")) {
      branch = line.slice("branch ".length).replace(/^refs\/heads\//, "");
    } else if (line === "prunable" || line.startsWith("prunable ")) {
      prunable = true;
    }
    // "HEAD <sha>", "detached", "bare", "locked" — not needed here
  }
  flush();
  return out;
}

/** Prune stale worktree admin entries (registered dirs that no longer exist). Idempotent, never a
 *  no-op error — clears the `.git/worktrees/<id>` entries a `rm -rf` left behind so a re-add succeeds. */
export async function gitWorktreePrune(cwd: string): Promise<void> {
  await git(cwd, ["worktree", "prune"]);
}

/**
 * Add a worktree at `path` checked out to `branch`. Creates the branch (`-b`) for a fresh run; if that
 * branch already exists (a re-dispatch of a run id whose worktree was removed but branch kept — "the
 * branch is the receipt"), attaches it instead. SELF-HEALS: a stale `.git/worktrees/` admin entry (a
 * prunable/registered-but-missing worktree) blocks BOTH forms with exit 128 ("already used by
 * worktree at <gone path>"); on that failure it prunes and retries once. Throws {@link GitError} only
 * if the add still fails after a clean prune.
 */
export async function gitWorktreeAdd(cwd: string, path: string, branch: string): Promise<void> {
  const attempt = async (): Promise<GitResult | null> => {
    const fresh = await git(cwd, ["worktree", "add", path, "-b", branch]);
    if (fresh.code === 0) return null;
    // -b failed — most likely the branch already exists; retry attaching it (no -b).
    const attach = await git(cwd, ["worktree", "add", path, branch]);
    return attach.code === 0 ? null : attach;
  };
  const first = await attempt();
  if (first === null) return;
  // A stale admin entry (prunable worktree) can block both forms — prune and retry once.
  await gitWorktreePrune(cwd);
  const second = await attempt();
  if (second === null) return;
  throw new GitError(["worktree", "add", path, branch], second.code, second.stderr);
}

/**
 * Remove a worktree's working directory. `force` is needed when it holds uncommitted/untracked
 * changes — safe for a completed run because the receipt lives in the branch commits + `runs/run-<id>/`,
 * not the ephemeral working tree. NEVER deletes the branch (that is the receipt). Throws on failure.
 */
export async function gitWorktreeRemove(
  cwd: string,
  path: string,
  opts: { force?: boolean } = {},
): Promise<void> {
  const args = ["worktree", "remove", ...(opts.force ? ["--force"] : []), path];
  const r = await git(cwd, args);
  if (r.code !== 0) throw new GitError(args, r.code, r.stderr);
}
