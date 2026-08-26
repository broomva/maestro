# Maestro autonomous loop — slice 2b-ii-A (respawn-RESUME + ceiling) DONE; NEXT = 2b-ii-B (retire the fixture)

**TL;DR.** The respawn-RESUME mechanism + ceiling-allowlist are merged (BRO-1855 slice 2b-ii-A, PR #51,
`5862666`) after three clean P20 passes (7→8→8). **BRO-1855 is still In Progress** — its done.check
("the 4 guardrail scenarios run through the REAL child, no fixture") is NOT yet met. **NEXT = slice
2b-ii-B: retire `loop-child.ts`** — re-point `loops.test.ts`'s four scenarios onto the real
`broomva-child`, then delete the fixture. That completes BRO-1855.

## State of the world (P15)

- **maestro repo** `apps/maestro/` — `main` @ **`5862666`**, tree clean, branch deleted.
- **Linear** `6c738b50`: P0+P1 done; P2 = runtime chain (11) + 1814 + M3 (1825) + routing (1824) +
  broomva-child slices 1 (1854) + 2a (1856) + 2b-i (1855) + **2b-ii-A (1855, In Progress)**. NEXT:
  **BRO-1855 slice 2b-ii-B**. Then slice 3 → P2-exit 1827 → M4 1826.
- Follow-up **BRO-1860** (child loop hardening, Low) now holds 8 P20 residuals across 2b-i + 2b-ii-A.
- CI-gated automerge unchanged (4 required jobs; CodeRabbit advisory; enforce_admins:false; green-only).

## What 2b-ii-A shipped (build 2b-ii-B on this)

The real `broomva-child` now supports lossless respawn + a production ceiling:
- **Resume**: at loop start `readProgress(runDir)` (guarded on empty runDir). A non-null checkpoint (a
  respawn) folds `stateOfTheWorld` + `whatsLeft` into the opening prompt via `resumeSuffix`, and seeds
  `resumedFrom = Math.max(0, checkpoint.iteration)` into `state.iterations` AND the loop bound
  (`beat = resumedFrom+1; beat <= resumedFrom+MAX_BEATS`), so `iteration_cap` accumulates across respawns.
- **Checkpoint content** (fixed in R1): `whatsLeft: []` (remaining TASKS — no fabricated list); errors are
  labeled context in `stateOfTheWorld`, never the task list.
- **Ceiling as a production knob**: `config.contextCeilingTokens` → supervisor → `ChildEnvSpec` →
  `buildChildEnv` sets `BROOMVA_CONTEXT_CEILING` (after the allowlist/secret filter) → `resolveCeiling()`.
- `parseProgress` rejects a negative iteration (a corrupted checkpoint can't loosen `iteration_cap`).

## Slice 2b-ii-B build plan (completes BRO-1855)

Retire `apps/runtime/src/proxy/loop-child.ts` by driving its four scenarios through the REAL child in
`apps/runtime/src/proxy/loops.test.ts` (they currently spawn `loop-child.ts` via `scenarioSpawn(scenario)`):

1. **Swap the spawn**: replace `scenarioSpawn` with a real-child spawn (mirror `broomva-child.test.ts`'s
   `makeChildSpawn` — it already spawns `broomva-child.ts` through the injectable `spawnChild` seam). Each
   scenario now drives behavior via the MOCK script (+ config, not `--scenario` flags):
   - **budget**: a per_run cap + a mock per-call cost → the proxy 402s mid-run → the child halts budget.
   - **no_progress**: the model calls a no-op tool each beat (`echo hi`) → empty diffs → engine halts.
   - **fresh_context**: `config.contextCeilingTokens` small + a tool that grows context → restart →
     checkpoint → respawn → the child RESUMES from `progress.md` (now real, 2b-ii-A) → completes (review).
     Assert `respawns===1` + the resume (the fixture's stub `readProgress`-then-finish is now the real path).
   - **kill**: the model emits a tool_use for a slow command (`sleep 30`); the test waits for `tool.call`
     then `sup.kill(runId)` → SIGKILL a live child mid-`executeTool` → `canceled` + `run.killed` (BRO-1801).
2. **Delete `loop-child.ts`** + the `scenarioSpawn` helper once the four scenarios are green.
3. Keep ZERO tokens (the mock scripts tool_use + text; it DEGRADES tool_use when no `tools` are advertised
   — the child sends `TOOL_SCHEMAS`, so tool_use flows). Preserve the exact-terminal-triple + `mock.calls`
   anti-vacuity assertions the fixture tests already have.

**If 2b-ii-B is too large, split it** (budget/no_progress first, then fresh_context+kill) — but it should
be one clean iteration: the child already supports every scenario; this is a re-point + delete.

## The loop

Driver `~/broomva/docs/maestro-orchestration-loop.md`. One ticket/iteration: snapshot → dep-chain →
**branch FIRST + verify `git branch --show-current`** → build (reuse the shipped seams) → bun test/tsc/biome
green → dogfood (the loops ARE the P11 dogfood: real processes/HTTP/worktrees, zero tokens) → **P20 Workflow
≥7 AND 0 surviving blocker/major, fix every finding mutation-proven, re-verify on the pushed code** → PR via
REST → resolve CodeRabbit threads → merge green via REST → bookkeep (2b-ii-B completes BRO-1855's done.check
→ move BRO-1855 → Done) → next handoff → `ScheduleWakeup` (next unit: slice 3 stdin + P2-exit 1827).

**P20 is LOAD-BEARING (rule-of-many).** Even the passing rounds surface real quality gaps (2b-ii-A R1 caught
a rule-of-three resume-content mislabel; R2 a corrupted-checkpoint guardrail-loosening). Treat the gate as
an unbounded re-verify loop; mutation-prove each fix. `[[fix-in-edge-dense-file-regresses]]`,
`[[checkpoint-before-restart-signal]]`. Stop = `ScheduleWakeup({stop:true})` when all P0–P5 non-OUTLOOK
tickets are Done + the P2 exit (1827) passes.
