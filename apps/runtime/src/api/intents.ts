// POST /api/intents — the ONLY write surface (API.md §1, PATTERNS §3: intents in,
// events out). One endpoint, a discriminated union; the response is `202 {accepted}`
// or a typed refusal (API §4), and the RESULT arrives on the SSE stream, never in the
// body. Every intent carries a required `Idempotency-Key`; the runtime records it in
// `lease`, so a retried POST is a no-op, not a double dispatch.
//
// P1 (BRO-1820) ships ONE handler — `new_mission` (FLOWS §F1): create folder + `_work.md`
// + git commit, with the COMMIT as the transaction (a failure leaves nothing half-created).
// The node row + `node.updated` stream event are NOT emitted here — the running watcher
// (BRO-1804) reconciles the new file into the index (F1 step 4; the synthetic list is
// closed — no `node.created`). Every other Intent type is a typed `unsupported_intent`
// until its seam lands (dispatch/gate verbs → P2).

import { randomUUID } from "node:crypto";
import { mkdir, rm, stat, writeFile } from "node:fs/promises";
import { join, relative, resolve, sep } from "node:path";
import {
  type ErrorCode,
  type ErrorResponse,
  EVENT_TYPES,
  type GateVerdict,
  IDEMPOTENCY_KEY_HEADER,
  type Intent,
  type IntentAccepted,
  KINDS,
  parseWorkFile,
  resolveGateVerdict,
  serializeWorkInput,
  type WorkContractInput,
} from "@maestro/protocol";
import { and, eq, gt, isNull, ne, or } from "drizzle-orm";
import type { Context, Hono } from "hono";
import type { ContentfulStatusCode } from "hono/utils/http-status";
import type { IndexDb } from "../db/client";
import { projectLiveNode } from "../db/project";
import { event, gate, lease, node, session } from "../db/schema";
import { gitCommit, gitUnstage } from "../git/git";
import { bindIndexWriter, fsJournal, SessionTee } from "../harness/stdio";

export interface IntentDeps {
  db: IndexDb;
  workspace: string;
  /**
   * Reconcile the workspace into the index + emit `node.updated` (FLOWS §F1 step 4). Called
   * fire-and-forget after a successful new_mission so the new card reaches the board over the
   * stream. Wired to the watcher's single-flight scheduler (`WatcherHandle.nudge`) so an
   * intent-driven reconcile never overlaps an fs.watch one. Absent in pure-unit tests + when the
   * watcher failed to start (the write still lands on disk; it indexes on the next scan/restart).
   */
  reconcile?: () => void;
  /**
   * Kill a live run by session id (FLOWS §F8, BRO-1801) — the supervisor's `kill` seam. Returns true
   * if a live run was killed (SIGKILL + bearer revoked; `run.killed` reaches the client on the stream),
   * false if no live run has that id. Absent until the supervisor is wired into the runtime (the kill
   * intent is `unsupported_intent` without it); pure-unit intent tests inject a fake.
   */
  kill?: (sessionId: string) => boolean;
}

/** Idempotency lease TTL — the no-op guard keys on existence, so this only bounds GC. */
const LEASE_TTL_MS = 24 * 60 * 60 * 1000;
/** This runtime's lease holder id. Single-runtime in P1 (D4 multi-holder locks are P2). */
const RUNTIME_HOLDER = "runtime";

/** A typed intent refusal carrying the API §4 code + the HTTP status to return. */
class IntentRefusal extends Error {
  constructor(
    readonly code: ErrorCode,
    message: string,
    readonly status: ContentfulStatusCode,
    readonly retryable = false,
  ) {
    super(message);
    this.name = "IntentRefusal";
  }
}

/** Kebab-case a mission title into a filesystem-safe folder name (DATA-MODEL §A.1). */
export function slugifyTitle(title: string): string {
  const s = title
    .normalize("NFKD")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 60)
    .replace(/-+$/g, "");
  return s.length > 0 ? s : "work";
}

/** Today as an ISO date (YYYY-MM-DD) — the frontmatter `created`/`updated` format. */
function isoDate(): string {
  return new Date().toISOString().slice(0, 10);
}

function refuse(c: Context, r: IntentRefusal) {
  const body: ErrorResponse = {
    error: { code: r.code, message: r.message, retryable: r.retryable },
  };
  return c.json(body, r.status);
}

