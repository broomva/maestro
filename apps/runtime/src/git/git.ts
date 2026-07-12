// Minimal git surface for the runtime (BRO-1820). The runtime owns the workspace
// git repo — "the branch is the receipt" (ARCHITECTURE §3a). new_mission (FLOWS §F1)
// makes the commit the transaction: folder + `_work.md` are created, then committed;
// if the commit fails the caller removes what it created, so nothing is half-landed.
//
// argv form ONLY (Bun.spawn with an array) — never a shell string. The commit message
// and paths derive from user input (a mission title / parentPath), so shelling out
// would be an injection surface. spawn's array form passes them as literal argv.

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
  const proc = Bun.spawn(["git", ...args], { cwd, stdout: "pipe", stderr: "pipe" });
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
