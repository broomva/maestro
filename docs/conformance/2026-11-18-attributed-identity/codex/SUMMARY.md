# BRO-3846 Codex live receipts

Date: 2026-11-18. `~/.local/bin/maestro --version`: `maestro 0.1.0`. Subject HEAD: `13372024f47e7dcc9e24a2ab4db53a96748382bd`.

Shell variables observed:
- `CODEX_THREAD_ID=01a1ef6c-e1a8-7302-ac90-a277d01677cf`
- `T3CODE_THREAD_ID=thread:delegated-task:command%3Amcp%3A6ed48328-2886-4b6b-93ce-74a0b99e42d1%3Adelegate-task%3Abae42cf9-2598-4f1b-9a39-e95e8f633cbb`

## Receipt 2 — PASS

CLI instance `fi_91e91794e75c887e81a993852e5addd6` reached `terminal: true`. Its Started, Claimed and Completed events record `identity.mode=t3-attributed`, `identity.variable=T3CODE_THREAD_ID` and the T3 thread value above; audit includes process PID, UID, start time, variable names and ancestors. Pin and check exited 0; surface exited 1 and selected `none`, as expected without a surface. See `r2-events.jsonl`, `r2-status.json`, `r2-spine.json` and individual command receipts.

## Receipt 3 — PASS

The configured Maestro MCP server was available. An actual `maestro_flow_start` MCP call omitted session and returned `ok: false`, `code: attribution_missing`, `recorded: false`; no instance was created. The server reported neither T3 thread variable was present. See `r3.json`.

## Receipt 6 — PASS

CLI instance `fi_215c086784710fa9a435fe4338d17541`, attributed using the T3 variable above, had pin claimed and `r6-open.json` reported `refuses_stop: true`. With both thread environment variables unset, stdin `session_id` was the CODEX_THREAD_ID above; the direct Codex stop-gate returned `decision: block` in `r6-direct.json`. The instance was subsequently completed to `terminal: true` in `r6-final-status.json`. The actual end-of-turn hook cannot be observed before this turn ends; no real-hook invocation or `r6-hook.txt` is claimed.
