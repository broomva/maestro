# Attributed session identity for flow writes

Status: proposed, round 3, 2026-11-17. Tracking: BRO-3841 (arc). BRO-3273's isolation and owner-token scope moves to its own arc.
Owner decision, 2026-11-17 (AskUserQuestion, after the round-1 review): **"Attributed identity now."**

**The round-1 design is withdrawn.** It used maestrod as `_maestro`, Ed25519 attestations and a T3 timeline nonce. Both review strata found it STRUCTURAL: Codex 3/10, Gemini 2/10, findings on maestro#43. The decisive fact is that T3, the only component that knows which thread is calling, runs as the owner's uid. Any evidence or key T3 could hold is therefore forgeable by a same-uid agent, so no design makes one thread unable to impersonate another while agents share the owner's uid. This spec names identity at the level that is actually achievable.

Probe receipts: [2026-11-17-auth-a-probes/probes.md](2026-11-17-auth-a-probes/probes.md), P1–P15. Round 2 review: Codex 4/10, Gemini 5/10 (`continue`). Round 3 makes a thread variable the primary evidence, because Gemini measured that most live threads share a worktree.
Reversal cost: low. Attribution is a field on events and a verifier mode; the later isolation arc replaces the mode without migrating any ledger.

## Objective

Let a live T3 thread on Claude, Codex or Gemini write to the flow ledger, through MCP or the CLI, under an identity that is:

- **attributed**: checked against T3's own record of the thread, so mix-ups, stale sessions, wrong-worktree claims and copied credentials are refused;
- **honestly labelled**: every event records how its identity was established, so a later reader can't mistake attribution for authentication.

## Context

- Today every live write is refused (P1). `verified_caller` (`crates/mcp/src/lib.rs`) rejects a T3 or Paseo claim that carries no signed credential, before the T3 registry is consulted, and only tests mint credentials.
- `T3Registry::verify` (`crates/hosts/src/t3.rs`) already checks that a thread exists, that it is live (not closed, archived or settled-and-idle), and that the caller's cwd is within the thread's worktree. That is the attribution check. It is simply unreachable without a credential.
- **What the harnesses expose** (P10–P13):
  - Claude gives each thread its own `maestro mcp` and shells, both carrying `T3CODE_THREAD_ID`.
  - Codex runs one shared `codex app-server` for every thread. It spawns one `maestro mcp` per thread with no thread variable, and its shells carry `CODEX_THREAD_ID`, which T3 maps to the thread through `nativeThreadRef.nativeId` (`strength: strong`).
  - Gemini processes carry no thread variable.
- The Stop gate names no session in a T3 thread (P2), so it never binds.

## Goals

1. **Live writes succeed where a thread variable names the thread.**
   - Claude: MCP and CLI, through `T3CODE_THREAD_ID` in the caller's process.
   - Codex: CLI from the thread's shell, through `CODEX_THREAD_ID`.
   - Any harness: a cwd-only claim when exactly one *running* thread's worktree contains the cwd.
2. **No mis-attribution without deliberate forgery.**
   - A thread variable that names a different thread than the claim is refused (`thread_mismatch`).
   - A claim with no thread variable is refused (`attribution_ambiguous`) unless its cwd matches exactly one running thread.
   - The P15 probe found 74.5% of live threads sharing a worktree, so ambiguity is the common case and must fail closed.
3. **Every flow event written under attribution records** `identity: {mode: "t3-attributed", evidence: [...]}` and the audit facts defined below.
4. **The Stop gate finds the session without `MAESTRO_SESSION`.** It derives the session from the hook's own evidence and treats any uncertainty as *binding unavailable*.
5. **Nothing that verifies today changes meaning.** Signed credentials, Paseo and Direct launch tokens keep their behaviour, and `MAESTRO_IDENTITY=strict` reproduces today's refusals exactly.

## Non-goals

