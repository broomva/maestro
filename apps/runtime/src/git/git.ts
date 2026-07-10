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

/** One entry of `git worktree list --porcelain` — path plus the branch it has checked out (if any). */
export interface Worktree {
  /** Absolute worktree path. */
  path: string;
  /** Short branch name (refs/heads/ stripped), or null for a detached-HEAD worktree. */
  branch: string | null;
}

/**
 * List the repo's worktrees. Parses the stable `--porcelain` format (blank-line-separated blocks,
 * each starting `worktree <path>`), so it never breaks on the human format's alignment. Used by the
 * sandbox factory to make `create` idempotent (a fresh-context respawn reuses the SAME worktree).
 */
export async function gitWorktreeList(cwd: string): Promise<Worktree[]> {
  const r = await git(cwd, ["worktree", "list", "--porcelain"]);
  if (r.code !== 0) throw new GitError(["worktree", "list", "--porcelain"], r.code, r.stderr);
  const out: Worktree[] = [];
  let path: string | null = null;
  let branch: string | null = null;
  const flush = () => {
    if (path !== null) out.push({ path, branch });
    path = null;
    branch = null;
  };
  for (const line of r.stdout.split("\n")) {
    if (line.startsWith("worktree ")) {
      flush();
      path = line.slice("worktree ".length);
    } else if (line.startsWith("branch ")) {
      branch = line.slice("branch ".length).replace(/^refs\/heads\//, "");
    }
    // "HEAD <sha>", "detached", "bare", "locked", blank — not needed here
  }
  flush();
  return out;
}

/**
 * Add a worktree at `path` checked out to `branch`. Creates the branch (`-b`) for a fresh run; if
 * that branch already exists (a re-dispatch of a run id whose worktree was removed but branch kept —
 * "the branch is the receipt"), attaches the existing branch instead. Throws {@link GitError} if
 * neither works (e.g. the path is occupied — the factory guards that case by checking the list first).
 */
export async function gitWorktreeAdd(cwd: string, path: string, branch: string): Promise<void> {
  const fresh = await git(cwd, ["worktree", "add", path, "-b", branch]);
  if (fresh.code === 0) return;
  // -b failed — most likely the branch already exists; retry attaching it (no -b).
  const attach = await git(cwd, ["worktree", "add", path, branch]);
  if (attach.code !== 0) {
    throw new GitError(
      ["worktree", "add", path, branch],
      attach.code,
      attach.stderr || fresh.stderr,
    );
  }
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
