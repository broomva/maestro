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

import { realpath, stat } from "node:fs/promises";
import { dirname, resolve } from "node:path";

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

/** Cap a string to `cap` UTF-16 units, WITHOUT splitting a surrogate pair at the boundary (a lone high
 *  surrogate would decode to U+FFFD in the model's view). Returns only the capped text; the caller
 *  appends any truncation marker so the marker tracks the real clipped decision, not just `s.length`. */
function capText(s: string, cap: number): string {
  if (s.length <= cap) return s;
  let end = cap;
  const last = s.charCodeAt(end - 1);
  if (last >= 0xd800 && last <= 0xdbff) end -= 1; // don't cut a high surrogate from its low half
  return s.slice(0, end);
}

function msg(err: unknown): string {
  return String((err as Error)?.message ?? err);
}

/** Read a byte stream, KEEPING at most `cap` bytes but DRAINING (discarding) the rest so the producer
 *  runs to its real exit code. Bounds MEMORY (never buffers the whole stream — the tail is discarded, not
 *  kept) without corrupting the exit code: an early `reader.cancel()` would SIGPIPE/EPIPE a still-writing
 *  producer, turning a SUCCESSFUL large-output command (`grep -r`, `cat big.log`) into a spurious non-zero
 *  exit. Draining lets `proc.exited` reflect the command's OWN status. `signal` (the caller's timeout)
 *  cancels a read that would otherwise block forever — a hung command, or a pipe held by a DETACHED
 *  grandchild (`sleep 300 &`) after the direct child exits, or an infinite producer (`yes`) whose drain
 *  never ends. Returns the kept text + whether anything was dropped. */
