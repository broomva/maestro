# Attributed session identity for flow writes: by thread variable

Status: proposed, round 6, 2026-11-17, implemented in the same PR (BRO-3846). From round 6 on, this spec is reviewed together with its implementation and the table-driven tests, not as prose alone. Where the spec was silent or wrong, the implementation's decision is stated in "Implementation decisions" below. Tracking: BRO-3846, arc BRO-3841. Isolation is BRO-3847, and T3 passing the thread id to MCP servers is BRO-3848.
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
| 5 | Decision matrix and integration points | Codex 5/10 `continue` (prediction confirmed and resolved) | A presented invalid credential must never fall back to attribution; missing unresolved and cross-surface rows |
| 5b | (Gemini on round 5) | Gemini 7.8/10 PASS, `converged` | Existing-code integration points: claimless requests, alert filing, test hermeticity, the T3 reader's wrong table |
| 6 | **This one: credential-first branching, the full matrix, qualified wording, and Gemini's integration fixes** | — | — |

Every round-2 and round-3 finding landed on cwd attribution, so round 4 removes it instead of hardening it.

**Review ledger exception.** Joint rounds 1–8 on the implementation are preserved as they are on maestro#43. Round 8 (Gemini 9.4 PASS `converged`, Codex 6 FAIL) was past the review budget. The owner approved one round beyond it, round 9, to fold BRO-3863 (pending permission records that do not decode, decision 17) into this PR. Round 9 is single: a failure stops the PR for the owner, with no override. Round 9 failed on its minimum (Codex 6, Gemini 9.7). The owner kept the behaviour it questioned (a payload that names no tool is a prompt named by its kind, decision 17) and approved one round 10 after round 9, for that wording-and-test pin and the guard's two refinements. After round 10, the owner approved one round 11 to move the permission rule out of the embedded Python reader into Rust, one rule for every surface (decision 17). Rounds 1–10 stay as recorded.

## Objective

Let a live T3 thread write to the flow ledger *without a credential* when the process making the write carries a thread variable that T3 maps to a thread, and refuse such an unsigned write otherwise. Credentialed writes follow the matrix below. Record on every event which variable the writer carried.

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

- **Unsigned Codex and Gemini writes over MCP.** Their MCP servers carry no variable, so an unsigned write from them is refused `attribution_missing` until BRO-3848 lands. A credentialed write follows the matrix. Codex uses the CLI meanwhile; Gemini can't write flows until then. The generated driver skills say so.
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
| attributed | stdio MCP or CLI | **presented but invalid** credential or token (bad signature, expired, stale generation, wrong node) | any | **the credential verifier's refusal, unchanged; never falls back to attribution** |
| attributed | stdio MCP or CLI | none, T3 session | present but unresolved, or two variables with one unresolved | refused `attribution_unresolved` |
| attributed | stdio MCP or CLI | valid Paseo or Direct credential | T3 variable present (resolved or not) | refused `thread_mismatch`: a T3 thread variable can never equal a Paseo or Direct session, so its presence contradicts the claim |
| strict or attributed | HTTP daemon, or strict mode | any | any | no registry read happens for attribution, so an unrelated registry failure can't affect these requests |

