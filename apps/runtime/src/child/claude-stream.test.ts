// claude-stream translation tests (BRO-1912). The fixtures are REAL lines captured from
// `claude -p --output-format stream-json` on this machine (claude_code 2.1.211, subscription,
// apiKeySource:none) — trimmed to the fields the translator reads. The full-run test replays a whole
// dispatch so the ordered event projection is locked, not just per-event mapping.

import { describe, expect, test } from "bun:test";
import {
  type ClaudeStreamEvent,
  type ClaudeTranslatorState,
  newClaudeTranslatorState,
  translateClaudeEvent,
} from "./claude-stream";

/** Drive a sequence of events through one fresh state, flattening the emitted events in order. */
function run(events: ClaudeStreamEvent[]): {
  emitted: ReturnType<typeof translateClaudeEvent>;
  state: ClaudeTranslatorState;
} {
  const state = newClaudeTranslatorState();
  const emitted = events.flatMap((e) => translateClaudeEvent(e, state));
  return { emitted, state };
}

describe("claude-stream — per-event mapping", () => {
  test("system/init → run.started {model}, exactly once", () => {
    const state = newClaudeTranslatorState();
    const init: ClaudeStreamEvent = {
      type: "system",
      subtype: "init",
      model: "claude-opus-4-8",
    };
    const first = translateClaudeEvent(init, state);
    expect(first).toEqual([
      { actor: "system", type: "run.started", payload: { model: "claude-opus-4-8" } },
    ]);
    // A second init (the CLI never sends one, but the mapping must be idempotent) emits nothing.
    expect(translateClaudeEvent(init, state)).toEqual([]);
  });

  test("system init reads message.model when the top-level model is absent", () => {
    const { emitted } = run([
      { type: "system", subtype: "init", message: { model: "claude-haiku-4-5-20251001" } },
    ]);
    expect(emitted).toEqual([
      {
        actor: "system",
        type: "run.started",
        payload: { model: "claude-haiku-4-5-20251001" },
      },
    ]);
  });

  test("operator-side system noise (hooks, thinking_tokens) is dropped", () => {
    const { emitted } = run([
      { type: "system", subtype: "hook_started" },
      { type: "system", subtype: "hook_response" },
      { type: "system", subtype: "thinking_tokens" },
    ]);
    expect(emitted).toEqual([]);
  });

  test("assistant thinking block is dropped; text becomes agent.said (trimmed)", () => {
    const { emitted } = run([
      { type: "assistant", message: { content: [{ type: "thinking", thinking: "planning..." }] } },
      {
        type: "assistant",
        message: { content: [{ type: "text", text: "  maestro wiring works  " }] },
      },
    ]);
    expect(emitted).toEqual([
      { actor: "agent", type: "agent.said", payload: { text: "maestro wiring works" } },
    ]);
  });

  test("empty / whitespace-only text emits nothing", () => {
    const { emitted } = run([
      { type: "assistant", message: { content: [{ type: "text", text: "   " }] } },
    ]);
    expect(emitted).toEqual([]);
  });

  test("assistant tool_use → tool.call + one run.beat; result maps the tool name by id", () => {
    const { emitted } = run([
      {
        type: "assistant",
        message: {
          content: [
            { type: "text", text: "Reading the roadmap." },
            { type: "tool_use", id: "toolu_1", name: "Read", input: { path: "roadmap.md" } },
          ],
        },
      },
      {
        type: "user",
        message: {
          content: [
            { type: "tool_result", tool_use_id: "toolu_1", content: "# Q3 roadmap\nship maestro" },
          ],
        },
      },
    ]);
    expect(emitted).toEqual([
      { actor: "agent", type: "agent.said", payload: { text: "Reading the roadmap." } },
      {
        actor: "agent",
        type: "tool.call",
        payload: { tool: "Read", input: { path: "roadmap.md" } },
      },
      { actor: "system", type: "run.beat", payload: { iteration: 1 } },
      {
        actor: "agent",
        type: "tool.result",
        payload: { tool: "Read", ok: true, summary: "# Q3 roadmap ship maestro" },
      },
    ]);
  });

  test("tool_result with is_error → ok:false", () => {
    const state = newClaudeTranslatorState();
    translateClaudeEvent(
      {
        type: "assistant",
        message: {
          content: [{ type: "tool_use", id: "t2", name: "Bash", input: { cmd: "false" } }],
        },
      },
      state,
    );
    const out = translateClaudeEvent(
      {
        type: "user",
        message: {
          content: [{ type: "tool_result", tool_use_id: "t2", is_error: true, content: "exit 1" }],
        },
      },
      state,
    );
    expect(out).toEqual([
      {
        actor: "agent",
        type: "tool.result",
        payload: { tool: "Bash", ok: false, summary: "exit 1" },
      },
    ]);
  });

  test("a tool_result whose id was never seen falls back to 'tool'", () => {
    const { emitted } = run([
      {
        type: "user",
        message: { content: [{ type: "tool_result", tool_use_id: "unknown", content: "x" }] },
      },
    ]);
    expect(emitted).toEqual([
      { actor: "agent", type: "tool.result", payload: { tool: "tool", ok: true, summary: "x" } },
    ]);
  });

  test("tool_result array content joins its text blocks", () => {
    const state = newClaudeTranslatorState();
    translateClaudeEvent(
      { type: "assistant", message: { content: [{ type: "tool_use", id: "t3", name: "Grep" }] } },
      state,
    );
    const out = translateClaudeEvent(
      {
        type: "user",
        message: {
          content: [
            {
              type: "tool_result",
              tool_use_id: "t3",
              content: [
                { type: "text", text: "match one" },
                { type: "text", text: "match two" },
              ],
            },
          ],
        },
      },
      state,
    );
    expect(out[0]?.payload).toMatchObject({
      tool: "Grep",
      ok: true,
      summary: "match one match two",
    });
  });
});

