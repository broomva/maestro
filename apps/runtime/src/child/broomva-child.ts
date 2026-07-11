/// <reference types="bun" />
// broomva-child (BRO-1854, slice 1 of the real F2/F3 loop child) — the process the supervisor spawns as
// CHILD_BIN "broomva-child", replacing the loop-child.ts fixture. This slice does ONE model turn: read
// the frozen contract, ask the role-pinned model THROUGH the supervisor's metered proxy (never a direct
// Anthropic call, never a key in the child), emit the reply as `agent.said`, exit 0. The F3 beat loop +
// tool execution is slice 2; stdin control (chat/stop/ping) + the P2-exit E2E is slice 3.
//
// Canon: AUTONOMY §2 (Loop 1 = the Agent SDK loop; "don't over-build it" — one turn here, no tools yet),
// HARNESS §1 (argv --role/--session + the BROOMVA_* env), §3 (the proxy: bearer, no model id → the proxy
// resolves the session's pinned model + meters in-path), §6 (child utterances: run.started / agent.said /
// run.exiting on stdout as NDJSON).

import type { WorkContract } from "@maestro/protocol";
import { readContractSnapshot } from "../harness/contract-snapshot";
import type { ChildEmittedEvent } from "../harness/runner";
import { parseChildArgv } from "../harness/spawn-contract";
import { beatExitEvents } from "../harness/stop-conditions";

/** A stalled proxy (accepts the connection, never responds) must not tie up a live run to the
 *  supervisor's coarse ~5-min liveness watchdog — a per-call abort turns it into a fast child-declared
 *  receipt (P20 BRO-1854). Sized to a single local-proxy call, well under the run-silence window. */
const MODEL_CALL_TIMEOUT_MS = 120_000;

const proxyUrl = process.env.BROOMVA_MODEL_PROXY ?? "";
const token = process.env.BROOMVA_MODEL_TOKEN ?? "";
const runDir = process.env.BROOMVA_RUN_DIR ?? "";

/** Emit one NDJSON event to stdout, FLUSHED before exit — `Bun.write(Bun.stdout, …)`, not
 *  process.stdout.write: a child killed before an OS flush loses un-flushed lines (the BRO-1767 lesson). */
async function emit(ev: ChildEmittedEvent): Promise<void> {
  await Bun.write(Bun.stdout, `${JSON.stringify(ev)}\n`);
}

/** Read the frozen contract snapshot (HARNESS §1) → the resolved WorkContract, or null if unreadable —
 *  a missing/torn/invalid snapshot must not crash the child before it can report; it degrades to a bare
 *  prompt. Reuses the VALIDATED reader (assertContractSnapshot) rather than a hand-rolled JSON.parse. */
async function readContract(): Promise<WorkContract | null> {
  if (runDir === "") return null;
  try {
    return (await readContractSnapshot(runDir)).node;
  } catch {
    return null;
  }
}

/** The turn's user prompt, derived from the contract — what am I working on + the success condition. */
export function promptFor(contract: WorkContract | null, session: string): string {
  if (contract === null) {
    return `You are a Maestro agent (session ${session}). Describe your first step.`;
  }
  const done = contract.done ? ` Done when: ${JSON.stringify(contract.done)}.` : "";
  return `Work on this ${contract.kind} (id ${contract.id}).${done} Describe your first step.`;
}

/** Pull the assistant text out of an Anthropic Messages response body — the `content[]` text blocks,
 *  joined. Defensive: a null / primitive / non-array-content body yields "" rather than throwing (a JSON
 *  `null` body parses fine, so the null guard is load-bearing — without it `null.content` throws). */
export function textOf(body: unknown): string {
  if (body === null || typeof body !== "object") return "";
  const content = (body as { content?: unknown }).content;
  if (!Array.isArray(content)) return "";
  const parts: string[] = [];
  for (const block of content) {
    if (block && typeof block === "object" && (block as { type?: unknown }).type === "text") {
      const t = (block as { text?: unknown }).text;
      if (typeof t === "string") parts.push(t);
    }
  }
  return parts.join("");
}

