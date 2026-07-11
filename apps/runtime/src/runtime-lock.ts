// The one-runtime-per-workspace lock (DECISIONS §D4, BRO-1814). The runtime writes a lock file
// (runtime id + heartbeat) at startup; a second runtime that sees a FRESH lock refuses to start against
// the same workspace. Cross-runtime lease arbitration is a distributed-systems bill the roadmap never
// orders — this is a single mutual-exclusion latch, nothing more.
//
// The lock path is `config.lockPath` (`<workspace>/.maestro/runtime.lock`). Acquisition is atomic on the
// happy path via an exclusive create (`flag: "wx"`); a pre-existing lock is inspected: FRESH + not ours
// → refuse; STALE (heartbeat older than staleMs — a prior runtime died without releasing) → steal it.
// Two runtimes racing a fresh start: `wx` lets exactly one create; the loser gets EEXIST, reads the
// just-written FRESH lock, and refuses. The holder must `heartbeat()` on an interval (< staleMs) so its
// liveness stays visible, and `release()` on shutdown (delete the file, only if it is still ours).

import { randomBytes } from "node:crypto";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname } from "node:path";

/** Heartbeat cadence: refresh the lock's timestamp this often (well under STALE). */
export const DEFAULT_LOCK_HEARTBEAT_MS = 5_000;
/** A lock whose heartbeat is older than this is considered dead + stealable (3× the heartbeat). */
export const DEFAULT_LOCK_STALE_MS = 15_000;

/** The on-disk lock record. */
interface LockRecord {
  id: string;
  heartbeat: number;
}

/** Thrown when a FRESH lock held by another runtime blocks acquisition (D4 refusal). */
export class RuntimeLockedError extends Error {
  constructor(
    readonly holderId: string,
    readonly ageMs: number,
  ) {
    super(
      `runtime lock held by a live runtime (id=${holderId}, last heartbeat ${ageMs}ms ago) — one runtime per workspace (D4)`,
    );
    this.name = "RuntimeLockedError";
  }
}

/** A held runtime lock — the caller heartbeats on an interval and releases on shutdown. */
export interface RuntimeLock {
  /** This runtime's id (written into the lock). */
  readonly id: string;
  /** Refresh the lock's heartbeat timestamp (call on an interval < staleMs). Best-effort: a write
   *  failure is swallowed (a transient FS hiccup must not crash the runtime; the next tick retries). */
  heartbeat(): Promise<void>;
  /** Release the lock on shutdown — delete the file ONLY if it is still ours (never steal-delete a lock
   *  a stealer has since taken). Idempotent + best-effort. */
  release(): Promise<void>;
}

/** Read + parse the lock file, or null if absent/malformed. */
async function readLock(lockPath: string): Promise<LockRecord | null> {
  let raw: string;
  try {
    raw = await readFile(lockPath, "utf8");
  } catch {
    return null;
  }
  try {
    const o = JSON.parse(raw) as Partial<LockRecord>;
    if (typeof o.id === "string" && typeof o.heartbeat === "number") {
      return { id: o.id, heartbeat: o.heartbeat };
    }
  } catch {
    // malformed lock (a torn write / hand edit) — treat as absent, overwritable
  }
  return null;
}

export interface AcquireOptions {
  /** Epoch-ms clock — default Date.now; tests pin it. */
  now?: () => number;
  /** This runtime's id — default a random hex; injected for deterministic tests. */
  id?: string;
  /** A lock older than this is stealable (default DEFAULT_LOCK_STALE_MS). */
  staleMs?: number;
}

/**
 * Acquire the workspace runtime lock, or throw `RuntimeLockedError` if a FRESH lock is held by another
 * runtime (D4). Steals a STALE lock (a crashed prior runtime). Returns a `RuntimeLock` the caller
 * heartbeats + releases.
 */
export async function acquireRuntimeLock(
  lockPath: string,
  opts: AcquireOptions = {},
): Promise<RuntimeLock> {
  const now = opts.now ?? Date.now;
  const id = opts.id ?? randomBytes(6).toString("hex");
  const staleMs = opts.staleMs ?? DEFAULT_LOCK_STALE_MS;
  await mkdir(dirname(lockPath), { recursive: true });

  const write = (flag: "wx" | "w"): Promise<void> =>
    writeFile(lockPath, JSON.stringify({ id, heartbeat: now() } satisfies LockRecord), {
      encoding: "utf8",
      flag,
    });

  try {
    // Happy path: atomic exclusive create — no lock existed, we own it. Wins any fresh-start race.
    await write("wx");
  } catch {
    // A lock file already exists — inspect it.
    const existing = await readLock(lockPath);
    if (existing !== null) {
      const age = now() - existing.heartbeat;
      if (existing.id !== id && age < staleMs) {
        // A DIFFERENT runtime, heartbeat still fresh → it is live. Refuse (D4).
        throw new RuntimeLockedError(existing.id, age);
      }
      // Ours (idempotent re-acquire) or STALE (prior runtime died) → steal by overwriting.
    }
    await write("w");
  }

  return {
    id,
    async heartbeat(): Promise<void> {
      try {
        await write("w");
      } catch {
        // best-effort — a transient FS failure must not crash the runtime; the next tick retries
      }
    },
    async release(): Promise<void> {
      const cur = await readLock(lockPath);
      if (cur !== null && cur.id !== id) return; // a stealer owns it now — never delete their lock
      try {
        await rm(lockPath, { force: true });
      } catch {
        // already gone / unlink race — release is best-effort + idempotent
      }
    },
  };
}
