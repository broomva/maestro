# Maestro loop handoff — BRO-1912 real Claude subscription runner (PR OPEN, mid-merge)

**Date:** 2026-07-15
**Ticket:** BRO-1912 (real provider runner — Claude Code CLI, subscription) — **In Progress**
**PR:** #79 (https://github.com/broomva/maestro/pull/79), branch `feature/bro-1912-real-provider-claude-cli-runner`, commit `685d0a7`, base `main`.
**Loop:** autonomous self-firing (user directive 2026-07-15). This iteration is NOT closed — PR open, P20 + CI in flight.

## What shipped (this PR)
The first REAL model provider so the loop runs on a live **Claude subscription** (not the mock). Spawns the `claude` CLI as a maestro runner on the runner-port/spawn seam; translates its `stream-json` → maestro `session.jsonl` events. Files: `apps/runtime/src/child/{claude-stream.ts (+test, 16 pass), claude-runner.ts}`, `config.ts` (MAESTRO_PROVIDER=claude|codex|mock), `dispatch.ts` (claudeSpawnChild + trust-model env inheritance), `index.ts` (mount gating).

## Dogfood (P11) — PROVEN at runtime level
- Standalone runner smoke: real Claude → `hello.txt`, correct event stream, exit 0.
- Full runtime: `MAESTRO_PROVIDER=claude` → mission dispatched via `/api/sessions/:id/chat` → real Claude ran in worktree `run/ecc92070`, wrote a real haiku, committed the run branch, parked at **`review`** ("Needs you"). `apiKeySource:none` = subscription.
- KEY integration fix: allowlisted child env stripped the CLI's auth ("Not logged in") → fixed via trusted-provider env inheritance in `claudeSpawnChild` (`env: {...process.env, ...args.env}`); the CLI is confined by the worktree + `--permission-mode`, not credential-denial.

## Gates: tsc 0, biome clean, 16 translator tests + config/mock-loop suites green.

## RESUME STEPS (next iteration)
1. Read the P20 background agent verdict (fires a completion notification). Address every blocker/major mutation-proven; re-verify.
2. Confirm CI green on PR #79 (`gh pr checks 79`); watcher log `/tmp/pr79-ci.log`.
3. Resolve any CodeRabbit/review threads.
4. Merge PR #79 via REST when green + P20 ≥7/10 + 0 surviving blocker/major.
5. Bookkeep; mark BRO-1912 Done; write the DONE handoff.
6. **Then continue the loop to the next ticket.** Candidates in priority: **BRO-1809 (M5 inspector chat/activity projections — the screenshot gap the user flagged; chat stack already exists at apps/app/src/chat/*, endpoint POST /api/sessions/:id/chat dispatch-then-chat)**; then the **board/gate finding below**; then **codex CLI adapter** (slice 2, same seam as BRO-1912).

## FOLLOW-UP FINDING (dogfood-surfaced, needs a ticket)
A session-less CLI completion reaches `review` in `/api/tree` (correct, via proxy AND direct) BUT the board rendered it as "Queued" + top-bar "nothing at your gate" (headless playwright capture). The review→gate→board chain likely needs a gate row/`look` the CLI completion path does not yet produce. HIGH UX priority (a run needing you is invisible). Board/gate arc, NOT the runner. File as a ticket next iteration.

## Live env (this session; will be gone after compaction — restart as needed)
- Runtime on :4319 (`MAESTRO_PROVIDER=claude`, workspace `scratchpad/dfws2`), vite SPA on :5173. Both throwaway.
- claude CLI: `/Users/broomva/.superconductor/bin/claude` (subscription, headless `-p --output-format stream-json` verified). Dogfood used `MAESTRO_CLAUDE_MODEL=claude-haiku-4-5-20251001` to conserve credits (subscription has no overage: `out_of_credits`).

Driver: `~/broomva/docs/maestro-orchestration-loop.md`. Topic: [[maestro-build-arc]].