/** Narrow an unknown parsed body to a `new_mission` intent, or throw invalid_intent. */
function requireNewMission(body: unknown): Extract<Intent, { type: "new_mission" }> {
  if (typeof body !== "object" || body === null) {
    throw new IntentRefusal("invalid_intent", "intent body must be a JSON object", 400);
  }
  const b = body as Record<string, unknown>;
  const str = (k: string): string => {
    const v = b[k];
    if (typeof v !== "string") {
      throw new IntentRefusal("invalid_intent", `new_mission.${k} must be a string`, 400);
    }
    return v;
  };
  const title = str("title").trim();
  if (title.length === 0) {
    throw new IntentRefusal("invalid_intent", "new_mission.title must be non-empty", 400);
  }
  const kind = b.kind;
  if (typeof kind !== "string" || !(KINDS as readonly string[]).includes(kind)) {
    throw new IntentRefusal(
      "invalid_intent",
      `new_mission.kind must be one of ${KINDS.join(" | ")}`,
      400,
    );
  }
  return {
    type: "new_mission",
    parentPath: str("parentPath"),
    title,
    brief: str("brief"),
    kind: kind as Extract<Intent, { type: "new_mission" }>["kind"],
  };
}

/**
 * FLOWS §F1: create the mission folder + `_work.md`, then commit — the commit is the
 * transaction. On ANY failure the freshly-created folder is removed AND its git-index entry
 * unstaged, so the workspace is left exactly as it was (nothing half-created).
 *
 * Frontmatter is minimal — `owner`/`budget` are OMITTED so the scanner inherits them from the
 * parent at index time (always valid to inherit). `gate` is the exception: it is pinned to
 * `human`. A fresh mission has no `done.check`, and `gate: auto` is invalid without one
 * (VERIFIER §1), so inheriting a parent's `gate: auto` would produce a contract that
 * `resolveWorkContract` → `materialize` rejects — which the scanner drops into a discarded
 * errors list, so the mission would be committed but never surface as a card (a silent write
 * loss on the sole write surface). Pinning `gate: human` keeps the child always valid; an
 * author raises it to `auto` later, once a `done.check` exists.
 */
async function handleNewMission(
  workspace: string,
  intent: Extract<Intent, { type: "new_mission" }>,
): Promise<void> {
  // Resolve + confine the parent path to the workspace (no `..` traversal).
  const parentAbs = resolve(workspace, intent.parentPath);
  if (parentAbs !== workspace && !parentAbs.startsWith(workspace + sep)) {
    throw new IntentRefusal("unauthorized", "parentPath escapes the workspace", 403);
  }
  // The parent must be an existing directory.
  try {
    const st = await stat(parentAbs);
    if (!st.isDirectory()) {
      throw new IntentRefusal(
        "not_found",
        `parentPath is not a directory: ${intent.parentPath}`,
        404,
      );
    }
  } catch (err) {
    if (err instanceof IntentRefusal) throw err;
    throw new IntentRefusal("not_found", `parentPath not found: ${intent.parentPath}`, 404);
  }

  const id = randomUUID();
  // Human-readable folder name from the title; append a short id if it collides so two
  // missions can share a title without clobbering (the id is the stable key, not the name).
  const slug = slugifyTitle(intent.title);
  let targetDir = join(parentAbs, slug);
  if (await pathExists(targetDir)) targetDir = join(parentAbs, `${slug}-${id.slice(0, 8)}`);

  const day = isoDate();
  const input: WorkContractInput = {
    id,
    kind: intent.kind,
    state: "proposed",
    // Pinned, not inherited — a checkless fresh mission cannot be gate:auto (see the doc above).
    gate: "human",
    created: day,
    updated: day,
  };
  const briefBody =
    intent.brief.trim().length > 0
      ? `# ${intent.title}\n\n${intent.brief.trim()}`
      : `# ${intent.title}`;
  const content = serializeWorkInput(input, briefBody);
  // Validate BEFORE touching the FS: a malformed contract is a refusal with nothing created.
  try {
    parseWorkFile(content);
  } catch (err) {
    throw new IntentRefusal(
      "invalid_intent",
      `could not build a valid _work.md: ${(err as Error).message}`,
      400,
    );
  }

  // FS transaction: create the folder + file, then commit. Roll back BOTH the working tree
  // (rm the folder) and the git index (unstage — a failed commit after a successful `git add`
  // would otherwise leave a phantom staged entry) on any failure.
  const rel = relative(workspace, targetDir);
  let created = false;
  try {
    await mkdir(targetDir, { recursive: false });
    created = true;
    await writeFile(join(targetDir, "_work.md"), content, "utf8");
    await gitCommit(workspace, [rel], `new work: ${intent.title}`);
  } catch (err) {
    if (created) {
      await rm(targetDir, { recursive: true, force: true });
      await gitUnstage(workspace, [rel]);
    }
    throw new IntentRefusal(
      "intent_failed",
      `new_mission side effect failed, nothing was created: ${(err as Error).message}`,
      500,
      true,
    );
  }
}