async function main(): Promise<never> {
  // Validate the supervisor argv (--role agent --session <id>) — a malformed argv is a supervisor bug, so
  // parseChildArgv throws loudly. `role` resolves the model PROXY-side (from the session token's context),
  // so the child never sends a model id; only `session` is used here (the prompt attribution). Read INSIDE
  // main so importing this module (for the pure textOf/promptFor helpers, in tests) has no side effect.
  const { session } = parseChildArgv(Bun.argv.slice(2));
  await emit({ actor: "system", type: "run.started" });

  const contract = await readContract();

  // The model call is guarded so EVERY failure path emits a `run.exiting` receipt (HARNESS §6) —
  // symmetric with the 402/non-2xx branches below. A transport error (proxy unreachable/reset, an unset
  // BROOMVA_MODEL_PROXY → a relative-URL fetch throw) would otherwise crash the child after run.started
  // with no receipt, leaving the supervisor to route a generic run.failed with no child-declared reason.
  let resp: Response;
  try {
    resp = await fetch(`${proxyUrl}/v1/messages`, {
      method: "POST",
      headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
      // No model id — the proxy resolves the role-pinned model. max_tokens is required for the proxy's
      // pre-forward budget reservation (HARNESS §3 / BRO-1788).
      body: JSON.stringify({
        max_tokens: 1024,
        messages: [{ role: "user", content: promptFor(contract, session) }],
      }),
      // Abort a stalled proxy call so it reports a fast receipt (via the catch) instead of hanging to the
      // supervisor's coarse liveness watchdog.
      signal: AbortSignal.timeout(MODEL_CALL_TIMEOUT_MS),
    });
  } catch (err) {
    await emit({
      actor: "system",
      type: "run.exiting",
      payload: { code: 1, reason: `model unreachable: ${msg(err)}` },
    });
    process.exit(1);
  }

  // Budget refused in-path (BRO-1788): the proxy 402'd before forwarding → halt-budget + exit 10, so the
  // supervisor parks the run blocked (F3.1). beatExitEvents emits budget.exhausted + run.exiting.
  if (resp.status === 402) {
    for (const ev of beatExitEvents({ action: "halt", reason: "budget" }, { iteration: 1 })) {
      await emit(ev);
    }
    process.exit(10);
  }
  // Any other non-2xx (401 revoked, 502 upstream, 503) → the child cannot proceed; exit non-zero so the
  // supervisor crash-contains it (blocked + run.failed), worktree preserved.
  if (resp.status < 200 || resp.status >= 300) {
    await emit({
      actor: "system",
      type: "run.exiting",
      payload: { code: 1, reason: `model ${resp.status}` },
    });
    process.exit(1);
  }

  // A 2xx with an unparseable body is a FAILED turn (not an empty success) → exit 1 with a receipt. A
  // body that parses to null/empty content is a valid-but-empty turn → agent.said "" + exit 0 (textOf
  // guards it). Both stay symmetric with the error branches: a run.exiting always lands.
  let body: unknown;
  try {
    body = await resp.json();
  } catch (err) {
    await emit({
      actor: "system",
      type: "run.exiting",
      payload: { code: 1, reason: `model response unreadable: ${msg(err)}` },
    });
    process.exit(1);
  }
  await emit({ actor: "agent", type: "agent.said", payload: { text: textOf(body) } });
  await emit({ actor: "system", type: "run.exiting", payload: { code: 0 } });
  process.exit(0);
}

/** A short message from an unknown thrown value (mirrors the supervisor's `msg`). */
function msg(err: unknown): string {
  return String((err as Error)?.message ?? err);
}

// Run ONLY as the spawned entrypoint (`bun broomva-child.ts`). Importing this module (for the pure
// textOf/promptFor helpers in tests) must NOT run the turn.
if (import.meta.main) await main();
