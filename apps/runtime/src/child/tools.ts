/// <reference types="bun" />
// The child's tool-execution layer (BRO-1856, slice 2a of the F3 loop). The child's HANDS: parse the
// model's Anthropic `tool_use` blocks, execute each in the run worktree, and build the `tool_result`
// blocks the next turn sends back. Pure + cwd-injected so it unit-tests without a child/proxy. The beat
// loop (slice 2b, BRO-1855) wires this into the model turn + emits tool.call/tool.result (HARNESS §6).
//
// Phase-1 sandbox boundary: read/edit are path-JAILED to cwd (a `..`/absolute escape is refused, not
// executed). shell runs in cwd — a hard container jail is phase-2 (the sandbox's concern), not this
// layer; the worktree cwd + the allowlisted child env are the phase-1 containment. Nothing here throws:
// a tool failure is DATA (ok:false + an error `content` the model can recover from), never a child crash.

import { resolve } from "node:path";

/** One tool request the model made — an Anthropic `tool_use` content block. */
export interface ToolUse {
  id: string;
  name: string;
  input: Record<string, unknown>;
}

/** The outcome of executing a tool. `ok` drives the HARNESS §6 `tool.result {ok}`; `content` is the text
 *  fed back to the model as the tool_result; `summary` is a short audit line (a decision, never the full
 *  payload — HARNESS §6 stores summaries). */
export interface ToolResult {
  ok: boolean;
  summary: string;
  content: string;
}

/** The tools this slice ships — the minimal agentic set. Broader / MCP tools land later. */
export const TOOL_NAMES = ["shell", "read", "edit"] as const;

/** Cap tool output fed back to the model — a runaway `cat` must not blow the context window. */
const MAX_OUTPUT = 16_000;

function clip(s: string): string {
  return s.length > MAX_OUTPUT ? `${s.slice(0, MAX_OUTPUT)}\n…(truncated)` : s;
}

function msg(err: unknown): string {
  return String((err as Error)?.message ?? err);
}

/** Extract the `tool_use` blocks from an Anthropic Messages response body (defensive: a null / non-object
 *  / non-array-content body yields []; a block missing id/name is skipped). */
export function parseToolUses(body: unknown): ToolUse[] {
  if (body === null || typeof body !== "object") return [];
  const content = (body as { content?: unknown }).content;
  if (!Array.isArray(content)) return [];
  const uses: ToolUse[] = [];
  for (const block of content) {
    if (block && typeof block === "object" && (block as { type?: unknown }).type === "tool_use") {
      const b = block as { id?: unknown; name?: unknown; input?: unknown };
      if (typeof b.id === "string" && typeof b.name === "string") {
        const input =
          b.input && typeof b.input === "object" ? (b.input as Record<string, unknown>) : {};
        uses.push({ id: b.id, name: b.name, input });
      }
    }
  }
  return uses;
}

/** Build the Anthropic `tool_result` content block the next turn appends to `messages`. `is_error` flips
 *  the model into failure handling; `content` is the tool's output text. */
export function toolResultBlock(id: string, result: ToolResult): Record<string, unknown> {
  return { type: "tool_result", tool_use_id: id, content: result.content, is_error: !result.ok };
}

/** Resolve a tool path WITHIN cwd, or null if it escapes (absolute outside cwd, or `..` above it). The
 *  jail is a resolved-prefix check: `resolve(cwd, p)` must equal `resolve(cwd)` or sit under `${cwd}/`. */
function jailedPath(cwd: string, p: unknown): string | null {
  if (typeof p !== "string" || p === "") return null;
  const base = resolve(cwd);
  const abs = resolve(base, p);
  if (abs !== base && !abs.startsWith(`${base}/`)) return null;
  return abs;
}

/** Execute one tool in the run worktree cwd → a ToolResult (never throws). An unknown tool or bad input
 *  is `ok:false` with an error `content` the model can see + recover from. */
export async function executeTool(
  name: string,
  input: Record<string, unknown>,
  cwd: string,
): Promise<ToolResult> {
  try {
    if (name === "shell") {
      const command = input.command;
      if (typeof command !== "string" || command === "") {
        return {
          ok: false,
          summary: "shell: missing command",
          content: "error: shell requires a non-empty `command` string",
        };
      }
      const proc = Bun.spawn(["sh", "-c", command], {
        cwd,
        stdin: "ignore",
        stdout: "pipe",
        stderr: "pipe",
      });
      const [out, err, code] = await Promise.all([
        new Response(proc.stdout).text(),
        new Response(proc.stderr).text(),
        proc.exited,
      ]);
      const combined = clip(`${out}${err}`.trim());
      return {
        ok: code === 0,
        summary: `shell \`${command.slice(0, 60)}\` → exit ${code}`,
        content: combined || `(no output; exit ${code})`,
      };
    }
    if (name === "read") {
      const path = jailedPath(cwd, input.path);
      if (path === null) {
        return {
          ok: false,
          summary: `read: refused ${String(input.path)}`,
          content: "error: path is missing or escapes the worktree",
        };
      }
      return {
        ok: true,
        summary: `read ${String(input.path)}`,
        content: clip(await Bun.file(path).text()),
      };
    }
    if (name === "edit") {
      const path = jailedPath(cwd, input.path);
      if (path === null) {
        return {
          ok: false,
          summary: `edit: refused ${String(input.path)}`,
          content: "error: path is missing or escapes the worktree",
        };
      }
      const content = typeof input.content === "string" ? input.content : "";
      await Bun.write(path, content);
      return {
        ok: true,
        summary: `edit ${String(input.path)} (${content.length}b)`,
        content: `wrote ${content.length} bytes to ${String(input.path)}`,
      };
    }
    return {
      ok: false,
      summary: `unknown tool ${name}`,
      content: `error: unknown tool \`${name}\``,
    };
  } catch (err) {
    return { ok: false, summary: `${name}: ${msg(err)}`, content: `error: ${msg(err)}` };
  }
}