async function pathExists(p: string): Promise<boolean> {
  try {
    await stat(p);
    return true;
  } catch {
    return false;
  }
}

/**
 * Emit a `node.updated` stream event after an intent-driven `node.state` write — the SAME projection
 * the supervisor (BRO-1913) + the FS watcher (BRO-1804) use (`projectLiveNode`, single definition in
 * `db/project.ts`, so the sources can never drift), `sessionId: null` so it rides the GLOBAL stream the
 * shell subscribes to. Best-effort: a failed read/insert just leaves the client to re-derive on reconnect
 * (D5); the DB `node.state` write is the durable truth. Standalone (not the supervisor's closure) because
 * the write path has no run context.
 */
async function emitNodeUpdated(db: IndexDb, nodeId: string, now: number): Promise<void> {
  try {
    const [row] = await db.select().from(node).where(eq(node.id, nodeId));
    if (!row || row.deletedAt !== null) return; // a tombstoned node never crosses the wire
    await db.insert(event).values({
      sessionId: null,
      ts: now,
      actor: "system",
      type: EVENT_TYPES.NODE_UPDATED,
      payload: JSON.stringify(projectLiveNode(row)),
    });
  } catch {
    // best-effort — the DB node.state write is the durable truth; a reload re-derives the view
  }
}

/**
 * Resolve `gateId → its live gate → live session → live node`, applying the epoch-ownership guard. The
 * SHARED front half of every gate action (F5): the terminating-verdict spine (`decideGateVerdict`) and the
 * NON-terminating `escalateGate` both start here, so the hard-won tombstone + epoch checks live in ONE place.
 *
 * A tombstoned/superseded session's stale gate must never act on the node it once owned — the read API filters
 * tombstones (reads.ts), and the write path must too. The epoch guard refuses when a NEWER live session exists
 * for the node (a rescan-revert — the BRO-1914 gap — can strand an OLD session's pending gate while a newer run
 * re-reviews the node); filtering `deletedAt` alone is insufficient because a superseded session stays live. The
 * residual check→act TOCTOU on the epoch is the same non-transactional class BRO-1914's coordinated writer closes.
 * Throws a typed `IntentRefusal` on any miss.
 */
async function resolveGateChain(
  db: IndexDb,
  gateId: string,
): Promise<{
  g: typeof gate.$inferSelect;
  s: typeof session.$inferSelect;
  n: typeof node.$inferSelect;
}> {
  const [g] = await db
    .select()
    .from(gate)
    .where(and(eq(gate.id, gateId), isNull(gate.deletedAt)));
  if (!g) throw new IntentRefusal("not_found", `no open gate ${gateId}`, 404);
  const [s] = await db
    .select()
    .from(session)
    .where(and(eq(session.id, g.sessionId), isNull(session.deletedAt)));
  if (!s) throw new IntentRefusal("not_found", `gate ${gateId} has no live session`, 404);
  const [n] = await db.select().from(node).where(eq(node.id, s.nodeId));
  if (!n || n.deletedAt !== null) {
    throw new IntentRefusal("not_found", `gate ${gateId} node is gone`, 404);
  }
  const [newer] = await db
    .select({ id: session.id })
    .from(session)
    .where(
      and(
        eq(session.nodeId, s.nodeId),
        isNull(session.deletedAt),
        gt(session.startedAt, s.startedAt),
      ),
    )
    .limit(1);
  if (newer) {
    throw new IntentRefusal(
      "invalid_intent",
      `gate ${gateId} is superseded by a newer review session`,
      409,
    );
  }
  return { g, s, n };
}

