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

/** The outcome of reading the lock file. DISTINGUISHES a confirmed-absent lock (ENOENT, stealable) from a
 *  transient read error (EIO/EMFILE/EACCES/EISDIR…) — the latter must NOT be treated as a missing lock, or
 *  a flaky fs would let a runtime steal a possibly-LIVE holder (CodeRabbit BRO-1814, fail-closed). */
type LockRead =
  | { kind: "held"; record: LockRecord } // a valid lock is present
  | { kind: "absent" } // ENOENT — confirmed missing → stealable
  | { kind: "malformed" } // present but unparseable/invalid — a torn write → overwritable
  | { kind: "error" }; // transient/unexpected read error — NOT stealable (fail closed)

/** Read + classify the lock file. Only "absent" (ENOENT) and "malformed" (unparseable body) are stealable;
 *  any other read failure is a transient "error" callers refuse/skip on rather than steal a live lock. */
async function readLock(lockPath: string): Promise<LockRead> {
  let raw: string;
  try {
    raw = await readFile(lockPath, "utf8");
  } catch (e) {
    if ((e as { code?: string }).code === "ENOENT") return { kind: "absent" };
    return { kind: "error" }; // transient/unexpected (EIO/EMFILE/EACCES/EISDIR…) — do NOT steal
  }
  try {
    const o = JSON.parse(raw) as Partial<LockRecord>;
    if (typeof o.id === "string" && typeof o.heartbeat === "number") {
      return { kind: "held", record: { id: o.id, heartbeat: o.heartbeat } };
    }
  } catch {
    // fall through — present but not valid JSON
  }
  return { kind: "malformed" }; // present but not a valid record — torn write / hand edit, overwritable
}

