# Maestro autonomous loop — broomva-child slice 2b-i (F3 beat loop) DONE; NEXT = slice 2b-ii (retire the fixture)

**TL;DR.** The F3 beat loop is merged (BRO-1855 slice 2b-i, PR #50, `aba7015`) after **six P20 rounds**
that caught three genuine correctness majors (truncation-as-completion, commit-as-no-progress,
tool-path-unreachable-in-production). **BRO-1855 stays In Progress** — its done.check ("the 4 guardrail
scenarios run through the REAL child, no fixture") is NOT yet met. **NEXT = slice 2b-ii: retire
`loop-child.ts`** — re-point `loops.test.ts` onto the real child (fresh_context respawn-RESUME + kill) +
wire `BROOMVA_CONTEXT_CEILING` through the supervisor allowlist + delete the fixture → completes 1855.

## State of the world (P15)

- **maestro repo** `apps/maestro/` — `main` @ **`aba7015`**, tree clean, branch deleted.
- **Linear** `6c738b50`: P0+P1 done; P2 = runtime chain (11) + 1814 + M3 (1825) + routing (1824) +
  broomva-child slices 1 (1854) + 2a (1856) + **2b-i (1855, In Progress)**. NEXT: **BRO-1855 slice 2b-ii**.
  Then slice 3 → P2-exit 1827 → M4 1826. Follow-up BRO-1860 (child loop hardening, Low — 6 P20 nits).
- CI-gated automerge unchanged (4 required jobs; CodeRabbit advisory; enforce_admins:false; green-only).

## What slice 2b-i shipped (build 2b-ii on this)

`apps/runtime/src/child/broomva-child.ts` — the real F3 beat loop:
- Per beat: `callProxy(messages)` (sends `{max_tokens:8192, system: SYSTEM_PROMPT, tools: TOOL_SCHEMAS,
  messages}`) → `parseToolUses` + `textOf` → execute each tool via `executeTool(name,input,cwd)` in the
  run worktree → tee `tool.call`/`tool.result` → append a `tool_result` user turn → next beat.
- A **no-tool** turn exits 0 (review) ONLY when `stop_reason` is end_turn/stop_sequence/absent; a
  `max_tokens` truncation **nudges and continues** (bounded by no_progress). Every exit emits a
  `run.exiting` receipt (bad-argv/error/402/non-2xx/completion/halt/restart/checkpoint-fail/hard-cap).
- **Beat effect** = `beatSignal(cwd, base)` — a content-sensitive `git diff --cached <base>` hash (base =
  `git rev-parse HEAD` at loop start, so COMMITS count as progress; delta-vs-previous so a no-op reads
  empty; returns `{sig, stat, measured}` — a git-failed beat is no-change and does NOT poison the baseline).
- **Stop engine** (`evaluateBeat`, the 1795 module): halt → `beatExitEvents` + exit 10 (blocked); restart
  (context ceiling from `resolveCeiling()` = `BROOMVA_CONTEXT_CEILING` env or `DEFAULT_CONTEXT_CEILING_TOKENS`)
  → `prepareRestart` checkpoint (guarded) + exit 10 fresh_context; else continue.
- **Mock fidelity (BRO-1856/1806, now tightened):** `createMockModel` DEGRADES a scripted tool_use body to
  text when the request advertises no `tools` — so a child that drops the schema turns the tool tests red.

## Slice 2b-ii build plan (completes BRO-1855)

Retire `apps/runtime/src/proxy/loop-child.ts` by driving its four guardrail scenarios through the REAL
`broomva-child`, in `apps/runtime/src/proxy/loops.test.ts` (or move them next to the child):

1. **budget / no_progress** — already provable through the real child today (see `broomva-child.test.ts`:
   the 402-halt and the diff/error no_progress tests). Port/confirm them in the loops suite.
2. **fresh_context RESPAWN-RESUME** (the new work): the respawned child (attempt ≥2) must READ `progress.md`
   (via `readProgress(runDir)`) and RESUME — skip done work, finish, exit 0. Slice 2b-i writes the
   checkpoint (`prepareRestart`) but does NOT read it on respawn; add the resume branch at the top of the
   loop (if a checkpoint exists, seed `messages`/state from it, like `loop-child.ts` does). Then the
   scenario = ceiling hit → checkpoint → supervisor respawns → attempt 2 resumes → completes (review).
   Wire `BROOMVA_CONTEXT_CEILING` through the supervisor's spawn-contract allowlist (spawn-contract.ts +
   supervisor.ts, resolved from the runtime config `contextCeilingTokens`) so it's not test-only.
3. **kill mid-tool** — the model emits a tool_use for a slow command; the supervisor SIGKILLs the live
   child mid-`executeTool` → `canceled` + `run.killed` (BRO-1801). `executeTool`'s own 120s timeout means
   the child is killable within it.
4. **Delete `loop-child.ts`** once the four scenarios are green through the real child; drop the
   `scenarioSpawn` fixture. Keep ZERO tokens (the mock scripts tool_use + text).

**If 2b-ii is too large, split it** (respawn-resume first, then kill + fixture-delete) — the same discipline.

## The loop

Driver `~/broomva/docs/maestro-orchestration-loop.md`. One ticket/iteration: snapshot → dep-chain →
**branch FIRST + verify `git branch --show-current`** → build (reuse the shipped seams) → bun test/tsc/biome
green → dogfood (the loops ARE the P11 dogfood: real processes/HTTP/worktrees, zero tokens) → **P20 Workflow
≥7 AND 0 surviving blocker/major, fix every finding mutation-proven, re-verify on the pushed code** → PR via
REST → resolve CodeRabbit threads → merge green via REST → bookkeep → next handoff → `ScheduleWakeup`.

**P20 is LOAD-BEARING (rule-of-many).** Slice 2b-i is the proof: six rounds, three real correctness majors
each green-CI-and-tests-certified-done between rounds. Treat the gate as an unbounded re-verify loop; never
merge below the bar on "I fixed it"; mutation-prove each fix in BOTH directions. `[[fix-in-edge-dense-file-
regresses]]` + `[[mock-fidelity-gap-false-green]]`. Stop = `ScheduleWakeup({stop:true})` when all P0–P5
non-OUTLOOK tickets are Done + the P2 exit (1827) passes.
