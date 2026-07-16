// The supervisor (FLOWS §F2 dispatch + §F3 reap, HARNESS §4 exit-code matrix) — the heart of Loop 1.
// One process per run; the runtime survives ANY child crash. This module owns the supervisor SIDE of
// the harness seam: it spawns a child PROCESS, tees its stdout through `superviseChildStdio` (BRO-1767),
// and reaps its exit code into session/node state transitions + lifecycle events. The child's own
// iteration loop (FLOWS §F3 — budget-guard-in-path, model→tools, stop-condition) runs INSIDE the child;
// the real Agent-SDK child (`broomva-child`) is spawned through the INJECTABLE `spawnChild` seam, which
// tests drive with fixture children — so the whole exit-code matrix is exercisable without the SDK.
//
// Dispatch is the happy path in order (§F2):
//   lease the node → read its resolved contract → mint a run id → insert session + zeroed run_budget →
//   provision the sandbox (worktree) → freeze the contract snapshot → mint the per-session proxy token →
//   build the allowlisted child env → SPAWN the child → wire the tee → node.state = running → return
//   once the child is LIVE (the reap runs in the background).
//
// Reap maps the child's REAL exit code (ground truth) per HARNESS §4:
//   0  → claims complete → run the verifier (F4, Loop 2): Stage 0 tamper/diff guard + Stage 1 checks +
//        Stage 2 LLM judge (when the contract pins a rubric) against base..run/<id>; pass → park review
//        (F5), fail → respawn with fix_plan OR park blocked at a cap, infra error → park blocked (no
//        attempt burned).
//   10 → stopped → read the child's declared reason; fresh_context → IMMEDIATE respawn (same run id,
//        same worktree, same run_budget — budgets span attempts, not processes); every other reason
//        (budget | iteration_cap | no_progress | user_stop) → park blocked
//   20 → needs input → open a question gate → park review
//   crash (any other code / signal / a spawn|provision throw) → park blocked + run.failed, worktree
//        PRESERVED; the runtime keeps serving (the SIGKILL-containment invariant)
// The child's self-declared `run.exiting {code}` is cross-checked against the real exit code; a
// disagreement emits `run.exit_mismatch` (a Loop-4 harness-bug signal) and the REAL code wins the route.

import { randomBytes, randomUUID } from "node:crypto";
import type {
  Actor,
  Budget,
  EventType,
  OrchState,
  SessionStatus,
  WorkContract,
} from "@maestro/protocol";
import {
  EVENT_TYPES,
  effectiveProtect,
  isValidRubricRef,
  normalizeChecks,
  parseWorkFile,
} from "@maestro/protocol";
import { and, desc, eq, gt, max } from "drizzle-orm";
import {
  DEFAULT_JUDGE_DIFF_MAX_BYTES,
  DEFAULT_MAX_ITERATIONS,
  DEFAULT_VERIFIER_MAX_ATTEMPTS,
  type RuntimeConfig,
} from "../config";
import type { IndexDb } from "../db/client";
import { projectLiveNode } from "../db/project";
import { event, gate, lease, node, runBudget, session } from "../db/schema";
import { gitDiffBounded, gitHead, gitShowBounded } from "../git/git";
import { writeContractSnapshot } from "../harness/contract-snapshot";
import type { ChildEmittedEvent } from "../harness/runner";
import type { ChildRole } from "../harness/spawn-contract";
import { buildChildEnv, serializeChildArgv } from "../harness/spawn-contract";
import {
  bindIndexWriter,
  type ChildStdioPort,
  fromBunSubprocess,
  fsJournal,
  SessionTee,
  superviseChildStdio,
} from "../harness/stdio";
import type { SessionTokenRegistry } from "../proxy/tokens";
import type { Sandbox, SandboxFactory } from "../sandbox/sandbox";
import { WORK_FILE } from "../scanner/scanner";
import { proxyJudgeCaller } from "../verifier/judge";
import {
  type RunVerificationDeps,
  type RunVerificationResult,
  runVerification,
} from "../verifier/run";
import { type RunEntry, RunRegistry } from "./registry";

/** The role every Loop-1 dispatch spawns. Verifier (Loop 2) + orchestrator (F6) are later runners. */
const AGENT_ROLE: ChildRole = "agent";

/** The role the Stage-2 judge's model call bills under (HARNESS §3, §7) — drives the proxy's verifier
 *  model pin. Minted per verify (a rubric was pinned) + revoked right after, distinct from the agent bearer. */
const VERIFIER_ROLE: ChildRole = "verifier";

/** How long a node's dispatch lease is held (dedup lock, not a run-state lock). Released on terminal
 *  reap; a runtime crash mid-run leaves it to expire (GC + crash-recovery reconcile = F9/BRO-1814). */
const DISPATCH_LEASE_TTL_MS = 24 * 60 * 60 * 1000;

/** Defense-in-depth bound on fresh-context respawns before parking blocked. The REAL iteration/no-
 *  progress guardrail is the child's own budget-in-path (BRO-1795); this only stops a pathological
 *  child that emits `fresh_context` forever from spinning the supervisor without limit. */
const DEFAULT_MAX_RESPAWNS = 50;

/** F8 kill grace (BRO-1912): send SIGTERM first so a child's signal handler can reap any grandchild it
 *  spawned (the subscription `claude` CLI), then SIGKILL if it outlives cooperation. Short — the run is
 *  ending either way; this only gives the runner a beat to tear its CLI down instead of orphaning it. */
const DEFAULT_KILL_GRACE_MS = 2000;

/** The one binary a Loop-1 child runs (HARNESS §1 argv `broomva-child --role … --session …`). */
export const CHILD_BIN = "broomva-child";

// ── The injectable spawn seam ────────────────────────────────────────────────
// The ONE place a real OS process is created. Default = `Bun.spawn` of the child binary inside the
// sandbox; tests inject a fixture spawner returning a scripted `ChildStdioPort`. A spawn THROW (ENOENT:
// broomva-child missing) propagates synchronously — dispatch catches it as a crash (node blocked +
// run.failed, worktree preserved).

/** The arguments the supervisor hands the spawn seam. */
export interface SpawnChildArgs {
  /** `serializeChildArgv({ role, session })` → `["--role","agent","--session",<id>]`. */
  argv: string[];
  /** The allowlisted child env (no host secrets) from `buildChildEnv`. */
  env: Record<string, string>;
  /** The sandbox's `spawnContext().cwd` — the worktree the child runs in. */
  cwd: string;
  /** The sandbox's `spawnContext().commandPrefix` — empty phase-1, a container-exec prefix phase-2. */
  commandPrefix: readonly string[];
}

/** Create the child process (or throw). The narrow port makes dispatch + tests interchangeable. */
export type SpawnChild = (args: SpawnChildArgs) => ChildStdioPort;

/** The production spawner: `Bun.spawn([...prefix, broomva-child, ...argv])`, piped for the tee. */
export const defaultSpawnChild: SpawnChild = (args) => {
  const proc = Bun.spawn([...args.commandPrefix, CHILD_BIN, ...args.argv], {
    cwd: args.cwd,
    env: args.env,
    stdout: "pipe",
    stderr: "pipe",
    stdin: "pipe",
  });
  return fromBunSubprocess(proc);
};

// ── Deps + result types ──────────────────────────────────────────────────────

