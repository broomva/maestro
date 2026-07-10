// The sandbox port (ARCHITECTURE §5) — "a sandbox I run in." The runtime↔execution boundary drawn
// ONCE so phase-2 physical isolation (containers / microVMs, STACK §"Where Rust does enter") is a
// SWAP, not a rewrite. Phase-1 is logical isolation: a git worktree on a `run/<id>` branch, sharing
// the host shell (honest limit — contamination-resistance, not containment; the runtime is the user's
// own, their trust boundary). The same four verbs — create · enter · exec · teardown — describe a
// worktree and a container identically; only the implementation behind this interface changes.
//
//   create   → SandboxFactory.create(runId)         provisions the sandbox, returns a handle
//   enter    → Sandbox.spawnContext()               how to launch a process INSIDE it (phase-2: exec prefix)
//   exec     → Sandbox.exec(command)                run a command inside + capture output (checks/verifier/git)
//   teardown → Sandbox.teardown({ preserve })       free it, or preserve the receipt (kill/crash)
//
// The resource hooks (cpu/mem/wall-clock) are recorded phase-1 (a worktree can't enforce them) and
// enforced phase-2 — the field exists now so the dispatch call site (BRO-1779) is already shaped for
// the swap. The interface-conformance suite (sandbox/conformance.ts) is the same suite a phase-2
// container implementation must pass.

/** Resource limits for a run. Advisory in phase-1 (worktree shares the host); enforced in phase-2. */
export interface ResourceLimits {
  /** Max CPUs the run may use (phase-2 cgroup). */
  cpuCount?: number;
  /** Max resident memory in MiB (phase-2 cgroup). */
  memoryMb?: number;
  /** Wall-clock ceiling in ms after which the run is force-stopped (phase-2). */
  wallClockMs?: number;
}

/** How to spawn a process INSIDE a sandbox — the phase-2 seam. Phase-1: just `cwd`. Phase-2: a
 *  `commandPrefix` like `["docker","exec","-i",<id>]` wraps the child argv, and `cwd` is the
 *  in-container path. BRO-1779 spawns the agent child through this, blind to which phase it is. */
export interface SandboxSpawnContext {
  /** Working directory the process runs in (phase-1: the worktree path). */
  cwd: string;
  /** Argv prefix that enters the sandbox (phase-2 container-exec); empty phase-1. */
  commandPrefix: readonly string[];
  /** Sandbox-injected env additions (phase-1 empty; phase-2 may add container hints). */
  env: Record<string, string>;
}

/** Options for a one-shot command run inside the sandbox. */
export interface SandboxExecOptions {
  /** Run relative to this dir instead of the sandbox root (must stay inside the sandbox). */
  cwd?: string;
  /** Extra env for this command only. */
  env?: Record<string, string>;
}

/** The captured result of a one-shot exec (mirrors GitResult — code + streams). */
export interface SandboxExecResult {
  code: number;
  stdout: string;
  stderr: string;
}

/**
 * A provisioned sandbox — one run's isolated execution context. The handle carries the paths the rest
 * of the harness needs (the child's cwd, the receipts dir) plus the enter/exec/teardown verbs.
 */
export interface Sandbox {
  /** The run id this sandbox belongs to (= session id). */
  readonly runId: string;
  /** The child's working directory — where the agent's work happens (phase-1: the worktree path). */
  readonly workdir: string;
  /** The receipts dir `runs/run-<id>/` — contract snapshot, session.jsonl, stderr log, verdicts.
   *  OUTSIDE the worktree (supervisor-owned; the child writes progress.md/fix_plan.md here via
   *  BROOMVA_RUN_DIR, but never session.jsonl — one writer, HARNESS §2). */
  readonly runDir: string;
  /** The run's git branch `run/<id>` — "the branch is the receipt" (ARCHITECTURE §3a). */
  readonly branch: string;
  /** The resource limits requested for this run (advisory phase-1). */
  readonly resources: ResourceLimits;

  /** ENTER — how to spawn the long-lived agent child inside this sandbox (the phase-2 seam). */
  spawnContext(): SandboxSpawnContext;

  /** EXEC — run a command inside and capture its output (verifier checks, git ops). */
  exec(command: readonly string[], opts?: SandboxExecOptions): Promise<SandboxExecResult>;

  /**
   * TEARDOWN — end the run's execution context. `preserve` defaults to TRUE: the receipt-preserving
   * default keeps the worktree so a crash/kill (or a still-pending verify/review) can inspect it
   * (HARNESS §4: crash → worktree preserved). `preserve: false` frees the worktree working dir for
   * explicit cleanup. EITHER way the `run/<id>` branch and `runs/run-<id>/` are kept — they are the
   * receipts, never destroyed by teardown.
   */
  teardown(opts?: { preserve?: boolean }): Promise<void>;
}

/** Provisions sandboxes — one factory per isolation phase. Phase-1 = worktree; phase-2 = container. */
export interface SandboxFactory {
  /** CREATE — provision (or idempotently re-attach, for a fresh-context respawn) the run's sandbox. */
  create(runId: string, opts?: { resources?: ResourceLimits }): Promise<Sandbox>;
}
