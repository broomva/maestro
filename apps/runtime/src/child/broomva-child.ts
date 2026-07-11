/// <reference types="bun" />
// broomva-child (BRO-1855, slice 2b-i of the real F2/F3 loop child) — the process the supervisor spawns
// as CHILD_BIN "broomva-child". Slice 1 (BRO-1854) did ONE model turn; this slice turns it into the F3
// BEAT LOOP with tool execution: each beat asks the role-pinned model THROUGH the metered proxy (never a
// direct Anthropic call, never a key in the child), executes any tools the model requested IN the run
// worktree, feeds the beat effect (the worktree diff) into the BRO-1795 stop-engine, and either continues,
// HALTS (park blocked), or RESTARTS fresh (respawn). A turn with NO tool call is a clean completion.
//
// SPLIT (documented on BRO-1855): slice 2b-i = the loop + tool execution + stop-engine wiring (halt +
// restart branches) + tool/beat events, proven through the real supervisor+proxy+mock. Slice 2b-ii =
// retire the loop-child.ts fixture (re-point loops.test.ts's fresh_context respawn-RESUME + kill scenarios
// onto this child) — the respawn-resume (reading progress.md on attempt ≥2) lands there.
//
// Canon: AUTONOMY §1 (the five beats: trigger → find work → act → verify → log), §3–4 (guardrails: loops
// don't get tired), HARNESS §3 (proxy: bearer, no model id → the proxy resolves + meters), §5 (stop
// conditions), §6 (child utterances: run.started / agent.said / tool.call / tool.result / run.beat /
// run.exiting on stdout as NDJSON; the proxy owns budget.metered/refused, the child owns budget.exhausted).

import type { Budget, WorkContract } from "@maestro/protocol";
import { DEFAULT_CONTEXT_CEILING_TOKENS } from "../config";
import { git } from "../git/git";
import { readContractSnapshot } from "../harness/contract-snapshot";
import type { ChildEmittedEvent } from "../harness/runner";
import { parseChildArgv } from "../harness/spawn-contract";
import {
  type BeatState,
  beatExitEvents,
  evaluateBeat,
  prepareRestart,
} from "../harness/stop-conditions";
import { executeTool, parseToolUses, toolResultBlock } from "./tools";

/** A stalled proxy (accepts the connection, never responds) must not tie up a live run to the
 *  supervisor's coarse ~5-min liveness watchdog — a per-call abort turns it into a fast child-declared
 *  receipt (P20 BRO-1854). Sized to a single local-proxy call, well under the run-silence window. */
const MODEL_CALL_TIMEOUT_MS = 120_000;

/** Paranoid backstop on the beat count — the stop-engine's iteration_cap (default 30) halts the loop
 *  first; this only guards against a misconfigured engine that never halts (it never should be reached). */
const MAX_BEATS = 10_000;

const proxyUrl = process.env.BROOMVA_MODEL_PROXY ?? "";
const token = process.env.BROOMVA_MODEL_TOKEN ?? "";
const runDir = process.env.BROOMVA_RUN_DIR ?? "";

/** Context-size ceiling (tokens) past which the child restarts fresh. Read from `BROOMVA_CONTEXT_CEILING`
 *  (a BROOMVA_* contract var the supervisor passes — 2b-ii wires it from the runtime config), else the
 *  runtime default. A non-positive / non-numeric value falls back to the default (never 0 = disabled). */
function resolveCeiling(): number {
  const raw = Number(process.env.BROOMVA_CONTEXT_CEILING);
  return Number.isFinite(raw) && raw > 0 ? raw : DEFAULT_CONTEXT_CEILING_TOKENS;
}

/** One Anthropic Messages turn in the running conversation the child accumulates across beats. */
interface Msg {
  role: "user" | "assistant";
  content: unknown;
}

/** Emit one NDJSON event to stdout, FLUSHED before exit — `Bun.write(Bun.stdout, …)`, not
 *  process.stdout.write: a child killed before an OS flush loses un-flushed lines (the BRO-1767 lesson). */
async function emit(ev: ChildEmittedEvent): Promise<void> {
  await Bun.write(Bun.stdout, `${JSON.stringify(ev)}\n`);
}

