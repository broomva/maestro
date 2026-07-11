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
import { link, mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
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

  const record = (): string => JSON.stringify({ id, heartbeat: now() } satisfies LockRecord);
  const tmpPath = (): string => `${lockPath}.${id}.${randomBytes(4).toString("hex")}.tmp`;

  // ATOMIC acquire: write the FULL content to a unique temp, then hard-`link` it onto lockPath. `link`
  // is atomic + fails EEXIST if the lock is held — and because the content is written to the temp BEFORE
  // the link, a concurrent reader never sees a half-written / empty lock (the create-then-write window
  // that `writeFile(flag:"wx")` leaves open, which let a racing loser read `null` and double-acquire).
  const tmp = tmpPath();
  await writeFile(tmp, record(), "utf8");
  try {
    try {
      await link(tmp, lockPath); // won the (possibly-racing) fresh acquire
    } catch {
      // lockPath already exists — inspect the (fully-written) holder.
      const existing = await readLock(lockPath);
      if (existing !== null && existing.id !== id && now() - existing.heartbeat < staleMs) {
        throw new RuntimeLockedError(existing.id, now() - existing.heartbeat); // fresh + other → refuse
      }
      // STALE (dead prior runtime), OURS (idempotent), or unreadable → steal: remove + retry the link.
      await rm(lockPath, { force: true });
      try {
        await link(tmp, lockPath);
      } catch {
        // A racing stealer linked between our rm and link. Refuse if it is a fresh other; else surface a
        // retryable contention error rather than risk a double-acquire by forcing.
        const raced = await readLock(lockPath);
        if (raced !== null && raced.id !== id && now() - raced.heartbeat < staleMs) {
          throw new RuntimeLockedError(raced.id, now() - raced.heartbeat);
        }
        throw new Error("runtime lock contended during steal — retry startup");
      }
    }
  } finally {
    // The link left a second name for the temp inode; unlink the temp (lockPath keeps the content). On a
    // refuse/throw this just cleans the orphaned temp.
    await rm(tmp, { force: true });
  }

  return {
    id,
    async heartbeat(): Promise<void> {
      // ATOMIC refresh: write a temp then `rename` over lockPath (atomic replace — no truncate window a
      // concurrent acquire could read as empty + steal). Best-effort: a failure is swallowed, next tick
      // retries. (A heartbeat that lands AFTER we were stale-stolen could clobber the stealer — bounded
      // by staleMs > max pause, the standard file-lock limit; D4 is mutual exclusion, not consensus.)
      const t = tmpPath();
      try {
        await writeFile(t, record(), "utf8");
        await rename(t, lockPath);
      } catch {
        try {
          await rm(t, { force: true });
        } catch {
          // temp already gone — nothing to clean
        }
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