/**
 * Decide a `review` node's open gate with a TERMINATING verdict (F5, BRO-1805 slice 2). The shared spine
 * for the terminating verbs: resolve the gate chain (`resolveGateChain`), elect a single decider with an atomic
 * CAS, journal `gate.decided` to the DURABLE run journal (FS-first, symmetric with the slice-1 `gate.opened`
 * write, so BRO-1915's replay projector can reconstruct the decided gate), then transition the node to the
 * verdict's terminal state and emit `node.updated`. Wires `block` (→ canceled, slice 2a) and `revise`
 * (→ triggered + `feedback`, slice 2b-i). The NON-terminating `escalate` (stays `review`, re-decidable —
 * gate-queue.md §4) does NOT use this spine: it must not commit a verdict, so it takes `escalateGate`. `approve`
 * (→ done + `approveMerge`) extends this spine with the merge in a later sub-slice.
 *
 * Concurrency + idempotency: the idempotency lease only dedupes a SAME-key retry, so the decision is
 * committed with an atomic conditional write (`WHERE verdict IS NULL`) electing a single winner even under
 * two concurrent DIFFERENT-key decides — only the winner journals. The node transition is ALSO conditional
 * (`WHERE state = 'review'`) so a stale-read race loser can't emit a duplicate `node.updated`, and a retry
 * that follows a partial write (a decided gate left on a still-`review` node) idempotently COMPLETES the
 * transition here rather than stranding it. `escalate` (target `review`) is a no-op transition by design.
 *
 * Durability scope: the gate row + node state are index writes (survive a normal restart; a `--rebuild` /
 * an interleaved `_work.md` rescan reverts the node state — the pre-existing BRO-1914 gap, shared with the
 * supervisor's own DB-only transitions). The `gate.decided` EVENT is FS-journaled here, so the decision
 * itself is durable + replayable; making the node state equally rebuild-durable is BRO-1914's coordinated
 * writer. Throws `IntentRefusal` (typed, API §4) on every rejection.
 */
async function decideGateVerdict(
  db: IndexDb,
  workspace: string,
  gateId: string,
  verdict: GateVerdict,
  extra: { reason?: string; feedback?: string },
  now: number,
): Promise<void> {
  const { g, n } = await resolveGateChain(db, gateId);

  // The verdict's terminal state — `resolveGateVerdict` is defined from `review` (the only decidable
  // state), so resolve against the literal `review` to get the target regardless of the node's CURRENT
  // state (a completion retry may find the node already moved). Never throws for a valid verdict.
  const target = resolveGateVerdict("review", verdict);

  if (g.verdict !== null) {
    // Already decided at read time. A conflicting verdict is refused; a MATCHING verdict falls through to
    // the idempotent completion below, so a retry after a partial write finishes the job.
    if (g.verdict !== verdict) {
      throw new IntentRefusal(
        "invalid_intent",
        `gate ${gateId} is already decided (${g.verdict})`,
        409,
      );
    }
  } else {
    // Fresh decision: the node must be at the gate (review) to decide it. Elect a single decider with an
    // atomic CAS — `WHERE verdict IS NULL` fuses the pending-check and the set, so two concurrent
    // different-key decides can't both journal (the lease only dedupes same-key). Only rowsAffected === 1
    // journals; a race loser re-reads and either 409s (conflict) or falls to the idempotent completion.
    if (n.state !== "review") {
      throw new IntentRefusal(
        "invalid_intent",
        `gate ${gateId} node is not awaiting a decision (state ${n.state})`,
        409,
      );
    }
    const won = await db
      .update(gate)
      .set({ verdict, decidedBy: "human", decidedAt: now, updatedAt: now })
      .where(and(eq(gate.id, gateId), isNull(gate.verdict), isNull(gate.deletedAt)));
    if (won.rowsAffected === 1) {
      await journalGateDecided(db, workspace, g, verdict, extra, now);
    } else {
      const [after] = await db.select().from(gate).where(eq(gate.id, gateId));
      if (after?.verdict !== verdict) {
        throw new IntentRefusal(
          "invalid_intent",
          `gate ${gateId} is already decided (${after?.verdict ?? "unknown"})`,
          409,
        );
      }
    }
  }

  // Idempotent completion — runs for the fresh winner AND a same-verdict retry / race-loser. Transition the
  // node with a CONDITIONAL write (`WHERE state = 'review'`): only the writer that actually flips
  // review → target emits `node.updated`, so a stale-read loser can't duplicate the emit, and a retry after
  // a partial write repairs the un-transitioned node here. If the node already moved, this is a no-op.
  if (target !== "review") {
    const moved = await db
      .update(node)
      .set({ state: target, updatedAt: now })
      .where(and(eq(node.id, n.id), eq(node.state, "review")));
    if (moved.rowsAffected === 1) await emitNodeUpdated(db, n.id, now);
  }
}

