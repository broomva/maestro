# auth-A probes, 2026-11-17, Mac (Darwin 25.5.0, T3 0.1.5-nightly.20261029)

Run as the owner's uid (501) from T3 thread `493baa7d…`. No sudo was used and no `_maestro` user exists yet, so every claim about a second uid comes from a cross-uid read of a root-owned process, the nearest stand-in.

| # | Question | Command | Result |
|---|---|---|---|
| P1 | Can a live T3 thread write a flow today? | MCP `maestro_flow_start` with `session: t3:local/<thread>`, `cwd: <worktree>`, no credential | Refused: `caller_refused`, `invalid_token`, "authenticated bridge credential or launch token is required" (`crates/mcp/src/lib.rs` `verified_caller`) |
| P2 | Does the Stop gate know a T3 session? | `echo '{…}' \| maestro flow stop-gate --harness claude` in a T3 thread | `no Maestro session is named (--session or MAESTRO_SESSION)`; prints `{}`, so the stop goes through |
| P3 | Process chain of a T3 tool call | `ps -o pid,ppid,comm`, walked up from the shell | `zsh → claude (harness) → t3 (server) → t3 (launcher) → launchd`; the harness is a direct child of the T3 server |
| P4 | Does T3 record provider pids? | `provider_session_runtime.runtime_payload_json`, `orchestration_v2_projection_provider_sessions` | No pid field in either; there is thread id, cwd, provider and model |
| P5 | Does T3 persist a tool call, attributed to its thread? | Echo the unique nonce `authA-probe-nonce-…` in a Bash call, then query `orchestration_v2_projection_turn_items` | A row in the calling thread only, `type = command_execution`, id `turn-item:provider:claudeAgent:native-item:toolu_…`, written about 1 s after the call (19:37:59 call, 19:38:00.995 `updated_at`). It is persisted while still `running`. Later calls that quoted the nonce matched too, so a match must be restricted to one tool name and one call |
| P6 | Can a second uid read process facts? | `sysctl {CTL_KERN, KERN_PROC, KERN_PROC_PID, 1}` (pid 1 is root-owned) from uid 501 | Succeeds, 648 bytes of `kinfo_proc` (ppid, start time, comm). The environment and argv of another uid's processes are not readable on macOS |
| P7 | Who can write T3's state? | `ls -l ~/.t3/userdata/statev2.sqlite` | `-rw-r--r-- broomva`: any process of the owner's uid, agents included, can write it |
| P8 | Can a second uid reach `~/.t3`? | `ls -ld /Users/broomva` | `drwxr-x---+`: only with an ACL the owner grants |
| P9 | Does the harness tell an MCP server the call id? | `strings` over the Claude Code and Codex binaries for `_meta` keys | Inconclusive. Maestro's `tool_use_id` fields are always `None` (`crates/flow/src/kernel/write.rs`). Nothing in this design depends on it |

## Round 2 probes (after the round-1 review)

| # | Question | Command | Result |
|---|---|---|---|
| P10 | Does each Claude thread's `maestro mcp` carry its thread id? | `ps eww` on each `maestro mcp` whose parent is `claude` | Yes: one server per thread, each carrying its own `T3CODE_THREAD_ID` (4 threads observed) |
| P11 | And Codex? | `ps eww` on the `maestro mcp` children of `codex app-server` (pid 91406) | 8 servers under one shared app-server, with no thread variable. Codex shells (e.g. `make`) carry `CODEX_THREAD_ID` and `CODEX_SESSION_ID` |
| P12 | Do provider-native subagents share their parent's process? | Gemini's round-1 review, `orchestration_v2_projection_subagents` | Yes: 274 Claude and 1230 Codex `provider_native` subagents run in their parent's harness and MCP server |
| P13 | Can `CODEX_THREAD_ID` be mapped to its T3 thread? | `orchestration_v2_projection_provider_threads.payload_json` | Yes: `nativeThreadRef: {driver: "codex", nativeId: <CODEX_THREAD_ID>, strength: "strong"}` on the thread's row |
| P14 | And Gemini? | `ps eww` on `gemini --acp` and its children | No thread variable. Gemini also records `input: {}` for MCP tool calls in T3 (round-1 review) |
| P15 | Round-2 live probes (Gemini stratum B, 2026-11-17) | `lsof -a -d cwd`, `ps eww`, `statev2.sqlite` | 242 of 325 live threads (74.5%) share a worktree or project root, because completed shared-isolation `delegate_task` children never settle. All 11 `maestro mcp` servers run in their thread's worktree. 3 agent shells had `cd`'d outside theirs. `nativeThreadRef.nativeId` lives in `orchestration_v2_projection_provider_threads` and is unique across 2,152 rows; 66 threads have more than one row. Codex Stop hooks have no thread variable but get `session_id` and `cwd` on stdin. Claude hooks inherit `T3CODE_THREAD_ID`. The CLI sends no `cwd` and `MAESTRO_CALLER_CWD` is unset everywhere, so every CLI flow write is refused today |
| P16 | Round-3 live probes (Gemini stratum B, 2026-11-17) | `statev2.sqlite`, `crates/hosts/src/{t3.rs,lib.rs}`, the Codex binary's Stop schema | `is_safe_t3_id` refuses 870 of 1,107 live thread ids (78.6%: `%` and 129–206 bytes), including every delegated thread. `SurfaceRegistry::from_env` reads only `MAESTRO_T3_HOME`, but T3 sets `T3CODE_HOME`. 2 of 5 `running` rows are zombies, and 7,164 overlapping run pairs exist in shared worktrees. The Codex Stop input requires `session_id`, which equals `nativeThreadRef.nativeId`. `nativeId` maps to exactly one thread (0 collisions across 2,155). 66 threads have more than one provider row, and `active_provider_thread_id` names the live one |
