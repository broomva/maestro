# kernel-ready: the flow kernel checks its own readiness, from Claude Code and Codex

These receipts close BRO-3842, the meta dogfood of the flow kernel. They were made on 2026-11-20 and 2026-11-21 on the owner's Mac. Each run is one instance of [`kernel-ready.yaml`](../../crates/flow/flows/kernel-ready.yaml), whose nodes are the readiness checks. Each run was driven from a harness, and its accepting spine was compared with the other harness's run through `maestro flow spine --against`.

There are two versions:
- **kernel-ready@1** ended in an owner `ready` node.
- **kernel-ready@2** (BRO-4044, #63) removes that node. The owner directed on 2026-11-20 that readiness is for agents to verify, not a question for the owner. An @2 instance reaching terminal means the head is ready.

## kernel-ready@2 at `59cb97b` (the result)

**Setup:** `~/.local/bin/maestro` was built from a clean checkout of `59cb97b` (#63's merge), and `maestro --version` reported `0.1.0 (59cb97b3f7c65c2bf247570fe7a3a8225200037c)`. Both instances used the same params:
- repo: `/tmp/kr-v2`, a clean clone at that commit
- head: `59cb97b`
- suite: `cargo test --locked -p maestro-flow -p maestro`
- consumer: content-flow
- writer_provider: `claude`
- surface: `maestro flow verify@1 at this head, driven from the CLI`

| Run | Instance | Driven from | `exercise` performer (attested) | Result |
|---|---|---|---|---|
| Claude | `fi_a0033ad0207dbb7528edac98439e8f42` | Claude Code in T3 (thread `493baa7d…`), started 05:10:50Z | `harness: gemini`, a T3 child of the driver | terminal, 7 of 7 nodes `done` |
| Codex | `fi_780ae2adaf5b6044da23ede0882601e5` | Codex, a T3 child thread, started 05:11:27Z | `harness: codex`, a T3 child of the driver | terminal, 7 of 7 nodes `done` |

- **Equal spines.** Both accept `pin, suite, installed, mcp_tools, stop_gate, consumers, exercise`, each with exit 0, under the same definition (`sha256:e18579f9…`). `maestro flow spine --instance fi_a0033ad0… --against fi_780ae2ad…` exits 0. See [against](2026-11-21-kernel-ready/v2/against-claude-codex.json) and the [Claude spine](2026-11-21-kernel-ready/v2/claude-spine.json) and [Codex spine](2026-11-21-kernel-ready/v2/codex-spine.json).
- **Routing held.** `exercise` is routed `different_from(params.writer_provider)`. Neither performer was Claude: the kernel recorded `gemini` for the Claude run and `codex` for the Codex run. See the [Claude events](2026-11-21-kernel-ready/v2/claude-events.jsonl) and [Codex events](2026-11-21-kernel-ready/v2/codex-events.jsonl).
- **Nothing waited on the owner.** Neither instance has a human node, and neither filed an ask.
- **The exercise was real.** The Claude run's receipt is [claude-exercise-receipt.json](2026-11-21-kernel-ready/v2/claude-exercise-receipt.json), with `result: pass`. Its evidence is a fresh `verify@1` instance that the Gemini verifier drove to terminal from an isolated checkout of the head.

## kernel-ready@1, the runs that shaped @2

| Run | Instance | Head | Outcome |
|---|---|---|---|
| First live run (Claude) | `fi_c938b824d3efbeae25f143e19d39bea3` | `4d6ede5` | Every check accepted. `suite` failed closed once on a macOS test-helper bug (BRO-4017, fixed in #60), and the Codex attempt found custom-named Codex instances unattested (BRO-4018, fixed in #61). Owner answer: **not ready**. Terminal. |
| Final, Claude | `fi_bb2686ac150f7d1417616e4ba6695679` | `fd9edf9` | Every check accepted, `exercise` by Gemini. Owner answer: **ready** (workspace#1379). Terminal. |
| Final, Codex | `fi_2d50569d20b5f89a565858d96049b885` | `fd9edf9` | Every check accepted, `exercise` by Codex. Never took the owner's answer: while it waited at `ready`, `pin` went stale past its 2 h `fresh_for`. Every node went stale with it and `ready` became `blocked`, and the Codex session that owned it had ended (BRO-4025). |

The two final @1 runs are identical on every check node. `spine --against` exits 1 at `ready` alone: "the instance accepted ready at step 7; the other never did". See the [comparison](2026-11-21-kernel-ready/v1/against-claude-codex.json) and the [error text](2026-11-21-kernel-ready/v1/against-claude-codex.stderr.txt).

## What the dogfood found

| Ticket | Finding | State |
|---|---|---|
| BRO-4017 | A macOS test helper compared an unresolved `/var` path with `/private/var` | fixed, #60 |
| BRO-4018 | A custom-named Codex instance had no attested provider | fixed, #61 |
| BRO-4044 | The owner `ready` node asked the owner to confirm evidence the agents had already verified | fixed, #63 (kernel-ready@2) |
| BRO-4020 | The @1 ask said "every check passed" where it meant "was accepted" | moot for @2, which has no ask |
| BRO-4025 | A waiting human node is pulled back when an upstream observation goes stale, and an instance whose owner session ended is stranded | open; @2 does not hit it |
| BRO-4053 | The Stop gate counts a heartbeated claim that waits on a delegated child as work the driver can do now, so the driver's turn cannot end | open |
| BRO-4054 | The driver skill tells a Codex driver to launch the verifier with native `spawn_agent`, which the kernel refuses as `performer_unverified`; a T3 session named without its `t3:<env>/` prefix is refused the same way | open |

## Limits

- **Redaction.** The Gemini verifier's evidence log held three `claim_token` lease tokens from its own `verify@1` run. The committed copy is [claude-exercise-evidence.redacted.log](2026-11-21-kernel-ready/v2/claude-exercise-evidence.redacted.log), with those tokens replaced. Because of that, its digest is not the one the receipt names. The receipt binds the unredacted original (`sha256:6686594a…`), which the kernel checked with `maestro dogfood verify-receipt` when `exercise` completed. No committed event log holds a claim token.
- **The Codex run's receipt** lives in that run's own temporary folder and is not committed. Its acceptance is recorded in the Codex events log (`Completed exercise`, `harness: codex`).
- **Terminal after staleness.** At 05:33Z, more than 2 hours after their last `pin` observation (about 03:28Z), both of the Claude session's @1 instances read `terminal: true` with every node `stale`, and `flow open` listed no instance for that session. The BRO-3870 fix holds.
- **Attribution, not authentication.** The performers are what T3 records the child threads ran (the BRO-3635 floor). See [2026-11-18-attributed-identity.md](2026-11-18-attributed-identity.md).
- **Subject state.** The flow state lives in the workspace's `.broomva/flows/`, which is not committed. The files here are copies taken after each instance finished.
