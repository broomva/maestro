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

import { readFile } from "node:fs/promises";
import type { WorkContract } from "@maestro/protocol";
import type { ChildEmittedEvent } from "../harness/runner";
import { parseChildArgv } from "../harness/spawn-contract";
import { beatExitEvents } from "../harness/stop-conditions";

// Validate the supervisor argv (--role agent --session <id>) — a malformed argv is a supervisor bug, so
// parseChildArgv throws loudly. `role` resolves the model PROXY-side (from the session token's context),
// so the child never sends a model id; only `session` is used here (the prompt attribution).
const { session } = parseChildArgv(Bun.argv.slice(2));

const proxyUrl = process.env.BROOMVA_MODEL_PROXY ?? "";
const token = process.env.BROOMVA_MODEL_TOKEN ?? "";
const contractPath = process.env.BROOMVA_CONTRACT ?? "";

/** Emit one NDJSON event to stdout, FLUSHED before exit — `Bun.write(Bun.stdout, …)`, not
 *  process.stdout.write: a child killed before an OS flush loses un-flushed lines (the BRO-1767 lesson). */
async function emit(ev: ChildEmittedEvent): Promise<void> {
  await Bun.write(Bun.stdout, `${JSON.stringify(ev)}\n`);
}

/** Read the frozen contract snapshot (HARNESS §1) → the resolved WorkContract, or null if unreadable —
 *  a missing/torn contract must not crash the child before it can report; it degrades to a bare prompt. */
async function readContract(): Promise<WorkContract | null> {
  if (contractPath === "") return null;
  try {
    const snap = JSON.parse(await readFile(contractPath, "utf8")) as { node?: WorkContract };
    return snap.node ?? null;
  } catch {
    return null;
  }
}

/** The turn's user prompt, derived from the contract — what am I working on + the success condition. */
function promptFor(contract: WorkContract | null): string {
  if (contract === null) {
    return `You are a Maestro agent (session ${session}). Describe your first step.`;
  }
  const done = contract.done ? ` Done when: ${JSON.stringify(contract.done)}.` : "";
  return `Work on this ${contract.kind} (id ${contract.id}).${done} Describe your first step.`;
}

/** Pull the assistant text out of an Anthropic Messages response body — the `content[]` text blocks,
 *  joined. Defensive: a non-array / non-text body yields "" rather than throwing. */
function textOf(body: unknown): string {
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
  await emit({ actor: "system", type: "run.started" });

  const contract = await readContract();
  const resp = await fetch(`${proxyUrl}/v1/messages`, {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    // No model id — the proxy resolves the role-pinned model. max_tokens is required for the proxy's
    // pre-forward budget reservation (HARNESS §3 / BRO-1788).
    body: JSON.stringify({
      max_tokens: 1024,
      messages: [{ role: "user", content: promptFor(contract) }],
    }),
  });

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

  const body = await resp.json().catch(() => ({}));
  await emit({ actor: "agent", type: "agent.said", payload: { text: textOf(body) } });
  await emit({ actor: "system", type: "run.exiting", payload: { code: 0 } });
  process.exit(0);
}

await main();