describe("claude-stream — result → exit code", () => {
  test("success → clean exit 0 (→ review / Needs you)", () => {
    expect(run([{ type: "result", subtype: "success", is_error: false }]).emitted).toEqual([
      { actor: "system", type: "run.exiting", payload: { code: 0 } },
    ]);
  });

  test("error_max_turns → halt exit 10 (→ Stuck, redispatchable)", () => {
    expect(run([{ type: "result", subtype: "error_max_turns", is_error: true }]).emitted).toEqual([
      { actor: "system", type: "run.exiting", payload: { code: 10, reason: "max_turns" } },
    ]);
  });

  test("other is_error → crash-contain exit 1 with the subtype reason", () => {
    expect(
      run([{ type: "result", subtype: "error_during_execution", is_error: true }]).emitted,
    ).toEqual([
      {
        actor: "system",
        type: "run.exiting",
        payload: { code: 1, reason: "error_during_execution" },
      },
    ]);
  });
});

describe("claude-stream — rate limit + unknown events", () => {
  test("rate_limit_event (allowed) is dropped", () => {
    expect(
      run([{ type: "rate_limit_event", rate_limit_info: { status: "allowed" } }]).emitted,
    ).toEqual([]);
  });

  test("an unknown future event type is dropped, not a crash", () => {
    expect(run([{ type: "some_new_kind_2027" }]).emitted).toEqual([]);
  });
});

describe("claude-stream — full run projection (captured real shape)", () => {
  test("a complete dispatch projects the ordered maestro event stream", () => {
    // The real sequence: init → (thinking) → (text) → result. Mirrors the captured fixture where
    // Claude replied 'maestro wiring works' with no tool call (a clean, one-turn completion).
    const fixture: ClaudeStreamEvent[] = [
      { type: "system", subtype: "hook_started" },
      { type: "system", subtype: "init", model: "claude-haiku-4-5-20251001" },
      { type: "system", subtype: "thinking_tokens" },
      {
        type: "assistant",
        message: { content: [{ type: "thinking", thinking: "reply with three words" }] },
      },
      { type: "assistant", message: { content: [{ type: "text", text: "maestro wiring works" }] } },
      { type: "rate_limit_event", rate_limit_info: { status: "allowed" } },
      { type: "result", subtype: "success", is_error: false, result: "maestro wiring works" },
    ];
    const { emitted } = run(fixture);
    expect(emitted).toEqual([
      {
        actor: "system",
        type: "run.started",
        payload: { model: "claude-haiku-4-5-20251001" },
      },
      { actor: "agent", type: "agent.said", payload: { text: "maestro wiring works" } },
      { actor: "system", type: "run.exiting", payload: { code: 0 } },
    ]);
  });

  test("a multi-tool run projects tool.call/result pairs with incrementing beats", () => {
    const fixture: ClaudeStreamEvent[] = [
      { type: "system", subtype: "init", model: "claude-opus-4-8" },
      {
        type: "assistant",
        message: { content: [{ type: "tool_use", id: "a", name: "Read", input: { path: "x" } }] },
      },
      {
        type: "user",
        message: { content: [{ type: "tool_result", tool_use_id: "a", content: "ok" }] },
      },
      {
        type: "assistant",
        message: { content: [{ type: "tool_use", id: "b", name: "Edit", input: { path: "x" } }] },
      },
      {
        type: "user",
        message: { content: [{ type: "tool_result", tool_use_id: "b", content: "done" }] },
      },
      { type: "assistant", message: { content: [{ type: "text", text: "Finished." }] } },
      { type: "result", subtype: "success", is_error: false },
    ];
    const { emitted, state } = run(fixture);
    const types = emitted.map((e) => e.type);
    expect(types).toEqual([
      "run.started",
      "tool.call",
      "run.beat",
      "tool.result",
      "tool.call",
      "run.beat",
      "tool.result",
      "agent.said",
      "run.exiting",
    ]);
    // Two acting beats, both tool names recovered, final clean exit.
    expect(state.beat).toBe(2);
    expect(emitted.filter((e) => e.type === "run.beat").map((e) => e.payload)).toEqual([
      { iteration: 1 },
      { iteration: 2 },
    ]);
  });
});
