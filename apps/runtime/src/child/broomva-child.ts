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

import { rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Budget, WorkContract } from "@maestro/protocol";
import { DEFAULT_CONTEXT_CEILING_TOKENS } from "../config";
import { readContractSnapshot } from "../harness/contract-snapshot";
import type { ChildEmittedEvent } from "../harness/runner";
import { parseChildArgv } from "../harness/spawn-contract";
import {
  type BeatState,
  beatExitEvents,
  evaluateBeat,
  prepareRestart,
} from "../harness/stop-conditions";
import { executeTool, parseToolUses, TOOL_SCHEMAS, type ToolUse, toolResultBlock } from "./tools";

/** The agent system prompt — sets the loop's contract with the model: use the tools to make real
 *  progress, and STOP (reply with no tool call) only when the work is genuinely done. Kept terse; the
 *  work specifics come from the contract-derived user prompt. */
const SYSTEM_PROMPT =
  "You are a Maestro agent working autonomously in a git worktree. Use the provided tools (shell, read, " +
  "edit) to make real progress on the work item, one step per turn. Verify your changes. When the work " +
  "is genuinely complete, reply with a short summary and NO tool call; do not stop early.";

/** A stalled proxy (accepts the connection, never responds) must not tie up a live run to the
 *  supervisor's coarse ~5-min liveness watchdog — a per-call abort turns it into a fast child-declared
 *  receipt (P20 BRO-1854). Sized to a single local-proxy call, well under the run-silence window. */
const MODEL_CALL_TIMEOUT_MS = 120_000;

/** Paranoid backstop on the beat count — the stop-engine's iteration_cap (default 30) halts the loop
 *  first; this only guards against a misconfigured engine that never halts (it never should be reached). */
const MAX_BEATS = 10_000;

/** How many recent per-beat signatures to retain — the no_progress halt reads only the last N
 *  (DEFAULT_NO_PROGRESS_N=3), so a generous window bounds memory without truncating what the halt needs. */
const RECENT_WINDOW = 16;

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

/** The assistant turn to append to `messages`: the response's TEXT blocks verbatim + a tool_use block for
 *  each ACCEPTED tool call (`uses`, from parseToolUses). Reconstructing the tool_use blocks from the
 *  accepted set — rather than echoing `content[]` verbatim — guarantees every tool_use the next turn
 *  references has a matching tool_result: a malformed tool_use that parseToolUses dropped can't survive as
 *  an unpaired echo that a conformant Anthropic endpoint would 400 on. */
function assistantContent(body: unknown, uses: readonly ToolUse[]): unknown[] {
  const blocks: unknown[] = [];
  if (body && typeof body === "object") {
    const c = (body as { content?: unknown }).content;
    if (Array.isArray(c)) {
      for (const b of c) {
        if (b && typeof b === "object" && (b as { type?: unknown }).type === "text") blocks.push(b);
      }
    }
  }
  for (const u of uses) blocks.push({ type: "tool_use", id: u.id, name: u.name, input: u.input });
  return blocks;
}

/** The Anthropic `stop_reason` of a response body, or undefined if absent/malformed. */
function stopReasonOf(body: unknown): string | undefined {
  if (body && typeof body === "object") {
    const sr = (body as { stop_reason?: unknown }).stop_reason;
    if (typeof sr === "string") return sr;
  }
  return undefined;
}

/** True when a tool-less turn means the model is DONE (→ clean exit 0). `end_turn` / `stop_sequence` (and
 *  an absent stop_reason — a malformed/bodyless turn is a valid-but-empty completion) are done. `max_tokens`
 *  (and any other non-completion reason) is NOT: the turn was TRUNCATED mid-thought, so exiting 0 would
 *  report a cut-off run as finished — the loop must continue instead (bounded by no_progress). */
function isCompletion(stopReason: string | undefined): boolean {
  return stopReason === undefined || stopReason === "end_turn" || stopReason === "stop_sequence";
}

/** The worktree's current HEAD (the run BASE, captured once at loop start), or "HEAD" if git can't
 *  resolve it. Beat signatures diff vs THIS base, not the moving HEAD, so a beat that COMMITS its work
 *  still reads as progress ("the branch is the receipt") rather than emptying the diff → false no_progress. */
async function gitHead(cwd: string): Promise<string> {
  try {
    const p = Bun.spawn(["git", "rev-parse", "HEAD"], { cwd, stdout: "pipe", stderr: "ignore" });
    const out = (await new Response(p.stdout).text()).trim();
    return (await p.exited) === 0 && out !== "" ? out : "HEAD";
  } catch {
    return "HEAD";
  }
}

