/// <reference types="bun" />
// claude-runner (BRO-1912) — the SUBSCRIPTION provider runner. A sibling to broomva-child on the same
// supervisor spawn seam (HARNESS §1): the supervisor spawns THIS with the run worktree as cwd, and it
// speaks the identical `session.jsonl` stdout protocol + stdin control channel. The difference is where
// the model comes from: broomva-child drives the loop itself and calls the metered proxy; this runner
// hands the whole loop to the `claude` CLI, which runs the agent on the user's Claude SUBSCRIPTION
// (Keychain OAuth — no API key, no proxy, no key confinement problem: the CLI owns its own auth). The
// runner CAPTURES the CLI's `stream-json` output and projects it into maestro events (claude-stream.ts),
// so the supervisor's tee → verifier → gate wraps a subscription run identically to a proxy run.
//
// Isolation (the child is still untrusted-by-worktree): the CLI is spawned with `--setting-sources
// project` (never the operator's user hooks/settings), `--strict-mcp-config` with no config (no operator
// MCP servers), and its writes are confined to the run worktree (cwd). The confinement here is the
// worktree + the CLI's permission mode, not credential-denial — the subscription runner is deliberately
// allowed to spend the user's subscription (that is the point).
//
// Scope (slice 1): a one-shot agentic dispatch — the CLI receives the mission, works autonomously to a
// terminal `result`, and exits. `stop` kills it (park blocked); `ping` answers `pong`. Mid-run chat
// injection (stream-json input) is a follow-up; a `chat` control line is acknowledged, not dropped
// silently.

import type { WorkContract } from "@maestro/protocol";
import { readContractSnapshot } from "../harness/contract-snapshot";
import { createNdjsonSplitter } from "../harness/ndjson";
import type { ChildEmittedEvent } from "../harness/runner";
import { parseChildArgv } from "../harness/spawn-contract";
import {
  type ClaudeStreamEvent,
  newClaudeTranslatorState,
  translateClaudeEvent,
} from "./claude-stream";

const runDir = process.env.BROOMVA_RUN_DIR ?? "";

/** The provider CLI binary (resolved on PATH by default; overridable so a bundled/pinned binary can be
 *  pointed at without a PATH change, mirroring Houston's bundled-codex rationale). */
const CLAUDE_BIN = process.env.MAESTRO_CLAUDE_BIN ?? "claude";

/** The role-pinned model. Defaults to the agent pin (models.ts DEFAULT_MODEL_PINS.agent); overridable per
 *  run so a dogfood can pin a cheaper model without touching config. */
const CLAUDE_MODEL = process.env.MAESTRO_CLAUDE_MODEL ?? "claude-opus-4-8";

/** Permission mode for the headless CLI. In `-p` (non-interactive) mode a permission prompt cannot be
 *  answered, so an autonomous agent needs a non-prompting mode. The worktree is the sandbox (the box IS
 *  the sandbox), so `bypassPermissions` is the default; overridable to `acceptEdits` for a tighter run. */
const CLAUDE_PERMISSION = process.env.MAESTRO_CLAUDE_PERMISSION ?? "bypassPermissions";

/** The maestro loop contract, appended to the CLI's own system prompt. Terse — the CLI already knows how
 *  to use tools; this only sets the unattended-autonomy contract. */
const SYSTEM_APPEND =
  "You are a Maestro agent running UNATTENDED in an isolated git worktree. Complete the assigned work " +
  "with your tools, verifying as you go. Do not ask for confirmation. When the work is genuinely " +
  "complete, reply with a short summary and stop; do not stop early.";

/** Emit one maestro NDJSON event to stdout, flushed (the BRO-1767 lesson: a child killed before an OS
 *  flush loses un-flushed lines). */
async function emit(ev: ChildEmittedEvent): Promise<void> {
  await Bun.write(Bun.stdout, `${JSON.stringify(ev)}\n`);
}

function msg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