/** A short message from an unknown thrown value (mirrors the supervisor's `msg`). */
function msg(err: unknown): string {
  return String((err as Error)?.message ?? err);
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

/** The run's opening user prompt, derived from the contract — what am I working on + the success condition. */
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

/** The assistant turn to append to `messages` — the response's `content[]` verbatim (text + tool_use
 *  blocks) so the next turn's `tool_result` blocks reference the same tool_use ids. `[]` if malformed. */
function assistantContent(body: unknown): unknown[] {
  if (body && typeof body === "object") {
    const c = (body as { content?: unknown }).content;
    if (Array.isArray(c)) return c;
  }
  return [];
}

/** The per-beat change signature — `git status --porcelain` of the worktree (cwd). "" on git failure: a
 *  worktree where git can't run can't show trackable progress, so the loop then converges via no_progress
 *  / iteration_cap rather than spinning. The CALLER diffs this against the previous beat to decide empty. */
async function porcelain(cwd: string): Promise<string> {
  try {
    const r = await git(cwd, ["status", "--porcelain"]);
    return r.code === 0 ? r.stdout : "";
  } catch {
    return "";
  }
}

/** A rough token estimate for the accumulated conversation (~4 chars/token) — the context-ceiling signal
 *  fed to the stop-engine. Deliberately cheap + monotonic-in-size; exact tokenization is not needed to
 *  decide "carrying too much, restart fresh". */
function estimateTokens(messages: readonly Msg[]): number {
  return Math.ceil(JSON.stringify(messages).length / 4);
}

/** POST one turn to the proxy. Returns the parsed body on 2xx, or a tagged failure the loop maps to a
 *  child-declared receipt — NEVER throws (a transport error / unreadable body is DATA, symmetric with the
 *  402/non-2xx branches, so every exit path lands a run.exiting). */
type ProxyTurn =
  | { kind: "ok"; body: unknown }
  | { kind: "budget" }
  | { kind: "http"; status: number }
  | { kind: "error"; reason: string };

async function callProxy(messages: readonly Msg[]): Promise<ProxyTurn> {
  let resp: Response;
  try {
    resp = await fetch(`${proxyUrl}/v1/messages`, {
      method: "POST",
      headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
      // No model id — the proxy resolves the role-pinned model. max_tokens is required for the proxy's
      // pre-forward budget reservation (HARNESS §3 / BRO-1788).
      body: JSON.stringify({ max_tokens: 1024, messages }),
      signal: AbortSignal.timeout(MODEL_CALL_TIMEOUT_MS),
    });
  } catch (err) {
    return { kind: "error", reason: `model unreachable: ${msg(err)}` };
  }
  if (resp.status === 402) return { kind: "budget" };
  if (resp.status < 200 || resp.status >= 300) return { kind: "http", status: resp.status };
  try {
    return { kind: "ok", body: await resp.json() };
  } catch (err) {
    return { kind: "error", reason: `model response unreadable: ${msg(err)}` };
  }
}

async function main(): Promise<never> {
  // Validate the supervisor argv (--role agent --session <id>). `role` resolves the model PROXY-side, so
  // the child never sends a model id; `session` is the prompt attribution. Read INSIDE main so importing
  // this module (for the pure textOf/promptFor helpers in tests) has no side effect.
  const { session } = parseChildArgv(Bun.argv.slice(2));
  // The supervisor spawns the child with cwd = the run worktree — tools operate there (the phase-1
  // containment); the diff signature is measured there too.
  const cwd = process.cwd();
  await emit({ actor: "system", type: "run.started" });

  const contract = await readContract();
  const budget: Budget = contract?.budget ?? {};
  const messages: Msg[] = [{ role: "user", content: promptFor(contract, session) }];
  const state: BeatState = {
    iterations: 0,
    budget,
    spentUsd: 0,
    dayUsd: 0,
    recentDiffs: [],
    recentErrors: [],
    contextTokens: 0,
    ceiling: resolveCeiling(),
  };
  let prevPorcelain = await porcelain(cwd);

  for (let beat = 1; beat <= MAX_BEATS; beat++) {
    const turn = await callProxy(messages);
    if (turn.kind === "error") {
      await emit({
        actor: "system",
        type: "run.exiting",
        payload: { code: 1, reason: turn.reason },
      });
      process.exit(1);
    }
    if (turn.kind === "budget") {
      // Budget refused in-path (BRO-1788): the proxy 402'd before forwarding → halt-budget + exit 10, so
      // the supervisor parks the run blocked (F3.1). beatExitEvents emits budget.exhausted + run.exiting.
      for (const ev of beatExitEvents({ action: "halt", reason: "budget" }, { iteration: beat })) {
        await emit(ev);
      }
      process.exit(10);
    }
    if (turn.kind === "http") {
      // Any other non-2xx (401 revoked, 502 upstream, 503) → cannot proceed; exit 1 so the supervisor
      // crash-contains it (blocked + run.failed), worktree preserved.
      await emit({
        actor: "system",
        type: "run.exiting",
        payload: { code: 1, reason: `model ${turn.status}` },
      });
      process.exit(1);
    }

    const body = turn.body;
    const uses = parseToolUses(body);
    const text = textOf(body);
    if (text !== "") await emit({ actor: "agent", type: "agent.said", payload: { text } });
    messages.push({ role: "assistant", content: assistantContent(body) });

    // A turn with NO tool call is the model saying it is done (end_turn) → clean completion → review.
    if (uses.length === 0) {
      await emit({ actor: "system", type: "run.exiting", payload: { code: 0 } });
      process.exit(0);
    }

    // ACT: execute each requested tool IN the worktree, tee tool.call then tool.result {tool, ok, summary}
    // (HARNESS §6 — summaries, never the full payload), and collect the tool_result blocks for the reply.
    const results: Record<string, unknown>[] = [];
    let worstError = "";
    for (const use of uses) {
      await emit({
        actor: "agent",
        type: "tool.call",
        payload: { tool: use.name, input: use.input },
      });
      const result = await executeTool(use.name, use.input, cwd);
      await emit({
        actor: "agent",
        type: "tool.result",
        payload: { tool: use.name, ok: result.ok, summary: result.summary },
      });
      results.push(toolResultBlock(use.id, result));
      if (!result.ok) worstError = result.summary; // the beat's terminal-error signature (no_progress input)
    }
    messages.push({ role: "user", content: results });

    // VERIFY + LOG: the beat effect (did the worktree change?) → the BRO-1795 stop-engine.
    const cur = await porcelain(cwd);
    const diffSig = cur === prevPorcelain ? "" : cur; // "" ⇒ this beat changed nothing (no_progress input)
    prevPorcelain = cur;
    state.iterations = beat;
    state.recentDiffs = [...state.recentDiffs, diffSig];
    state.recentErrors = [...state.recentErrors, worstError];
    state.contextTokens = estimateTokens(messages);
    await emit({
      actor: "system",
      type: "run.beat",
      payload: { iteration: beat, diffstat: diffSig === "" ? "(no change)" : diffSig },
    });

    const decision = evaluateBeat(state);
    if (decision.action === "halt") {
      for (const ev of beatExitEvents(decision, { iteration: beat })) await emit(ev);
      process.exit(10);
    }
    if (decision.action === "restart") {
      // Context ceiling → lossless restart: checkpoint progress.md (so the respawn resumes — slice 2b-ii),
      // then run.restart_requested + exit 10 fresh_context; the supervisor respawns same session/worktree.
      const events = await prepareRestart(runDir, {
        progress: {
          session,
          iteration: beat,
          updated: new Date().toISOString(),
          stateOfTheWorld: `hit the context ceiling at beat ${beat}`,
          whatsLeft: state.recentErrors.filter((e) => e !== ""),
        },
      });
      for (const ev of events) await emit(ev);
      process.exit(10);
    }
    // continue → next beat with the tool results in the conversation
  }

  // Hard-cap terminal — the stop-engine should have halted long before this; complete cleanly rather
  // than spinning if a misconfigured engine ever lets the loop run to the backstop.
  await emit({ actor: "system", type: "run.exiting", payload: { code: 0 } });
  process.exit(0);
}

// Run ONLY as the spawned entrypoint (`bun broomva-child.ts`). Importing this module (for the pure
// textOf/promptFor helpers in tests) must NOT run the loop.
if (import.meta.main) await main();