- **Stopping a deliberately malicious agent running as the owner's uid.** It can forge T3 state, set environment variables, edit the owner-writable ledger or drive another process. That needs agents under a separate OS uid: BRO-3847. The hash-chained log makes tampering visible, not impossible.
- **Signed credentials and maestrod.** Signing forgeable evidence adds no strength.
- **Telling a parent from its provider-native subagent.** They share a process, environment and worktree (P12), so a subagent's writes are attributed to its parent. The kernel's per-session claim tokens and leases still serialize the work.
- **Codex and Gemini over MCP in a shared worktree.** Their MCP servers carry no thread variable (P11, P14), so in a shared worktree they are refused `attribution_ambiguous`. Those sessions use the CLI from their shell, or wait for the follow-up in which T3 passes `T3CODE_THREAD_ID` to every MCP server it starts (a change in the owner's t3code fork, ticketed with this spec).

## Design

### Evidence

| Evidence | Source | Maps to a thread by |
|---|---|---|
| `thread_env` | `T3CODE_THREAD_ID` in the environment of the process making the claim: the CLI process itself, or the `maestro mcp` server for stdio MCP. It is never taken from tool arguments | Equality with the claimed thread id |
| `native_thread` | `CODEX_THREAD_ID` (same sources), or the Stop hook's stdin `session_id` | `orchestration_v2_projection_provider_threads.payload_json → $.nativeThreadRef.nativeId`, any row of the thread (66 threads have more than one row after provider switches). Unique across 2,152 rows (P15) |
| `cwd_unique` | The caller's directory, never a tool argument: for the CLI, `std::env::current_dir()`; for stdio MCP, the server process's own cwd; for the Stop hook, its stdin `cwd`. Matched with `within` (both sides canonicalized, so the `~/.t3/worktrees` symlink resolves; component-wise prefix, so `t3-abc` never matches `t3-abcd`) | Exactly one **running** thread (one with a run in progress) whose worktree, or project `workspace_root` when `worktreePath` is null, contains it. Nested worktrees match both, so they count as ambiguous |

Liveness for attribution is stricter than `T3Registry::liveness`. A thread must have a run in progress, because completed shared-isolation children never settle and stay "live" indefinitely (P15). The registry gains a read of `orchestration_v2_projection_provider_threads`, and its JSON snapshot loader (`RawT3Thread`) gains the same field, so fixtures model it.

### Verification order (`verified_caller`)

Every path goes through `SurfaceRegistry::verify_session`, never around it, so the node, environment, generation and cross-surface checks keep applying.

1. **Gather** the thread variables from the caller process, as in the table above.
2. **Mismatch check, first and for every path:** if a variable maps to a thread other than the claimed session, refuse `thread_mismatch`. That includes claims carrying a signed credential or a Direct token.
3. **Signed credential or Direct token present:** verify it as today. Nothing else changes.
4. **Strict mode** (`MAESTRO_IDENTITY=strict`) without a credential: refuse as today.
5. **Attributed mode** (the default): accept when either:
   - `thread_env` or `native_thread` matches the claim, and the thread is running; the cwd is *not* required, because agents' shells often `cd` elsewhere (P15). Or:
   - only `cwd_unique` holds for the claimed thread.
   Otherwise refuse `attribution_ambiguous`, naming the candidate count. Never fall back to choosing one.
   The `VerifiedSession` carries `Attribution::T3 { evidence }`.

The CLI's flow verbs need no new flag: the cwd comes from the process itself (P15 found the CLI sent none, which refused every CLI write).

### Stop gate

The hook derives a session, in order, from:

1. `T3CODE_THREAD_ID` in its environment (Claude hooks inherit it);
2. stdin `session_id` mapped through `nativeThreadRef` (Codex hooks, whose environment carries no thread variable);
3. a `cwd_unique` match on stdin `cwd`.

Candidates that disagree, a candidate thread that isn't running, or a T3 read error all mean **binding unavailable**. The stop goes through. The gate appends `StopUnverified {reason, candidates}` to every unfinished instance whose owner is one of the candidates, and files the fleet alert keyed by the workspace and the reason, so the alert has a subject even when no session was found. If the append fails, the stop still goes through and the alert names the failure. No candidate at all means **never bound**, and the stop goes through silently. With one derived session, the gate checks `flow open` as today.

### Audit facts

- The subject is **the process that made the claim**: the CLI process, or the `maestro mcp` server process for stdio MCP.
- Recorded: pid, start time and uid, its ppid chain up to 8 ancestors (`sysctl KERN_PROC_PID` on macOS, `/proc/<pid>/stat` on Linux), the thread-variable names present (never their values beyond the thread id), and the evidence list.
- A fact that can't be read is recorded as `null` with a reason, never omitted and never blocking.
- Recorded on `Started`, `Claimed`, `Completed`, `Refused`, `Chosen`, `Heartbeat` and every ask event. The spine ignores them.

## Alternatives considered

| Alternative | Why not | What we keep |
|---|---|---|
| Round-1 design: maestrod (`_maestro`) Ed25519 attestations bound through a T3 timeline nonce | STRUCTURAL in both strata. One Codex app-server serves all threads; Gemini records no tool inputs; CLI and subagents break process binding; T3's evidence is same-uid forgeable (maestro#43) | The probes, and the isolation arc as the real fix |
| Run agents under a separate OS uid now | The only design that stops a malicious same-uid agent, but it is cross-cutting (T3 fork, toolchains, credentials), and the owner made it its own arc | The `identity.mode` field it will set to a stronger value |
| Launcher (Direct) tokens only, with T3 threads read-only | Leaves content-flow and the orchestrators unable to drive flows from T3 threads, which is the point of "ready" | Direct tokens stay valid for launcher-started sessions |
| Same-user signing key (interim issuer) | A signature over forgeable evidence is no stronger than the evidence, and it adds key handling | Nothing |

## Acceptance

```bash
cargo test -p maestro-hosts -p maestro-mcp -p maestro-cli -p maestro-flow --locked
bash scripts/mutation-check.sh
```

Each test below fails when its guard is removed. The suites run in **both** modes with explicit expected results per mode: `strict` reproduces today's results exactly (the authority suites `crates/mcp/tests/{authority,dispatch}.rs` and `crates/cli/tests/requests.rs` keep their assertions), and `attributed` adds the following.

**Refused (0 of 12 accepted):**
1. Unknown thread.
2. Archived thread.
3. Thread with no run in progress.
4. `thread_env` naming another thread.
5. `CODEX_THREAD_ID` mapping to another thread.
6. A thread variable mismatching a claim that carries a valid signed credential.
7. The same with a Direct token.
8. cwd matching two running threads (shared worktree).
9. cwd matching nested worktrees.
10. cwd matching no thread.
11. A cwd-only claim whose cwd is only an alias for a *different* running thread.
12. Strict mode without a credential.

**Accepted (4 of 4):**
1. Matching `thread_env`, with the cwd outside the worktree. Evidence is `[thread_env]`.
2. Matching `native_thread` from the CLI.
3. A unique running cwd. Evidence is `[cwd_unique]`.
4. A cwd reached through the `~/.t3/worktrees` symlink. `t3-abc` versus `t3-abcd` is refused.

Every accepted event records `identity.mode = "t3-attributed"`, the evidence and the audit facts. An unreadable fact records `null` with a reason.

**Stop gate (6 of 6):**
1. Claude environment session with an open instance: refuses.
2. Codex stdin `session_id` with an open instance: refuses.
3. Disagreeing candidates: let through, with `StopUnverified` appended and the alert filed.
4. Candidate not running: let through, with `StopUnverified` appended.
5. No candidate: let through silently.
6. Failed append: let through, and the alert names the failure.

**Compatibility:**
- Paseo with a credential and Direct with a token pass as today.
- An unsigned Paseo claim is refused as today.

The T3 fixture database models the shared Codex app-server, multiple provider-thread rows and completed shared-isolation children.

**Live on the Mac** (receipts under `docs/conformance/`, each recording the session, cwd, evidence, event and outcome):
1. A Claude T3 thread runs `verify@1` to terminal through MCP.
2. A Codex T3 thread runs it through the CLI from its shell.
3. A Codex MCP write in a shared worktree is refused `attribution_ambiguous`.
4. A cross-thread claim is refused `thread_mismatch`.
5. Claude's Stop hook refuses its final message while its instance is open.
6. Codex's Stop hook does the same, through stdin `session_id`.

## Drawbacks and costs

- **Cost:** roughly 700–900 lines of Rust: the evidence gathering, the provider-thread reader, the stop-gate derivation, the audit facts and the dual-mode suites. Plus a richer T3 fixture database, and an ongoing dependency on T3's private schema (`orchestration_v2_projection_*`, `nativeThreadRef`). The reader is versioned and fails closed: a refusal, never a wrong identity.
- **Attribution is not authentication.** A malicious same-uid agent can still impersonate a thread. Every event says so, and flows that gate money or publication keep a human node until BRO-3847.
- **Codex and Gemini over MCP are refused in shared worktrees** until T3 passes `T3CODE_THREAD_ID` to MCP servers. Their sessions use the CLI meanwhile, and the generated driver skills say so.
- **Subagents write as their parent.**

## Open questions

1. Should the T3 fork change (pass `T3CODE_THREAD_ID` to every MCP server T3 starts, for Codex and Gemini) land before "ready", or after? Recommended default: after. Next step: the coordinator files it as its own ticket against broomva/t3code, linked from this spec.
