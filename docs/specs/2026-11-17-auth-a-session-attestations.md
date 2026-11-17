# Attributed session identity for flow writes

Status: proposed, round 2, 2026-11-17. Tracking: BRO-3841 (arc). BRO-3273's isolation and owner-token scope moves to its own arc.
Owner decision, 2026-11-17 (AskUserQuestion, after the round-1 review): **"Attributed identity now."**

**The round-1 design is withdrawn.** It used maestrod as `_maestro`, Ed25519 attestations and a T3 timeline nonce. Both review strata found it STRUCTURAL: Codex 3/10, Gemini 2/10, findings on maestro#43. The decisive fact is that T3, the only component that knows which thread is calling, runs as the owner's uid. Any evidence or key T3 could hold is therefore forgeable by a same-uid agent, so no design makes one thread unable to impersonate another while agents share the owner's uid. This spec names identity at the level that is actually achievable.

Probe receipts: [2026-11-17-auth-a-probes/probes.md](2026-11-17-auth-a-probes/probes.md), P1–P14.
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

1. A live thread's flow write succeeds when its claim verifies against T3 (exists, live, cwd within the worktree), from MCP and from the CLI. That holds on Claude and Codex now, and on Gemini through MCP arguments.
2. A claim naming another thread is refused unless the caller's cwd is within that thread's worktree. So are a settled, archived or unknown thread and a malformed id. Any explicit thread variable present must equal the claimed thread, with no fallback.
3. Every flow event written under attribution records `identity: {mode: "t3-attributed", evidence: [...]}`, plus the kernel peer facts (pid, process start time, uid and the ppid chain up to the T3 server) for audit.
4. The Stop gate derives the session without `MAESTRO_SESSION`. It distinguishes **never bound** (let the stop through) from **binding unavailable** (let it through, but append an event and file an alert, as `StopUnblockable` does today).
5. BRO-3828 can read the claimer's provider from T3's thread record, labelled as attributed.

## Non-goals

- **Stopping a deliberately malicious agent running as the owner's uid.** It can forge T3 state, edit the owner-writable ledger, or drive another process. That needs agents under a separate OS uid, which is the isolation arc (BRO-3273's successor). The hash-chained log makes such tampering visible, not impossible.
- **Signed credentials and maestrod.** They add no strength while all evidence is same-uid. The HMAC `BridgeCredential` path stays as it is for Paseo and the existing tests, and is neither extended nor removed here.
- **Telling a parent thread from its provider-native subagent.** They share a process and a worktree (P12). A subagent's writes are attributed to the parent thread, and the event says so (`evidence` lacks a thread variable match). The kernel's per-session claim tokens still apply.

## Design

### Verification order for a T3 claim (`verified_caller`)

1. **Signed credential.** If a credential is presented, verify it as today; nothing changes.
2. **Attribution.** Otherwise, if `MAESTRO_IDENTITY` is not `strict` (the default is `attributed`, set in the MCP server's config, never an argument), call `T3Registry::verify` with the claimed thread and the caller's cwd.
   - On success the `VerifiedSession` carries `Attribution::T3Registry { evidence }`. `evidence` lists what corroborated the claim: `cwd_within_worktree` always; `thread_env` when the caller process (CLI) or the MCP server's own environment holds `T3CODE_THREAD_ID` equal to the claim; `codex_native` when `CODEX_THREAD_ID` maps to the claimed thread.
   - **A present but mismatching thread variable is a refusal** (`thread_mismatch`), never a fallback.
3. **Strict mode** keeps today's behaviour: refuse without a credential. The conformance runner and the authority tests run strict and attributed both.

The CLI uses the same order with its own process environment. A CLI verb run from a thread's shell therefore carries `thread_env` (Claude) or `codex_native` (Codex) evidence without extra arguments. On a Gemini shell it has `cwd_within_worktree` only.

### Stop gate session derivation

`maestro flow stop-gate` derives a T3 session, in order:

1. `T3CODE_THREAD_ID` from the hook's environment;
2. `CODEX_THREAD_ID`, mapped through T3;
3. the single live thread whose worktree contains the hook's cwd.

An ambiguous result (more than one live thread for the cwd) or a T3 read error is **binding unavailable**. The gate lets the stop through, appends an event to every open instance in that cwd's workspace, and files the fleet alert. No candidate at all means **never bound**: the stop goes through silently. With a derived session, the gate checks `flow open` as today.

### Audit facts

The writer appends the identity mode, the evidence list and the peer facts to `Claimed`, `Completed`, `Chosen` and `Started`. They are gathered with `sysctl KERN_PROC_PID` (macOS) or `/proc/<pid>/stat` (Linux), up to 8 ancestors. The spine ignores them (as it already ignores performers), so conformance comparisons don't change.

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

Mutation-checked tests (each fails when its guard is removed). The pass bar is 7 of 7 refusal cases refused, 2 of 2 acceptance cases accepted, and 3 of 3 stop-gate cases correct:

- **Refusals:**
  - an unknown thread;
  - a settled thread;
  - an archived thread;
  - a caller cwd outside the claimed worktree;
  - a `T3CODE_THREAD_ID` naming another thread;
  - a `CODEX_THREAD_ID` mapping to another thread;
  - strict mode without a credential.
- **Acceptances:** cwd within the worktree with no variable (evidence `[cwd_within_worktree]`), and with a matching variable (evidence includes `thread_env`). Each event records `identity.mode = "t3-attributed"`.
- **Stop gate:** never bound lets the stop through; binding unavailable lets it through and appends the event; a derived session with an open, workable instance refuses the stop.
- **Fixture coverage:** a T3 fixture database models the shared Codex app-server and its `nativeThreadRef` mapping.

Live on the Mac, each kept as a receipt under `docs/conformance/`:

1. A Claude T3 thread runs `verify@1` to terminal through MCP.
2. A Codex T3 thread runs `verify@1` to terminal through the CLI from its shell.
3. A Gemini T3 thread passes `maestro_flow_start` through MCP.
4. A claim of another thread from a different worktree is refused (`thread_mismatch` or `cwd_outside`).
5. The Claude thread's Stop hook refuses its final message while its instance is open.

## Drawbacks and costs

- **Cost:** about 400–600 lines of Rust (verifier mode, evidence, stop-gate derivation, audit facts) plus a T3 fixture database, one PR with two review strata, and an ongoing dependency on T3's private schema.

- **Attribution is not authentication.** A malicious same-uid agent can still impersonate a thread or edit the ledger. The label makes that explicit in every event, and flows that gate money or publication keep a human node until the isolation arc lands.
- **Subagents write as their parent.** The kernel's claim tokens and leases still serialize the work.
- **Dependence on T3's schema** (`orchestration_v2_projection_*`, `nativeThreadRef`). The reader is versioned and fails closed: a refusal, never a wrong identity.

## Open questions

1. Should a Gemini thread's CLI writes, with `cwd_within_worktree` as the only evidence, be allowed, or require MCP? The recommended default is to allow them, with the weaker evidence recorded. Next step: the owner answers this in the arc's asks ledger before the implementation PR merges; until then the implementation ships the default.