async function readContract(): Promise<WorkContract | null> {
  if (runDir === "") return null;
  try {
    return (await readContractSnapshot(runDir)).node;
  } catch {
    return null;
  }
}

/** The mission prompt handed to the CLI. Points it at the work definition in the worktree (the brief
 *  lives in `_work.md`, not the frozen contract frontmatter) and states the done-condition when present. */
function buildPrompt(contract: WorkContract | null, session: string): string {
  const kind = contract?.kind ? `${contract.kind} ` : "";
  const done = contract?.done ? ` Done when: ${JSON.stringify(contract.done)}.` : "";
  return (
    `You are working in an isolated git worktree that contains a Maestro ${kind}work item (session ${session}). ` +
    "Find and read the work definition (a `_work.md` file in this directory tree), then complete the work " +
    `it describes.${done} Make real changes with your tools, verify them, and when the work is genuinely ` +
    "complete, reply with a short summary of what you did and stop."
  );
}

async function main(): Promise<void> {
  // Emit run.started FIRST — a guaranteed receipt even if the CLI never launches (spawn ENOENT, auth
  // failure), so the supervisor never sees a receiptless crash. The translator is pre-seeded started:true
  // below so the CLI's own `system/init` does not emit a second run.started.
  await emit({
    actor: "system",
    type: "run.started",
    payload: { provider: "claude", model: CLAUDE_MODEL },
  });

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

  const cwd = process.cwd(); // the run worktree (supervisor sets it) — the CLI writes only here
  const contract = await readContract();
  const prompt = buildPrompt(contract, session);

  const args = [
    "-p",
    prompt,
    "--output-format",
    "stream-json",
    "--verbose", // required with --print + --output-format=stream-json for the full event stream
    "--model",
    CLAUDE_MODEL,
    "--permission-mode",
    CLAUDE_PERMISSION,
    "--setting-sources",
    "project", // NEVER the operator's user hooks/settings — the worktree has none, so this is clean
    "--strict-mcp-config", // no --mcp-config given → zero MCP servers (no operator MCP contamination)
    "--append-system-prompt",
    SYSTEM_APPEND,
  ];

  let proc: Bun.Subprocess<"ignore", "pipe", "pipe">;
  try {
    proc = Bun.spawn([CLAUDE_BIN, ...args], {
      cwd,
      // The runner's own env is the supervisor's allowlisted child env (HOME/PATH for Keychain auth + the
      // CLI binary); pass it through. No API key, no proxy vars — the CLI self-authenticates.
      env: process.env,
      stdin: "ignore", // one-shot: the mission is the positional prompt; no stdin turns to the CLI (yet)
      stdout: "pipe",
      stderr: "pipe",
    });
  } catch (err) {
    await emit({
      actor: "system",
      type: "run.exiting",
      payload: { code: 1, reason: `claude spawn failed: ${msg(err)}` },
    });
    process.exit(1);
  }

  // Kill the CLI if the runner is asked to terminate (supervisor SIGTERM / our own exit paths) so a live
  // model call can't outlive its supervisor.
  let killed = false;
  const killCli = () => {
    if (!killed) {
      killed = true;
      try {
        proc.kill();
      } catch {
        /* already gone */
      }
    }
  };
  process.on("SIGTERM", () => {
    killCli();
    process.exit(143);
  });

  // Track the terminal run.exiting (the CLI's `result` maps to one). If the CLI dies WITHOUT a result
  // (crash, auth failure, kill), the process-exit path below synthesizes one from the exit code + a
  // stderr tail, so every run still lands a receipt. `maestroExit` is the code the RUNNER process exits
  // with — the supervisor reads the child's exit code (0 review / 10 park-blocked / 1 crash-contain), so
  // it must be the maestro code, NOT the CLI's own exit code (the CLI exits 0 even on a max_turns halt).
  let exited = false;
  let maestroExit: number | null = null;
  const emitExit = async (code: number, reason?: string): Promise<void> => {
    if (exited) return;
    exited = true;
    maestroExit = code;
    await emit({
      actor: "system",
      type: "run.exiting",
      payload: reason ? { code, reason } : { code },
    });
  };

  // Pre-seed the translator so the CLI's own system/init does not re-emit run.started (already emitted).
  const state = newClaudeTranslatorState();
  state.started = true;

  const handleLine = async (line: string): Promise<void> => {
    const trimmed = line.trim();
    if (trimmed === "") return;
    let parsed: ClaudeStreamEvent;
    try {
      parsed = JSON.parse(trimmed) as ClaudeStreamEvent;
    } catch {
      return; // a non-JSON line (should not happen with stream-json) is skipped, never fatal
    }
    for (const ev of translateClaudeEvent(parsed, state)) {
      if (ev.type === "run.exiting") {
        // The CLI declared its own terminal receipt — adopt its maestro code for the process exit.
        exited = true;
        maestroExit = (ev.payload as { code: number }).code;
      }
      await emit(ev);
    }
  };

  // ── stdin control (maestro side), concurrent with the stdout pump ──────────────────────────────────
  // ping → pong (raw line, matching broomva-child); stop → kill the CLI + park blocked; chat → ack (the
  // stream-json input path is a follow-up — acknowledged, never silently dropped).
  const readControl = async (): Promise<void> => {
    const split = createNdjsonSplitter();
    const dec = new TextDecoder();
    try {
      for await (const chunk of Bun.stdin.stream()) {
        for (const line of split.push(dec.decode(chunk, { stream: true }))) {
          let ctl: { type?: string };
          try {
            ctl = JSON.parse(line) as { type?: string };
          } catch {
            continue;
          }
          if (ctl.type === "ping") {
            await Bun.write(Bun.stdout, '{"type":"pong"}\n');
          } else if (ctl.type === "stop") {
            killCli();
            await emitExit(10, "user_stop");
            process.exit(10);
          }
          // chat: acknowledged; mid-run injection needs the CLI in stream-json input mode (follow-up).
        }
      }
    } catch {
      /* stdin closed / unreadable — control simply ends; the run continues to its terminal receipt */
    }
  };
  void readControl();

  // Capture a bounded stderr tail for the crash reason (auth failure, bad flag) — the diagnostic the
  // supervisor's blocked receipt needs.
  let stderrTail = "";
  const pumpStderr = async (): Promise<void> => {
    const dec = new TextDecoder();
    try {
      for await (const chunk of proc.stderr) {
        stderrTail = (stderrTail + dec.decode(chunk, { stream: true })).slice(-1000);
      }
    } catch {
      /* ignore */
    }
  };
  void pumpStderr();

  // ── stdout pump — the CLI's stream-json → maestro events ────────────────────────────────────────────
  const split = createNdjsonSplitter();
  const dec = new TextDecoder();
  try {
    for await (const chunk of proc.stdout) {
      for (const line of split.push(dec.decode(chunk, { stream: true }))) await handleLine(line);
    }
    const rest = split.flush();
    if (rest) await handleLine(rest);
  } catch (err) {
    await emitExit(1, `stream read failed: ${msg(err)}`);
    killCli();
    process.exit(1);
  }

  const code = await proc.exited;
  // The CLI emitted its own terminal `result` (mapped to run.exiting) in the happy path. If not (killed,
  // crashed before result), synthesize one from the exit code + stderr tail so the run always has a receipt.
  if (!exited) {
    if (code === 0) {
      await emitExit(0);
    } else {
      const tail = stderrTail.trim();
      await emitExit(
        1,
        tail !== "" ? `claude exited ${code}: ${tail.slice(-300)}` : `claude exited ${code}`,
      );
    }
  }
  // Exit with the maestro code (review 0 / park-blocked 10 / crash 1) so the supervisor classifies the run
  // correctly — never the CLI's own exit code (0 even on a halt).
  process.exit(maestroExit ?? 1);
}

if (import.meta.main) await main();
