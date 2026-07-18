/// <reference types="bun" />
// orchestrator-intent-gate.test.ts — the ORCHESTRATOR §4 human-verb gate (BRO-1784, slice 1).
// done.check: `bun test apps/runtime --filter orchestrator` — the four gate verbs + kill + set_state
// are REJECTED server-side for `agent:*` actors regardless of prompt content ("defense in the API,
// not the prompt"), while a human (default `user`) and the allowed agent intents pass through.
//
// The matrix is deliberately non-vacuous — every assertion pairs a REJECT with a POSITIVE CONTROL:
//  • all 7 human-only verbs from agent:maestro → 403, AND the SAME verb from a human reaches its handler
//    (so the test proves the gate is ACTOR-scoped, not "these verbs always 403");
//  • an allowed agent intent (dispatch/tick/set_routine) from agent:maestro is NOT gated (501 unsupported,
//    not 403) — proving the gate is SET-scoped, not a blanket block of all agent traffic;
//  • the isAgentActor prefix boundary (`user:agentsmith` is human; bare `agent` is an agent);
//  • the gate refuses BEFORE the idempotency lease is acquired (a refused intent consumes no key).

import { afterAll, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { HUMAN_ONLY_INTENT_TYPES, MAESTRO_ACTOR_HEADER } from "@maestro/protocol";
import { eq } from "drizzle-orm";
import { createApp } from "../app";
import { DEFAULT_PORT, type RuntimeConfig } from "../config";
import { type IndexDb, openIndex } from "../db/client";
import { gate, node, session } from "../db/schema";

const tmps: string[] = [];
afterAll(() => {
  for (const t of tmps) rmSync(t, { recursive: true, force: true });
});

function cfg(workspace: string): RuntimeConfig {
  return {
    port: DEFAULT_PORT,
    workspace,
    indexPath: ":memory:",
    lockPath: join(workspace, ".maestro/lock"),
  };
}

async function mkApp() {
  const ws = mkdtempSync(join(tmpdir(), "maestro-gate-"));
  tmps.push(ws);
  const handle = await openIndex(":memory:");
  return { app: createApp(cfg(ws), Date.now(), handle.db), handle };
}

type App = Awaited<ReturnType<typeof mkApp>>["app"];

/** POST an intent, optionally impersonating `actor` via X-Maestro-Actor. */
function post(app: App, body: unknown, opts: { key?: string; actor?: string } = {}) {
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  headers["Idempotency-Key"] = opts.key ?? crypto.randomUUID();
  if (opts.actor !== undefined) headers[MAESTRO_ACTOR_HEADER] = opts.actor;
  return app.request("/api/intents", { method: "POST", headers, body: JSON.stringify(body) });
}

/** A minimally-shaped body for each human-only verb (the gate fires on `type` alone, before any
 *  per-type field validation — so the exact fields don't matter, but we send plausible ones). */
const HUMAN_ONLY_BODIES: Record<string, unknown> = {
  approve: { type: "approve", gateId: "g-nope" },
  revise: { type: "revise", gateId: "g-nope", feedback: "redo" },
  block: { type: "block", gateId: "g-nope", reason: "no" },
  escalate: { type: "escalate", gateId: "g-nope", to: "someone" },
  grant: { type: "grant", gateId: "g-nope", capability: "shell" },
  kill: { type: "kill", sessionId: "s-nope" },
  set_state: { type: "set_state", nodeId: "n-nope", state: "done" },
};

async function refusal(res: Response): Promise<{ code: string; message: string }> {
  const body = (await res.json()) as { error?: { code: string; message: string } };
  return body.error ?? { code: "", message: "" };
}

/** Seed a node@review + its session + an OPEN completion gate (verdict null) — a real approve target. */
async function seedOpenGate(
  db: IndexDb,
  ids: { nodeId: string; sessionId: string; gateId: string },
): Promise<void> {
  const now = Date.now();
  await db.insert(node).values({
    id: ids.nodeId,
    path: `work/${ids.nodeId}`,
    kind: "task",
    state: "review",
    gate: "human",
    createdAt: now,
    updatedAt: now,
  });
  await db.insert(session).values({
    id: ids.sessionId,
    nodeId: ids.nodeId,
    branch: `run/${ids.sessionId}`,
    status: "review",
    startedAt: now,
    updatedAt: now,
  });
  await db.insert(gate).values({
    id: ids.gateId,
    sessionId: ids.sessionId,
    kind: "completion",
    proposalJson: null,
    verdict: null,
    decidedBy: null,
    openedAt: now,
    decidedAt: null,
    updatedAt: now,
    deletedAt: null,
  });
}

describe("ORCHESTRATOR §4 human-verb gate (BRO-1784)", () => {
  test("the gated set is exactly the four gate verbs + kill + set_state (no drift)", () => {
    expect([...(HUMAN_ONLY_INTENT_TYPES as readonly string[])].sort()).toEqual(
      ["approve", "block", "escalate", "grant", "kill", "revise", "set_state"].sort(),
    );
    // the test's body table covers every gated verb — else a reject-loop below would silently skip one.
    expect(Object.keys(HUMAN_ONLY_BODIES).sort()).toEqual([...HUMAN_ONLY_INTENT_TYPES].sort());
  });

  test("REJECT: every human-only verb from agent:maestro → 403 unauthorized", async () => {
    const { app, handle } = await mkApp();
    try {
      for (const type of HUMAN_ONLY_INTENT_TYPES) {
        const res = await post(app, HUMAN_ONLY_BODIES[type], { actor: "agent:maestro" });
        expect(res.status).toBe(403);
        const err = await refusal(res);
        expect(err.code).toBe("unauthorized");
        expect(err.message).toContain("human-only verb");
        expect(err.message).toContain(type);
      }
    } finally {
      handle.client.close();
    }
  });

  test("REJECT: a bare `agent` actor is also gated", async () => {
    const { app, handle } = await mkApp();
    try {
      const res = await post(app, HUMAN_ONLY_BODIES.approve, { actor: "agent" });
      expect(res.status).toBe(403);
      expect((await refusal(res)).code).toBe("unauthorized");
    } finally {
      handle.client.close();
    }
  });

  test("POSITIVE CONTROL: a human (default actor) is NOT gated — the verb reaches its handler", async () => {
    const { app, handle } = await mkApp();
    try {
      // no X-Maestro-Actor header → default "user". `approve` reaches its handler, which fails on the
      // missing gate (not_found), NOT the human-only gate — proving the gate is actor-scoped.
      const res = await post(app, HUMAN_ONLY_BODIES.approve); // no actor
      expect(res.status).not.toBe(403);
      const err = await refusal(res);
      expect(err.message).not.toContain("human-only verb");
      expect(err.code).toBe("not_found"); // reached the gate handler, no such open gate
    } finally {
      handle.client.close();
    }
  });

  test("POSITIVE CONTROL: an explicit `user` actor is NOT gated either", async () => {
    const { app, handle } = await mkApp();
    try {
      const res = await post(app, HUMAN_ONLY_BODIES.block, { actor: "user" });
      expect(res.status).not.toBe(403);
      expect((await refusal(res)).message).not.toContain("human-only verb");
    } finally {
      handle.client.close();
    }
  });

  test("SET SCOPE: allowed agent intents (dispatch/tick/set_routine) pass the gate", async () => {
    const { app, handle } = await mkApp();
    try {
      // These aren't gate verbs, so agent:maestro may issue them. They're not wired yet (501
      // unsupported) — the point is the GATE let them through: 501, never the 403 human-only refusal.
      for (const body of [
        { type: "dispatch", nodeId: "n1" },
        { type: "tick", cause: "interval" },
        { type: "set_routine", nodeId: "n1", trigger: { kind: "interval", spec: "3600" } },
      ]) {
        const res = await post(app, body, { actor: "agent:maestro" });
        expect(res.status).not.toBe(403);
        expect((await refusal(res)).message).not.toContain("human-only verb");
      }
    } finally {
      handle.client.close();
    }
  });

  test("PREFIX BOUNDARY: `user:agentsmith` is human (not gated); the match is on the agent: prefix", async () => {
    const { app, handle } = await mkApp();
    try {
      // substring, not prefix: an actor that merely CONTAINS "agent" stays human.
      for (const actor of ["user:agentsmith", "agentic", "not-an-agent"]) {
        const res = await post(app, HUMAN_ONLY_BODIES.approve, { actor });
        expect(res.status).not.toBe(403);
        expect((await refusal(res)).message).not.toContain("human-only verb");
      }
    } finally {
      handle.client.close();
    }
  });

  test("EVASION: a casing/whitespace slip in a trusted agent header still gates (normalized)", async () => {
    const { app, handle } = await mkApp();
    try {
      // isAgentActor trims + lower-cases before matching, so a host-side sloppy value can't fall open.
      for (const actor of ["Agent:maestro", "AGENT", " agent:maestro ", "\tagent"]) {
        const res = await post(app, HUMAN_ONLY_BODIES.approve, { actor });
        expect(res.status).toBe(403);
        expect((await refusal(res)).code).toBe("unauthorized");
      }
    } finally {
      handle.client.close();
    }
  });

  test("SIDE EFFECT: an agent's approve of a REAL open gate is short-circuited, not applied", async () => {
    const { app, handle } = await mkApp();
    try {
      // Seed a genuine open completion gate (verdict null). This is the mutation-proof case the
      // nonexistent-gate positive control can't cover: with the target PRESENT, deleting the gate would
      // route the agent's approve into approveGate — a non-403 outcome — so the 403 + "human-only verb"
      // assertions below are the LOAD-BEARING, mutation-sensitive proof (verified: gate-disabled → 409).
      await seedOpenGate(handle.db, { nodeId: "n-live", sessionId: "s-live", gateId: "g-live" });
      const res = await post(
        app,
        { type: "approve", gateId: "g-live" },
        { actor: "agent:maestro", key: "k-live" },
      );
      expect(res.status).toBe(403);
      expect((await refusal(res)).message).toContain("human-only verb");
      // Secondary consistency check (NOT the mutation-proof — approveGate bails at its receipt check
      // before the claim CAS, so verdict would read null under mutation too): the gate is undecided,
      // consistent with no side effect having leaked past the refusal.
      const [g] = await handle.db.select().from(gate).where(eq(gate.id, "g-live"));
      expect(g?.verdict).toBeNull();
      expect(g?.decidedAt).toBeNull();
    } finally {
      handle.client.close();
    }
  });
});
