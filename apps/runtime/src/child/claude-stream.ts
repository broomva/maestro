/// <reference types="bun" />
// claude-stream (BRO-1912) — the PURE translator from Claude Code's `stream-json` NDJSON to maestro's
// `session.jsonl` event vocabulary (HARNESS §6). The subscription path spawns the `claude` CLI as a
// runner (claude-runner.ts): the CLI IS the agent loop (it calls the model on the user's subscription,
// executes its own tools), so maestro does not drive beats or tools here — it CAPTURES the CLI's stream
// and projects it into the same events the custom `broomva-child` emits, so the supervisor's
// tee → verifier → gate flow wraps a CLI run identically to a proxy run.
//
// This module is deliberately I/O-FREE and STATELESS-EXCEPT-FOR-THE-ACCUMULATOR: given one parsed
// stream-json object + the running translator state, it returns zero or more `ChildEmittedEvent`s. The
// runner owns the process, the pipes, and git; the mapping table lives here, unit-tested against a real
// captured `claude -p --output-format stream-json` fixture (claude-stream.test.ts).
//
// Event map (captured 2026-07-15 from `claude -p --output-format stream-json`, claude_code 2.1.211):
//   system/init            → run.started {model}              (once; carries the model + apiKeySource)
//   system/{hook_*,        → (dropped — operator-side noise, not run utterances)
//     thinking_tokens}
//   assistant text block   → agent.said {text}               (trimmed; empty dropped)
//   assistant thinking     → (dropped — internal reasoning is not a run utterance, matching the child)
//   assistant tool_use     → tool.call {tool, input}         (+ one run.beat per acting turn)
//   user tool_result       → tool.result {tool, ok, summary} (tool name recovered via tool_use_id)
//   result                 → run.exiting {code, reason}      (success→0, max_turns→10 halt, else→1)
//   rate_limit_event       → (dropped unless blocking — the `result` carries a real block)

import { type ChildEmittedEvent, summarizeInput } from "../harness/runner";

/** One content block inside an `assistant`/`user` stream-json message. Only the fields we read are typed;
 *  the CLI carries more (signatures, cache stats) we intentionally ignore. */
export interface ClaudeContentBlock {
  type: string; // "text" | "thinking" | "tool_use" | "tool_result"
  text?: string;
  thinking?: string;
  // tool_use
  id?: string;
  name?: string;
  input?: unknown;
  // tool_result
  tool_use_id?: string;
  content?: unknown;
  is_error?: boolean;
}

/** One parsed line of the CLI's `stream-json` output. A permissive shape — unknown `type`s translate to
 *  nothing, so a CLI version that adds event kinds degrades to "dropped", never a crash. */
export interface ClaudeStreamEvent {
  type: string; // "system" | "assistant" | "user" | "result" | "rate_limit_event" | ...
  subtype?: string; // system: "init"|"hook_*"|"thinking_tokens"; result: "success"|"error_*"
  model?: string;
  message?: {
    model?: string;
    content?: ClaudeContentBlock[];
    usage?: unknown;
  };
  // result
  is_error?: boolean;
  result?: string;
  num_turns?: number;
  total_cost_usd?: number;
  rate_limit_info?: { status?: string };
}

/** The accumulator threaded across a run's events: whether run.started fired, the acting-beat counter,
 *  and the tool_use_id → tool-name map (a `tool_result` names its call only by id). */
export interface ClaudeTranslatorState {
  started: boolean;
  beat: number;
  toolNames: Map<string, string>;
}

export function newClaudeTranslatorState(): ClaudeTranslatorState {
  return { started: false, beat: 0, toolNames: new Map() };
}

/** Cap a tool-name / summary map so a pathological run can't grow it without bound (the CLI assigns a
 *  fresh tool_use_id per call; a very long run would otherwise retain every id forever). */
const MAX_TOOL_NAMES = 512;

/** A short, log-safe one-line summary of a tool_result's content (HARNESS §6: summaries, never payloads).
 *  Never throws — a circular / non-serializable content degrades to String(...). */
function summarizeResult(content: unknown): string {
  let s: string;
  if (content === undefined || content === null) {
    s = "";
  } else if (typeof content === "string") {
    s = content;
  } else if (Array.isArray(content)) {
    // Claude wraps tool_result content as [{type:"text", text}, ...] — join the text blocks.
    s = content
      .map((b) =>
        b && typeof b === "object" && typeof (b as { text?: unknown }).text === "string"
          ? (b as { text: string }).text
          : "",
      )
      .join(" ")
      .trim();
    if (s === "") {
      try {
        s = JSON.stringify(content) ?? String(content);
      } catch {
        s = String(content);
      }
    }
  } else {
    try {
      s = JSON.stringify(content) ?? String(content);
    } catch {
      s = String(content);
    }
  }
  // Collapse whitespace + clamp: the audit trail stores a glance, not the output.
  const oneLine = s.replace(/\s+/g, " ").trim();
  return oneLine.length > 200 ? `${oneLine.slice(0, 197)}...` : oneLine;
}