async function readCapped(
  stream: ReadableStream<Uint8Array>,
  cap: number,
  signal?: AbortSignal,
): Promise<{ text: string; clipped: boolean }> {
  const reader = stream.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  let clipped = false;
  const abort = (): void => {
    reader.cancel().catch(() => {});
  };
  if (signal?.aborted) reader.cancel().catch(() => {});
  else signal?.addEventListener("abort", abort, { once: true });
  try {
    let r = await reader.read();
    while (!r.done) {
      const chunk = r.value;
      if (chunk) {
        const room = cap - total;
        if (room <= 0) {
          clipped = true; // past the cap — drain-and-discard so the producer reaches its real exit
        } else if (chunk.byteLength <= room) {
          chunks.push(chunk);
          total += chunk.byteLength;
        } else {
          chunks.push(chunk.subarray(0, room)); // keep exactly up to the cap, discard the overflow
          total = cap;
          clipped = true;
        }
      }
      r = await reader.read();
    }
  } catch {
    // A read cancelled mid-flight (abort or a closed pipe) is not a crash — return what we captured.
  } finally {
    signal?.removeEventListener("abort", abort);
    await reader.cancel().catch(() => {});
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
        // Normalize a non-object (incl. an ARRAY — `typeof [] === "object"`) input to {}.
        const input =
          b.input && typeof b.input === "object" && !Array.isArray(b.input)
            ? (b.input as Record<string, unknown>)
            : {};
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

/** True iff `abs` is the jail root or sits under it. The trailing slash is load-bearing: it stops a
 *  sibling that shares the name prefix (`/w/subX` must NOT count as inside `/w/sub`). */
function insideJail(base: string, abs: string): boolean {
  return abs === base || abs.startsWith(`${base}/`);
}

/** Resolve a tool path WITHIN cwd, or null if it escapes (absolute outside cwd, or `..` above it). This
 *  is a LEXICAL check only — it does not resolve symlinks; callers that touch the filesystem must also
 *  pass the resolved real path through `insideJail` (see `realInsideJail`) so a symlink the agent created
 *  via its free shell can't tunnel read/edit out of the worktree. */
function jailedPath(cwd: string, p: unknown): string | null {
  if (typeof p !== "string" || p === "") return null;
  const base = resolve(cwd);
  const abs = resolve(base, p);
  return insideJail(base, abs) ? abs : null;
}

/** Symlink-aware jail check for read/edit. `resolve()` is lexical, so a symlink INSIDE cwd pointing out
 *  (or a symlinked parent dir) passes `jailedPath` yet resolves to a target outside the worktree. Follow
 *  the links: the real path of the target (or, for a not-yet-created edit target, its nearest existing
 *  ancestor) must still be inside the jail. The base is ALSO realpath'd — cwd itself may sit under a
 *  symlinked prefix (e.g. macOS `/var` → `/private/var`), so comparing a resolved target against a
 *  lexical base would false-refuse every path. Returns true when it is safe to touch `abs`. */
async function realInsideJail(cwd: string, abs: string): Promise<boolean> {
  const base = await realpath(cwd).catch(() => resolve(cwd)); // real cwd; lexical fallback if unresolved
  const real = await realpath(abs).catch(() => null);
  if (real !== null) return insideJail(base, real); // target exists — check where it really points
  // Target does not exist (a fresh edit): walk up to the nearest existing ancestor and check THAT, so a
  // symlinked parent (`d/link/new` with link → /etc) is refused even though `new` isn't there yet.
  const parentReal = await realpath(dirname(abs)).catch(() => null);
  return parentReal === null ? false : insideJail(base, parentReal);
}

/** A promise that settles when `signal` aborts (already-settled if it is). Used to bound `proc.exited`
 *  by the same deadline that fires the SIGKILL, so a command that closes its fds but keeps running can't
 *  slip past a disarmed timer into an unbounded wait. */
function abortSignaled(signal: AbortSignal): Promise<void> {
  if (signal.aborted) return Promise.resolve();
  return new Promise((res) => {
    signal.addEventListener("abort", () => res(), { once: true });
  });
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
      // One deadline governs BOTH the reads and the exit wait, via a single abort. The timer SIGKILLs
      // the child and aborts; `ac.signal.aborted` is then the ONE source of truth for "did we hit the
      // deadline" — read once at the end, so `code` and the timeout decision can never disagree. This
      // covers every runaway shape: a hung command (reads + exit both block), a DETACHED grandchild that
      // holds the pipe after `sh` exits (reads block), and a command that closes its fds but keeps
      // running (reads EOF but exit blocks — the exit wait is bounded by the same abort, never naked).
      const ac = new AbortController();
      const timer = setTimeout(() => {
        proc.kill(9);
        ac.abort();
      }, timeoutMs);
      let out: { text: string; clipped: boolean };
      let err: { text: string; clipped: boolean };
      let code = -1;
      try {
        // Bounded, streamed, cancellable reads: memory can't run away.
        [out, err] = await Promise.all([
          readCapped(proc.stdout as ReadableStream<Uint8Array>, MAX_OUTPUT, ac.signal),
          readCapped(proc.stderr as ReadableStream<Uint8Array>, MAX_OUTPUT, ac.signal),
        ]);
        // Exit wait is bounded by the abort too (never a naked `await proc.exited`).
        code = await Promise.race([proc.exited, abortSignaled(ac.signal).then(() => -1)]);
      } finally {
        clearTimeout(timer);
      }
      const didTimeout = ac.signal.aborted; // the deadline fired at some point → a timeout, whatever code
      if (didTimeout) code = -1;
      // Join the two streams with a newline so an unterminated stdout can't be glued onto stderr; the
      // clipped flag is honest about the COMBINED cut, not just each stream's own cap. The `…(truncated)`
      // marker is appended from `clipped` (not from length) so it also fires at the exact-cap boundary —
      // the model reads `content`, never the summary, so the clip signal must live in the content.
      const raw = [out.text, err.text].filter(Boolean).join("\n").trim();
      const clipped = out.clipped || err.clipped || raw.length > MAX_OUTPUT;
      const body = clipped ? `${capText(raw, MAX_OUTPUT)}\n…(truncated)` : raw;
      if (didTimeout) {
        return {
          ok: false,
          summary: `shell \`${command.slice(0, 60)}\` → timed out (${timeoutMs}ms)`,
          content: `${body}\n(timed out after ${timeoutMs}ms)`.trim(),
        };
      }
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
      // Symlink-aware re-check: the lexical jail can't see through a symlink the agent made via shell.
      if (!(await realInsideJail(cwd, path))) {
        return {
          ok: false,
          summary: `read: refused ${String(input.path)} (symlink escapes the worktree)`,
          content: "error: path resolves through a symlink outside the worktree",
        };
      }
      // stat (never blocks): refuse anything that is not a regular file. Opening a FIFO / device for
      // read would block forever with no writer — the read tool must not be able to hang the beat.
      const st = await stat(path); // ENOENT on a missing file → caught below → ok:false
      if (!st.isFile()) {
        return {
          ok: false,
          summary: `read: refused ${String(input.path)} (not a regular file)`,
          content: "error: read only supports regular files (not a directory, FIFO, or device)",
        };
      }
      // Bounded read: pull at most MAX_OUTPUT bytes so a huge file can't OOM the child. The byte slice
      // can split a multibyte char at the boundary, which only affects the last glyph of a clipped view.
      const text = await Bun.file(path).slice(0, MAX_OUTPUT).text();
      const clipped = st.size > MAX_OUTPUT;
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
      // Symlink-aware re-check (target or, for a new file, its parent dir) so a symlink can't tunnel the
      // write outside the worktree past the lexical jail.
      if (!(await realInsideJail(cwd, path))) {
        return {
          ok: false,
          summary: `edit: refused ${String(input.path)} (symlink escapes the worktree)`,
          content: "error: path resolves through a symlink outside the worktree",
        };
      }
      // Refuse to overwrite a non-regular target (FIFO/device/directory) — a new file (ENOENT) is fine.
      const existing = await stat(path).catch(() => null);
      if (existing && !existing.isFile()) {
        return {
          ok: false,
          summary: `edit: refused ${String(input.path)} (not a regular file)`,
          content: "error: edit only supports regular files (not a directory, FIFO, or device)",
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
