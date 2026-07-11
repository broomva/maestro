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
//
// Resource bounds (both fed by ARBITRARY model output, so both are load-bearing, not theoretical):
//   • per-tool TIMEOUT — a `sleep infinity` / stdin-reading command is SIGKILLed at `shellTimeoutMs`
//     so it can't block the beat forever;
//   • bounded READS — shell stdout/stderr and file reads stop at MAX_OUTPUT *bytes* (streamed, never
//     buffered whole), so a `yes` / gigabyte file can't OOM the child before the clip.

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

/** Options for a single tool execution — the beat loop uses defaults; tests pin the timeout. */
export interface ToolOpts {
  /** SIGKILL a `shell` command that outruns this budget (default `DEFAULT_SHELL_TIMEOUT_MS`). */
  shellTimeoutMs?: number;
}

/** The tools this slice ships — the minimal agentic set. Broader / MCP tools land later. */
export const TOOL_NAMES = ["shell", "read", "edit"] as const;

/** Cap tool output fed back to the model — a runaway `cat` must not blow the context window. Exported so
 *  tests assert the boundary against the real constant, not a copy. */
export const MAX_OUTPUT = 16_000;

/** Default per-`shell` wall-clock budget. A hanging command is SIGKILLed here so the beat is never
 *  blocked longer than this by any single tool call. */
export const DEFAULT_SHELL_TIMEOUT_MS = 120_000;

/** Truncate to `cap` UTF-16 units + a marker, WITHOUT splitting a surrogate pair at the boundary (a lone
 *  high surrogate would decode to U+FFFD in the model's view). */
function truncate(s: string, cap: number): string {
  if (s.length <= cap) return s;
  let end = cap;
  const last = s.charCodeAt(end - 1);
  if (last >= 0xd800 && last <= 0xdbff) end -= 1; // don't cut a high surrogate from its low half
  return `${s.slice(0, end)}\n…(truncated)`;
}

function msg(err: unknown): string {
  return String((err as Error)?.message ?? err);
}

/** Read a byte stream up to `cap` bytes then STOP, cancelling the source. Bounds MEMORY: a command that
 *  spews gigabytes (or never ends, e.g. `yes`) is read only up to the cap — never buffered whole — and
 *  the cancel unblocks a runaway producer via EPIPE. Returns the decoded text + whether more remained. */
async function readCapped(
  stream: ReadableStream<Uint8Array>,
  cap: number,
): Promise<{ text: string; clipped: boolean }> {
  const reader = stream.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  let clipped = false;
  try {
    let r = await reader.read();
    while (!r.done) {
      if (r.value) {
        chunks.push(r.value);
        total += r.value.byteLength;
      }
      if (total >= cap) {
        clipped = true;
        break;
      }
      r = await reader.read();
    }
  } finally {
    await reader.cancel().catch(() => {}); // stop the producer; a closed pipe is not a crash here
  }
  return { text: new TextDecoder().decode(Buffer.concat(chunks)), clipped };
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
  opts: ToolOpts = {},
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
      const timeoutMs = opts.shellTimeoutMs ?? DEFAULT_SHELL_TIMEOUT_MS;
      const proc = Bun.spawn(["sh", "-c", command], {
        cwd,
        stdin: "ignore",
        stdout: "pipe",
        stderr: "pipe",
      });
      let timedOut = false;
      const timer = setTimeout(() => {
        timedOut = true;
        proc.kill(9); // SIGKILL — closing its pipes also unblocks the readers below
      }, timeoutMs);
      let out: { text: string; clipped: boolean };
      let err: { text: string; clipped: boolean };
      let code: number;
      try {
        // Bounded, streamed reads: memory can't run away before proc.exited resolves.
        [out, err] = await Promise.all([
          readCapped(proc.stdout as ReadableStream<Uint8Array>, MAX_OUTPUT),
          readCapped(proc.stderr as ReadableStream<Uint8Array>, MAX_OUTPUT),
        ]);
        code = await proc.exited;
      } finally {
        clearTimeout(timer);
      }
      const body = truncate(`${out.text}${err.text}`.trim(), MAX_OUTPUT);
      if (timedOut) {
        return {
          ok: false,
          summary: `shell \`${command.slice(0, 60)}\` → timed out (${timeoutMs}ms)`,
          content: `${body}\n(timed out after ${timeoutMs}ms)`.trim(),
        };
      }
      const clipped = out.clipped || err.clipped;
      return {
        ok: code === 0,
        summary: `shell \`${command.slice(0, 60)}\` → exit ${code}${clipped ? " (clipped)" : ""}`,
        content: body || `(no output; exit ${code})`,
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
      const file = Bun.file(path);
      // Bounded read: pull at most MAX_OUTPUT bytes so a huge file can't OOM the child (a missing file
      // still throws below → the catch returns ok:false). The byte slice can split a multibyte char at
      // the boundary, which only affects the last glyph of an already-truncated view.
      const text = await file.slice(0, MAX_OUTPUT).text();
      const clipped = file.size > MAX_OUTPUT;
      return {
        ok: true,
        summary: `read ${String(input.path)}${clipped ? " (clipped)" : ""}`,
        content: clipped ? `${text}\n…(truncated)` : text,
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
      // Bad input is ok:false like every other tool — NEVER coerce a non-string to "", which would
      // silently truncate the target file to empty while reporting success (a data-loss bug).
      if (typeof input.content !== "string") {
        return {
          ok: false,
          summary: `edit: refused ${String(input.path)} (content not a string)`,
          content: "error: edit requires a string `content`",
        };
      }
      const content = input.content;
      await Bun.write(path, content);
      const bytes = Buffer.byteLength(content, "utf8"); // true UTF-8 bytes on disk, not UTF-16 units
      return {
        ok: true,
        summary: `edit ${String(input.path)} (${bytes}b)`,
        content: `wrote ${bytes} bytes to ${String(input.path)}`,
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
