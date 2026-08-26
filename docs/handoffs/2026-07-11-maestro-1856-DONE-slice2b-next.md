# Maestro autonomous loop — broomva-child slice 2a DONE; NEXT = slice 2b (the F3 beat loop)

**TL;DR.** The child's tool-execution LAYER is merged & DONE (BRO-1856, PR #49, `e3d3764`)
after **eight P20 rounds** (the gate caught a distinct real defect every round; four were
regressions in my own prior-round fix — see `[[fix-in-edge-dense-file-regresses]]`). **NEXT =
slice 2b (BRO-1855)** — the F3 beat loop that wires this tool layer + the 1795 stop-engine into
the SHIPPED child and retires the `loop-child.ts` fixture. Slice 3 (stdin + the 1827 E2E) after.

## State of the world (P15)

- **maestro repo** `apps/maestro/` — `main` @ **`e3d3764`**, tree clean, branch deleted.
- **Linear** `6c738b50`: P0+P1 done; P2 = runtime chain (11) + M3 board (1825) + routing (1824)
  + **child slice 1 (1854) + slice 2a tool-layer (1856)**. **NEXT: BRO-1855** (slice 2b). Then
  slice 3 → **P2-exit 1827**. 1822 chat (rerouted → Backlog) lands with slice 3's stdin contract.
- CI-gated automerge unchanged (4 required jobs: quality/secrets/build/governance +
  conversation-resolution; CodeRabbit advisory, not required; enforce_admins:false; green-only).

## What slice 2a shipped — the API slice 2b consumes

`apps/runtime/src/child/tools.ts` (32 tests in `tools.test.ts`). A **pure, cwd-injected** layer —
no child/proxy needed to unit-test it. The beat loop imports these:

- `parseToolUses(body): ToolUse[]` — extract Anthropic `tool_use` blocks (`{id,name,input}`) from a
  Messages response body. Defensive: null/non-object body → `[]`; a block missing id/name skipped;
  non-object (incl. **array**) `input` normalized to `{}`.
- `executeTool(name, input, cwd, opts?): Promise<ToolResult>` — run ONE tool. **Never throws** —
  a failure is `{ok:false, summary, content}` data the model can recover from. `opts.shellTimeoutMs`
  defaults to `DEFAULT_SHELL_TIMEOUT_MS` (120s); the beat loop uses the default, tests pin it small.
  - `shell` — `sh -c` in cwd; stdout+stderr newline-joined, output bytes preserved verbatim (NOT
    trimmed); `ok = exit===0`; a hang / detached-grandchild / infinite output is SIGKILLed at the
    deadline (→ `ok:false`, summary "timed out"); output over `MAX_OUTPUT` (16k) is drained-and-
    discarded past the cap (memory bounded, real exit code preserved) with a `…(truncated)` marker
    IN the content + `(clipped)` in the summary.
  - `read` / `edit` — path-JAILED to cwd (lexical `..`/absolute refuse + symlink-aware
    `realInsideJail`: a symlink/symlinked-parent/dangling-symlink that resolves outside cwd is
    refused). read/edit refuse a non-regular target (FIFO/device/dir). edit refuses non-string
    content. Reports true UTF-8 byte count.
- `toolResultBlock(id, result): {type:"tool_result", tool_use_id, content, is_error}` — the block
  the NEXT turn appends to `messages`. `is_error = !ok`.
- Exported: `MAX_OUTPUT`, `DEFAULT_SHELL_TIMEOUT_MS`, `TOOL_NAMES = ["shell","read","edit"]`,
  `capText` (surrogate-safe cap).

**Contract the beat loop must honor:** the model reads `content` (never the summary), so the clip
signal lives in the content. `executeTool` is the child's HANDS; the beat loop is the ARM (the
turn/tool-result protocol, event emission, stop conditions).

## Slice 2b build plan (BRO-1855)

Turn slice-1's single turn (`broomva-child.ts`, `agent.said` → `run.exiting{0}`) into the F3 beat
loop (AUTONOMY §1 five beats: trigger → find work → act → verify → log):

1. **Beat loop**: `for (beat…)` — POST the proxy `/v1/messages` with the running `messages` array →
   `parseToolUses(body)`. If there are tool_use blocks: `executeTool` each in the run worktree cwd,
   append `toolResultBlock` to `messages`, and go again. Coalesce assistant text into `agent.said`
   per completed turn. Emit `tool.call {tool, input}` then `tool.result {tool, ok, summary}` (HARNESS
   §6 — summaries, never full payloads), and `run.beat {iteration, diffstat}`.
2. **Stop conditions**: feed the beat effect (diffstat / progress / context) into `evaluateBeat`
   (the 1795 engine → `continue | halt | restart`). Halt → `beatExitEvents` + exit 10. Restart →
   `prepareRestart` checkpoint + exit 10 `fresh_context` (the supervisor respawns; the checkpoint
   makes it lossless — `loop-child.ts` already demonstrates the pattern). Clean completion → exit 0
   (review). **Preserve slice-1's invariant: EVERY exit path emits a `run.exiting` receipt.**
3. **Retire the fixture**: rewrite `loops.test.ts` so the four guardrail scenarios (budget /
   no-progress / fresh-context / kill) run through the REAL `broomva-child` instead of
   `loop-child.ts`; delete the fixture once green. Keep ZERO tokens (the mock scripts tool_use +
   text; the child parses tool_use via `parseToolUses` and executes via `executeTool`).

**cwd for tools** = the run worktree (`BROOMVA_RUN_DIR` or the supervisor-provided worktree). The
tool layer is already cwd-injected — pass the run's worktree so tools operate inside `run/<id>`.

**If slice 2b is still too large for one clean iteration, split it** (loop+text-only-with-tools
first, then stop-engine + fixture retirement) — the same discipline that kept 2a converging.

## The loop

Driver `~/broomva/docs/maestro-orchestration-loop.md`. One ticket/iteration, full cycle: snapshot →
dep-chain → **branch FIRST + verify `git branch --show-current`** (before any edit — the rule-of-two
branch-to-main slip) → build (reuse the shipped seams) → bun test/tsc/biome green → dogfood (the
loops ARE the P11 dogfood: real processes/HTTP/worktrees, zero tokens) → **P20 Workflow ≥7 AND 0
surviving blocker/major, fix every confirmed finding mutation-proven, re-verify on the pushed code**
→ PR via REST → resolve every CodeRabbit thread → merge green via REST → bookkeep → next handoff →
`ScheduleWakeup`.

**P20 is LOAD-BEARING (rule-of-18).** Slice 2a is the proof: eight rounds, a distinct real defect
each (four regressions in my own fixes), all green-CI-and-tests-certified-done between rounds. The
gate is an *unbounded* re-verify loop — never merge below the bar on "I fixed it." Mutation-prove
each fix in BOTH directions. Stop = `ScheduleWakeup({stop:true})` when all P0–P5 non-OUTLOOK tickets
are Done + the P2 exit (1827) passes.
