# Attributed identity: live receipts on the Mac

These receipts check BRO-3846, attributed session identity, merged as #43 (`f3dfded`). The spec is
[2026-11-17-auth-a-session-attestations.md](../specs/2026-11-17-auth-a-session-attestations.md),
section "Live on the Mac". The receipts were made on 2026-11-18 between 16:11Z and 16:20Z, on the owner's Mac.

## Setup

- **Binary:** the release binary was built from `f3dfded` and installed at `~/.local/bin/maestro` at about 16:09Z.
- **MCP server:** the Claude thread's maestro MCP server was restarted at 16:11:21Z, so it runs that binary. The Codex thread started after the install.
- **Credentials:** none. No `MAESTRO_SESSION`, launch token or bridge credential was set in either thread. Identity came only from the thread variables.
- **Flow:** each run used [`verify.yaml`](../../crates/flow/flows/verify.yaml) (`verify@1`) against a throwaway one-commit git subject, with `check: "true"` and no surface. Each accepting spine has pin (exit 0), surface (exit 1, `none`) and check (exit 0). `check` needs only `pin` and `surface` needs nothing, so their order varies: Claude's runs accepted pin, surface, check; Codex's accepted pin, check, surface.

| Thread | Harness | Variable |
|---|---|---|
| Claude | Claude Code, in T3 | `T3CODE_THREAD_ID=493baa7d-3bee-423f-946a-3c07bac908cf` |
| Codex | Codex, a T3 child thread | `T3CODE_THREAD_ID=thread:delegated-task:…bae42cf9…` and `CODEX_THREAD_ID=01a1ef6c-e1a8-7302-ac90-a277d01677cf` |

## Receipts

| # | Spec receipt | Evidence | Outcome |
|---|---|---|---|
| 1 | A Claude T3 thread runs `verify@1` to terminal over MCP | Instance `fi_5e8e671ec450f96b80baf65d5dccee85`. MCP calls made with no session argument. Terminal at seq 7. All 7 events carry `audit.identity = {mode: t3-attributed, thread: 493baa7d…, variable: T3CODE_THREAD_ID}` and the process chain `maestro mcp → claude → t3 → t3 → launchd`. [mcp](2026-11-18-attributed-identity/claude/r1-mcp.json), [events](2026-11-18-attributed-identity/claude/r1-events.jsonl) | PASS |
| 2 | A Codex T3 thread runs it through the CLI from its shell | Instance `fi_91e91794e75c887e81a993852e5addd6`. Run with `maestro flow …` and no `--session`. Terminal. All 7 events are `t3-attributed` from `T3CODE_THREAD_ID`, with ancestors `zsh → codex → node → t3 → t3 → launchd`. [events](2026-11-18-attributed-identity/codex/r2-events.jsonl), [status](2026-11-18-attributed-identity/codex/r2-status.json), [spine](2026-11-18-attributed-identity/codex/r2-spine.json) | PASS |
| 3 | A Codex MCP write is refused `attribution_missing` | A real `maestro_flow_start` call through Codex's configured maestro MCP server, with no session. It returned `ok: false, code: attribution_missing, recorded: false`: the MCP process carries no thread variable (BRO-3848). [r3](2026-11-18-attributed-identity/codex/r3.json) | PASS |
| 4 | A cross-thread claim is refused `thread_mismatch` | From the Claude thread's shell, `flow start --session t3:030f399c…/b5d7c1a9-6d9c-4f30-83aa-e24704e0d08b` exited 1 with `thread_mismatch`, `recorded: false`. [r4](2026-11-18-attributed-identity/claude/r4.json) | PASS |
| 5 | Claude's Stop hook refuses while an instance is open | Instance `fi_d4bd37528747cd37f63f1b99db50bf5f` had `pin` claimed and not completed, and `flow open` reported `refuses_stop: true`. The real Claude Code Stop hook blocked the thread's turn end, naming that instance. The gate run directly also returned `decision: block`. The instance was then completed to terminal (seq 7, 16:13:59Z), and a `flow open` run at that time reported `refuses_stop: false`; that output was not saved. [open](2026-11-18-attributed-identity/claude/r5-open.json), [hook](2026-11-18-attributed-identity/claude/r5-hook.txt), [direct](2026-11-18-attributed-identity/claude/r5-direct.json), [events](2026-11-18-attributed-identity/claude/r5-events.jsonl), [final status](2026-11-18-attributed-identity/claude/r5-final-status.json) | PASS |
| 6 | Codex's Stop hook does the same through stdin `session_id` | Instance `fi_215c086784710fa9a435fe4338d17541` had `pin` claimed. `maestro flow stop-gate --harness codex` ran in Codex's hook format, with both thread variables unset and the stdin `session_id` set to the Codex thread id, so the binding came only from stdin. It returned `decision: block`, naming the instance. The instance was then completed to terminal. [direct](2026-11-18-attributed-identity/codex/r6-direct.json), [open](2026-11-18-attributed-identity/codex/r6-open.json), [final](2026-11-18-attributed-identity/codex/r6-final-status.json) | PASS, run directly |

## Limits

- **Receipt 5's hook text** ([r5-hook.txt](2026-11-18-attributed-identity/claude/r5-hook.txt)) was transcribed from the hook feedback the harness delivered to the thread. It is not a raw extract from the harness log. The event log corroborates the turn boundary: `pin` was claimed at 16:13:20Z (seq 2) and completed 24 s later (seq 3), after the blocked stop. Every other transition took 2–9 s.
- **Receipt 6** ran the hook command directly, as Codex invokes it. The real Codex end-of-turn hook was not observed: the Codex thread completed its instance before ending its turn, so the real hook would have let the stop through. Receipt 5 is the observed real-hook case. [r6-direct.json](2026-11-18-attributed-identity/codex/r6-direct.json) keeps only the gate's stdout. The command, the unset variables and the stdin payload are recorded in the Codex thread's [summary](2026-11-18-attributed-identity/codex/SUMMARY.md), not in the artifact itself. Receipt 6 commits no events log.
- **Receipt 2** was attributed through `T3CODE_THREAD_ID`, which T3 sets alongside `CODEX_THREAD_ID` in a Codex thread. The `CODEX_THREAD_ID` path is exercised by receipt 6's stdin binding and by the attribution suites, not by a separate CLI run.
- **Variable inheritance:** these receipts show attribution, not authentication. Any process that inherits a thread's variable is attributed to that thread, as the spec states (BRO-3847 tracks per-agent OS isolation).
- **Subject repositories** were throwaway one-commit repositories under `/tmp`. The flow state lives in the workspace's `.broomva/flows/`, which is not committed.
- **Redaction:** the `claim_token` lease tokens in the Codex claim responses are redacted. Their claims are finished, but a bearer token has no place in a public repository. Instance ids, thread ids, hashes and event logs are kept as recorded.
- **Finding while recording (BRO-3870):** about 10 minutes after finishing, both Claude instances read `terminal: false` again. `pin` has `fresh_for: 10m`; once it lapses, `pin` and `check` turn `stale`, and `flow open` lists `pin` as work again. The Stop gate then refuses the owning session's stop again. Each receipt's outcome is stated as of its own run; the reopening is a kernel defect filed as BRO-3870, outside BRO-3846's scope.