/** A minimal handle on the runtime's model proxy — the child dials `url` as BROOMVA_MODEL_PROXY. */
export interface ProxyRef {
  url: string;
  /** Set in unix-socket mode (carried out-of-band; loopback `url` is dialable directly). */
  socketPath?: string;
}

export interface SupervisorDeps {
  db: IndexDb;
  /** Provisions sandboxes (phase-1 worktree; phase-2 container) — one per run, idempotent create. */
  factory: SandboxFactory;
  /** The supervisor-owned per-session bearer registry (HARNESS §3). */
  tokens: SessionTokenRegistry;
  /** The runtime's model proxy the child targets (BRO-1788). */
  proxy: ProxyRef;
  /** Create the child process — default `defaultSpawnChild`; tests inject a fixture spawner. */
  spawnChild?: SpawnChild;
  /** Epoch-ms clock — default `Date.now`; injected for deterministic tests (no ambient time). */
  now?: () => number;
  /** Mint a run id — default `randomBytes(4).hex`; injected for deterministic tests. Must satisfy the
   *  sandbox `SAFE_RUN_ID` charset ([A-Za-z0-9._-], alnum-bounded, no `..`, no `.lock`). */
  mintRunId?: () => string;
  /** This runtime's lease-holder id (default "runtime"). */
  holder?: string;
  /** Liveness cadences threaded to `superviseChildStdio` (default HARNESS §2 timings). */
  config?: RuntimeConfig;
  /** Host env `buildChildEnv` filters to the allowlist (default `process.env`). */
  hostEnv?: Record<string, string | undefined>;
  /** Fresh-context respawn safety bound (default 50). */
  maxRespawns?: number;
  /** F8 kill grace window in ms before SIGKILL (default 2000) — injected small/zero in tests. */
  killGraceMs?: number;
  /** Grace timer (default real `setTimeout`) — injected for deterministic kill tests (no wall-time). */
  delay?: (ms: number) => Promise<void>;
  /** Read the run diff for the Stage-2 judge (default {@link gitDiffBounded}). Injected so a test can drive
   *  the kill-during-verify race deterministically — the reap awaits this between the reap's own guards and
   *  the verifier-bearer mint, so a fixture that kills mid-read exercises the pre-mint kill guard. */
  readDiff?: typeof gitDiffBounded;
}

/** The terminal lifecycle event the reap derives (D-EVENTNAMES). `run.killed` = killed by intent (F8). */
export type TerminalEvent = "run.finished" | "run.failed" | "run.killed";

/** The outcome of a run reaped to terminal (following any fresh-context respawns to the final state). */
export interface ReapResult {
  runId: string;
  /** The child's REAL process exit code (ground truth; NaN/negative for a signal kill). */
  exitCode: number;
  /** The exit-10 reason the child declared, if any. */
  reason?: string;
  /** The terminal session status. */
  sessionStatus: SessionStatus;
  /** The terminal node state. */
  nodeState: OrchState;
  /** Which terminal lifecycle event the supervisor derived. */
  event: TerminalEvent;
  /** True when a crash (signal / unexpected code / spawn|provision failure) drove the terminal state. */
  crash: boolean;
  /** The question-gate id opened on exit 20, if any. */
  gateId?: string;
  /** True when the child's declared `run.exiting` code disagreed with the real exit code. */
  mismatch: boolean;
  /** How many fresh-context respawns preceded this terminal reap. */
  respawns: number;
}

/** The outcome of a dispatch call. `dispatched:true` once the child is LIVE (the reap runs in the
 *  background — await `reaped` for the terminal state). `dispatched:false` is a clean no-op. */
export type DispatchOutcome =
  | { dispatched: true; runId: string; reaped: Promise<ReapResult> }
  | { dispatched: false; reason: "lease_held" | "node_not_found" };

