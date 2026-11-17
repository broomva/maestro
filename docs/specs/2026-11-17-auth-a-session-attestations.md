# Attributed session identity for flow writes: by thread variable

Status: proposed, round 5, 2026-11-17. Tracking: BRO-3846, arc BRO-3841. Isolation is BRO-3847, and T3 passing the thread id to MCP servers is BRO-3848.
Owner decisions (AskUserQuestion, 2026-11-17): "Attributed identity now", after round 1. "Thread variable only", after round 3.
Reversal cost: low. Attribution is a verifier mode plus a field on events; the isolation arc replaces the mode without migrating any ledger.
Probe receipts: [2026-11-17-auth-a-probes/probes.md](2026-11-17-auth-a-probes/probes.md), P1–P16. Review history is on maestro#43.

## How we got here

| Round | Design | Verdicts | What it taught |
|---|---|---|---|
| 1 | maestrod as `_maestro`, Ed25519 attestations, T3 timeline nonce | Codex 3, Gemini 2: STRUCTURAL | T3 runs as the owner's uid, so no thread identity can be *authenticated* while agents share it |
| 2 | T3-registry check, cwd within the worktree | Codex 4, Gemini 5 | 74.5% of live threads share a worktree; cwd can't tell them apart |
| 3 | Thread variable first, cwd when unique among running threads | Codex 4, Gemini 3: STRUCTURAL | Zombie `running` rows and overlapping parent/child runs make "unique running cwd" unusable |
| 4 | Thread variable only. The cwd is subtracted | Codex 6/10 `continue` (Gemini's run returned no result) | The promise needed a precise matrix; strict mode, the bridge-secret guard and alias owners needed stating |
| 5 | **This one: round 4 plus the decision matrix and the precise integration points** | — | — |

Every round-2 and round-3 finding landed on cwd attribution, so round 4 removes it instead of hardening it.

## Objective

Let a live T3 thread write to the flow ledger when the process making the write carries a thread variable that T3 maps to a thread. Refuse it otherwise. Record on every event which variable the writer carried.

## What this promises, and what it doesn't

- **Promised, for the unsigned T3 path:** an unsigned T3 write is accepted only if the writing process carries a thread variable (below) that resolves to exactly one existing, non-archived, non-deleted T3 thread, and that thread equals the claimed session (when one is named). The event records the variable, the thread, and audit facts about the process. Credentialed writes (signed bridge credential, Direct token) keep today's rules, plus one addition in attributed mode: a thread variable present in the caller that contradicts the claim is refused. The matrix below is normative.
- **Not promised: that the variable belongs to the work being done.** A long-lived process (a shell, a `make` job) that inherited thread A's variable and later works for thread B writes as A. That is a mis-attribution with no forgery involved. It is a limit of the evidence, not a defect to guard against. The audit facts (the process chain) give clues for investigating such cases afterwards. They don't reveal every one: a reused process can have an ordinary-looking ancestry.
- **Not promised: anything against a deliberately malicious same-uid agent.** That is BRO-3847.

## Context (probes)

- **Variables per harness** (P10–P16):

  | Harness | MCP server | Shells and CLI | Stop hooks |
  |---|---|---|---|
  | Claude | `T3CODE_THREAD_ID` | `T3CODE_THREAD_ID` | `T3CODE_THREAD_ID` |
  | Codex (one shared app-server) | none | `CODEX_THREAD_ID` | stdin `session_id` |
  | Gemini | none | none | — |

  `CODEX_THREAD_ID` and a hook's `session_id` resolve through `orchestration_v2_projection_provider_threads.payload_json.$.nativeThreadRef.nativeId`, which has 0 collisions across 2,155 ids. 66 threads have more than one provider-thread row after provider switches.
- **Existing defects found in review (P16), fixed here:**
  1. `is_safe_t3_id` rejects 870 of 1,107 live thread ids (78.6%), every delegated thread among them. T3 percent-encodes composite ids (`%3A`) and some run 129–206 bytes, past the 128-byte cap.
  2. `SurfaceRegistry::from_env` reads only `MAESTRO_T3_HOME`, which nothing sets. T3 sets `T3CODE_HOME`. So out of the box no T3 claim can verify.
  3. `session` is a required CLI and MCP argument, while no T3 process sets `MAESTRO_SESSION`.
  4. The session's environment part (`t3:local/…`, `t3:-/…`, `t3:<uuid>/…`) isn't canonicalized against the registry's.

## Goals

1. **Claude** threads write over MCP and the CLI. **Codex** threads write through the CLI from their shell. Neither needs setup or a `--session` argument.
2. Refusals, each with its own code:
   - no thread variable (`attribution_missing`);
   - a variable that maps to no thread, more than one thread, or an archived or deleted thread (`attribution_unresolved`);
   - a variable whose thread differs from the claimed session (`thread_mismatch`);
   - `T3CODE_THREAD_ID` and `CODEX_THREAD_ID` that disagree (`thread_mismatch`).
3. Events carry `identity: {mode: "t3-attributed", variable, thread}` and the audit facts.
4. The Stop gate binds through the same variables and refuses a final message while that thread owns workable flow work.
5. `MAESTRO_IDENTITY=strict` adds no new path and no new check: every claim that verifies today verifies, and every refusal stays. The only exception is two bug fixes (thread-id charset and registry root, below), which apply in both modes because they correct defects. They are approved here and get strict-mode tests of their own. Paseo and Direct are unchanged in strict mode. In attributed mode they gain only the contradiction check.

## Non-goals

- **Codex and Gemini over MCP.** Their MCP servers carry no variable, so they are refused `attribution_missing` until BRO-3848 lands. Codex uses the CLI meanwhile; Gemini can't write flows until then. The generated driver skills say so.
- **Any cwd-based attribution.** Removed.
- **Telling a parent from its provider-native subagent.** They share a process and an environment, so the subagent writes as its parent.
- **Malicious same-uid agents.** BRO-3847.

## Design

### Decision matrix (normative)

| Mode | Transport | Credential | T3 thread variable | Outcome |
|---|---|---|---|---|
| strict | any | valid signed credential or Direct token | any (ignored) | today's result |
| strict | any | none | any (ignored) | today's result: refused |
| attributed | stdio MCP or CLI | valid credential or Direct token | absent | today's result |
| attributed | stdio MCP or CLI | valid credential or Direct token | resolves to the claimed thread | today's result |
| attributed | stdio MCP or CLI | valid credential or Direct token | resolves to another thread, or doesn't resolve | refused `thread_mismatch` / `attribution_unresolved` |
| attributed | stdio MCP or CLI | none, T3 session | resolves to the claimed thread, or no session named | **accepted, attributed** |
| attributed | stdio MCP or CLI | none, T3 session | absent | refused `attribution_missing` |
| attributed | stdio MCP or CLI | none, T3 session | resolves to another thread | refused `thread_mismatch` |
| attributed | stdio MCP or CLI | none, Paseo or Direct surface | any | today's result: refused |
| attributed | HTTP daemon | valid credential | n/a (no caller environment) | today's result |
| attributed | HTTP daemon | none | n/a | refused `attribution_missing` |

Precedence, in order:
1. A registry read error is `registry_unavailable`.
2. Then `attribution_unresolved`.
3. Then `thread_mismatch`.
4. Then the credential or attribution result.

Each cell is a table-driven test.

### Variables and resolution

The variables are read from **the environment of the process making the claim**: the CLI process itself, or the `maestro mcp` server process for stdio MCP. They are never read from tool arguments. Over the HTTP daemon, which serves every caller, there is no caller environment, so attribution doesn't apply there (`attribution_missing`).

| Variable | Resolves by |
|---|---|
| `T3CODE_THREAD_ID` | Equality with a row in `orchestration_v2_projection_threads` |
| `CODEX_THREAD_ID`, or a Stop hook's stdin `session_id` | `nativeThreadRef.nativeId` in the provider-thread row that is the thread's **active** one (`orchestration_v2_projection_threads.active_provider_thread_id`), so a retired provider row never attributes |

- The resolved thread must exist and be neither archived nor deleted. No run-state check is needed: the variable, not a cwd, picks the thread, so the zombie-run problem doesn't arise.
- When both variables are present they must resolve to the same thread.
- Any registry read error refuses `registry_unavailable`; it never falls back.

### Fixes to existing code

1. `is_safe_t3_id` allows `%` and up to 512 bytes. It still requires a leading alphanumeric and refuses `/`, `..` and NUL. Thread ids are **opaque**: never percent-decoded, and used only as SQL parameters and in string comparisons, never as a path component. So `%2F` is safe. It is mutation-checked against the percent-encoded ids the probes found, with boundary tests at 512 and 513 bytes and for `%2F`, `/`, `..` and NUL.
2. The T3 registry root resolves from `MAESTRO_T3_HOME`, then `T3CODE_HOME`, then `~/.t3` when it exists.
3. **Owner aliases.** No flow ledger instance exists today on the Mac or gcp-1 (checked 2026-11-17: 0 instances; conformance used throwaway sandboxes), so there are no stored owners to migrate. New owners are stored in canonical form. Owner comparison canonicalizes *both* the stored and the presented session (`local`, `-` and a uuid all become this host's registry environment id), so a fixture or future alias still matches its owner.
4. `session` becomes optional on every flow write (CLI `--session`, MCP `session`). In attributed mode, when it is omitted, the verifier derives the session from the resolved thread. When it is given, its environment part is canonicalized to the registry's environment id before comparison; `local` and `-` mean "this host's T3", and any other mismatch is refused `thread_mismatch`. Downstream code, `stop.rs` included, compares only canonical sessions.

### Verification order (`verified_caller` → `SurfaceRegistry::verify_session`)

1. **Strict mode** (`MAESTRO_IDENTITY=strict`): exactly today's code path.
2. **Attributed mode** (the default):
   - (a) Gather the variables from the claiming process.
   - (b) If any is present, resolve it and check it against the claimed session: a mismatch is refused, *including* when a signed credential or Direct token is also presented. Attributed mode adds this mismatch check to the credential path; strict mode does not.
   - (c) A valid signed credential or Direct token verifies as today.
   - (d) Otherwise, a resolved T3 variable verifies a T3 session through a new `T3Registry::verify_attributed(thread_id)`. It is called from `verify_session`, so the node, environment and generation checks and the cross-surface refusal still apply. It requires no cwd containment; `T3Registry::verify`, used with credentials, keeps its cwd rule unchanged.
     - **Bridge-secret guard.** The guard in `verify_session` that refuses an unsigned claim when a bridge secret is configured is unchanged for every claim, *except* a T3 claim carrying `CallerClaim.attribution = Some(resolved)`. That field is set only by `verified_caller` from the claiming process's environment, never deserialized from arguments. Credential verification is untouched.
     - **Owner marker.** `verify_attributed` sets the same T3 owner marker `T3Registry::verify` sets (thread id + `created_at`), so `check_owner` compares exactly as today. A thread without `created_at` yields no marker, so writes are refused `no_owner_marker`, as today.
     - **Tests:** a configured bridge secret, wrong node, stale generation, missing incarnation, and a cross-surface claim, each with an attributed T3 claim.
   - (e) Otherwise refuse `attribution_missing`.

### Stop gate

The gate binds through `T3CODE_THREAD_ID` from its environment, or `session_id` from its stdin (Codex), resolved as above.

| Outcome | Behaviour |
|---|---|
| One thread | Refuse the final message while that thread owns an unfinished, workable instance (`flow open`) |
| No variable | **Never bound**: the stop goes through silently |
| Variables that disagree, don't resolve, or hit a read error | **Binding unavailable**: the stop goes through, `StopUnverified {reason}` is appended to every unfinished instance owned by any candidate thread, and the fleet alert is filed keyed by `(workspace, reason)`. A failed append still lets the stop through, and the alert names the failure |

### Audit facts

- The subject is the claiming process.
- Recorded:
  - pid, start time and uid;
  - the ppid chain, up to 8 ancestors, with each ancestor's command name;
  - the names of the thread variables present;
  - `identity.variable` and `identity.thread`.
- An unreadable fact is recorded as `null` with a reason.
- Recorded on `Started`, `Claimed`, `Completed`, `Refused`, `Chosen`, `Heartbeat`, `AskPinned`, `AskAnswered`, `Adopted`, `Retired` and `StopUnverified`.
- The spine ignores them.

## Alternatives considered

| Alternative | Rejected because | What we keep |
|---|---|---|
| cwd attribution, in any form (rounds 2–3) | Rejected because 74.5% of live threads share a worktree. Zombie `running` rows and overlapping parent/child runs (7,164 pairs) make every cwd rule ambiguous | Nothing |
| maestrod with Ed25519 attestations (round 1) | Rejected because same-uid evidence makes signing meaningless; T3 runs as the owner | The isolation arc, BRO-3847 |
| Launcher (Direct) tokens only | Rejected because it leaves T3 threads, and so content-flow and the orchestrators, unable to write flows | Direct stays valid |

## Acceptance

```bash
cargo test -p maestro-hosts -p maestro-mcp -p maestro-cli -p maestro-flow --locked
bash scripts/mutation-check.sh
```

Each test fails when its guard is removed. Suites run in both modes.

- **Strict mode:** the existing authority suites (`crates/mcp/tests/{authority,dispatch}.rs`, `crates/cli/tests/requests.rs`) pass unchanged. New strict tests cover the two approved fixes, a percent-encoded long id and a registry found through `T3CODE_HOME`, and show that a thread variable is ignored in strict mode.
- **Attributed mode, refused (0 of 15 accepted):**
  1. No variable.
  2. Unknown `T3CODE_THREAD_ID`.
  3. Archived thread.
  4. Variable thread ≠ claimed session.
  5. `T3CODE_THREAD_ID` ≠ `CODEX_THREAD_ID` thread.
  6. A `CODEX_THREAD_ID` that matches only a retired (non-active) provider row.
  7. A variable mismatch alongside a valid signed credential.
  8. The same alongside a Direct token.
  9. Registry unreadable.
  10. HTTP daemon caller without a credential.
  11. A deleted thread.
  12. Duplicate active-native-id rows (refused `attribution_unresolved`).
  13. Session `t3:<other-uuid>/<id>` (refused `thread_mismatch`).
  14. Ids at 513 bytes, or containing `/`, `..` or NUL.
  15. An unresolved variable while the registry read fails (`registry_unavailable` wins).
- **Attributed mode, accepted (8 of 8):**
  1. `T3CODE_THREAD_ID` with no `session` argument; the session is derived.
  2. The same with `session: t3:local/<id>`, canonicalized.
  3. `CODEX_THREAD_ID` through the active provider row.
  4. A percent-encoded, 129–206-byte delegated thread id.
  5. A registry found through `T3CODE_HOME` with `MAESTRO_T3_HOME` unset.
  6. `session: t3:-/<id>` and `t3:<registry-uuid>/<id>`, canonicalized to the same owner.
  7. A 512-byte id and a `%2F` id, kept opaque.
  8. A pre-existing instance owned as `t3:local/<id>`, written to with `t3:<uuid>/<id>` and gated by the Stop hook.
- **Stop gate (6 of 6):**
  1. Claude variable with open work: refuses.
  2. Codex stdin `session_id` with open work: refuses.
  3. No variable: lets the stop through silently.
  4. Disagreeing variables: lets it through, with `StopUnverified` appended and the alert filed.
  5. Read error: same as 4.
  6. Failed append: lets it through, and the alert names the failure.
- **Every event type listed above** carries `identity` and the audit facts, and an unreadable fact is `null` with a reason.

Live on the Mac (receipts under `docs/conformance/`, each recording the variable, thread, event and outcome):
1. A Claude T3 thread runs `verify@1` to terminal over MCP.
2. A Codex T3 thread runs it through the CLI from its shell.
3. A Codex MCP write is refused `attribution_missing`.
4. A cross-thread claim is refused `thread_mismatch`.
5. Claude's Stop hook refuses while an instance is open.
6. Codex's Stop hook does the same through stdin `session_id`.

## Drawbacks and costs

- **Cost:** about 500–700 lines of Rust (variable resolution, the three fixes, optional session, Stop-gate binding, audit facts, dual-mode suites) and a T3 fixture database with provider rows. Plus the dependency on T3's private schema; its reader is versioned and fails closed.
- **Inherited variables can mis-attribute,** as stated above. It's visible in the audit chain, not prevented.
- **Gemini can't write flows,** and Codex only through the CLI, until BRO-3848.

## Open questions

1. Should a write whose audit chain shows the writer is *not* a descendant of a T3-spawned harness (a detached long-lived process) be refused rather than recorded? Recommended default: record only. Next step: measure how often that happens in the first week of live use, then decide in the arc's asks ledger.
