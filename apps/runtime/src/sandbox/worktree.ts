// The phase-1 sandbox: a git worktree on a `run/<id>` branch (ARCHITECTURE §5). This is the concrete
// implementation behind the Sandbox port — the one a phase-2 container implementation replaces without
// touching a single caller. Layout (all under the workspace, all scanner/git-invisible):
//
//   workdir  = <workspace>/.maestro/worktrees/run-<id>   the child's cwd — a git worktree checkout.
//              Under `.maestro/` DELIBERATELY: it is in the scanner's SKIP_DIRS, so the worktree's
//              copy of the workspace `_work.md` files is NOT indexed as duplicate nodes (a real bug if
//              the worktree were scanned), and `.maestro/` is gitignored so the checkout never dirties
//              the main tree's `git status`.
//   runDir   = <workspace>/runs/run-<id>                 the receipts dir (contract.json, session.jsonl,
//              child.stderr.log, progress.md, verdicts) — OUTSIDE the worktree, supervisor-owned. The
//              watcher suppresses `runs/…` so its churn never wakes a reconcile.
//   branch   = run/<id>                                  "the branch is the receipt" (ARCHITECTURE §3a).
//
// create() is idempotent: a fresh-context respawn (HARNESS §5 — exit 10, same session id, same
// worktree, same budget) re-attaches the existing worktree instead of failing.

import { mkdir, realpath, stat } from "node:fs/promises";
import { join, resolve } from "node:path";
import { gitWorktreeAdd, gitWorktreeList, gitWorktreePrune, gitWorktreeRemove } from "../git/git";
import type {
  ResourceLimits,
  Sandbox,
  SandboxExecOptions,
  SandboxExecResult,
  SandboxFactory,
  SandboxSpawnContext,
} from "./sandbox";

/** Configuration for the worktree sandbox factory. Only `workspace` is required; the roots default
 *  under it. Both roots are overridable so tests (and a future multi-tenant layout) can relocate them. */
export interface WorktreeSandboxConfig {
  /** The runtime's git repo root (RuntimeConfig.workspace). Worktrees branch off THIS repo. */
  workspace: string;
  /** Where worktree checkouts live. Default `<workspace>/.maestro/worktrees` (SKIP_DIRS + gitignored). */
  worktreesRoot?: string;
  /** Where receipts live. Default `<workspace>/runs` (matches the harness convention + watcher). */
  runsRoot?: string;
}

// A run id becomes a path segment AND a git branch component, so it must be a single safe token —
// no path separators, no `..`, nothing that could escape the worktree root or forge a branch ref.
const SAFE_RUN_ID = /^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$/;

function assertRunId(runId: string): void {
  if (!SAFE_RUN_ID.test(runId)) {
    throw new Error(
      `invalid run id ${JSON.stringify(runId)} — must be [A-Za-z0-9._-], no path separators or leading/trailing punctuation`,
    );
  }
}

/** Canonicalize a path for comparison against `git worktree list` (which reports realpath'd paths).
 *  Falls back to path.resolve when the path does not exist yet (a first create), which never matches
 *  a real registered worktree — so the idempotency check adds on first create, reuses on respawn. */
async function canonical(p: string): Promise<string> {
  try {
    return await realpath(p);
  } catch {
    return resolve(p);
  }
}

/** True if a path exists on disk. */
async function exists(p: string): Promise<boolean> {
  try {
    await stat(p);
    return true;
  } catch {
    return false;
  }
}

/** Is `workdir` a git worktree that is BOTH registered AND present on disk? A registered-but-missing
 *  (prunable) entry — the `.git/worktrees/<id>/` that outlives a `rm -rf .maestro/` — is NOT live: a
 *  reuse would hand back a Sandbox whose cwd does not exist. Checked by both create (reuse guard) and
 *  teardown (idempotency). */
async function isLiveWorktree(workspace: string, workdir: string): Promise<boolean> {
  if (!(await exists(workdir))) return false;
  const target = await canonical(workdir);
  const list = await gitWorktreeList(workspace);
  for (const w of list) {
    if (w.prunable) continue; // stale registration, not live
    if ((await canonical(w.path)) === target) return true;
  }
  return false;
}

/** The phase-1 Sandbox: a git worktree. Constructed by the factory once paths are provisioned. */
class WorktreeSandbox implements Sandbox {
  constructor(
    readonly runId: string,
    readonly workdir: string,
    readonly runDir: string,
    readonly branch: string,
    readonly resources: ResourceLimits,
    private readonly workspace: string,
  ) {}