/** Journal `gate.decided` to the gate's run journal (FS-first + index) via a `SessionTee` — the SAME
 *  durable path the supervisor uses for `gate.opened`, so a decided gate survives beyond the index row and
 *  BRO-1915's replay can reconstruct it. runDir = `<workspace>/runs/run-<sessionId>` (the receipt dir,
 *  preserved until merge/janitor; `fsJournal` self-creates it). Session-scoped so it rides the per-session
 *  stream too. The widened payload carries what a rebuild projector needs to fold onto the opened row. */
async function journalGateDecided(
  db: IndexDb,
  workspace: string,
  g: typeof gate.$inferSelect,
  verdict: GateVerdict,
  extra: { reason?: string; feedback?: string },
  now: number,
): Promise<void> {
  const runDir = join(workspace, "runs", `run-${g.sessionId}`);
  const tee = new SessionTee({
    writer: bindIndexWriter(db),
    journal: fsJournal(runDir),
    sessionId: g.sessionId,
    now: () => now,
  });
  await tee.append({
    actor: "user",
    type: EVENT_TYPES.GATE_DECIDED,
    payload: {
      gateId: g.id,
      kind: g.kind,
      verdict,
      decidedBy: "human",
      decidedAt: now,
      // `reason` rides a `block`, `feedback` rides a `revise` (the send-back note the redispatched run picks
      // up). Only the present one is written — the payload a BRO-1915 rebuild folds onto the opened row.
      ...(extra.reason ? { reason: extra.reason } : {}),
      ...(extra.feedback ? { feedback: extra.feedback } : {}),
    },
  });
}

/**
 * Journal `gate.escalated` to the gate's run journal (FS-first + index) via a `SessionTee` — the SAME durable
 * path as `gate.decided`, but for the NON-terminating escalate: the gate is NOT decided (verdict stays null,
 * re-decidable — gate-queue.md §4), so this records the owner reassignment as its own event rather than a
 * verdict. runDir = `<workspace>/runs/run-<sessionId>`; session-scoped so it rides the per-session stream.
 */
async function journalGateEscalated(
  db: IndexDb,
  workspace: string,
  g: typeof gate.$inferSelect,
  to: string,
  now: number,
): Promise<void> {
  const runDir = join(workspace, "runs", `run-${g.sessionId}`);
  const tee = new SessionTee({
    writer: bindIndexWriter(db),
    journal: fsJournal(runDir),
    sessionId: g.sessionId,
    now: () => now,
  });
  await tee.append({
    actor: "user",
    type: EVENT_TYPES.GATE_ESCALATED,
    payload: {
      gateId: g.id,
      kind: g.kind,
      to,
      escalatedBy: "human",
      escalatedAt: now,
    },
  });
}

/**
 * Reassign a `review` node's owner via the NON-terminating `escalate` verb (point — gate-queue.md §4). Unlike
 * the terminating verdicts, escalate must NOT commit a gate verdict: the gate stays OPEN so it can still be
 * approved / revised / blocked afterward. Requires an open gate (verdict null) on a `review` node; reassigns
 * `node.owner` with a CONDITIONAL write (`state = 'review'` AND an actual owner change) so a re-escalate to the
 * SAME owner is an idempotent no-op (no duplicate journal / emit). On a real change: journal `gate.escalated`
 * (FS-first) then emit `node.updated`. Throws a typed `IntentRefusal` on any rejection.
 *
 * Durability scope mirrors `decideGateVerdict`: `node.owner` is an index write (reverts on a `_work.md` rescan
 * — the BRO-1914 gap), while the `gate.escalated` EVENT is FS-journaled and durable.
 */
async function escalateGate(
  db: IndexDb,
  workspace: string,
  gateId: string,
  to: string,
  now: number,
): Promise<void> {
  const { g, n } = await resolveGateChain(db, gateId);
  if (g.verdict !== null) {
    throw new IntentRefusal(
      "invalid_intent",
      `gate ${gateId} is already decided (${g.verdict}) and cannot be escalated`,
      409,
    );
  }
  if (n.state !== "review") {
    throw new IntentRefusal(
      "invalid_intent",
      `gate ${gateId} node is not awaiting a decision (state ${n.state})`,
      409,
    );
  }
  // Reassign owner, keeping the gate open + the node at review. Conditional on `state = 'review'` AND an actual
  // owner change (NULL-safe: `owner IS NULL OR owner != to`) so a re-escalate to the same owner is a no-op and a
  // stale-read loser can't duplicate the emit. Only the writer that actually changes the owner journals + emits.
  const changed = await db
    .update(node)
    .set({ owner: to, updatedAt: now })
    .where(
      and(eq(node.id, n.id), eq(node.state, "review"), or(isNull(node.owner), ne(node.owner, to))),
    );
  if (changed.rowsAffected === 1) {
    await journalGateEscalated(db, workspace, g, to, now);
    await emitNodeUpdated(db, n.id, now);
  }
}