**Credential first.** When a credential or token is *presented*, the existing credential verifier runs in full and its result is final (after the attributed-mode contradiction check). Attribution is considered only when *no* credential or token is presented. Registry reads for attribution happen only on the unsigned T3 path and in the contradiction check. A present but invalid credential with a matching variable is a required test, with and without a configured bridge secret. So are expired and stale-generation credentials.

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
5. **Claimless agent requests stay as today.** `resolve_caller` (`Authority::AgentRequest`: `maestro_ask_file`, `handback`, `work_queue`, …) keeps returning `Principal::Unverified` for a claim with no claim metadata, even once the registry root is found through `T3CODE_HOME`. Only flow writes (`Authority::AgentWrite`) require attribution. This keeps `crates/mcp/tests/authority.rs` (e.g. `a_filed_ask_with_a_busy_journal_lock_has_no_busy_flag`) and claimless production requests unchanged.
6. **Thread variables are read only for T3 claims, or when the session is omitted, and only through the registry.** The variables are captured once by `SurfaceRegistry::from_env` and carried in the registry. They are not read with ad-hoc `std::env::var` in `verified_caller`, so a test that passes its own `SurfaceRegistry` (`call_with_hosts`) is hermetic even when `cargo test` runs inside a T3 shell. A Paseo or Direct claim never consults them, except for the attributed-mode contradiction check, which runs only when the registry carries a variable.
7. **Liveness for attribution is `archived_at IS NULL AND deleted_at IS NULL`.** `settled_at` is ignored, because 71% of live threads are settled. `T3Registry::verify`'s own liveness rule, used with credentials, is unchanged. (Correction, found in implementation: `SQLITE_READER_PY` has read the latest row of `orchestration_v2_projection_runs`, `ORDER BY ordinal DESC LIMIT 1`, since BRO-3268; it never queried `orchestration_v2_projection_turns`. Nothing needed fixing; a test now pins the latest-run read.)
8. `session` becomes optional on every flow write and on `flow open` (CLI `--session`, MCP `session`). In attributed mode, when it is omitted, the verifier derives the session from the resolved thread. When it is given, its environment part is canonicalized to the registry's environment id before comparison; `local` and `-` mean "this host's T3", and any other mismatch is refused `thread_mismatch`. Downstream code, `stop.rs` included, compares only canonical sessions.

### Verification order (`verified_caller` → `SurfaceRegistry::verify_session`)

1. **Strict mode** (`MAESTRO_IDENTITY=strict`): exactly today's code path.
2. **Attributed mode** (the default):
   - (a) Gather the variables from the claiming process.
   - (b) If any is present, resolve it and check it against the claimed session: a mismatch is refused, *including* when a signed credential or Direct token is also presented. Attributed mode adds this mismatch check to the credential path; strict mode does not.
   - (c) A *presented* signed credential or Direct token runs the existing verifier in full, and its result (accept or refuse) is final. There is no fallback to attribution.
   - (d) Otherwise, a resolved T3 variable verifies a T3 session through a new `T3Registry::verify_attributed(thread_id)`. It is called from `verify_session`, so the node, environment and generation checks and the cross-surface refusal still apply. It requires no cwd containment; `T3Registry::verify`, used with credentials, keeps its cwd rule unchanged.
     - **MCP pre-check.** `verified_caller` in `crates/mcp/src/lib.rs` currently refuses an unsigned non-Direct claim before reaching the registry (the `authenticated bridge credential or launch token is required` branch). That pre-check gains the same narrow exception: a T3 claim with a resolved environment attribution. Without it, the host-level change is unreachable.
     - **Bridge-secret guard.** The guard in `verify_session` that refuses an unsigned claim when a bridge secret is configured is unchanged for every claim, *except* a T3 claim carrying `CallerClaim.attribution = Some(resolved)`. That field is set only by `verified_caller` from the claiming process's environment, never deserialized from arguments. Credential verification is untouched.
     - **Owner marker.** `verify_attributed` sets the same T3 owner marker `T3Registry::verify` sets (thread id + `created_at`), so `check_owner`'s marker comparison is exactly as today. Its session comparison changes to compare canonical forms (owner aliases, above), which is the one deliberate change to `check_owner`. A thread without `created_at` yields no marker, so writes are refused `no_owner_marker`, as today.
     - **Tests:** a configured bridge secret, wrong node, stale generation, missing incarnation, and a cross-surface claim, each with an attributed T3 claim.
   - (e) Otherwise refuse `attribution_missing`.

### Stop gate

The gate binds through `T3CODE_THREAD_ID` from its environment, or `session_id` from its stdin (Codex), resolved as above.

| Outcome | Behaviour |
|---|---|
| One thread | Refuse the final message while that thread owns an unfinished, workable instance (`flow open`) |
| No variable | **Never bound**: the stop goes through silently |
| Variables that disagree, don't resolve, or hit a read error | **Binding unavailable**: the stop goes through, `StopUnverified {reason}` is appended to every unfinished instance owned by any candidate thread, and the fleet alert is filed keyed by `(workspace, reason)`. The alert is filed through `requests::file_ask` as `Principal::Unverified`, with the first candidate session or `t3:-/unverified`, never through `verified_caller`, which would fail with the same error. A failed append still lets the stop through, and the alert names the failure |

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

## Implementation decisions

