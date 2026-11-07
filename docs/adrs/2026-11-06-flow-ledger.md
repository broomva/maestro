# ADR: The flow ledger, a new ledger family with one writer from birth (BRO-3624)

Date: 2026-11-06
Status: proposed
Tracking: BRO-3624 (phase 1 of the flow kernel), BRO-3622 (spec), BRO-3426 (ideal agentic stack)

## Context

The workspace spec `docs/specs/2026-11-06-flow-kernel.md` (broomva/workspace#1229, merged
`4fbc54ca3`) adds a pull-based flow kernel. Agents from any harness call it to learn which step
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
   - **Within a host:** the CLI and the MCP server are separate processes. Each write takes an
     exclusive file lock on the instance and checks the log's sequence number before appending,
     so concurrent writers serialize or are refused.
   - **Across hosts:** a file lock does not serialize writers on two machines that reach the same
     tree. Phase 1 is therefore single-host, matching the spec's non-goal ("no distributed
     execution in phase 1"). The first write creates `.broomva/flows/HOST`, recording the host's
     identity. Every later write compares it with the current host and refuses with
     `foreign_host` on a mismatch. A second host's checkout of the workspace is a different root
     and so a different ledger. That is allowed, because the two ledgers never share instances.
   - **Later:** when a shared substrate or the phase 2 lease in `crates/ledger` exists, the flow
     family moves under it and the host binding is lifted. That move is a change to one call
     site, not a change of writer.
3. **No other ledger gains a writer.** The flow crate never writes the asks, initiatives, loops or
   work ledgers. A human step in a flow files its ask through the existing agent path
   (`maestro ask file` / `maestro_ask_file`, validated by the workspace's `scripts/ask_ledger.py`)
   and resolves by *reading* the owner's answer. The answers writer stays whatever rev 2 names.
4. **Who may write.** In the BRO-3186 contract, agent requests are recorded for review and do not
   take effect for an unverified caller. A flow write does take effect. So the write verbs
   (`start`, `claim`, `complete`, `choose`, `heartbeat`) require a verified agent principal
   (`Principal::Agent`, from the surface registry that `agent_request` already uses). The claim
   token is bound to that session. An `Unverified` caller can read (`next`, `status`) and nothing
   else.
5. **No owner verb.** The flow verbs (`start`, `next`, `claim`, `complete`, `choose`, `status`,
   `heartbeat`) act on an agent's own flow instance. None of them answers, approves, dispatches,
   reviews or decides for the owner. Their names pass the owner-word lists in
   `crates/mcp/tests/authority.rs` and `crates/cli/tests/requests.rs` unchanged. That is why the
   branch verb is `choose`, since both lists refuse `decide`. `crates/mcp/tests/dispatch.rs` does
   not check names. It checks that no `OwnerVerb` tool is reachable, and that each tool's
   annotations match its authority (reads read-only, writes not idempotent). The flow tools must
   pass it once registered.
6. **Maestro still does not act on the fleet.** `next` returns a launch hint (performer, skill,
   routing) for whoever drives the flow. The flow crate never spawns a session or sends agent
   mail, which keeps "the fleet skill acts, Maestro reads".
7. **Unread is not empty.** `flow status` and `flow next` report a log that cannot be read or does
   not verify (a broken hash chain, an unparseable line) as an error with its source, never as an
   empty or finished instance.
8. **The log is the source of truth.** `events.jsonl` is append-only and hash-chained. The SQLite
   projection the spec names is a cache derived from the log. It is written only by the same
   crate and can be deleted and rebuilt by replay at any time. A disagreement between the two is
   resolved in favour of the log. There is no daemon.

## Consequences

- `AGENTS.md` changes its "one writer per ledger" rule to name this family and its writer. The
  read-only rule stays in force for every existing family until its cut-over.
- The crate must ship with tests that fail when a guard is removed. In particular, a second
  concurrent writer must be refused, and no flow verb may write outside `.broomva/flows/`.
- The flow ledger needs backup the same way the workspace tree does. It lives in the workspace, so
  Time Machine and git hygiene cover it. Phase 1 does not commit instance logs to git.
- A harness can write flows only where the surface registry can verify its session. Today that
  is T3, Paseo and Direct. A harness session the registry cannot verify can read flows but not
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

Each command below must pass, and each guard test must fail when its guard is removed, per
`scripts/ci.sh`'s mutation check.

```bash
cargo test -p maestro-flow --test single_writer     # a second concurrent writer (separate process) is serialized or refused
cargo test -p maestro-flow --test host_binding      # a write from a host other than .broomva/flows/HOST is refused (foreign_host)
cargo test -p maestro-flow --test write_scope       # no flow verb writes outside .broomva/flows/
cargo test -p maestro-flow --test principal         # an Unverified caller is refused every write verb, and can still read
cargo test -p maestro-flow --test projection        # deleting the SQLite projection and replaying reproduces it
cargo test -p maestro-mcp --test authority          # owner-word list passes with the maestro_flow_* tools registered
cargo test -p maestro-mcp --test dispatch           # no OwnerVerb reachable; flow tool annotations match their authority
cargo test -p maestro --test requests               # owner-word list passes with the `maestro flow` subcommands
```