/**
 * Mount POST /api/intents. Requires the open index (the idempotency lease lives there),
 * so it registers behind the same `if (index)` gate as the reads + stream in createApp.
 */
export function registerIntentRoutes(app: Hono, deps: IntentDeps): void {
  const { db, workspace, reconcile } = deps;

  app.post("/api/intents", async (c) => {
    // 1. Idempotency-Key is required on every intent (API §1).
    const key = c.req.header(IDEMPOTENCY_KEY_HEADER);
    if (!key) {
      return refuse(
        c,
        new IntentRefusal("invalid_intent", `${IDEMPOTENCY_KEY_HEADER} header is required`, 400),
      );
    }

    // 2. Parse + validate the body BEFORE acquiring the lease (a malformed intent must
    //    not consume an idempotency key).
    let body: unknown;
    try {
      body = await c.req.json();
    } catch {
      return refuse(c, new IntentRefusal("invalid_intent", "body must be valid JSON", 400));
    }
    const type = (body as { type?: unknown } | null)?.type;
    if (typeof type !== "string") {
      return refuse(c, new IntentRefusal("invalid_intent", "intent.type is required", 400));
    }
    // F8 (BRO-1801): kill { sessionId } → SIGKILL the run (canceled + run.killed on the stream). The
    // seam is `deps.kill`; without it (supervisor not wired) the intent is a typed unsupported_intent.
    if (type === "kill") {
      const kill = deps.kill;
      if (!kill) {
        return refuse(
          c,
          new IntentRefusal(
            "unsupported_intent",
            "kill is not available (no supervisor wired)",
            501,
          ),
        );
      }
      const sessionId = (body as { sessionId?: unknown }).sessionId;
      if (typeof sessionId !== "string" || sessionId.trim() === "") {
        return refuse(
          c,
          new IntentRefusal("invalid_intent", "kill.sessionId must be a non-empty string", 400),
        );
      }
      // Idempotency lease (as new_mission) — a retried same-key kill is a no-op 202, not a re-kill.
      const killNow = Date.now();
      const killIns = await db
        .insert(lease)
        .values({
          key,
          holder: RUNTIME_HOLDER,
          acquiredAt: killNow,
          expiresAt: killNow + LEASE_TTL_MS,
        })
        .onConflictDoNothing({ target: lease.key });
      if (killIns.rowsAffected === 0) {
        const ok: IntentAccepted = { accepted: true };
        return c.json(ok, 202);
      }
      // kill is a synchronous seam (SIGKILL + revoke); the run.killed event reaches the client on the
      // stream, not in this body (intents-in, events-out). Wrap it like new_mission's dispatch — a throw
      // (e.g. SIGKILL on an already-exited pid) must RELEASE the lease so a retry re-attempts, not leak
      // it behind a 500. A false = no live run → release + not_found.
      let killedLive: boolean;
      try {
        killedLive = kill(sessionId);
      } catch (err) {
        await db.delete(lease).where(eq(lease.key, key));
        return refuse(
          c,
          new IntentRefusal("intent_failed", `kill failed: ${(err as Error).message}`, 500, true),
        );
      }
      if (!killedLive) {
        await db.delete(lease).where(eq(lease.key, key));
        return refuse(
          c,
          new IntentRefusal("not_found", `no live run for session ${sessionId}`, 404),
        );
      }
      const ok: IntentAccepted = { accepted: true };
      return c.json(ok, 202);
    }
    // F5 (BRO-1805 slice 2a): block { gateId, reason? } → decide the review node's gate `block` → canceled
    // (D-GATE, gate-queue.md §4). The gate.decided + node.updated events reach the client on the stream
    // (intents-in, events-out), NOT this 202. NO reconcile — a rescan would revert the DB-only node.state
    // (the BRO-1914 durability gap, shared with the supervisor's transitions). `revise` shares this spine
    // (below); the non-terminating `escalate` takes its own path; `approve` (+ merge) is a later sub-slice.
    if (type === "block") {
      const gateId = (body as { gateId?: unknown }).gateId;
      if (typeof gateId !== "string" || gateId.trim() === "") {
        return refuse(
          c,
          new IntentRefusal("invalid_intent", "block.gateId must be a non-empty string", 400),
        );
      }
      const reasonRaw = (body as { reason?: unknown }).reason;
      if (reasonRaw !== undefined && typeof reasonRaw !== "string") {
        return refuse(
          c,
          new IntentRefusal("invalid_intent", "block.reason must be a string when present", 400),
        );
      }
      // Idempotency lease (as new_mission/kill) — a retried same-key block is a no-op 202, not a re-decide.
      // The lease acquisition is INSIDE the typed boundary so a SQLITE_BUSY / closed-db there returns a
      // typed intent_failed, never an untyped framework 500 (the write-surface contract, API §4).
      const bNow = Date.now();
      try {
        const bIns = await db
          .insert(lease)
          .values({ key, holder: RUNTIME_HOLDER, acquiredAt: bNow, expiresAt: bNow + LEASE_TTL_MS })
          .onConflictDoNothing({ target: lease.key });
        if (bIns.rowsAffected === 0) {
          const ok: IntentAccepted = { accepted: true };
          return c.json(ok, 202);
        }
      } catch (err) {
        return refuse(
          c,
          new IntentRefusal(
            "intent_failed",
            `block lease failed: ${(err as Error).message}`,
            500,
            true,
          ),
        );
      }
      // Decide the gate. On failure RELEASE the lease (best-effort — a failed release must not mask the real
      // error) so a genuine retry re-attempts; the completion path is idempotent, so a retry after a partial
      // write repairs it rather than double-deciding. An unexpected (non-refusal) DB/FS error maps to a typed
      // `intent_failed` (retryable) so the write surface never leaks an untyped 500.
      try {
        await decideGateVerdict(db, workspace, gateId, "block", { reason: reasonRaw }, bNow);
      } catch (err) {
        try {
          await db.delete(lease).where(eq(lease.key, key));
        } catch {
          // best-effort release — the lease TTL reaps it; a failed delete must not mask the real error
        }
        if (err instanceof IntentRefusal) return refuse(c, err);
        return refuse(
          c,
          new IntentRefusal("intent_failed", `block failed: ${(err as Error).message}`, 500, true),
        );
      }
      const ok: IntentAccepted = { accepted: true };
      return c.json(ok, 202);
    }
    // F5 (BRO-1805 slice 2b-i): revise { gateId, feedback } → decide the gate `revise` → triggered (send back
    // for a fresh dispatch). The SAME terminating spine as block; the `feedback` note rides the gate.decided
    // payload so the redispatched run can pick it up. Events on the stream, not this 202.
    if (type === "revise") {
      const gateId = (body as { gateId?: unknown }).gateId;
      if (typeof gateId !== "string" || gateId.trim() === "") {
        return refuse(
          c,
          new IntentRefusal("invalid_intent", "revise.gateId must be a non-empty string", 400),
        );
      }
      const feedbackRaw = (body as { feedback?: unknown }).feedback;
      if (typeof feedbackRaw !== "string" || feedbackRaw.trim() === "") {
        return refuse(
          c,
          new IntentRefusal("invalid_intent", "revise.feedback must be a non-empty string", 400),
        );
      }
      // Idempotency lease inside the typed boundary (as block) — a SQLITE_BUSY here is a typed intent_failed.
      const rNow = Date.now();
      try {
        const rIns = await db
          .insert(lease)
          .values({ key, holder: RUNTIME_HOLDER, acquiredAt: rNow, expiresAt: rNow + LEASE_TTL_MS })
          .onConflictDoNothing({ target: lease.key });
        if (rIns.rowsAffected === 0) {
          const ok: IntentAccepted = { accepted: true };
          return c.json(ok, 202);
        }
      } catch (err) {
        return refuse(
          c,
          new IntentRefusal(
            "intent_failed",
            `revise lease failed: ${(err as Error).message}`,
            500,
            true,
          ),
        );
      }
      try {
        await decideGateVerdict(db, workspace, gateId, "revise", { feedback: feedbackRaw }, rNow);
      } catch (err) {
        try {
          await db.delete(lease).where(eq(lease.key, key));
        } catch {
          // best-effort release — the lease TTL reaps it; a failed delete must not mask the real error
        }
        if (err instanceof IntentRefusal) return refuse(c, err);
        return refuse(
          c,
          new IntentRefusal("intent_failed", `revise failed: ${(err as Error).message}`, 500, true),
        );
      }
      const ok: IntentAccepted = { accepted: true };
      return c.json(ok, 202);
    }
    // F5 (BRO-1805 slice 2b-i): escalate { gateId, to } → reassign the review node's owner ("point"). NON-
    // terminating (gate-queue.md §4): the gate STAYS open + re-decidable — no verdict is committed. The
    // node.updated + gate.escalated events reach the client on the stream, not this 202.
    if (type === "escalate") {
      const gateId = (body as { gateId?: unknown }).gateId;
      if (typeof gateId !== "string" || gateId.trim() === "") {
        return refuse(
          c,
          new IntentRefusal("invalid_intent", "escalate.gateId must be a non-empty string", 400),
        );
      }
      const toRaw = (body as { to?: unknown }).to;
      if (typeof toRaw !== "string" || toRaw.trim() === "") {
        return refuse(
          c,
          new IntentRefusal("invalid_intent", "escalate.to must be a non-empty string", 400),
        );
      }
      const to = toRaw.trim();
      // Idempotency lease inside the typed boundary (as block/revise).
      const eNow = Date.now();
      try {
        const eIns = await db
          .insert(lease)
          .values({ key, holder: RUNTIME_HOLDER, acquiredAt: eNow, expiresAt: eNow + LEASE_TTL_MS })
          .onConflictDoNothing({ target: lease.key });
        if (eIns.rowsAffected === 0) {
          const ok: IntentAccepted = { accepted: true };
          return c.json(ok, 202);
        }
      } catch (err) {
        return refuse(
          c,
          new IntentRefusal(
            "intent_failed",
            `escalate lease failed: ${(err as Error).message}`,
            500,
            true,
          ),
        );
      }
      try {
        await escalateGate(db, workspace, gateId, to, eNow);
      } catch (err) {
        try {
          await db.delete(lease).where(eq(lease.key, key));
        } catch {
          // best-effort release — the lease TTL reaps it; a failed delete must not mask the real error
        }
        if (err instanceof IntentRefusal) return refuse(c, err);
        return refuse(
          c,
          new IntentRefusal(
            "intent_failed",
            `escalate failed: ${(err as Error).message}`,
            500,
            true,
          ),
        );
      }
      const ok: IntentAccepted = { accepted: true };
      return c.json(ok, 202);
    }
    // P1 implements new_mission (+ kill above); every other (valid or unknown) type is refused typed.
    if (type !== "new_mission") {
      return refuse(
        c,
        new IntentRefusal("unsupported_intent", `intent '${type}' is not implemented yet`, 501),
      );
    }
    let mission: Extract<Intent, { type: "new_mission" }>;
    try {
      mission = requireNewMission(body);
    } catch (err) {
      if (err instanceof IntentRefusal) return refuse(c, err);
      throw err;
    }

    // 3. Idempotency lease — atomic insert-or-conflict on the PK. A conflict means the key
    //    was already accepted (a retry or a concurrent duplicate) → no-op 202, no re-dispatch.
    //    KNOWN EDGE (deferred, follow-up): a TRULY concurrent same-key duplicate whose FIRST
    //    dispatch then fails yields a "phantom 202" — the second caller was acked but the first
    //    released the lease, so nothing lands. Under-dispatch (never double-dispatch), recoverable
    //    by re-post; the shipped SPA posts once. A pending/committed lease state fixes it in P2.
    const now = Date.now();
    const ins = await db
      .insert(lease)
      .values({ key, holder: RUNTIME_HOLDER, acquiredAt: now, expiresAt: now + LEASE_TTL_MS })
      .onConflictDoNothing({ target: lease.key });
    if (ins.rowsAffected === 0) {
      const ok: IntentAccepted = { accepted: true };
      return c.json(ok, 202);
    }

    // 4. Dispatch. On failure, RELEASE the lease so a genuine retry re-attempts (the key
    //    guards a SUCCESSFUL side effect, not a failed one), then return the typed refusal.
    try {
      await handleNewMission(workspace, mission);
    } catch (err) {
      await db.delete(lease).where(eq(lease.key, key));
      if (err instanceof IntentRefusal) return refuse(c, err);
      throw err;
    }

    // Intents in, events out (PATTERNS §3, F1 step 4): reconcile the new file into the index +
    // emit node.updated. Fire-and-forget through the watcher's single-flight scheduler — the
    // result reaches the client on the stream, not in this 202.
    reconcile?.();

    const ok: IntentAccepted = { accepted: true };
    return c.json(ok, 202);
  });
}
