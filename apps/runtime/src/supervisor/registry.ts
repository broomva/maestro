// The run registry (FLOWS §F2 step 6) — the supervisor's in-memory map of every LIVE run: run id →
// its sandbox, its child stdio port, its supervision handle. It is the reach-through the two downstream
// seams need:
//
//   * F8 kill (BRO-1801) resolves a session → its `child` to SIGKILL + its entry to drop.
//   * F10 chat (BRO-1822) resolves a session → its `supervised.control` to route a UIMessage into the
//     live loop.
//
// In-memory ON PURPOSE (like the token registry): an entry is valid only while its process is alive, so
// it must NOT survive a runtime restart — a restart respawns children fresh (crash-recovery F9 replays
// the durable receipts, never this map). Single-writer, like the rest of the runtime: dispatch adds,
// reap/kill removes, chat reads. A fresh-context respawn (HARNESS §5) REPLACES an entry in place (same
// run id, new child/supervised), so `set` overwrites rather than requiring a delete-then-add.

import type { ChildStdioPort, SupervisedChild } from "../harness/stdio";
import type { Sandbox } from "../sandbox/sandbox";

/** One live run — everything the kill/chat seams need to reach its process. */
export interface RunEntry {
  /** The run id (= session id). */
  readonly runId: string;
  /** The node this run works on — reap transitions its `node.state`, kill releases its dispatch lease. */
  readonly nodeId: string;
  /** The isolation context (worktree). Preserved on crash/kill (the receipt); freed on clean teardown. */
  readonly sandbox: Sandbox;
  /** The child process stdio port — kill (F8) SIGKILLs through it. */
  readonly child: ChildStdioPort;
  /** The supervision handle — chat (F10) routes through `supervised.control`; reap awaits `done`. */
  readonly supervised: SupervisedChild;
}

/**
 * The live-run registry. A thin typed wrapper over a Map so the seam is named (not a bare Map passed
 * around) and the "overwrite on respawn" contract is explicit.
 */
export class RunRegistry {
  #byRun = new Map<string, RunEntry>();

  /** Register (or, on a fresh-context respawn, replace) a live run. */
  set(entry: RunEntry): void {
    this.#byRun.set(entry.runId, entry);
  }

  /** The live entry for a run, or null (unknown / already reaped). */
  get(runId: string): RunEntry | null {
    return this.#byRun.get(runId) ?? null;
  }

  /** Drop a run on terminal reap / kill. Idempotent — a double-drop (reap racing kill) is a no-op. */
  delete(runId: string): boolean {
    return this.#byRun.delete(runId);
  }

  /** True if a run is live. */
  has(runId: string): boolean {
    return this.#byRun.has(runId);
  }

  /** Every live run — the observability surface (AUTONOMY §4) + the shutdown sweep. */
  list(): RunEntry[] {
    return Array.from(this.#byRun.values());
  }

  /** Live run count. */
  get size(): number {
    return this.#byRun.size;
  }
}