export interface AcquireOptions {
  /** Epoch-ms clock — default Date.now; tests pin it. */
  now?: () => number;
  /** This runtime's id — default a random hex; injected for deterministic tests. */
  id?: string;
  /** A lock older than this is stealable (default DEFAULT_LOCK_STALE_MS). */
  staleMs?: number;
  /** Called ONCE if a heartbeat discovers the lock is now held by ANOTHER runtime (we were stale-stolen
   *  after a pause > staleMs). The caller should stand down — two runtimes on one workspace is the D4
   *  violation this signals — so single-writer is restored. Best-effort: absent → heartbeat still refuses
   *  to clobber the stealer, it just does not halt. */
  onOwnershipLost?: (holderId: string) => void;
  /** TEST SEAM (internal): awaited after the stale-lock decision but BEFORE the rename-capture, so a test
   *  can deterministically interleave a competing steal+refresh and drive the post-capture reverify branch
   *  (`captured is now fresh+other → restore + refuse`). Undefined in production. */
  _afterStaleRead?: () => Promise<void>;
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
      if (existing.kind === "error") {
        // unreadable via a transient fs error — the lock may be LIVE; fail closed, never steal it.
        throw new Error("runtime lock unreadable (transient fs error) — refusing to start");
      }
      if (
        existing.kind === "held" &&
        existing.record.id !== id &&
        now() - existing.record.heartbeat < staleMs
      ) {
        throw new RuntimeLockedError(existing.record.id, now() - existing.record.heartbeat); // fresh + other → refuse
      }
      // STALE (dead prior runtime), OURS (idempotent), or unreadable → steal it ATOMICALLY. `rm`-then-
      // `link` is a non-atomic TOCTOU (P20 BRO-1814): two runtimes both observing the SAME stale lock each
      // `rm` then `link`, and the loser's `rm` deletes the winner's just-linked lock → BOTH acquire.
      // Instead CAPTURE the stale inode by renaming it aside to a unique per-acquirer name: `rename` is
      // atomic, so of N racing stealers exactly ONE rename of lockPath succeeds; the others' rename fails
      // (lockPath already moved). Only the capturer `link`s its fresh lock into the now-free path; a loser
      // re-inspects and refuses/contends. `link` stays the final arbiter, so even a fresh runtime that
      // first-tries `link` into the brief post-rename gap funnels through its EEXIST — never two holders.
      await opts._afterStaleRead?.(); // test seam: interleave a competing steal here (no-op in production)
      const dead = `${lockPath}.dead.${id}.${randomBytes(4).toString("hex")}`;
      try {
        await rename(lockPath, dead); // atomic capture — exactly one racing stealer wins this
      } catch {
        // Lost the capture race (another stealer moved lockPath first) or it vanished. Re-inspect: a fresh
        // other holder → refuse; otherwise a stealer is mid-capture → retryable contention.
        const raced = await readLock(lockPath);
        if (
          raced.kind === "held" &&
          raced.record.id !== id &&
          now() - raced.record.heartbeat < staleMs
        ) {
          throw new RuntimeLockedError(raced.record.id, now() - raced.record.heartbeat);
        }
        throw new Error("runtime lock contended during steal — retry startup");
      }
      // RE-VALIDATE the captured record (the TOCTOU `existing` didn't close): between our stale-read above
      // and this rename, a racing stealer may have stolen + refreshed the lock, so what we atomically
      // captured could now be a FRESH other lock — stealing it would double-acquire. The captured record
      // is frozen at `dead` (we hold its only name) so this freshness re-check is stable. Fresh + other →
      // RESTORE it (link it back, non-clobbering) and refuse; null/ours/stale → genuinely stealable.
      const captured = await readLock(dead);
      const capturedFreshOther =
        captured.kind === "held" &&
        captured.record.id !== id &&
        now() - captured.record.heartbeat < staleMs;
      if (capturedFreshOther || captured.kind === "error") {
        // Fresh other (a racer stole+refreshed in the TOCTOU window) OR unreadable (can't confirm it is
        // stealable) → do NOT steal: RESTORE the captured record (non-clobbering) and refuse (fail closed).
        try {
          await link(dead, lockPath); // put the record back (EEXIST if a newer holder exists)
        } catch {
          // a newer holder already occupies lockPath — leave it; our captured copy is superseded
        }
        await rm(dead, { force: true });
        if (captured.kind === "held") {
          throw new RuntimeLockedError(captured.record.id, now() - captured.record.heartbeat);
        }
        throw new Error("runtime lock capture unreadable (transient fs error) — retry startup");
      }
      try {
        await link(tmp, lockPath); // lockPath is free (we captured it) — link our fresh lock atomically
      } catch {
        // A fresh runtime `link`ed into the post-rename gap before us — it holds the lock now. Refuse if
        // fresh+other, else retryable contention. Clean up the stale inode we captured either way.
        await rm(dead, { force: true });
        const raced = await readLock(lockPath);
        if (
          raced.kind === "held" &&
          raced.record.id !== id &&
          now() - raced.record.heartbeat < staleMs
        ) {
          throw new RuntimeLockedError(raced.record.id, now() - raced.record.heartbeat);
        }
        throw new Error("runtime lock contended during steal — retry startup");
      }
      await rm(dead, { force: true }); // drop the captured stale record — we hold the fresh lock now
    }
  } finally {
    // The link left a second name for the temp inode; unlink the temp (lockPath keeps the content). On a
    // refuse/throw this just cleans the orphaned temp.
    await rm(tmp, { force: true });
  }

  let lost = false;
  return {
    id,
    async heartbeat(): Promise<void> {
      if (lost) return; // already stood down — never refresh a lock we no longer own
      // GUARD lost ownership before writing: a runtime that paused > staleMs may have been stale-stolen,
      // and an UNCONDITIONAL refresh would clobber the stealer's lock → two runtimes on one workspace
      // (split-brain, CodeRabbit BRO-1814). Re-read: if ANOTHER id owns it now, do NOT write — signal
      // loss so the caller stands down (restoring single-writer). A missing/malformed lock is nobody's,
      // so re-asserting it does not clobber a holder — refresh normally. (This narrows but cannot fully
      // close the read→rename window; true fencing needs monotonic tokens at the resource layer — the
      // documented D4 scope is best-effort mutual exclusion under staleMs > max-pause, not consensus.)
      const cur = await readLock(lockPath);
      if (cur.kind === "error") return; // transient read error — skip this tick, don't risk clobbering
      if (cur.kind === "held" && cur.record.id !== id) {
        lost = true;
        opts.onOwnershipLost?.(cur.record.id);
        return;
      }
      // ATOMIC refresh: write a temp then `rename` over lockPath (atomic replace — no truncate window a
      // concurrent acquire could read as empty + steal). Best-effort: a failure is swallowed, next tick
      // retries.
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
      // CAPTURE-based release (symmetric to the steal path): a naive readLock-then-`rm(lockPath)` is a
      // TOCTOU — a steal completing between the read and the rm makes the rm delete the STEALER's fresh
      // lock (P20 BRO-1814). Instead atomically move the lock aside, then delete only our OWN captured
      // copy: if what we captured is a stealer's lock, link it BACK (never delete their lock) and drop our
      // extra name. We only ever `rm` the uniquely-named capture file, never the shared lockPath.
      const grave = `${lockPath}.rel.${id}.${randomBytes(4).toString("hex")}`;
      try {
        await rename(lockPath, grave); // atomic capture (ENOENT → already released/gone → nothing to do)
      } catch {
        return;
      }
      const cur = await readLock(grave);
      const safeToDrop =
        cur.kind === "absent" ||
        cur.kind === "malformed" ||
        (cur.kind === "held" && cur.record.id === id);
      if (!safeToDrop) {
        // A stealer's lock, or unreadable (can't confirm it is ours) — restore it (non-clobbering), never
        // delete it.
        try {
          await link(grave, lockPath);
        } catch {
          // a newer holder already re-linked lockPath — leave it
        }
      }
      // Ours (or malformed) → dropping `grave` IS the release (lockPath stays absent). Stealer → this drops
      // only our extra name; lockPath keeps the restored record.
      await rm(grave, { force: true });
    },
  };
}