/** The per-beat worktree signature — CONTENT-sensitive, not just file-status. `git status --porcelain`
 *  shows only status (added/modified/deleted), so a beat that edits the CONTENT of an already-dirty file
 *  would read as no change → a FALSE no_progress halt on the common "refine the same file" pattern. Here
 *  we stage the whole worktree into a THROWAWAY index (GIT_INDEX_FILE, so the real index/worktree are
 *  untouched; .gitignore still applies) and hash the diff vs the run BASE → the hash moves iff any file's
 *  content moved (committed OR uncommitted). Returns `{sig, stat}`: `sig` (compared beat-to-beat) + `stat`
 *  for the
 *  run.beat receipt, and `measured` (false on git failure). On git failure the caller treats the beat as
 *  no-change but does NOT advance the baseline (so an intermittent failure can't read the next real beat
 *  as spurious progress and reset the no_progress window); a persistently git-broken worktree converges
 *  via no_progress / iteration_cap rather than spinning (an accepted degradation — it's broken anyway). */
async function beatSignal(
  cwd: string,
  base: string,
): Promise<{ sig: string; stat: string; measured: boolean }> {
  const idxPath = join(tmpdir(), `maestro-beat-${process.pid}.idx`);
  const env = { ...process.env, GIT_INDEX_FILE: idxPath };
  try {
    const add = Bun.spawn(["git", "add", "-A"], { cwd, env, stdout: "ignore", stderr: "ignore" });
    if ((await add.exited) !== 0) return { sig: "", stat: "(unmeasured)", measured: false };
    // Diff the staged worktree vs the run BASE (not HEAD) — a committed beat stays "ahead of base", so
    // committing counts as progress instead of emptying a HEAD-relative diff.
    const diff = Bun.spawn(["git", "diff", "--cached", base], {
      cwd,
      env,
      stdout: "pipe",
      stderr: "ignore",
    });
    const out = await new Response(diff.stdout).text();
    if ((await diff.exited) !== 0) return { sig: "", stat: "(unmeasured)", measured: false };
    // Count +/- CONTENT lines (skip the +++/--- file headers) for a cheap human diffstat.
    let changed = 0;
    for (const line of out.split("\n")) {
      if (
        (line.startsWith("+") || line.startsWith("-")) &&
        !line.startsWith("+++") &&
        !line.startsWith("---")
      ) {
        changed++;
      }
    }
    return {
      sig: String(Bun.hash(out)),
      stat: changed === 0 ? "(no change)" : `${changed} line(s)`,
      measured: true,
    };
  } catch {
    return { sig: "", stat: "(unmeasured)", measured: false };
  } finally {
    // Don't leave the throwaway index behind — a fresh one is staged next beat (git creates it on absence).
    await rm(idxPath, { force: true }).catch(() => {});
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
      // No model id — the proxy resolves the role-pinned model. `tools` is LOAD-BEARING: without it a
      // real Anthropic model can never emit a tool_use block, so the whole agentic loop degenerates to a
      // single narration turn (the [[mock-fidelity-gap-false-green]] the mock used to hide). `system` sets
      // the loop contract. max_tokens is required for the proxy's pre-forward budget reservation
      // (HARNESS §3 / BRO-1788), sized for a real turn (narration + a tool call) so it isn't truncated.
      body: JSON.stringify({
        max_tokens: 8192,
        system: SYSTEM_PROMPT,
        tools: TOOL_SCHEMAS,
        messages,
      }),
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
  // Emit run.started FIRST so EVERY exit — including a malformed-argv failure below — is preceded by a
  // child receipt (the every-exit-emits-a-run.exiting invariant), not a receiptless crash the supervisor
  // has to infer. (main() is guarded behind import.meta.main, so importing the module for its pure
  // helpers still has no side effect.)
  await emit({ actor: "system", type: "run.started" });
  // Validate the supervisor argv (--role agent --session <id>). `role` resolves the model PROXY-side, so
  // the child never sends a model id; `session` is the prompt attribution. A malformed argv is a
  // supervisor bug — report it with a receipt (not a receiptless throw) so the run parks with a reason.
  let session: string;
  try {
    ({ session } = parseChildArgv(Bun.argv.slice(2)));
  } catch (err) {
    await emit({
      actor: "system",
      type: "run.exiting",
      payload: { code: 1, reason: `bad argv: ${msg(err)}` },
    });
    process.exit(1);
  }
  // The supervisor spawns the child with cwd = the run worktree — tools operate there (the phase-1
  // containment); the diff signature is measured there too.
  const cwd = process.cwd();

  const contract = await readContract();
  const budget: Budget = contract?.budget ?? {};
  const messages: Msg[] = [{ role: "user", content: promptFor(contract, session) }];
  const state: BeatState = {
    iterations: 0,
    budget,
    // spentUsd/dayUsd stay 0: the child has no channel to the running spend in this slice, so the
    // engine's end-of-beat `budget` backstop is inert — the IN-PATH proxy guard (402, BRO-1788) is the
    // budget enforcement (tested), and it cannot be disabled. Feeding real spend here is a follow-up.
    spentUsd: 0,
    dayUsd: 0,
    recentDiffs: [],
    recentErrors: [],
    contextTokens: 0,
    ceiling: resolveCeiling(),
    // Honor a contract that narrows the halts (Done.stop_on); undefined keeps the safe default (all three).
    stopOn: contract?.done?.stop_on,
  };
  const base = await gitHead(cwd); // the run base — beat signatures diff vs this, so commits count
  let prevSig = (await beatSignal(cwd, base)).sig;

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
    // Never push an empty-content assistant turn: a conformant Anthropic endpoint 400s on it, and it
    // breaks user/assistant alternation with the nudge below. A tool-less, text-less, non-completion turn
    // reconstructs to [] → substitute a minimal placeholder so the conversation stays valid and the
    // max_tokens nudge path stays bounded by no_progress (rather than crashing on the next request).
    const asst = assistantContent(body, uses);
    messages.push({
      role: "assistant",
      content: asst.length > 0 ? asst : [{ type: "text", text: "(no output)" }],
    });

    let worstError = "";
    if (uses.length === 0) {
      // No tool call. A COMPLETED turn (end_turn / stop_sequence / an absent stop_reason) → the model is
      // done → clean exit 0 → review. A TRUNCATED turn (stop_reason "max_tokens") is NOT done — it was cut
      // off mid-thought before it could act; exiting 0 would certify incomplete work as a clean completion
      // (the "fake receipt" canon forbids). Nudge and continue; the empty beat effect below feeds
      // no_progress, so a model that keeps truncating without acting is bounded, not looped forever.
      if (isCompletion(stopReasonOf(body))) {
        await emit({ actor: "system", type: "run.exiting", payload: { code: 0 } });
        process.exit(0);
      }
      messages.push({ role: "user", content: "Continue." });
    } else {
      // ACT: execute each requested tool IN the worktree, tee tool.call then tool.result {tool,ok,summary}
      // (HARNESS §6 — summaries, never the full payload), and collect the tool_result blocks for the reply.
      const results: Record<string, unknown>[] = [];
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
        if (!result.ok) worstError = result.summary; // the beat's terminal-error signature (no_progress)
      }
      messages.push({ role: "user", content: results });
    }

    // VERIFY + LOG (a tool beat AND a nudge-continue beat both feed the stop-engine, so a repeatedly
    // truncated or stalled loop converges): did the worktree CONTENT change vs the run base?
    const cur = await beatSignal(cwd, base);
    // "" ⇒ this beat changed nothing (a no_progress input). Only a MEASURED beat advances the baseline —
    // an unmeasured (git-failed) beat counts as no-change but leaves prevSig at the last real signature,
    // so the next measured beat compares honestly instead of reading a poisoned "" baseline as progress.
    let diffSig: string;
    if (cur.measured) {
      diffSig = cur.sig === prevSig ? "" : cur.sig;
      prevSig = cur.sig;
    } else {
      diffSig = "";
    }
    state.iterations = beat;
    // Keep only the recent window the engine reads (no_progress looks at the last N) — the arrays must
    // not grow with the run.
    state.recentDiffs = [...state.recentDiffs, diffSig].slice(-RECENT_WINDOW);
    state.recentErrors = [...state.recentErrors, worstError].slice(-RECENT_WINDOW);
    state.contextTokens = estimateTokens(messages);
    await emit({
      actor: "system",
      type: "run.beat",
      payload: { iteration: beat, diffstat: cur.stat },
    });

    const decision = evaluateBeat(state);
    if (decision.action === "halt") {
      for (const ev of beatExitEvents(decision, { iteration: beat })) await emit(ev);
      process.exit(10);
    }
    if (decision.action === "restart") {
      // Context ceiling → lossless restart: checkpoint progress.md (so the respawn resumes — slice 2b-ii),
      // then run.restart_requested + exit 10 fresh_context; the supervisor respawns same session/worktree.
      // prepareRestart does file I/O (writeProgress); guard it so a checkpoint-write failure (ENOSPC /
      // EROFS / unset run dir) still lands a receipt rather than crashing receiptless — the every-exit-
      // emits-a-run.exiting invariant every other branch upholds.
      let events: ChildEmittedEvent[];
      try {
        events = await prepareRestart(runDir, {
          progress: {
            session,
            iteration: beat,
            updated: new Date().toISOString(),
            stateOfTheWorld: `hit the context ceiling at beat ${beat}`,
            whatsLeft: state.recentErrors.filter((e) => e !== ""),
          },
        });
      } catch (err) {
        await emit({
          actor: "system",
          type: "run.exiting",
          payload: { code: 1, reason: `checkpoint failed: ${msg(err)}` },
        });
        process.exit(1);
      }
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
