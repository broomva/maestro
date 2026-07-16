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
import { eq } from "drizzle-orm";
import type { Context, Hono } from "hono";
import type { ContentfulStatusCode } from "hono/utils/http-status";
import type { IndexDb } from "../db/client";
import { projectLiveNode } from "../db/project";
import { event, gate, lease, node, session } from "../db/schema";
import { gitCommit, gitUnstage } from "../git/git";

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
 * Decide a `review` node's open gate with a verdict (F5, BRO-1805 slice 2). The gate-decision SPINE the
 * four verbs share: resolve `gateId → session → node`, validate, set the gate's `verdict` (the commit
 * point), journal `gate.decided` (D-DURABILITY — payload widened so BRO-1915's rebuild projector can fold
 * the decision onto the opened row), then transition the node per `resolveGateVerdict` and emit
 * `node.updated`. Slice 2a wires ONLY `block` (→ canceled); `approve` (→ done + `approveMerge`, needing
 * the durable-done write of BRO-1914) and `revise`/`escalate` land in later sub-slices on this same spine.
 *
 * Idempotency: a gate already decided with the SAME verdict is a no-op success (a same-key retry is caught
 * earlier by the lease; a different key re-hitting a decided gate lands here); a DIFFERENT prior verdict is
 * a 409 refusal. The node transition is a DB-only projection (like the supervisor's) — durable across an
 * F9 rebuild / an interleaved rescan is BRO-1914's coordinated workspace state-writer, not this slice; a
 * crash between the gate-decide and the node-transition leaves a decided gate on a still-`review` node that
 * BRO-1914/BRO-1915 reconcile. Throws `IntentRefusal` (typed, API §4) on every rejection.
 */
async function decideGateVerdict(
  db: IndexDb,
  gateId: string,
  verdict: GateVerdict,
  extra: { reason?: string },
  now: number,
): Promise<void> {
  const [g] = await db.select().from(gate).where(eq(gate.id, gateId));
  if (!g || g.deletedAt !== null) {
    throw new IntentRefusal("not_found", `no open gate ${gateId}`, 404);
  }
  const [s] = await db.select().from(session).where(eq(session.id, g.sessionId));
  if (!s) throw new IntentRefusal("not_found", `gate ${gateId} has no session`, 404);
  const [n] = await db.select().from(node).where(eq(node.id, s.nodeId));
  if (!n || n.deletedAt !== null) {
    throw new IntentRefusal("not_found", `gate ${gateId} node is gone`, 404);
  }

  // Already decided → idempotent same-verdict no-op, or a hard refusal on a conflicting prior verdict.
  if (g.verdict !== null) {
    if (g.verdict === verdict) return;
    throw new IntentRefusal(
      "invalid_intent",
      `gate ${gateId} is already decided (${g.verdict})`,
      409,
    );
  }

  // Pending → decide. The node must be at the gate (review); `resolveGateVerdict` throws off-review, and a
  // gate row only opens at review (BRO-1805 slice 1), so a non-review node here is a defensive refusal.
  let target: ReturnType<typeof resolveGateVerdict>;
  try {
    target = resolveGateVerdict(n.state, verdict);
  } catch {
    throw new IntentRefusal(
      "invalid_intent",
      `gate ${gateId} node is not awaiting a decision (state ${n.state})`,
      409,
    );
  }

  // Commit point — the verdict going non-null IS the decision. Then journal it (durable), transition the
  // node (DB projection), and emit the live update. decidedBy is the single human for now (no auth yet).
  await db
    .update(gate)
    .set({ verdict, decidedBy: "human", decidedAt: now, updatedAt: now })
    .where(eq(gate.id, gateId));
  await db.insert(event).values({
    sessionId: g.sessionId,
    ts: now,
    actor: "user",
    type: EVENT_TYPES.GATE_DECIDED,
    payload: JSON.stringify({
      gateId,
      kind: g.kind,
      verdict,
      decidedBy: "human",
      decidedAt: now,
      ...(extra.reason ? { reason: extra.reason } : {}),
    }),
  });
  await db.update(node).set({ state: target, updatedAt: now }).where(eq(node.id, n.id));
  await emitNodeUpdated(db, n.id, now);
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
    // (the BRO-1914 durability gap, shared with the supervisor's transitions). The other three verdicts
    // (approve/revise/escalate) ride the same `decideGateVerdict` spine in later sub-slices.
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
      const bNow = Date.now();
      const bIns = await db
        .insert(lease)
        .values({ key, holder: RUNTIME_HOLDER, acquiredAt: bNow, expiresAt: bNow + LEASE_TTL_MS })
        .onConflictDoNothing({ target: lease.key });
      if (bIns.rowsAffected === 0) {
        const ok: IntentAccepted = { accepted: true };
        return c.json(ok, 202);
      }
      // Decide the gate. On failure RELEASE the lease so a genuine retry re-attempts (the key guards a
      // successful decision, not a failed one), then return the typed refusal.
      try {
        await decideGateVerdict(db, gateId, "block", { reason: reasonRaw }, bNow);
      } catch (err) {
        await db.delete(lease).where(eq(lease.key, key));
        if (err instanceof IntentRefusal) return refuse(c, err);
        throw err;
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
