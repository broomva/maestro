# ADR: The flow ledger, a new ledger family with one writer from birth (BRO-3624)

Date: 2026-11-06
Status: proposed
Tracking: BRO-3624 (phase 1 of the flow kernel), BRO-3622 (spec), BRO-3426 (ideal agentic stack)

## Context

The flow kernel spec lives in the private **broomva/workspace** repository, not in this one. It is
`docs/specs/2026-11-06-flow-kernel.md` at commit `4fbc54ca3` (PR broomva/workspace#1229). To
read it with access:
`gh api 'repos/broomva/workspace/contents/docs/specs/2026-11-06-flow-kernel.md?ref=4fbc54ca3' --jq .content | base64 -d`.
This ADR depends on three statements in it. They are quoted exactly below (markdown emphasis
removed) so the ADR can be reviewed without that access:

- Non-goals: "No distributed execution in phase 1. A single host with file locks. Instances
  shared across the Mac and the NUC wait for a shared substrate."
- Storage: "Each instance has `events.jsonl` (append-only, hash-chained through `prev_event`) and
  a SQLite projection, kept under the flows root (see the Maestro boundary below)." and "There is
  no daemon."
- Decision D1: "The kernel is a Rust crate in the Maestro workspace (`crates/flow`), exposed
  through Maestro's existing CLI and MCP server"

The spec adds a pull-based flow kernel. Agents from any harness call it to learn which step
of a flow is ready (`next`), take it (`claim`), finish it (`complete`) or pick a declared branch
(`choose`). The kernel runs each step's verify command itself and records every transition in an
append-only, hash-chained event log. The owner decided on 2026-11-06 that the kernel is a Rust
crate in this repository, exposed through Maestro's existing CLI and MCP server.

That conflicts with a rule in `AGENTS.md`: "Maestro is read-only in phase 0. Do not add a write
path … without the phase that transfers ownership." The rule exists because of the BRO-3186
contract and the rev 2 objective, "without ever having two writers, or none, for a ledger". Every
ledger Maestro reads today (asks, initiatives, loops, the work root) has a current writer, the
Paseo plugin. Rev 2 moves each family to Maestro at a cut-over, behind the phase 2 lease in
`crates/ledger`. Neither the lease nor `crates/ledger` exists yet.

Flow instances are different in kind. No ledger of flow instances exists anywhere, so there is no
current writer to transfer from and no cut-over to stage.

## Decision

1. **A new ledger family: flows.** Its root is `.broomva/flows/` in the workspace, with one
   directory per instance holding `events.jsonl`. Nothing else in the workspace reads or writes it
   today.
2. **One writer from birth, on one host.** The `maestro-flow` crate (`crates/flow`) is the only
   code that writes the flow ledger. The CLI (`maestro flow …`) and the MCP tools
   (`maestro_flow_*`) both call into that crate, not into their own write paths.
   - **Within a host:** the CLI and the MCP server are separate processes. Every write takes an
     exclusive lock on `.broomva/flows.lock`, a sibling of the flows root and never inside it, for
     the whole check-and-append. The lock file also carries the ledger's **epoch**, and the root
     holds a matching `.broomva/flows/EPOCH`. After taking the lock, a writer acts only if:
     - the lock's epoch equals the root's `EPOCH`; or
     - the lock file is empty, meaning no ledger has ever existed here. Only then may the kernel
       create the root, writing a fresh epoch to both the lock and `EPOCH`.

     Anything else is refused with `root_moved`, and the writer creates nothing: a `moved` epoch
     in the lock, a missing root under a non-empty lock, or a root whose `EPOCH` differs. The
     writer then re-checks the instance log's sequence number. A writer whose expected sequence number
     is stale is refused with `stale_sequence`. Concurrent writers are therefore serialized by the
     lock, and a writer acting on an outdated view is refused. That is the one concurrency rule,
     and the tests check exactly it.
   - **Across hosts:** a file lock does not serialize writers on two machines that reach the same
     tree. Phase 1 is therefore single-host, matching the spec's non-goal ("no distributed
     execution in phase 1"). Under the root lock, the first write creates `.broomva/flows/HOST`
     with an exclusive create (`create_new`). If two hosts race, exactly one creation succeeds,
     and the other reads the winner's identity. Every write, the first included, compares `HOST`
     with the current host before touching any instance, and refuses with `foreign_host` on a
     mismatch. A second host's checkout of the workspace is a different root
     and so a different ledger. That is allowed, because the two ledgers never share instances.
   - **Host identity:**
     - The T3 environment ID (`$T3CODE_HOME/userdata/environment-id`) when T3 is installed. It is
       stable per machine: the Mac and the NUC have different ones.
     - Otherwise the platform machine ID: `IOPlatformUUID` on macOS, `/etc/machine-id` on Linux.
     - `HOST` records which source was used. A host whose identity cannot be read can read flows
       but not write them.
   - **Retiring an abandoned instance** (for example, one whose owner can no longer write it) is
     an operator step, not a model-visible verb. While holding the exclusive `flows.lock`, the
     operator renames `.broomva/flows/<instance>/` to `.broomva/flows/retired/<instance>/`. The log
     is kept unchanged, and the kernel never reads or writes under `retired/`. Instance IDs are
     minted fresh by the kernel. A write naming an ID that exists under `retired/` is refused
     (`retired`), and the kernel never recreates `.broomva/flows/<instance>/` for it.
   - **Moving to another host is not a phase 1 operation.** Phase 1 has no rebind. The ledger
     moves only once every instance is finished or retired. While holding the exclusive
     `flows.lock`, the operator renames `.broomva/flows/` aside and sets the lock's epoch to
     `moved`. From then on every writer, waiting or new, refuses with `root_moved` and creates
     nothing, because the lock is no longer empty. Starting the ledger on the new host is
     another operator step under the lock: install a fresh epoch in the lock, then create the
     root with the same `EPOCH` and the new `HOST`. Live migration waits for the shared substrate
     or the lease (next item).
   - **Later:** when a shared substrate or the phase 2 lease in `crates/ledger` exists, the flow
     family moves under it and the host binding is lifted. That move is a change to one call
     site, not a change of writer.
3. **No other ledger gains a writer.** The flow crate never writes the asks, initiatives, loops or
   work ledgers. A human step in a flow files its ask through the existing agent path
   (`maestro ask file` / `maestro_ask_file`, validated by the workspace's `scripts/ask_ledger.py`)
   and resolves by *reading* the owner's answer. The answers writer stays whatever rev 2 names.
   `maestro_ask_file` can report an error after the ask is already written (for example, when
   its requests journal line fails). A retry is then refused as a duplicate. The ask's ID is
   chosen by the caller before filing (`crates/store/src/requests.rs`), and that failure carries
   it back in the error's data as `filed.id` (`crates/mcp/src/lib.rs`, `unjournalled`). So the
   flow mints an ask ID that is not already an `id` or a `uid` in the working-tree ledger. It
   writes that ID, and a hash of the body it will submit, to the instance log *before* filing.
   The ask is linked only when a working-tree row has exactly that `id` and that body. Each
   outcome:
   - **Success:** confirm the row, then link it.
   - **An "ask stands" error, a crash, or a lost response:** confirm against the working tree.
     `file_ask` writes the working tree, and Maestro's default store follows `origin/main`, so the
     confirmation reads the working-tree store, not a default `maestro_ask_get`. If the matching
     row is there, link it.
   - **No matching row:** the flow may file again with the same ID and body. It never files under
     a new ID.
   - **Busy (exit 75):** the ledger lock is held, possibly by an in-flight filing of this same ask.
     Nothing is linked. The step stays `waiting`, and the next attempt confirms first.
   - **Any other refusal, including a duplicate-ID refusal without a matching row:** nothing is
     linked. The step fails visibly with `ask_refused` and the ledger's reason. A refusal never
     counts as success.
   The flow never searches `maestro_asks_list`, which is paged and filtered. The flows lock is never
   held while filing: logging the ID and linking the ask are separate short writes, so a slow
   ask filing (up to its 30-second lock wait) cannot block other flow writers.
4. **Who may write.** In the BRO-3186 contract, agent requests are recorded for review and do not
   take effect for an unverified caller. A flow write does take effect. So the write verbs
   (`start`, `claim`, `complete`, `choose`, `heartbeat`) require a verified agent principal
   (`Principal::Agent`, from the surface registry that `agent_request` already uses). An
   `Unverified` caller can read (`next`, `status`) and nothing else.
   **This is a prerequisite that does not exist yet for every surface.** The MCP server can
   produce `Principal::Agent` where a surface registry is configured (T3, Paseo).
   `resolve_caller` (`crates/mcp/src/lib.rs`) also verifies a caller presenting
   `MAESTRO_DIRECT_TOKEN` or `MAESTRO_BRIDGE_CREDENTIAL`. The CLI routes through it, but its
   stock subcommands send no such claim, so they arrive unverified (`crates/cli/src/main.rs`,
   "The caller is unverified (BRO-3268)"). Phase 1 makes every `maestro flow` write obtain a
   `VerifiedSession` through that path (Direct, T3 or Paseo). A `maestro flow` write without one
   is refused. Ownership works as follows:
   - **The instance is owned by the session that started it.** Only that session may `claim`.
   - **A claim is bound to the verified session that made it.** Only that session may
     `complete`, `choose` or `heartbeat` the claim. The claim token is a handle that names the
     claim, not a capability: on its own it grants nothing, so passing it to another session
     gives that session no write.
   - **Performers report through the claimant.** A subagent that does the work returns its result
     to the session holding the claim, and that session calls `complete`. This matches the pull
     model, in which the driving session talks to the kernel.
   - **Tokens are never readable.** The log and the SQLite projection store only a hash of each
     token, and `next`, `status` and replay never return a token. A token appears once, in the
     `claim` response to its claimant.
   - **No cross-agent writes.** A verified session that did not start the instance cannot claim
     in it, and a session that did not make a claim cannot act on it.
   - **No claim resumption in phase 1.** A claim lives only as long as its lease. A process that
     restarts has lost the token, because the token is shown once and stored only hashed, so it
     cannot re-present the claim. The claim is recovered only by lease expiry: the token becomes
     void (`lease_expired`), the node is released, and the owner (same `SessionRef` and owner marker)
     may claim it again as the next attempt.
   - **Invariant: a flow write requires a `SessionRef` plus a verified owner marker that stays
     fixed for the life of that session and changes when the session is replaced.**
     `Principal::Agent` carries only the `SessionRef`, and a replacement can reuse the visible ID
     (`crates/hosts/tests/identity.rs`, the Paseo case). The registries' existing `incarnation`
     cannot serve as the marker, because it changes far more often than replacement:
     - T3's incarnation includes the provider and the thread's latest `run_id`
       (`crates/hosts/src/t3.rs`), so it changes on every turn and every provider switch;
     - Paseo's includes the provider, `pid` and `started_at` (`crates/hosts/src/paseo.rs`), so it
       changes on every process restart.

     Phase 1 therefore adds an owner marker to `crates/hosts`, read from the `VerifiedSession`:
     - **T3:** the thread ID plus the thread's `created_at`;
     - **Paseo:** the agent ID plus `handle_id` (`persistence.sessionId`), which changes on
       replacement (handle-v1 to handle-v2 in `identity.rs`);
     - **Direct:** `sha256(token)`, the same hash the Direct incarnation uses
       (`direct:{cwd}:{sha256(token)}` in `crates/hosts/src/lib.rs`). `cwd` is left out on
       purpose, so moving the directory is not a new owner. The session ID is already the
       `SessionRef`.

     A marker exists only when every part is present. A T3 thread whose `created_at` is NULL
     (the legacy `projection_threads` path), or a Paseo agent with no `persistence.sessionId`,
     has *no* marker. Such a session is never treated as a marker made of the visible ID
     alone.

     The owner of an instance and the holder of a claim are recorded as `SessionRef` plus owner
     marker. A caller whose verified session yields no owner marker is refused on every write
     verb. A caller whose marker differs from the one an instance recorded is refused `claim`,
     `complete`, `choose` and `heartbeat` on that instance. Such a caller may still `start` a new
     instance: after a replacement, the work restarts as a new instance, and the old one is
     retired by the operator step in decision 2. Phase 1 has no transfer, adoption or orphan detection. `status` reports each open
     instance's last event time, so a stalled instance is visible.
5. **No owner verb.** The flow verbs (`start`, `next`, `claim`, `complete`, `choose`, `status`,
   `heartbeat`) act on an agent's own flow instance. None of them answers, approves, dispatches,
   reviews or decides for the owner. Their names pass the owner-word checks in
   `crates/mcp/tests/authority.rs` and `crates/cli/tests/requests.rs` unchanged. That is why the
   branch verb is `choose`, since both lists refuse `decide`.
6. **Flow writes are a new kind of agent-surface tool, and the authority suites say so.** Today
   every tool is a read or a held agent request (`every_tool_is_a_read_or_an_agent_request`). A
   held request changes nothing but the requests journal
   (`a_held_request_changes_nothing_but_the_journal`). A flow write is neither. It takes effect,
   but only for the caller's own flow. So phase 1 adds a classification for agent writes to the
   flow ledger, still on the agent surface of the BRO-3186 contract and never an owner action.
   Those two tests get explicit cases for it: a flow write may change only
   `.broomva/flows/<instance>/`, `.broomva/flows.lock`, and `HOST` and `EPOCH` (only when the
   ledger is first created),
   and never the requests journal. They are extended, not weakened.
   `crates/mcp/tests/dispatch.rs` does not check names. It checks that no `OwnerVerb` tool is
   reachable, and that annotations match authority (reads read-only, writes not idempotent). The
   flow tools are covered there too.
7. **Maestro still does not act on the fleet.** `next` returns a launch hint (performer, skill,
   routing) for whoever drives the flow. The flow crate never spawns a session or sends agent
   mail, which keeps "the fleet skill acts, Maestro reads".
8. **Unread is not empty.** `flow status` and `flow next` report a log that cannot be read or does
   not verify (a broken hash chain, an unparseable line) as an error with its source, never as an
   empty or finished instance.
9. **The log is the source of truth.** `events.jsonl` is append-only and hash-chained. The SQLite
   projection the spec names is a cache derived from the log. It is written only by the same
   crate and can be deleted and rebuilt by replay at any time. A disagreement between the two is
   resolved in favour of the log. Only write verbs update the projection. `next` and `status`
   write nothing, including the cache: if it is stale, they replay in memory. That keeps
   `a_read_tool_writes_nothing` true. There is no daemon.

   This narrows the spec's storage line ("Every CLI or MCP call takes a file lock, replays the
   log incrementally and appends"). Reads take a *shared* lock on `.broomva/flows.lock` and append
   nothing, and only write verbs take the exclusive lock and append. A reader therefore never
   sees a half-written line from a live writer. A torn final line left by a crash is reported as
   unread (decision 8) with its source, never as a failed or finished instance. The next write,
   under the exclusive lock, refuses to append after it. Recovery is an operator step of the same
   class as retiring: while holding `flows.lock`, truncate the file back to its last complete,
   hash-verified line, keeping the cut bytes in `events.jsonl.torn`. The next write then appends
   normally, and the effect-reconcile rule handles any effect whose completion was lost.

## Consequences

- `AGENTS.md` changes its "one writer per ledger" rule to name this family and its writer. The
  read-only rule stays in force for every existing family until its cut-over.
- The crate must ship with tests that fail when a guard is removed. In particular, a second
  concurrent writer is serialized by the root lock, a writer with a stale sequence number is
  refused, and no flow verb writes outside `.broomva/flows/` and `.broomva/flows.lock`.
- The flow ledger needs backup the same way the workspace tree does. It lives in the workspace, so
  Time Machine and git hygiene cover it. Phase 1 does not commit instance logs to git.
- A harness can write flows only where the surface registry verifies its session *and* phase 1's
  owner marker is defined for that surface: T3, Paseo and Direct. A harness session the registry cannot verify can read flows but not
  drive them. The spec's live capability test (phase 1 acceptance, check 4) records which
  harnesses that leaves out.
- Reversal is cheap until other flows depend on it. Deleting `crates/flow` and the directory
  removes the family completely, because nothing else reads it.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Wait for phase 2 (`crates/ledger` and the lease) before any flow write | Blocks the flow kernel on an unrelated cut-over. The lease exists to move existing families without two writers; a new family has no second writer to guard against |
| A separate repository and MCP server for the kernel | Rejected by the owner on 2026-11-06. It would be a second server to install on every harness, while Maestro already serves Claude, Codex, Gemini CLI and Antigravity and holds the asks that human steps need |
| Keep flow state inside each agent's conversation | That is today's failure. Order lives in prose and gets skipped |
| Write flow state into the existing work ledger | Gives the work ledger a second writer before its cut-over, which is exactly the defect BRO-3186 forbids |

## Verification

Each command below must pass. Each guard test must fail when its guard is removed, proved by a
line in `scripts/mutation-check.sh`, as `AGENTS.md` requires.

```bash
cargo test -p maestro-flow --test single_writer     # two processes writing at once are serialized by the root lock; a write with a stale sequence number is refused
cargo test -p maestro-flow --test archive           # after an archive (epoch `moved`) a waiting writer and a brand-new writer both refuse root_moved and create nothing; a recreated root with a different EPOCH is refused; a write naming a retired ID is refused and no live dir is created
cargo test -p maestro-flow --test host_binding      # two first writers race on an empty root: one HOST is created; the other host is refused (foreign_host); later foreign writes refused
cargo test -p maestro-flow --test write_scope       # no flow verb writes outside .broomva/flows/ and the sibling .broomva/flows.lock
cargo test -p maestro-flow --test principal         # an Unverified caller is refused every write verb, and can still read
cargo test -p maestro-flow --test cross_session     # another verified session cannot claim in, or act on a claim of, an instance it did not start
cargo test -p maestro-hosts --test owner_marker     # T3 marker stable across turns and provider switches; Paseo marker changes on handle replacement; Direct marker changes with the token, not the cwd; legacy T3 row with NULL created_at and Paseo without persistence.sessionId yield no marker
cargo test -p maestro-flow --test resumption        # a restarted process cannot use its old claim; after lease expiry the same SessionRef and owner marker claims the next attempt; a different SessionRef cannot write the old instance
cargo test -p maestro-flow --test owner_marker      # every write verb refused without a marker; a replacement (new marker, same SessionRef) is refused claim/complete/choose/heartbeat on the old instance but may start a new one
cargo test -p maestro-flow --test token_handle      # a token passed to another session grants nothing; no token appears in the log, projection or status
cargo test -p maestro-mcp --test flow_principal     # MCP: unverified flow writes refused; verified T3 and Paseo writes succeed
cargo test -p maestro --test flow_principal         # CLI: writes refused until the Direct path verifies; a verified Direct write succeeds
cargo test -p maestro-flow --test ask_recovery      # minted ID collides with no id or uid; links only a working-tree row with that id and body; exit 75 and every other refusal (incl. duplicate with no matching row) link nothing
cargo test -p maestro-flow --test projection        # deleting the SQLite projection and replaying reproduces it
cargo test -p maestro-flow --test torn_tail         # a torn last line reads as unread and blocks appends; after truncation to the last verified line the next write appends
cargo test -p maestro-mcp --test authority          # owner words pass unchanged; flow writes touch only their instance dir, flows.lock and a first HOST and EPOCH; reads write nothing
cargo test -p maestro-mcp --test dispatch           # no OwnerVerb reachable; flow tool annotations match their authority
cargo test -p maestro --test requests               # owner-word list passes with the `maestro flow` subcommands
```