/** The public supervisor surface. */
export interface Supervisor {
  /** F2 — dispatch a node into a live run. Idempotent on the node lease (held → no-op). */
  dispatch(nodeId: string): Promise<DispatchOutcome>;
  /** F8 (BRO-1801) — SIGKILL a live run by intent + revoke its token; the reap ends it `canceled` +
   *  `run.killed`, worktree preserved. True if it was live. */
  kill(runId: string): boolean;
  /** F8 stop-all (BRO-1801) — kill every live run; returns how many were killed. */
  killAll(): number;
  /** F10 seam (BRO-1822) — the live run's control channel (chat/stop/ping), or null. */
  get(runId: string): RunEntry | null;
  /** Every live run — observability + shutdown sweep. */
  list(): RunEntry[];
  /** The live-run registry (for the seams that need direct reach-through). */
  readonly registry: RunRegistry;
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/** An ISO date (YYYY-MM-DD) derived from a STORED epoch — deterministic, never ambient time. */
function isoDate(epochMs: number): string {
  return new Date(epochMs).toISOString().slice(0, 10);
}

/** Reconstruct a node row's resolved `WorkContract` for the child's frozen snapshot (the inverse of
 *  the scanner's row build — `budgetJson`/`doneJson` were `JSON.stringify`'d there). */
function nodeRowToContract(row: typeof node.$inferSelect): WorkContract {
  return {
    id: row.id,
    kind: row.kind,
    state: row.state,
    owner: row.owner ?? undefined,
    gate: row.gate,
    budget: row.budgetJson ? (JSON.parse(row.budgetJson) as Budget) : undefined,
    done: row.doneJson ? JSON.parse(row.doneJson) : undefined,
    created: isoDate(row.createdAt),
    updated: isoDate(row.updatedAt),
  };
}

// ── Stage-2 judge input resolution (VERIFIER §2, BRO-1794 slice 1b-ii-B) ─────────
// The inputs the LLM judge is allowed to see, ALL read from the agent-immutable BASE commit's object store
// (`git show <base>:<path>`), never the working tree — so an uncommitted worktree edit can neither tamper
// with the grading rubric nor inject content into the judge prompt (rubric.md + _work.md are protected
// paths; base is the canonical copy), and the read cannot block on an agent-planted worktree FIFO. Each is
// BYTE-BOUNDED (the judge holds the whole input; an unbounded read would OOM the shared 24/7 supervisor).
// The rubric is fail-CLOSED (unusable → "" → parseRubric errors → park blocked, never a silent pass); the
// brief is best-effort context; the DIFF (`git diff base branch`, also commit-based) is read in verifyAndRoute.

/** Read the pinned judge rubric (`done.judge`) from the BASE commit — never the working tree. The ref is a
 *  work-subtree-relative path that "lives next to `_work.md`", resolved against the node's folder
 *  (`nodePath`) as a repo-root-relative pathspec, validated by {@link isValidRubricRef} first (a
 *  `..`/absolute/non-`.md` ref is rejected). Reading from `base` (where rubric.md is a PROTECTED path) is
 *  tamper-proof: an uncommitted worktree rewrite of the rubric is invisible, closing the reward-hacking
 *  vector (the writer cannot rewrite its own grading rubric). Fail-CLOSED: an invalid ref, an absent/over-cap
 *  blob → "" so `parseRubric` errors → verdict `error` → park blocked. Never throws. */
async function readRubricText(
  cwd: string,
  base: string,
  nodePath: string,
  ref: string,
  maxBytes: number,
): Promise<string> {
  if (!isValidRubricRef(ref)) return "";
  const pathspec = nodePath === "" ? ref : `${nodePath}/${ref}`;
  const { text, truncated } = await gitShowBounded(cwd, base, pathspec, maxBytes);
  return truncated ? "" : text;
}

/** The run's brief — the `_work.md` body the work was scoped from (the judge's context, VERIFIER §2 Stage 2,
 *  never a gate). Read from the BASE commit (not the working tree) so an uncommitted worktree edit can't
 *  inject content into the judge prompt; `_work.md` is a PROTECTED path, so base IS the canonical body.
 *  Best-effort: an absent/over-cap/malformed `_work.md` yields "" (the judge prompt handles an empty brief).
 *  Never throws. */
async function readBrief(
  cwd: string,
  base: string,
  nodePath: string,
  maxBytes: number,
): Promise<string> {
  const pathspec = nodePath === "" ? WORK_FILE : `${nodePath}/${WORK_FILE}`;
  try {
    const { text, truncated } = await gitShowBounded(cwd, base, pathspec, maxBytes);
    return truncated ? "" : parseWorkFile(text).brief;
  } catch {
    return "";
  }
}

/** Everything one run threads through dispatch → launch → reap → respawn. */
interface RunContext {
  runId: string;
  nodeId: string;
  contract: WorkContract;
  budget: Budget;
  sandbox: Sandbox;
  /** The commit the run branched from — captured ONCE at dispatch (the worktree's HEAD at create time,
   *  = the workspace branch point). The verifier diffs `base..run/<id>`. Threaded on the context (never
   *  the index) so a respawn keeps the ORIGINAL base even after the child's commits move the branch tip. */
  base: string;
  /** The node's workspace-relative folder path (`""` for the root) — where its `_work.md` lives. */
  nodePath: string;
}

/** The verification loop's cross-attempt state (VERIFIER §5). Distinct from the fresh-context `respawns`
 *  bound: `attempt` counts VERIFY attempts (each exit-0 claim), and `priorSignature` is the immediately-
 *  preceding verdict's signature for the no-progress check. Threaded through reap → respawn so a verify
 *  fail → respawn → re-verify chain accumulates the attempt count and remembers the last verdict. */
interface VerifyState {
  /** Verify attempts already made on this run (0 before the first). */
  attempt: number;
  /** The previous verification's signature, for the no-progress halt (undefined before the first). */
  priorSignature?: string;
}

export function createSupervisor(deps: SupervisorDeps): Supervisor {
  const {
    db,
    factory,
    tokens,
    proxy,
    spawnChild = defaultSpawnChild,
    now = () => Date.now(),
    mintRunId = () => randomBytes(4).toString("hex"),
    holder = "runtime",
    config,
    hostEnv = process.env,
    maxRespawns = DEFAULT_MAX_RESPAWNS,
    readDiff = gitDiffBounded,
    killGraceMs = DEFAULT_KILL_GRACE_MS,
    delay = (ms: number) => new Promise<void>((r) => setTimeout(r, ms)),
  } = deps;
  const registry = new RunRegistry();
  // Run ids a `kill` landed on. A fresh-context respawn consults this after each await so a kill that
  // races the respawn WINS — it must not resurrect a killed run with a fresh child + token. Cleared on
  // terminal (the run id is unique per run, so this only bounds the set between kill and reap).
  const cancelled = new Set<string>();

  // A per-run event emitter over the run's journal + the index — the same FS-first, single-writer
  // serialization the tee uses (so a supervisor-derived run.finished lands byte-identically alongside
  // the child's own events). One SessionTee per reap/contain call serializes its emits in order.
  function runEmitter(runId: string, runDir: string): (ev: ChildEmittedEvent) => Promise<void> {
    const tee = new SessionTee({
      writer: bindIndexWriter(db),
      journal: fsJournal(runDir),
      sessionId: runId,
      now,
    });
    return (ev) => tee.append(ev);
  }

  function sys(type: EventType, payload?: Record<string, unknown>): ChildEmittedEvent {
    return payload === undefined
      ? { actor: "system" satisfies Actor, type }
      : { actor: "system" satisfies Actor, type, payload };
  }

  /** The highest `event.seq` for this session RIGHT NOW — captured just before each (re)launch so the
   *  next attempt's `run.exiting` is discriminated from a prior attempt's by `seq > watermark`. Guarded:
   *  a read fault yields 0 (the whole session is then in scope, the pre-fix behavior — never worse). */
  async function currentWatermark(runId: string): Promise<number> {
    try {
      const rows = await db
        .select({ m: max(event.seq) })
        .from(event)
        .where(eq(event.sessionId, runId));
      return rows[0]?.m ?? 0;
    } catch {
      return 0;
    }
  }

  /** The child's declared terminal `run.exiting {code, reason}` for the CURRENT attempt, read from the
   *  durable event log after the tee has drained. Scoped to `seq > watermark` so a fresh-context respawn
   *  (same session id, append-only `event` table with no attempt column) never reads the PRIOR attempt's
   *  row — an attempt that exits without re-declaring resolves to null (→ parks blocked, not respawns).
   *  Guarded: an index read fault resolves to null (reap then routes on the real exit code) — the same
   *  fail-safe the JSON.parse fallback gives, extended to the whole select so reap can never reject. */
  async function lastRunExiting(
    runId: string,
    watermark: number,
  ): Promise<{ code?: number; reason?: string } | null> {
    let row: typeof event.$inferSelect | undefined;
    try {
      const rows = await db
        .select()
        .from(event)
        .where(
          and(
            eq(event.sessionId, runId),
            eq(event.type, EVENT_TYPES.RUN_EXITING),
            gt(event.seq, watermark),
          ),
        )
        .orderBy(desc(event.seq))
        .limit(1);
      row = rows[0];
    } catch {
      return null; // index closed/busy on the read path — declared unknown, route on the real code
    }
    if (!row) return null;
    let payload: unknown = {};
    try {
      payload = row.payload ? JSON.parse(row.payload) : {};
    } catch {
      payload = {};
    }
    const p = payload as Record<string, unknown>;
    return {
      code: typeof p.code === "number" ? p.code : undefined,
      reason: typeof p.reason === "string" ? p.reason : undefined,
    };
  }

  /** Best-effort: park the session row + node row. Each guarded — a closed/full index leaves the child
   *  dealt with and D5 reconcile re-derives on restart; neither throw wedges the reap. */
  /**
   * Emit a `node.updated` stream event after a SUPERVISOR-driven node.state write (BRO-1913). The FS
   * watcher emits `node.updated` only for `_work.md` changes; run-lifecycle transitions (running →
   * review / blocked / done / canceled) are DB-only writes the watcher never sees, so WITHOUT this the
   * LIVE board (store reducer `node.updated` → `selectBoard` → "Needs you") + the top-bar gate count
   * never reflect a run's state until a full reload re-hydrates from `/api/tree`. The payload is
   * `projectLiveNode(row)` — the SAME shape the watcher emits (single definition, `db/project.ts`, so
   * the two projection sources can never drift) — with `sessionId: null` so it rides the GLOBAL stream
   * (`stream.ts` `tailWhere`) the shell subscribes to. Best-effort: a failed read/insert just leaves the
   * client to re-derive on reconnect (D5); the DB `node.state` write is the durable truth.
   */
  async function emitNodeUpdated(nodeId: string): Promise<void> {
    try {
      const rows = await db.select().from(node).where(eq(node.id, nodeId));
      const row = rows[0];
      if (!row || row.deletedAt !== null) return; // a tombstoned node never crosses the wire
      await db.insert(event).values({
        sessionId: null,
        ts: now(),
        actor: "system",
        type: EVENT_TYPES.NODE_UPDATED,
        payload: JSON.stringify(projectLiveNode(row)),
      });
    } catch {
      // best-effort — the DB node.state write above is the durable truth; a reload re-derives the view
    }
  }

  async function park(
    runId: string,
    nodeId: string,
    sessionStatus: SessionStatus,
    nodeState: OrchState,
    ended: boolean,
  ): Promise<void> {
    const at = now();
    try {
      await db
        .update(session)
        .set({ status: sessionStatus, updatedAt: at, ...(ended ? { endedAt: at } : {}) })
        .where(eq(session.id, runId));
    } catch {
      // index closed/full — the load-bearing killing already happened; D5 re-derives the row
    }
    try {
      await db.update(node).set({ state: nodeState, updatedAt: at }).where(eq(node.id, nodeId));
    } catch {
      // as above
    }
    // Propagate the state change to the live client (BRO-1913) — the watcher never sees a DB-only write.
    await emitNodeUpdated(nodeId);
  }

  /** Release a node's dispatch lease on terminal reap (idempotent — a re-delete is a no-op). */
  async function releaseLease(nodeId: string): Promise<void> {
    try {
      await db.delete(lease).where(eq(lease.key, nodeId));
    } catch {
      // best-effort — a stale lease expires / is reconciled by crash-recovery
    }
  }

  // ── Terminal reap — the one place a run leaves the live set ──────────────────
  // Emits the terminal lifecycle event, parks session + node, revokes the token, drops the registry
  // entry, stops liveness, releases the dispatch lease. Every step is idempotent so a reap racing a
  // kill (F8) — or a fresh-context reap that already re-minted — never double-frees. The worktree is
  // NEVER torn down here: it is the receipt (crash inspection, pending verify/review). teardown is
  // F9/janitor's call once the run is truly done + merged.
  async function terminal(
    ctx: RunContext,
    entry: RunEntry | null,
    opts: {
      exitCode: number;
      sessionStatus: SessionStatus;
      nodeState: OrchState;
      event: TerminalEvent;
      crash: boolean;
      reason?: string;
      gateId?: string;
      mismatch: boolean;
      respawns: number;
      /** Skip the terminal-event emit — set when the tee's own failSupervision (BRO-1767) already
       *  emitted run.failed + parked, so the supervisor reap does not write a DUPLICATE terminal event. */
      skipEmit?: boolean;
    },
  ): Promise<ReapResult> {
    if (!opts.skipEmit) {
      const emit = runEmitter(ctx.runId, ctx.sandbox.runDir);
      const payload: Record<string, unknown> = { code: opts.exitCode };
      if (opts.reason !== undefined) payload.reason = opts.reason;
      try {
        await emit(sys(opts.event, payload));
      } catch {
        // the durable event failed to append — parking below is the load-bearing change; D5 re-derives
      }
    }
    await park(ctx.runId, ctx.nodeId, opts.sessionStatus, opts.nodeState, true);
    tokens.revoke(ctx.runId);
    registry.delete(ctx.runId);
    cancelled.delete(ctx.runId);
    entry?.supervised.stop();
    await releaseLease(ctx.nodeId);
    return {
      runId: ctx.runId,
      exitCode: opts.exitCode,
      reason: opts.reason,
      sessionStatus: opts.sessionStatus,
      nodeState: opts.nodeState,
      event: opts.event,
      crash: opts.crash,
      gateId: opts.gateId,
      mismatch: opts.mismatch,
      respawns: opts.respawns,
    };
  }

  /** Open a pending `question` gate on exit 20 (HARNESS §4), returning its id — or `undefined` if the
   *  index insert faults (the run still parks review for a human; no gate row, no `gate.opened`). The
   *  guard is what keeps a reap on the exit-20 path from rejecting and skipping cleanup. */
  async function openQuestionGate(
    ctx: RunContext,
    declared: { reason?: string } | null,
    emit: (ev: ChildEmittedEvent) => Promise<void>,
  ): Promise<string | undefined> {
    const gateId = randomUUID();
    const at = now();
    try {
      await db.insert(gate).values({
        id: gateId,
        sessionId: ctx.runId,
        kind: "question",
        proposalJson: declared ? JSON.stringify(declared) : null,
        verdict: null,
        decidedBy: null,
        openedAt: at,
        decidedAt: null,
        updatedAt: at,
        deletedAt: null,
      });
    } catch {
      // index closed/busy — park review without a gate row rather than reject the reap (cleanup must run)
      return undefined;
    }
    try {
      await emit(sys(EVENT_TYPES.GATE_OPENED, { gateId, kind: "question" }));
    } catch {
      // gate.opened is journal-backed elsewhere (D-DURABILITY); the row above is the durable truth
    }
    return gateId;
  }

  // ── Crash containment — a partial/failed dispatch or a child crash, contained ──
  // The runtime survives: mark the session + node blocked, emit run.failed (through the run journal
  // when a runDir exists, else index-only), revoke any minted token, drop the registry, release the
  // lease. The worktree (if provisioned) is PRESERVED — a crash's receipt, replayed by F9.
  async function containCrash(
    ctx: { runId: string; nodeId: string; runDir?: string },
    reason: string,
    respawns: number,
  ): Promise<ReapResult> {
    if (ctx.runDir) {
      const emit = runEmitter(ctx.runId, ctx.runDir);
      try {
        await emit(sys(EVENT_TYPES.RUN_FAILED, { reason }));
      } catch {
        // fall through — the park is the load-bearing change
      }
    } else {
      try {
        await db.insert(event).values({
          sessionId: ctx.runId,
          ts: now(),
          actor: "system",
          type: EVENT_TYPES.RUN_FAILED,
          payload: JSON.stringify({ reason }),
        });
      } catch {
        // index unavailable — nothing more we can durably record; D5 reconcile handles it
      }
    }
    await park(ctx.runId, ctx.nodeId, "blocked", "blocked", true);
    tokens.revoke(ctx.runId);
    registry.delete(ctx.runId);
    cancelled.delete(ctx.runId);
    await releaseLease(ctx.nodeId);
    return {
      runId: ctx.runId,
      exitCode: Number.NaN,
      sessionStatus: "blocked",
      nodeState: "blocked",
      event: "run.failed",
      crash: true,
      mismatch: false,
      respawns,
    };
  }

  // ── Kill-by-intent terminal (F8, BRO-1801) ──────────────────────────────────
  // A run the human killed ends DISTINCTLY from a crash: session `canceled` (the human chose to stop
  // it, not a fault) + node `blocked` (a human should look) + `run.killed`, worktree PRESERVED (the
  // receipt of the partial work, FLOWS §F8). Same cleanup as terminal/containCrash — revoke the token
  // (already done by kill(), idempotent here), drop the registry entry, clear cancelled, release the
  // lease. `entry` is null when the kill lands mid-respawn (no live child to stop).
  async function terminalKilled(
    ids: { runId: string; nodeId: string; runDir?: string },
    entry: RunEntry | null,
    exitCode: number,
    respawns: number,
    // Skip the run.killed emit when the tee's failSupervision (BRO-1767) already wrote a terminal
    // event for this run (a tee-write failure that coincided with the kill) — the park below still runs
    // (correcting failSupervision's `blocked` to the kill's `canceled`), only the DUPLICATE event is
    // suppressed. Mirrors the crash branch's skipEmit.
    skipEmit = false,
  ): Promise<ReapResult> {
    if (!skipEmit) {
      if (ids.runDir) {
        const emit = runEmitter(ids.runId, ids.runDir);
        try {
          await emit(sys(EVENT_TYPES.RUN_KILLED, { reason: "kill intent" }));
        } catch {
          // fall through — the park is the load-bearing change; D5 reconcile re-derives the event
        }
      } else {
        try {
          await db.insert(event).values({
            sessionId: ids.runId,
            ts: now(),
            actor: "system",
            type: EVENT_TYPES.RUN_KILLED,
            payload: JSON.stringify({ reason: "kill intent" }),
          });
        } catch {
          // index unavailable — the park below is what matters
        }
      }
    }
    await park(ids.runId, ids.nodeId, "canceled", "blocked", true);
    tokens.revoke(ids.runId);
    registry.delete(ids.runId);
    cancelled.delete(ids.runId);
    entry?.supervised.stop();
    await releaseLease(ids.nodeId);
    return {
      runId: ids.runId,
      exitCode,
      reason: "killed",
      sessionStatus: "canceled",
      nodeState: "blocked",
      event: "run.killed",
      crash: false,
      mismatch: false,
      respawns,
    };
  }

  // ── Reap — map the child's real exit code (HARNESS §4) ──────────────────────
  // `reap` NEVER rejects: any unexpected throw in the routing (an index fault on a read/gate path the
  // inner logic did not already guard) is caught and contained, so the background `reaped` chain always
  // resolves a ReapResult and cleanup (token revoke, registry drop, lease release) is guaranteed.
  // A fault on a path where a kill may have landed (a reap/respawn await that throws while `cancelled`
  // is set) must STILL honor the kill: end `canceled`/`run.killed`, not `blocked`/`run.failed`. Route to
  // terminalKilled when killed-by-intent, else contain as a crash. (BRO-1801 P20 — the F8 classification
  // must win regardless of a concurrent provision/spawn/index fault.)
  function containOrKilled(
    ids: { runId: string; nodeId: string; runDir?: string },
    reason: string,
    respawns: number,
  ): Promise<ReapResult> {
    if (cancelled.has(ids.runId)) return terminalKilled(ids, null, Number.NaN, respawns);
    return containCrash(ids, reason, respawns);
  }

  /** The run's model-call iteration count (`run_budget.iterations`) — the global-budget backstop the
   *  verifier's `iteration_cap` halt checks against `max_iterations`. Guarded: a read fault yields 0 so
   *  the reap never rejects on it (an under-count only delays the cap, never fails a good run). */
  async function readIterations(runId: string): Promise<number> {
    try {
      const rows = await db
        .select({ i: runBudget.iterations })
        .from(runBudget)
        .where(eq(runBudget.sessionId, runId))
        .limit(1);
      return rows[0]?.i ?? 0;
    } catch {
      return 0;
    }
  }

  // ── Verify (F4, Loop 2) — the exit-0 route ──────────────────────────────────
  // The child claims complete → run the verifier (Stage 0 tamper/diff guard + Stage 1 checks + Stage 2 LLM
  // judge) against `base..run/<id>`, persist verdict.md + the check.* stream, and route on the verdict:
  //   pass          → park review (the human gate, F5 — nothing auto-completes under gate:human)
  //   fail          → respawn the coding agent with the appended fix_plan, OR park blocked when a cap hits
  //   error (infra) → park blocked, NO attempt burned (a broken harness is never the agent's fault)
  // Stage 2 runs only when the contract pins a rubric (`done.judge`); a rubric-less contract skips the judge
  // (verdict carries judge: { score: null }). The judge is a SUPPLEMENT under gate:human (every clean run
  // parks review regardless of its score); it becomes decision-load-bearing only under gate:auto (BRO-1802).
  async function verifyAndRoute(
    ctx: RunContext,
    entry: RunEntry,
    mismatch: boolean,
    respawns: number,
    verify: VerifyState,
  ): Promise<ReapResult> {
    const done = ctx.contract.done;
    // No verifiable success function → nothing to check; the human gate holds it (the pre-verifier
    // behavior). Never auto-completes (gate:human).
    if (!done) {
      return terminal(ctx, entry, {
        exitCode: 0,
        sessionStatus: "review",
        nodeState: "review",
        event: "run.finished",
        crash: false,
        mismatch,
        respawns,
      });
    }

    const attempt = verify.attempt + 1;
    const iterations = await readIterations(ctx.runId);
    const maxIterations =
      ctx.contract.budget?.max_iterations ?? config?.maxIterations ?? DEFAULT_MAX_ITERATIONS;
    const maxAttempts = config?.verifierMaxAttempts ?? DEFAULT_VERIFIER_MAX_ATTEMPTS;

    // One SessionTee for the whole check.* stream (started → result(s) → judge → verdict), so the events
    // land ordered alongside the run's other events (the same FS-first single-writer path the child uses).
    const teeEmit = runEmitter(ctx.runId, ctx.sandbox.runDir);

    // Stage 2 (the LLM judge) runs ONLY when the contract pins a rubric (`done.judge`). We mint a
    // VERIFIER-role bearer for the judge's model call — the judge dials the SAME metered proxy as the agent
    // but never holds the runtime/Anthropic key (the proxy attaches it at forward time). `tokens.mint` is
    // idempotent per session: it revokes the now-dead agent bearer (the child exited 0) and installs the
    // verifier one. The bearer is revoked in the `finally` below, so a live verifier bearer exists only for
    // the duration of the judge call (key-confinement). Without a rubric the judge is skipped: `rubricText:
    // null` → `attachJudge` yields `judge: { score: null }` and `call` is never invoked.
    const judgeRef = done.judge;
    let verifierBearer: string | null = null;
    let judge: RunVerificationDeps["judge"];
    if (judgeRef === undefined) {
      judge = {
        rubricText: null,
        diff: "",
        brief: "",
        call: () => {
          throw new Error("verifier judge caller invoked without a rubric");
        },
      };
    } else {
      // Read the run diff BYTE-BOUNDED before it reaches the judge (see the resolution comment above) — the
      // judge must hold the whole diff, so an unbounded read would OOM the shared supervisor.
      const maxBytes = config?.judgeDiffMaxBytes ?? DEFAULT_JUDGE_DIFF_MAX_BYTES;
      const runDiff = await readDiff(ctx.sandbox.workdir, ctx.base, ctx.sandbox.branch, maxBytes);
      // A kill that landed while we read the diff (or earlier) must WIN — checked IMMEDIATELY after the
      // `await`, so it DOMINATES every branch below (the truncation early-return AND the mint) with NO await
      // in between. Placing it here (not after the truncation check) is load-bearing on TWO counts:
      //   (1) F8 provenance — a kill during the diff read must end canceled/run.killed, NOT be misclassified
      //       as the over-cap park blocked/run.finished below (the run is still registered during verify, so
      //       a human kill lands here; the over-cap read is the LONGEST such window).
      //   (2) no resurrection — `tokens.mint` is idempotent per session, so minting after `kill()`'s revoke
      //       would re-install a LIVE bearer, letting the judge bill a model call AFTER the kill. JS is
      //       single-threaded, so `kill()` (which runs at an await point) cannot interleave between this
      //       synchronous check and the synchronous `tokens.mint` below.
      // A kill AFTER the mint instead revokes the live bearer → the in-flight forward 401s → the post-verify
      // `cancelled` re-check routes terminalKilled.
      if (cancelled.has(ctx.runId)) {
        return terminalKilled(
          { runId: ctx.runId, nodeId: ctx.nodeId, runDir: ctx.sandbox.runDir },
          entry,
          0,
          respawns,
          entry.supervised.supervisionFailed(),
        );
      }
      // A diff past the cap fails CLOSED: park blocked (`diff_too_large`) — the human looks — rather than
      // buffering an adversarial diff into the shared supervisor (gitDiffBounded stopped at the cap).
      if (runDiff.truncated) {
        return terminal(ctx, entry, {
          exitCode: 0,
          sessionStatus: "blocked",
          nodeState: "blocked",
          event: "run.finished",
          crash: false,
          reason: "diff_too_large",
          mismatch,
          respawns,
        });
      }
      verifierBearer = tokens.mint({
        session: ctx.runId,
        runDir: ctx.sandbox.runDir,
        role: VERIFIER_ROLE,
        budget: ctx.budget,
      });
      judge = {
        // All three inputs read from the agent-immutable BASE commit (see the resolution comment above),
        // never the worktree — tamper-proof (the writer can't rewrite its own rubric) and hang-proof (no
        // worktree FIFO). fail-CLOSED: an invalid ref / absent / over-cap rubric → "" → parseRubric errors →
        // verdict error → park blocked. The brief is best-effort context; the diff is the byte-bounded read above.
        rubricText: await readRubricText(
          ctx.sandbox.workdir,
          ctx.base,
          ctx.nodePath,
          judgeRef,
          maxBytes,
        ),
        diff: runDiff.text,
        brief: await readBrief(ctx.sandbox.workdir, ctx.base, ctx.nodePath, maxBytes),
        call: proxyJudgeCaller({ proxyUrl: proxy.url, bearer: verifierBearer }),
      };
    }

    let result: RunVerificationResult;
    try {
      result = await runVerification({
        stage0: {
          cwd: ctx.sandbox.workdir,
          base: ctx.base,
          branch: ctx.sandbox.branch,
          protect: effectiveProtect(done),
          maxFiles: done.diff?.max_files,
          maxLines: done.diff?.max_lines,
        },
        checks: {
          checks: normalizeChecks(done.check),
          spawnContext: ctx.sandbox.spawnContext(),
        },
        judge,
        runDir: ctx.sandbox.runDir,
        attempt,
        maxAttempts,
        iterations,
        maxIterations,
        priorSignature: verify.priorSignature,
        emit: (type, payload) => teeEmit(sys(type, payload)),
        now: () => new Date(now()).toISOString(),
      });
    } finally {
      // Revoke the verifier bearer the instant the judge's model call is done (or if runVerification threw)
      // — minimize the window a live bearer exists. Only when we minted one. Idempotent with the
      // terminal/kill revoke; a respawn re-mints a fresh AGENT bearer inside `launch`.
      if (verifierBearer !== null) tokens.revoke(ctx.runId);
    }

    // A kill that landed DURING the verify must WIN (F8): it set `cancelled` + revoked the token while we
    // awaited runVerification. End canceled/run.killed, not park review/respawn. (A kill that made the
    // verify THROW is already handled by reap's outer catch → containOrKilled, which honors `cancelled`.)
    if (cancelled.has(ctx.runId)) {
      return terminalKilled(
        { runId: ctx.runId, nodeId: ctx.nodeId, runDir: ctx.sandbox.runDir },
        entry,
        0,
        respawns,
        entry.supervised.supervisionFailed(),
      );
    }

    const outcome = result.outcome;
    if (outcome.action === "respawn") {
      // Fail with attempts left → the fresh coding agent re-reads the appended fix_plan.md at boot
      // (runVerification already wrote it). Carry the verify attempt count + this verdict's signature
      // forward; `respawns` (the fresh-context bound) is UNCHANGED — a verify-respawn is a distinct loop
      // bounded by maxAttempts, not by maxRespawns.
      entry.supervised.stop();
      return respawn(ctx, respawns, { attempt, priorSignature: result.signature });
    }
    if (outcome.action === "park_blocked") {
      // A cap hit (verifier_exhausted / iteration_cap / no_progress) or an infra verify_error → blocked,
      // event run.finished (a clean verify halt, NOT a crash's run.failed). The human looks.
      return terminal(ctx, entry, {
        exitCode: 0,
        sessionStatus: "blocked",
        nodeState: "blocked",
        event: "run.finished",
        crash: false,
        reason: outcome.reason,
        mismatch,
        respawns,
      });
    }
    // park_review — a clean pass waits at the human gate (F5). Nothing auto-completes under gate:human.
    return terminal(ctx, entry, {
      exitCode: 0,
      sessionStatus: "review",
      nodeState: "review",
      event: "run.finished",
      crash: false,
      mismatch,
      respawns,
    });
  }

  async function reap(
    ctx: RunContext,
    entry: RunEntry,
    watermark: number,
    respawns: number,
    verify: VerifyState,
  ): Promise<ReapResult> {
    try {
      return await reapInner(ctx, entry, watermark, respawns, verify);
    } catch (err) {
      return containOrKilled(
        { runId: ctx.runId, nodeId: ctx.nodeId, runDir: ctx.sandbox.runDir },
        `reap failed: ${msg(err)}`,
        respawns,
      );
    }
  }

  async function reapInner(
    ctx: RunContext,
    entry: RunEntry,
    watermark: number,
    respawns: number,
    verify: VerifyState,
  ): Promise<ReapResult> {
    // Ground truth: the real process exit code (Bun resolves a signal kill to a non-{0,10,20} value).
    const realCode = await entry.child.exited;
    // Killed by intent (F8, BRO-1801) → a DISTINCT terminal: `canceled` + `run.killed`, regardless of
    // the exit code the SIGKILL produced (it would otherwise route via the crash branch to
    // blocked/run.failed). `cancelled` is set ONLY by kill() — a human chose to stop this run.
    if (cancelled.has(ctx.runId)) {
      // skipEmit when a tee-write failure already reaped this run in-band (failSupervision wrote a
      // terminal event) — avoid a DUPLICATE terminal event; the park still corrects blocked→canceled.
      return terminalKilled(
        { runId: ctx.runId, nodeId: ctx.nodeId, runDir: ctx.sandbox.runDir },
        entry,
        realCode,
        respawns,
        entry.supervised.supervisionFailed(),
      );
    }
    // Scoped to THIS attempt (seq > watermark) so a respawn never reads a prior attempt's run.exiting.
    const declared = await lastRunExiting(ctx.runId, watermark);
    const mismatch =
      declared?.code !== undefined && Number.isInteger(realCode) && declared.code !== realCode;
    if (mismatch) {
      const emit = runEmitter(ctx.runId, ctx.sandbox.runDir);
      try {
        await emit(
          sys(EVENT_TYPES.RUN_EXIT_MISMATCH, { declared: declared?.code, actual: realCode }),
        );
      } catch {
        // advisory Loop-4 signal — its loss does not change the terminal routing below
      }
    }

    // Route on the REAL code (ground truth); the declared reason sub-routes exit 10.
    if (Number.isInteger(realCode) && realCode === 0) {
      // Claims complete → run the verifier (F4, Loop 2) and route on its verdict.
      return verifyAndRoute(ctx, entry, mismatch, respawns, verify);
    }

    if (Number.isInteger(realCode) && realCode === 10) {
      const reason = declared?.reason;
      if (reason === "fresh_context") {
        // IMMEDIATE respawn: same run id, same worktree, same run_budget row (budgets span attempts,
        // NOT processes — do NOT re-zero / re-insert). The token is re-minted inside launch (mint
        // revokes the prior). NOT a terminal — do not revoke/drop/release here. The verify state passes
        // through UNCHANGED: a mid-coding context-restart is not a new verification attempt.
        entry.supervised.stop(); // the old tick is already stopped (done ran it), belt-and-suspenders
        return respawn(ctx, respawns + 1, verify);
      }
      // budget | iteration_cap | no_progress | user_stop | (missing/unknown reason) → park blocked.
      return terminal(ctx, entry, {
        exitCode: 10,
        sessionStatus: "blocked",
        nodeState: "blocked",
        event: "run.finished",
        crash: false,
        reason,
        mismatch,
        respawns,
      });
    }

    if (Number.isInteger(realCode) && realCode === 20) {
      const emit = runEmitter(ctx.runId, ctx.sandbox.runDir);
      const gateId = await openQuestionGate(ctx, declared, emit);
      return terminal(ctx, entry, {
        exitCode: 20,
        sessionStatus: "review",
        nodeState: "review",
        event: "run.finished",
        crash: false,
        gateId,
        mismatch,
        respawns,
      });
    }

    // Any other code / a signal kill (SIGKILL, segfault) → crash. Park blocked + run.failed; the
    // worktree is preserved and the runtime keeps serving (the containment invariant). If the tee's own
    // failSupervision (BRO-1767) already emitted run.failed + parked (a mid-run durability loss that
    // SIGKILLed the child), skip a DUPLICATE run.failed — the cleanup below (revoke/drop/release) still
    // runs, since failSupervision does not own those.
    const supFailed = entry.supervised.supervisionFailed();
    return terminal(ctx, entry, {
      exitCode: realCode,
      sessionStatus: "blocked",
      nodeState: "blocked",
      event: "run.failed",
      crash: true,
      reason: `unexpected exit code ${realCode}`,
      mismatch,
      respawns,
      skipEmit: supFailed,
    });
  }

  // ── Launch — snapshot + token + env + spawn + supervise + register ──────────
  // Shared by dispatch (first launch) and respawn (fresh-context). Returns the live entry, or THROWS
  // if the spawn seam throws (ENOENT) — the caller contains that as a crash. `factory.create` is
  // idempotent, so a respawn re-attaches the SAME worktree.
  async function launch(ctx: RunContext): Promise<{ entry: RunEntry; watermark: number }> {
    const { sandbox } = ctx;
    // The event-seq high-water mark BEFORE this attempt speaks — so reap reads only THIS attempt's
    // run.exiting (seq > watermark), never a prior attempt's on a fresh-context respawn.
    const watermark = await currentWatermark(ctx.runId);
    // Freeze the child's contract snapshot (its frozen "what am I working on").
    const contractPath = await writeContractSnapshot(sandbox.runDir, {
      session: ctx.runId,
      node: ctx.contract,
      dispatchedAt: new Date(now()).toISOString(),
    });
    // Mint the per-session proxy bearer (idempotent — a respawn revokes the prior token).
    const token = tokens.mint({
      session: ctx.runId,
      runDir: sandbox.runDir,
      role: AGENT_ROLE,
      budget: ctx.budget,
    });
    const env = buildChildEnv(hostEnv, {
      session: ctx.runId,
      runDir: sandbox.runDir,
      contractPath,
      modelProxyUrl: proxy.url,
      modelToken: token,
      // The child restarts fresh past this token ceiling (HARNESS §5). Resolved from the runtime config so
      // it is a real production knob, not a test-only env — the child falls back to its own default if unset.
      contextCeilingTokens: config?.contextCeilingTokens,
    });
    const spawnCtx = sandbox.spawnContext();
    // A spawn THROW here (broomva-child missing, cwd gone) propagates to the caller's crash path.
    const child = spawnChild({
      argv: serializeChildArgv({ role: AGENT_ROLE, session: ctx.runId }),
      env: { ...spawnCtx.env, ...env },
      cwd: spawnCtx.cwd,
      commandPrefix: spawnCtx.commandPrefix,
    });
    const supervised = superviseChildStdio(child, {
      db,
      sessionId: ctx.runId,
      runDir: sandbox.runDir,
      now,
      config,
    });
    const entry: RunEntry = {
      runId: ctx.runId,
      nodeId: ctx.nodeId,
      sandbox,
      child,
      supervised,
    };
    registry.set(entry); // overwrites on a respawn (same run id)
    return { entry, watermark };
  }

  /** Re-launch a run (same run id / worktree / budget). Used by BOTH the fresh-context restart (exit 10,
   *  `respawns + 1`) and a verify-fail retry (`respawns` unchanged, `verify` advanced) — the `verify`
   *  state threads through so the next reap's exit-0 verify continues the attempt count. A re-provision
   *  or re-spawn throw is contained as a crash. The safety bound stops an infinite `fresh_context` loop. */
  async function respawn(
    ctx: RunContext,
    respawns: number,
    verify: VerifyState,
  ): Promise<ReapResult> {
    // A kill(runId) that landed while the prior attempt was reaping must WIN — never resurrect a killed
    // run with a fresh child + token. Checked here AND after the create await (a kill can land in it).
    // A killed run ends `canceled`/`run.killed` (F8), same as a kill caught in reap — not a crash.
    if (cancelled.has(ctx.runId)) {
      return terminalKilled(
        { runId: ctx.runId, nodeId: ctx.nodeId, runDir: ctx.sandbox.runDir },
        null,
        Number.NaN,
        respawns,
      );
    }
    if (respawns > maxRespawns) {
      return containCrash(
        { runId: ctx.runId, nodeId: ctx.nodeId, runDir: ctx.sandbox.runDir },
        `fresh_context respawn limit (${maxRespawns}) exceeded`,
        respawns,
      );
    }
    let sandbox: Sandbox;
    try {
      sandbox = await factory.create(ctx.runId); // idempotent → same worktree
    } catch (err) {
      // A kill that landed during this await must still win the classification (canceled/run.killed).
      return containOrKilled(
        { runId: ctx.runId, nodeId: ctx.nodeId, runDir: ctx.sandbox.runDir },
        `respawn provision failed: ${msg(err)}`,
        respawns,
      );
    }
    // Re-check: kill may have landed during the create await (it SIGKILLs the OLD, dead child + revokes,
    // then we must NOT spawn a replacement).
    if (cancelled.has(ctx.runId)) {
      return terminalKilled(
        { runId: ctx.runId, nodeId: ctx.nodeId, runDir: sandbox.runDir },
        null,
        Number.NaN,
        respawns,
      );
    }
    const nextCtx: RunContext = { ...ctx, sandbox };
    let launched: { entry: RunEntry; watermark: number };
    try {
      launched = await launch(nextCtx);
    } catch (err) {
      // A kill that landed during launch must still win the classification (canceled/run.killed).
      return containOrKilled(
        { runId: nextCtx.runId, nodeId: nextCtx.nodeId, runDir: nextCtx.sandbox.runDir },
        `respawn spawn failed: ${msg(err)}`,
        respawns,
      );
    }
    await launched.entry.supervised.done;
    return reap(nextCtx, launched.entry, launched.watermark, respawns, verify);
  }

  // ── Dispatch (F2) ────────────────────────────────────────────────────────
  async function dispatch(nodeId: string): Promise<DispatchOutcome> {
    // 1. Read the node's resolved contract (before the lease — a missing node is a caller error, not a
    //    consumed lease). The scanner already resolved defaults into the row.
    const rows = await db.select().from(node).where(eq(node.id, nodeId)).limit(1);
    const row = rows.find((r) => r.deletedAt === null) ?? rows[0];
    if (!row || row.deletedAt !== null) return { dispatched: false, reason: "node_not_found" };

    // 2. Lease the node id — free-before-claim via an atomic insert-or-conflict on the PK. Held →
    //    someone else is dispatching this node → drop silently (idempotency, NOT an error). No
    //    db.transaction() (libSQL :memory: opens a separate connection — a tx there hits an empty db).
    const at = now();
    const ins = await db
      .insert(lease)
      .values({ key: nodeId, holder, acquiredAt: at, expiresAt: at + DISPATCH_LEASE_TTL_MS })
      .onConflictDoNothing({ target: lease.key });
    if (ins.rowsAffected === 0) return { dispatched: false, reason: "lease_held" };

    // 3. Mint the run id + reconstruct the contract/budget.
    const runId = mintRunId();
    const contract = nodeRowToContract(row);
    const budget: Budget = row.budgetJson ? (JSON.parse(row.budgetJson) as Budget) : {};

    // 4. Insert the session (running) + a ZEROED run_budget. The branch is `run/<id>` (the receipt).
    //    Guarded: a transient index fault here (post-lease, pre-child) must RELEASE the just-acquired
    //    lease and park anything half-created, never orphan the node lease for the 24h TTL nor leave a
    //    running-state session with no process (the step-5 comment's invariant, extended upstream).
    try {
      await db.insert(session).values({
        id: runId,
        nodeId,
        branch: `run/${runId}`,
        status: "running",
        startedAt: at,
        updatedAt: at,
      });
      await db.insert(runBudget).values({ sessionId: runId, spentUsd: 0, iterations: 0 });
    } catch (err) {
      const reaped = containCrash(
        { runId, nodeId },
        `session/budget insert failed: ${msg(err)}`,
        0,
      );
      return { dispatched: true, runId, reaped };
    }

    // 5. Provision the sandbox (worktree). A provision failure is contained as a crash — the session
    //    exists, so it parks blocked and never orphans as running-with-no-process.
    let sandbox: Sandbox;
    try {
      sandbox = await factory.create(runId);
    } catch (err) {
      const reaped = containCrash({ runId, nodeId }, `provision failed: ${msg(err)}`, 0);
      return { dispatched: true, runId, reaped };
    }
    // Capture the base commit ONCE, here: the freshly-created worktree's HEAD IS the workspace branch
    // point, and the verifier diffs `base..run/<id>` at reap. On the RunContext, never the index — a
    // respawn keeps the ORIGINAL base even after the child's commits move the branch tip past it.
    let base: string;
    try {
      base = await gitHead(sandbox.workdir);
    } catch (err) {
      const reaped = containCrash(
        { runId, nodeId, runDir: sandbox.runDir },
        `base capture failed: ${msg(err)}`,
        0,
      );
      return { dispatched: true, runId, reaped };
    }
    const ctx: RunContext = { runId, nodeId, contract, budget, sandbox, base, nodePath: row.path };

    // 6. Launch the child. A spawn throw (ENOENT: broomva-child) → crash, worktree preserved.
    let launched: { entry: RunEntry; watermark: number };
    try {
      launched = await launch(ctx);
    } catch (err) {
      const reaped = containCrash(
        { runId, nodeId, runDir: sandbox.runDir },
        `spawn failed: ${msg(err)}`,
        0,
      );
      return { dispatched: true, runId, reaped };
    }
    const { entry, watermark } = launched;

    // 7. node.state → running. (The CHILD emits run.started per HARNESS §6 — the supervisor does NOT,
    //    to avoid a double run.started.) Then emit node.updated so the LIVE board moves the card to
    //    "Running" without waiting for a reload (BRO-1913 — a DB-only write is invisible to the watcher).
    try {
      await db.update(node).set({ state: "running", updatedAt: now() }).where(eq(node.id, nodeId));
    } catch {
      // index hiccup — the session row already records the run; a rescan reconciles node.state
    }
    await emitNodeUpdated(nodeId);

    // 8. Start the reap in the background (do NOT await — dispatch returns once the child is LIVE). reap
    //    never rejects (it contains its own throws), but the `.catch` is the belt to that suspenders so a
    //    truly-unexpected rejection on the fire-and-forget chain can never surface unhandled.
    const reaped = entry.supervised.done
      .then(() => reap(ctx, entry, watermark, 0, { attempt: 0 }))
      .catch((err) =>
        containCrash(
          { runId, nodeId, runDir: sandbox.runDir },
          `reap chain failed: ${msg(err)}`,
          0,
        ),
      );
    return { dispatched: true, runId, reaped };
  }

  // ── Kill switch (F8, BRO-1801) ──────────────────────────────────────────────
  // Intent → terminate the child → the run ends `canceled` + `run.killed` (the reap / respawn-check reads
  // `cancelled` and routes to terminalKilled), worktree + branch PRESERVED. The bearer is revoked HERE so
  // no in-flight model call survives the kill.
  //
  // BRO-1912: SIGTERM-FIRST, not immediate SIGKILL. A runner that spawns a grandchild (the subscription
  // `claude` CLI) reaps that CLI in its SIGTERM handler; a bare SIGKILL bypasses the handler and ORPHANS
  // the CLI (it keeps spending the subscription + mutating the worktree after the operator hit kill). We
  // still guarantee death: SIGKILL after `killGraceMs` if the child outlives cooperation. broomva-child
  // installs no SIGTERM handler, so it simply dies on SIGTERM — same terminal, no orphan to reap.
  async function terminateChild(child: RunEntry["child"]): Promise<void> {
    child.kill("SIGTERM");
    let exited = false;
    const exitedP = child.exited.then(
      () => {
        exited = true;
      },
      () => {
        exited = true; // a rejected `exited` (spawn error) is still "gone" — nothing left to SIGKILL
      },
    );
    await Promise.race([exitedP, delay(killGraceMs)]);
    if (!exited) child.kill("SIGKILL");
  }

  function kill(runId: string): boolean {
    const entry = registry.get(runId);
    if (!entry) return false;
    // Mark cancelled FIRST — the background reap AND any in-flight respawn consult it: a killed run ends
    // `canceled`/`run.killed`, and a respawn refuses to resurrect it with a fresh child + token. The
    // graceful terminate runs in the background (fire-and-forget); the reap already awaits `child.exited`
    // and, seeing `cancelled`, routes to terminalKilled regardless of the exit code the signal produced.
    cancelled.add(runId);
    void terminateChild(entry.child);
    tokens.revoke(runId); // bearer invalid immediately — a mid-tool-call model request now 401s
    return true;
  }

  // F8 stop-all — kill every live run. registry.list() already returns a fresh array copy, and kill()
  // only SIGKILLs + revokes synchronously (registry removal is deferred to the async reap), so iterating
  // it directly is safe — no live entry is skipped by a concurrent mutation.
  function killAll(): number {
    let killed = 0;
    for (const entry of registry.list()) {
      if (kill(entry.runId)) killed++;
    }
    return killed;
  }

  return {
    dispatch,
    kill,
    killAll,
    get: (runId) => registry.get(runId),
    list: () => registry.list(),
    registry,
  };
}

/** A short message from an unknown thrown value. */
function msg(err: unknown): string {
  return String((err as Error)?.message ?? err);
}
