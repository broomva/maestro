# Maestro autonomous loop — BRO-1855 DONE (fixture retired); NEXT = BRO-1862 slice 3 (child stdin control)

**TL;DR.** The `loop-child.ts` fixture is retired and the four D8-layer-1 guardrail scenarios now run
through the REAL `broomva-child` (BRO-1855 slice 2b-ii-B, PR #52, merged `bf24bcf`) after two clean P20
passes (8/10 → 8/10, round 2 with 0 confirmed findings). **BRO-1855 is DONE** — the whole `broomva-child`
keystone is complete (slices 1 / 2a / 2b-i / 2b-ii-A / 2b-ii-B). **NEXT = BRO-1862: broomva-child slice 3 —
stdin control (chat / stop / ping)**, the last child capability, which unblocks BRO-1822 (chat→stdin) and
the interactive parts of the P2-exit BRO-1827.

## State of the world (P15)

- **maestro repo** `apps/maestro/` — `main` @ **`bf24bcf`**, tree clean, branch deleted, no open PRs.
- **Linear** `6c738b50`: P0+P1 done. P2 = runtime chain (11) + 1814 + M3 (1825) + routing (1824) +
  **broomva-child COMPLETE (BRO-1855 DONE)**: slices 1 (1854) / 2a (1856) / 2b-i / 2b-ii-A / 2b-ii-B.
  **1822 chat** stays Backlog (routes into the slice-3 stdin `chat`).
- **NEXT: BRO-1862** (created this iteration, child of 1827, P2/High, Backlog). Then BRO-1827 (P2-exit
  phase gate, P1) → chat 1822 → M4 1826.
- Follow-up **BRO-1860** (child loop hardening, Low) holds 9 P20 residuals (2b-i / 2b-ii-A / 2b-ii-B incl.
  the refuted budget-exact-count nit).
- CI-gated automerge unchanged (4 required jobs: quality/secrets/build/governance; CodeRabbit advisory;
  enforce_admins:false; green-only). Merge via REST (`gh api .../merge -f sha=<full-40-char>` — the short
  sha 422s). P20 uses local `git diff main...HEAD`.

## What 2b-ii-B shipped (build 1862 on this)

- `apps/runtime/src/proxy/loops.test.ts` re-pointed: a `makeChildSpawn(extra)` factory spawns the REAL
  `broomva-child.ts` via the injectable `spawnChild` seam (mirrors `broomva-child.test.ts`). The four
  scenarios are driven by a mock SCRIPT + runtime `config`, NOT `--scenario` flags.
- `loop-child.ts` + `scenarioSpawn` DELETED (zero remaining code refs).
- The mock `MockModelOptions` = `{ script?: MockResponse[], fallback?: MockResponse, usagePerCallUsd? }`;
  `MockResponse = { status?, body?, usage? }`. It DEGRADES tool_use to text when a request advertises no
  `tools`; the real child sends `TOOL_SCHEMAS`, so tool_use flows at zero tokens.
- The child's stdin seam ALREADY exists on the supervisor side: `ChildStdioPort.writeStdin(b: string)` (see
  `makeChildSpawn` in both test files). Slice 3's job is to make the CHILD consume it.

## Slice 3 build plan (BRO-1862)

Wire `apps/runtime/src/child/broomva-child.ts` to read an NDJSON control channel from stdin CONCURRENTLY
with the beat loop (HARNESS §6):
1. **ping** → liveness ack (a `run.ping`/pong receipt) without disturbing the loop.
2. **stop** → GRACEFUL halt at the beat boundary (never mid-`executeTool`) → exit 10 with a `stopped`/
   `canceled` reason, node parked. Distinct from the F8 SIGKILL hard-kill path (which stays).
3. **chat** → inject a `{role:"user", content}` turn into the running `messages` so the next beat sees it
   (the seam 1822 routes UI messages into).
- Concurrent reader that never blocks a beat; malformed/partial lines skipped, not fatal (mirror the tool
  layer's never-throw discipline). Control messages teed as child events.
- Tests spawn the real child (`makeChildSpawn`) and drive stdin directly via `writeStdin`; the mock drives
  the model side. done.check: `bun test apps/runtime/src/child` — ping/stop/chat through the real child,
  zero tokens. Assert `chat` via the next beat's `mock.calls` carrying the injected turn.

**Split if needed** (e.g. ping+stop first, then chat) documenting the choice — but it should be one clean
iteration on the seams already in place.

## Reusable lessons banked (2b-ii-B)

- **An end-to-end scenario whose completion path is reachable WITHOUT the guardrail firing is vacuous even
  when green.** The fresh_context loops test completed on the exhausted-script text fallback before the
  ceiling could re-fire, so a broken `readProgress` passed — the fix was an assertion (attempt-2's prompt
  carries `RESUMING`) that reds ONLY when the resume regresses. Mutation-prove the discriminator, not the
  happy path. `[[self-hosting-vacuous-pass]]`, `[[fix-in-edge-dense-file-regresses]]`.
- **SIGKILL does not cascade to shell grandchildren** — killing a child mid-`sh -c` orphans the grandchild
  (reparented to PID 1) for the command's full duration. Bound the command (short sleep); don't rely on the
  kill to reap it. (Empirically reproduced: a pre-fix `sleep 300` survived across runs.)
- **The supervisor pins one run dir per run id across respawns**, so a checkpoint round-trips in
  loops.test.ts without the sibling's explicit `BROOMVA_RUN_DIR` override.

## The loop

Driver `~/broomva/docs/maestro-orchestration-loop.md`. One ticket/iteration: snapshot → dep-chain → **branch
FIRST + verify `git branch --show-current`** → build (reuse shipped seams) → bun test/tsc/biome green →
dogfood (P11) → **P20 Workflow ≥7 AND 0 surviving blocker/major, fix every CONFIRMED finding mutation-proven,
re-verify on the pushed code** → PR via REST → resolve CodeRabbit threads → merge green via REST → bookkeep →
next handoff → `ScheduleWakeup` (next unit: BRO-1862 slice 3).

**P20 is LOAD-BEARING (rule-of-many).** A clean terminal pass merges; refuted/defense-in-depth nits become
follow-up tickets (BRO-1860), NOT another round. Stop = `ScheduleWakeup({stop:true})` when all P0–P5
non-OUTLOOK tickets are Done + the P2 exit (1827) passes.