/** Map a `result` event to the child exit code the supervisor interprets (supervisor.ts): 0 = clean
 *  completion → review (Needs you); 10 = halt → park blocked (Stuck); 1 = crash → park blocked + failed. */
function exitFromResult(ev: ClaudeStreamEvent): { code: number; reason?: string } {
  // A no-error result is a clean completion regardless of subtype — the agent replied and stopped.
  if (ev.is_error !== true && (ev.subtype === "success" || ev.subtype === undefined)) {
    return { code: 0 };
  }
  // The turn cap is a soft stop (the CLI ran out of allowed turns, not a fault) — halt → Stuck, the
  // human can redispatch, distinct from a crash.
  if (ev.subtype === "error_max_turns") return { code: 10, reason: "max_turns" };
  // Anything else that reached `result` with is_error (execution error, rate-block) → crash-contain.
  return { code: 1, reason: ev.subtype ?? "error" };
}

/**
 * Translate ONE stream-json event into zero or more maestro events, advancing `state`. The runner calls
 * this for every parsed line and emits the results in order. Unknown event types return `[]` (forward
 * compatibility with newer CLIs).
 */
export function translateClaudeEvent(
  ev: ClaudeStreamEvent,
  state: ClaudeTranslatorState,
): ChildEmittedEvent[] {
  switch (ev.type) {
    case "system": {
      // The one system event that matters: init announces the model + that the loop is live.
      if (ev.subtype === "init" && !state.started) {
        state.started = true;
        const model = ev.model ?? ev.message?.model;
        return [
          {
            actor: "system",
            type: "run.started",
            ...(model ? { payload: { model } } : {}),
          },
        ];
      }
      return [];
    }

    case "assistant": {
      const blocks = ev.message?.content ?? [];
      const out: ChildEmittedEvent[] = [];
      let acted = false;
      for (const b of blocks) {
        if (b.type === "text") {
          const text = (b.text ?? "").trim();
          if (text !== "") out.push({ actor: "agent", type: "agent.said", payload: { text } });
        } else if (b.type === "tool_use") {
          acted = true;
          if (typeof b.id === "string" && typeof b.name === "string") {
            if (state.toolNames.size >= MAX_TOOL_NAMES) state.toolNames.clear();
            state.toolNames.set(b.id, b.name);
          }
          // Bound the input to a short, log-safe summary — the durable session.jsonl audit trail stores
          // decisions, not raw payloads (HARNESS §6). Matches broomva-child's tool.call shape (a clamped
          // string, never an unbounded object). Omit the field entirely when there's nothing to show.
          const input = summarizeInput(b.input);
          out.push({
            actor: "agent",
            type: "tool.call",
            payload:
              input === undefined ? { tool: b.name ?? "tool" } : { tool: b.name ?? "tool", input },
          });
        }
        // thinking blocks are intentionally dropped (internal reasoning, not a run utterance).
      }
      // A turn that CALLED a tool is one acting beat — the receipt the timeline shows. A text-only turn
      // is a narration/completion, not a beat (the child emits run.beat only after acting, too).
      if (acted) {
        state.beat += 1;
        out.push({ actor: "system", type: "run.beat", payload: { iteration: state.beat } });
      }
      return out;
    }

    case "user": {
      // The CLI reports each tool's outcome as a user turn carrying tool_result blocks.
      const blocks = ev.message?.content ?? [];
      const out: ChildEmittedEvent[] = [];
      for (const b of blocks) {
        if (b.type === "tool_result") {
          const tool =
            (typeof b.tool_use_id === "string" ? state.toolNames.get(b.tool_use_id) : undefined) ??
            "tool";
          out.push({
            actor: "agent",
            type: "tool.result",
            payload: { tool, ok: b.is_error !== true, summary: summarizeResult(b.content) },
          });
        }
      }
      return out;
    }

    case "result": {
      const { code, reason } = exitFromResult(ev);
      return [
        {
          actor: "system",
          type: "run.exiting",
          payload: reason ? { code, reason } : { code },
        },
      ];
    }

    default:
      // rate_limit_event, control_*, and any future event kind — dropped. A real block surfaces as an
      // is_error `result`, which the case above maps to a crash-contain exit.
      return [];
  }
}
