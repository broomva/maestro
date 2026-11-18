# Claude T3 thread receipts (BRO-3846, maestro main f3dfded)

Thread: T3CODE_THREAD_ID=493baa7d-3bee-423f-946a-3c07bac908cf, environment 030f399c-549a-4f0e-8f56-4c18c8b48e51. No MAESTRO_SESSION, launch token or bridge credential set.
Binary: ~/.local/bin/maestro, built from f3dfded and installed 2026-11-18 ~16:09Z. The session's maestro MCP server (pid 48003) was restarted at 16:11:21Z, so it runs that binary.

| # | Receipt | Instance or refusal | Outcome |
|---|---|---|---|
| 1 | verify@1 to terminal over MCP, no session argument | fi_5e8e671ec450f96b80baf65d5dccee85; terminal at seq 7; spine pin 0, surface 1, check 0. All 7 events have audit.identity = {mode: t3-attributed, thread: 493baa7d…, variable: T3CODE_THREAD_ID}, and the process chain is maestro mcp → claude → t3 → t3 → launchd | PASS |
| 4 | Cross-thread claim via CLI: --session t3:030f399c…/b5d7c1a9-6d9c-4f30-83aa-e24704e0d08b | exit 1, code thread_mismatch, recorded false | PASS |
| 5 | Claude Stop hook refuses while an instance is open | fi_d4bd37528747cd37f63f1b99db50bf5f with pin claimed. Direct gate: decision block. The real Stop hook blocked the coordinator's turn end (r5-hook.txt). The instance was then completed to terminal, and refuses_stop is false | PASS |
