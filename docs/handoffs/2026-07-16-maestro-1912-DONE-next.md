# Maestro loop handoff — BRO-1912 real Claude subscription runner (DONE, MERGED)

**Date:** 2026-07-16
**Ticket:** BRO-1912 — **Done** (Linear, completedAt 2026-07-16).
**PR:** #79 MERGED — squash `e03ddf6` on `main` (main tip = `e03ddf6`). Branch pruned (P8/P10).
**Loop:** autonomous self-firing. This iteration is CLOSED. Next iteration continues the provider/UX arc.

## What shipped
The first REAL model provider: the loop runs on a live **Claude subscription** (not the mock). The
`claude` CLI is spawned as a maestro runner on the runner-port/spawn seam; its `stream-json` is
translated → maestro `session.jsonl` events. Provider selected by `MAESTRO_PROVIDER=claude|codex|mock`.

Files (all `apps/runtime/src/`): `child/claude-stream.ts` (+test, pure translator), `child/claude-runner.ts`
(+test, child entry), `harness/spawn-contract.ts` (`buildClaudeProviderEnv`), `config.ts`, `dispatch.ts`,
`index.ts`, `supervisor/supervisor.ts` (graceful F8 kill).

## The P20 saga (this PR earned the gate — DO-NOT-MERGE 5/10 → MERGE 8/10)
The first cut spawned the CLI with `env: {...process.env}`. P20 (Strata-B) caught a **BLOCKER** and **MAJOR**
past green CI + passing tests. All fixed, mutation-proven, empirically validated:

- **BLOCKER — env exfil (R5 KEY-EXFIL + billing):** `{...process.env}` leaked every host secret
  (ANTHROPIC_API_KEY, CLOUDFLARE_API_TOKEN, …) into a `bypassPermissions` agent AND billed the API not the
  subscription. Fix = `buildClaudeProviderEnv`: deny-by-default `filterPassthroughEnv` floor + a NARROW
  named auth channel (USER/LOGNAME + CLAUDE_CODE_OAUTH_TOKEN) + `delete ANTHROPIC_API_KEY`. Confined at BOTH
  hops (runner spawn in dispatch.ts, CLI spawn in claude-runner.ts). Operator escape hatch
  `MAESTRO_CLAUDE_ENV_PASSTHROUGH` refuses ALL secret-named vars (not just the exact key).
- **MAJOR — orphaned CLI on kill:** F8 `kill()` SIGKILL'd the runner directly, bypassing its SIGTERM handler
  → the spawned `claude` CLI orphaned (kept spending + mutating). Fix = `terminateChild` SIGTERM→grace→SIGKILL
  (reuses the `escalateHung` pattern); the runner's SIGTERM handler SIGKILLs its CLI.
- MINORs: stop-race no longer flips clean-completion (0→review) to park-blocked; `--setting-sources ""`
  (load no settings, not the untrusted work-repo hooks). NIT: tool.call input bounded via `summarizeInput`.
- CodeRabbit: codex provider now FAILS CLOSED (was silent mock fallback); tool-name map self-bounds
  (delete-on-consume + evict-oldest, not clear-all). Both mutation-proven; threads resolved.

## LOAD-BEARING LESSONS (reusable — see also the memory topic file)
- **Subscription auth channel is USER/LOGNAME, NOT HOME.** The subscription OAuth token lives in the macOS
  login **Keychain** (item `Claude Code-credentials`), not `~/.claude/.credentials.json` and not an env var.
  The CLI needs USER+LOGNAME to resolve the keychain owner. `filterPassthroughEnv`'s HOME alone did NOT
  authenticate on this host (contra the P20 reviewer's assumption). Empirically pinned via env bisection.
- **Superconductor shim confound.** `/Users/broomva/.superconductor/bin/claude` is a WRAPPER that delegates
  to the real binary `/Users/broomva/.local/bin/claude`. Env-auth diagnostics must target the REAL binary
  (`MAESTRO_CLAUDE_BIN=/Users/broomva/.local/bin/claude`) or the shim's PATH-delegation confounds the test.
- **SIGKILL-orphans-grandchild class.** Any runner that spawns a grandchild process needs SIGTERM-first at
  the kill seam so the runner reaps its grandchild before dying; a bare SIGKILL to the runner orphans it.
- **Dogfood proof pattern:** inject a BOGUS ANTHROPIC_API_KEY into the runtime env; a successful subscription
  completion proves BOTH that the key was stripped AND that auth survived confinement (strongest signal).

## Dogfood (P11) — PROVEN
Real `claude` (haiku, to conserve credits) under the CONFINED env with bogus ANTHROPIC_API_KEY/
CLOUDFLARE_API_TOKEN injected: authenticated on the subscription, found+read `_work.md`, wrote a real
`haiku.txt`, full event stream → `run.exiting{0}` → **review ("Needs you")**. 600/601 runtime tests pass
(the 1 failure is an unrelated real-FS watcher-reconcile timing flake, passes in isolation).

## RESUME — next iteration candidates (ranked; snapshot re-picks)
1. **Board/gate "review-not-shown-as-Needs-you"** (HIGH UX; needs a ticket). A session-less CLI completion
   reaches `review` in `/api/tree` but the board renders it "Queued" + top-bar "nothing at your gate". Now
   that real Claude runs actually reach `review`, this invisibility breaks the core disclosure ladder (the
   gate is the human's). The review→gate→board chain likely needs a gate row/`look` the CLI path doesn't emit.
2. **BRO-1809** (M5 inspector chat/activity projections — the screenshot gap the user flagged in msg 1: the
   inspector shows no chat). Chat stack at `apps/app/src/chat/*`; endpoint `POST /api/sessions/:id/chat`
   (dispatch-then-chat). Dogfood against real Claude now that it produces real chat events.
3. **Codex CLI adapter** (slice 2, same seam as BRO-1912 — completes "claude AND codex"). `codex exec`,
   ChatGPT-OAuth auth in `~/.codex/auth.json`; mirror `claude-runner.ts` + a `codex-stream.ts` translator.
   Note: dispatch already FAILS CLOSED for codex until this lands.

Then resume the P3 backend chain: 1805 (F5 gate slice) → 1809 → 1813 → 1818 → 1821, then P4/P5.

Driver: `~/broomva/docs/maestro-orchestration-loop.md`. Topic: [[maestro-build-arc]]. Linear project `6c738b50`.