Taken in BRO-3846 where the text above was silent or wrong. Each has a test.

1. **Found is not configured.** The T3 root found through `T3CODE_HOME` or `~/.t3` makes T3 a surface for verification, but not a *configured* one: a claimless agent request, or a claimless strict-mode write, from a host with only a found root stays unverified, exactly as before the root was found. An explicit `MAESTRO_T3_HOME` keeps today's refusal (`SurfaceRegistry::has_configured_surface`).
2. **Paseo or Direct beside a T3 variable reads nothing.** The matrix row "valid Paseo or Direct credential, T3 variable present (resolved or not)" is refused `thread_mismatch` on the variable's presence alone, with no registry read. No read means no read error, so the precedence ladder does not reach it.
3. **The HTTP row is about T3.** "HTTP daemon, none, `attribution_missing`" applies to an unsigned T3 claim and to a write that names no session. An unsigned Paseo or Direct claim over HTTP keeps today's result.
4. **A credential with no session named.** In attributed mode the session is derived from the resolved thread variable and the credential verifier then runs for it, in full. With no variable it is `attribution_missing`. In strict mode a write that names no session is refused `missing_session_id`.
5. **`MAESTRO_IDENTITY` values.** Unset, empty or `attributed` is attributed; `strict` is strict; any other value is strict, the mode that adds no path, so a typo never widens what is accepted.
6. **Codes.** A flow write refused for one of the four new reasons returns that reason as its `code`; every other identity refusal stays `caller_refused` with the reason in `identity`.
7. **The canonical environment** is the resolved thread's own registry environment: `userdata/environment-id` for `statev2.sqlite`; for a JSON snapshot, the row's `environment_id`, or else the `environment-id` file beside the snapshot (a row with none takes the snapshot's).
8. **Canonical owner comparison has no mode.** The kernel compares owners canonically in both modes (`write::same_owner`, used by `check_owner` and `stop::open`). `local` and `-` resolve to the presented verified session's environment, which is always this host's registry's. The marker must still match, and a non-alias environment never matches another. Proposals (`adoption`) still compare exactly: both sides there are always verified, so always canonical.
9. **`verify_attributed(thread_id, node)`** takes the claim's node too, so the thread's node is checked as `verify` checks it (one shared `check_node`).
10. **Ids.** The charset has no `/` and no NUL, so `..` is the only sequence refused by its own check.
11. **Audit facts.** `identity.mode` is `t3-attributed`, `bridge-credential`, `direct-token`, or `unbound` (on `StopUnverified`). `identity.variable` and `identity.thread` are null when no variable resolved (strict mode, or a credential with no variable). `process.variables` names the thread variables present in every mode: names only, never used as proof. An unread fact is `null`, and `process.unread` maps the fact to its reason. Process facts come from `/proc` on Linux and `ps(1)` elsewhere: the workspace forbids `unsafe`, so macOS reads `kinfo_proc` through `ps`, which calls `sysctl` itself. A `ProcessReader` trait lets tests inject them. "AskAnswered" above is the `Answered` event.
12. **The Stop gate.** stdin `session_id` binds only under `--harness codex`; Claude binds through `T3CODE_THREAD_ID` alone. The candidates of an unavailable binding are the threads that resolved (none on a read error). `StopUnverified` goes only on a candidate's unfinished instances (not terminal, not retired), takes no owner check (no owner is known; the log's host, epoch, sequence and clock checks still apply), and is not written again while the instance has not moved. The alert is keyed by the workspace, the reason's code and the UTC day, as the gate's other fail-open alerts are, so a reason that recurs on a later day alerts again; failed records are added to the key and named in the alert.
14. **Joint review (Codex, at `15b53d8`).** Four fixes:
    - **Named sessions are bound too.** A named T3 session (`--session`, `MAESTRO_SESSION`) is bound through the hook's thread variables, a Codex hook's stdin `session_id` included, and their thread must be the canonical named session. Beside a named Paseo or Direct session, the stdin `session_id` is Codex's own id, which a Direct `codex exec` has too: it contradicts the session (`thread_mismatch`) only once it resolves to a T3 thread. The stdin id counts only on a host with a T3 registry. A named session that presents a credential is judged by it alone, so a bad credential still fails open as a credential refusal.
    - **Every attribution or registry failure at the gate is a binding that is unavailable.** That covers a verification that fails after resolution, a named session's attribution refusal, and a T3 registry failure. The gate appends `StopUnverified` on the known candidates (the resolved threads and the named T3 session) and files the alert as `Principal::Unverified`.
    - **Every Stop-gate alert files as an unverified caller** (Gemini's joint review). `fail_open`, `report_unread`, `unblockable` and `unbound` all file through the asks ledger's writer of record as `Principal::Unverified`, never through a caller check. An alert usually reports a claim that did not verify, which a caller check would refuse again and drop.
    - **A partial read is unavailable.** Resolving a native id reads every active provider row. A missing table, an active pointer with no row of its thread, or a payload or `nativeThreadRef` that does not parse is `registry_unavailable`. A row with no `nativeThreadRef` at all has no native id yet.
    - **Attribution needs no folder.** The attribution lookup takes a live thread with no `worktreePath` and no project root, with an empty `cwd`. The credentialed reader keeps its cwd rule.
15. **Joint round 2 (Gemini, at `fc6dcfa`).** Five fixes:
    - **Credential first, even when malformed.** A presented credential that does not parse (the `credential` argument or `MAESTRO_BRIDGE_CREDENTIAL`) is presented all the same. Its parse error refuses the caller, at a flow write and at the Stop gate, named or not, and the thread variable never attributes it.
    - **A named Paseo or Direct session inside a T3 thread is unbound.** On a host with a T3 registry, a hook variable that resolves to a T3 thread (`T3CODE_THREAD_ID`, `CODEX_THREAD_ID`, a Codex hook's stdin `session_id`) names a thread the session can never be. That makes the binding unavailable, with that thread a candidate, whether the session is unsigned or presents its token. A variable that does not read is `registry_unavailable`; one that names no thread is Codex's own id and binds nothing.
    - **No silent `{}` for attribution.** The attribution reader refuses a thread payload that does not parse or is not an object (`registry_unavailable`), because it carries `archivedAt`, `deletedAt` and `node`. A snapshot row's `cwd` defaults to none only when missing or null; any other non-string is malformed. (Joint round 3, Codex.) A NULL, empty or whitespace-only payload is no payload either. The attribution reader and the native provider scan refuse it exactly as a corrupt one: `registry_unavailable` (the exit statuses are below). (Joint round 4, Codex: revised.) The credentialed `get` reader fails closed the same way.

Round 3 kept its `{}` fallback on the reasoning that `{}` has no folder, so the thread would simply not be found. Codex showed that reasoning wrong. The project root still gives the thread a folder, so the thread *was* found: a valid signed credential stayed accepted after its thread's payload was emptied. The payload's own `archivedAt` and `deletedAt` were lost too, so a thread archived after its credential was minted still verified as live.

So `SQLITE_READER_PY` now has one payload parser (`payload_of`), used for every thread and provider-row payload in every mode: `get`, `attr` and `native`. NULL, empty, blank, non-JSON or non-object payloads raise `PayloadError`, which exits 4. A database that does not read exits 2. The readers map them differently (joint round 5, Codex: corrected):
- **Credentialed lookup** (`get`, under `T3Registry::verify`) and `verify_attributed`'s own read: exit 2 is `registry_disconnected`, exit 4 is `registry_unavailable`.
- **Resolution of a thread variable** (`resolve`, through `attr` or `native`): every failure, exit 2 or 4, is `registry_unavailable`. A variable that cannot be resolved is never a disconnected registry to the caller check.

The JSON snapshot follows the same rule through one helper (`snapshot_rows`). A file that does not read or parse is `registry_disconnected`. A row that is not an object, has a non-string `cwd`, or does not deserialize is `registry_unavailable`.

Three deliberate exceptions:
- The `list` mode feeds permission prompts, not identity, so it fails open with a signal (joint round 5, Codex):
  - A thread whose payload does not read is still listed from what is known: its columns, and its pending prompts, which are stored apart from the payload. Its payload fields are unknown, and its prompts are never dropped.
  - A snapshot row that does not read is left out alone, never every row.
  - Each of these, and a registry that does not read at all, is named by `T3Registry::list_live_report` and `SurfaceRegistry::live_permission_report`. `maestro_snapshot` and `maestro_needs_list` add them to the needs section's `errors`; `maestro_context` marks the needs incomplete and names "t3 permission prompts" an unread source.
  - The identity readers stay fail-closed on the same thread.
  - **One boundary for every surface** (joint round 6, Codex). `SurfaceRegistry::live_permission_report` is where every read failure of a *configured* registry becomes an unread source, tagged with its surface. That covers a T3 registry that is missing, inaccessible, disconnected, or whose snapshot is not a JSON array. It covers a Paseo registry that is disconnected, or whose agents folder, an agent folder or an agent file does not read or parse. It covers a Direct registry file that is missing or does not read or parse. None of them returns "empty and complete".
  - A surface with no registry configured has nothing to read and stays complete. `maestro_context` names each degraded surface ("t3/paseo/direct permission prompts (n unread)").
- Runtime-request (permission prompt) payloads keep their existing per-row skip.
- The legacy `projection_threads` table has no payload column.
    - **Native resolution reads live threads only.** Archived and deleted threads' provider rows are not scanned, so a pruned one breaks nothing. That includes a thread archived or deleted only in its payload (`archivedAt`, `deletedAt`, which the thread reader honours; joint round 3, Gemini), so its native id never collides with a live thread's. That thread payload must read to tell: NULL, empty, blank or corrupt is `registry_unavailable`. The JSON snapshot takes `archivedAt` and `deletedAt` as aliases of its columns. A `nativeThreadRef.nativeId` of `null` (not yet assigned) matches nothing. A malformed row of a live thread is still `registry_unavailable`.
    - **Fail-open alerts are keyed by the workspace,** the code and the UTC day, never the named session, which is the caller's own string when it does not verify.
16. **One read rule** (joint round 7, Codex: the fourth layer of "a read failure becomes absence", ended with one primitive rather than another guard).
    - `maestro_hosts::read` defines `Read<T>`: `Absent` (`NotFound`, or `NotADirectory` when a parent is a file), `Present`, and `Unreadable(why)` for any other error, never absence.
    - Every registry probe and enumeration goes through it:
      - the `~/.t3` discovery probe;
      - T3's `locate` (snapshot or database), `discover`, the snapshot read and `environment_id`;
      - Paseo's agents folder, its entries (a `read_dir` entry that fails is an unread item, never dropped), its agent folders and agent files;
      - the Direct registry file;
      - the reader's `environment-id` (only a missing file is absent).
    - **The `~/.t3` fallback is a registry only when one is there** (Gemini's joint round 7). It is found when T3's own `locate` finds a database or snapshot under it, or cannot tell. A `~/.t3` with neither (client config, logs and worktrees only, before `userdata/statev2.sqlite` exists) is no registry, so the owner's views stay complete. An explicit `MAESTRO_T3_HOME` or `T3CODE_HOME` is configured, so a registry missing there is unread.
    - Discovery that cannot read (an inaccessible home) yields a found registry that is unreadable. It reaches `live_permission_report` as an unread source naming the surface. Identity on it fails closed: `registry_disconnected` from a lookup, `registry_unavailable` from a variable's resolution, never "no such session".
    - Plugin parity holds. A Paseo agent file that reads but does not parse, or names another agent, is still "no such session" to identity, as `callers-golden.json` requires. The listing reports it unread.
    - `crates/hosts/tests/read_guard.rs` fails if a raw `is_dir`, `is_file`, `exists`, `read_dir`, `read_to_string`, or a dropped entry error comes back in those functions.
17. **The permission rule** (round 9, BRO-3863 folded in by the owner; kind fallback kept in round 10; moved into Rust in round 11). A pending permission record is a prompt or an unread record with why, never dropped. The rule has one home, in Rust: `permission_record` in `crates/hosts/src/paseo.rs`. Every surface reaches it: `decode_permissions` for Paseo agent files and T3 snapshot rows, and `decode_runtime_requests` for T3 runtime requests.
    - **The reader does not interpret.** For each pending `approval`, `permission` or `tool_approval` runtime request, the embedded SQLite reader emits only `{"id", "kind", "payload_json"}`, the payload as its raw string or null. It has no permission payload parsing, no name logic and no `try`/`except` for them.
    - **The record or payload must be an object.** A runtime request's payload that is NULL, empty or blank is unread (`is empty`), one that is not JSON is unread (`does not parse`), and one that is not an object is unread (`is not an object`). A Paseo or snapshot record that is not an object is unread (`is not an object`).
    - **The tool name is read by key presence and type, never truthiness.**
      - If the key `toolName` is present, it is the name, and it must be a non-blank string.
      - Otherwise, if the key `name` is present, it is the name, by the same test.
      - A present key holding `""`, `"   "`, `0`, `false`, `null`, `[]` or `7` is unread: `has a tool name that is not a non-blank string`, naming the key.
      - **Precedence: the first present key governs.** `toolName` comes before `name`, so a present but invalid `toolName` is unread even beside a valid `name`. A record whose own tool key is malformed is not rescued by another key.
    - **A record with neither key is a prompt named by its kind** (the owner's decision after round 9). A runtime request's payload `{}` is a prompt titled `approval`, `permission` or `tool_approval`; it is never hidden and never unread, so the views stay complete. Paseo and snapshot records carry no kind, so on those surfaces a record with neither key is unread (`names no tool`).
    - **Optional fields stay typed.** `id`, `title`, `description` (strings) and `input` (an object) are read when they have their type and ignored when they do not; a wrong type never makes a record unread. A runtime request's own id is the prompt's id when it is non-blank, else the payload's `id`.
    - **One answer on every surface.** `one_permission_rule_answers_alike_on_every_surface` (`crates/hosts/tests/attribution.rs`) puts one fixture list, each beside a well-formed neighbour, through a T3 runtime request, a Paseo agent file and a T3 snapshot row. The answers agree except where the surfaces differ by construction: `{}` (a runtime request has a kind), and NULL, blank and non-JSON (only a runtime request's payload is text).
    - `T3Registry::list_live_report` and Paseo's listing report each live session's unread records. They reach `live_permission_report` as unread sources tagged with the surface, naming the agent file or thread and the record or request. The owner's views are then incomplete (`needs_complete: false`), and the well-formed prompts beside them are still listed.
    - **Identity is not affected.** A verified session carries the prompts that decoded, and no lookup's outcome depends on a permission record: a malformed one never refuses, admits or changes a session. Paseo plugin parity (`callers-golden.json`) holds.
    - `crates/hosts/tests/read_guard.rs` guards the rule's functions (`permission_record`, `decode_permissions`, `decode_runtime_requests`) with the rest, and flags, in the functions it guards (and across the whole embedded reader for the last):
      - `.ok()` in a `filter_map` or on a decode, matched over each statement with whitespace normalised (round 10), so a chain split across lines is still seen;
      - raw `fs::metadata`, `fs::read` and `File::open`;
      - a `continue` anywhere in an `except` block's body (round 10). Blank and comment-only lines neither end the body nor count (round 11), so a comment at a shallower indent does not hide a later `continue`.

      The guard is lexical and claims no more: a skip spelt otherwise (`pass`, `return None`) is review's to catch.

      It is a scoped lexical guard and claims no more than it checks. Its one reviewed exception, listed in the guard with why, is `discover`'s display-only runtime metadata (a version and an endpoint), which identity and needs never read.
13. **Existing tests.** The authority suites (`crates/mcp/tests/{authority,dispatch}.rs`, `crates/cli/tests/requests.rs`) are unchanged. Three tests outside them changed: `crates/mcp/tests/flow.rs` `verified_t3_and_paseo_sessions_write` now runs in both modes (attributed refuses the unsigned T3 claim `attribution_missing`). `crates/mcp/tests/stop.rs` and `transports.rs` name `paseo:-/nobody` for their unverified `flow open`, so they hold in any environment. The stop test also asserts the T3 case's `attribution_missing`.

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
- **Inherited variables can mis-attribute,** as stated above. The audit chain gives clues; it neither prevents nor guarantees detection.
- **Gemini can't write flows,** and Codex only through the CLI, until BRO-3848.

## Open questions

1. Should a write whose audit chain shows the writer is *not* a descendant of a T3-spawned harness (a detached long-lived process) be refused rather than recorded? Recommended default: record only. Next step: measure how often that happens in the first week of live use, then decide in the arc's asks ledger.