  // ENTER: phase-1 spawns the child directly in the worktree — no exec wrapper, no injected env.
  // Phase-2 returns a `["docker","exec",…]` prefix + the in-container cwd here; the caller is unchanged.
  spawnContext(): SandboxSpawnContext {
    return { cwd: this.workdir, commandPrefix: [], env: {} };
  }

  // EXEC: run a command inside the worktree and capture output (verifier checks, git). Inherits the
  // host env (this is the SUPERVISOR running trusted checks — NOT the untrusted agent child, which
  // gets the allowlisted env from buildChildEnv). An empty command is a caller bug, not a spawn.
  async exec(command: readonly string[], opts?: SandboxExecOptions): Promise<SandboxExecResult> {
    if (command.length === 0) throw new Error("sandbox.exec requires a non-empty command");
    const cwd = opts?.cwd ?? this.workdir;
    const env = opts?.env ? { ...process.env, ...opts.env } : undefined;
    const proc = Bun.spawn([...command], { cwd, env, stdout: "pipe", stderr: "pipe" });
    const [stdout, stderr, code] = await Promise.all([
      new Response(proc.stdout).text(),
      new Response(proc.stderr).text(),
      proc.exited,
    ]);
    return { code, stdout, stderr };
  }

  // TEARDOWN: preserve (default) keeps everything — the receipt. `preserve: false` frees only the
  // worktree working dir (force: a completed run's uncommitted delta is not a receipt; the branch
  // commits + runDir are). The branch and runDir are NEVER removed here. IDEMPOTENT: the reap path
  // (BRO-1779) may call teardown more than once (double-stop, crash-then-cleanup), so an already-gone
  // worktree is a no-op, not a throw — `git worktree remove` exits 128 on an unregistered path. Mirror
  // gitUnstage's "never throws on an already-clean state" philosophy.
  async teardown(opts?: { preserve?: boolean }): Promise<void> {
    const preserve = opts?.preserve ?? true;
    if (preserve) return; // keep the worktree receipt (crash/kill, or a pending verify/review)
    if (!(await isLiveWorktree(this.workspace, this.workdir))) {
      // already removed (a prior teardown) or registered-but-missing (prunable) — clear any stale
      // admin entry so the slate is clean for a future re-dispatch, then no-op.
      await gitWorktreePrune(this.workspace).catch(() => {});
      return;
    }
    await gitWorktreeRemove(this.workspace, this.workdir, { force: true });
  }
}

/** Build the phase-1 (worktree) sandbox factory. */
export function createWorktreeSandboxFactory(cfg: WorktreeSandboxConfig): SandboxFactory {
  const worktreesRoot = cfg.worktreesRoot ?? join(cfg.workspace, ".maestro", "worktrees");
  const runsRoot = cfg.runsRoot ?? join(cfg.workspace, "runs");

  return {
    async create(runId: string, opts?: { resources?: ResourceLimits }): Promise<Sandbox> {
      assertRunId(runId);
      const workdir = join(worktreesRoot, `run-${runId}`);
      const runDir = join(runsRoot, `run-${runId}`);
      const branch = `run/${runId}`;

      // The receipts dir always exists (contract snapshot + tee write into it immediately after).
      await mkdir(runDir, { recursive: true });
      // git worktree add creates the leaf, but the parent must exist first.
      await mkdir(worktreesRoot, { recursive: true });

      // Idempotent: a fresh-context respawn reuses the SAME worktree (HARNESS §5) — but ONLY if it is
      // LIVE (registered AND present on disk). A registered-but-missing (prunable) entry — the
      // `.git/worktrees/<id>/` that survives a `rm -rf .maestro/` (the design invites this: .maestro/
      // is a documented rebuildable cache) — must NOT be reused: that would hand back a Sandbox whose
      // cwd does not exist and ENOENT-crash the first child spawn. When not live, re-add;
      // gitWorktreeAdd self-heals a stale admin entry (prune + retry) and attaches the kept branch.
      if (!(await isLiveWorktree(cfg.workspace, workdir))) {
        await gitWorktreeAdd(cfg.workspace, workdir, branch);
      }

      return new WorktreeSandbox(
        runId,
        workdir,
        runDir,
        branch,
        opts?.resources ?? {},
        cfg.workspace,
      );
    },
  };
}
