# ADR: The flow ledger, a new ledger family with one writer from birth (BRO-3624)

Date: 2026-11-06
Status: accepted (owner, 2026-11-06, as a decision-only ADR; see Review history)
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

This ADR records decisions only. How each one is enforced (locks, epochs, host binding, leases,
tokens, owner-marker sources, ask filing and linking, crash recovery, archive and retire) is
designed in `crates/flow` and settled by its mutation-checked tests, listed in BRO-3624. Those
mechanisms are deliberately not specified here. See Review history for why.

1. **A new ledger family: flows.** Its root is `.broomva/flows/` in the workspace, with one
   directory per instance holding an append-only, hash-chained `events.jsonl`. Its coordination
   files (lock and similar) sit at `.broomva/flows/` or beside it, never anywhere else. Nothing
   else in the workspace reads or writes this family.
2. **One writer from birth.** `crates/flow` is the only code that writes the flow ledger. The CLI
   (`maestro flow …`) and the MCP tools (`maestro_flow_*`) call into it, not into write paths of
   their own. Because no flow ledger has ever had another writer, no cut-over is needed. When the
   phase 2 lease in `crates/ledger` exists, the family moves under it.
3. **Single host in phase 1.** As the spec's non-goal says, writers on one host are serialized,
   and a second host writing the same tree is refused. Live migration between hosts waits for the
   shared substrate or the lease.
4. **No other ledger gains a writer.** The flow crate never writes the asks, initiatives, loops or
   work ledgers. A human step files its ask through the existing agent path (`maestro_ask_file`,
   validated by `scripts/ask_ledger.py`) and resolves by reading the owner's answer.
5. **Who may write.** A flow write takes effect, unlike a held agent request (BRO-3186). So every
   write verb (`start`, `claim`, `complete`, `choose`, `heartbeat`) requires a verified agent
   session (`VerifiedSession`, via `resolve_caller`), and an unverified caller may only read
   (`next`, `status`). An instance is owned by the verified session that started it. A claim is
   bound to the session that made it.
6. **Ownership survives turns and refuses replacements.** Ownership is keyed on the
   `SessionRef` plus an owner marker that stays fixed for the life of the session and changes
   when the session is replaced. The registries' existing `incarnation` does not qualify: it
   changes on every T3 turn (provider, latest `run_id`) and every Paseo restart (`pid`). Phase 1
   adds the marker in `crates/hosts`. A session with no marker can read but not write.
7. **No owner verb.** The flow verbs act on an agent's own instances. None answers, approves,
   dispatches, reviews or decides for the owner, and their names pass the owner-word checks in
   `crates/mcp/tests/authority.rs` and `crates/cli/tests/requests.rs`. Hence `choose`, not
   `decide`. Flow writes are a new agent-surface class, so the authority suites gain explicit
   cases for them rather than being weakened.
8. **Maestro still does not act on the fleet.** `next` returns a launch hint. The flow crate never
   spawns a session or sends agent mail.
9. **Reads write nothing, and unread is not empty.** `next` and `status` write nothing (keeping
   `a_read_tool_writes_nothing` true). A log that cannot be read or does not verify is reported as
   an error with its source, never as an empty or finished instance.
10. **The log is the source of truth.** The SQLite projection the spec names is a cache,
    rebuilt from the log. A disagreement is resolved in favour of the log. There is no daemon.

## Consequences

- `AGENTS.md` names this one exception to "one writer per ledger". Every existing family stays
  read-only until its cut-over.
- Phase 1 cannot merge any write path without the BRO-3624 test list, each guard mutation-checked
  in `scripts/mutation-check.sh`.
- Reversal is cheap until flows depend on it: deleting `crates/flow` and `.broomva/flows/`
  removes the family completely.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Wait for phase 2 (`crates/ledger` and the lease) before any flow write | Blocks the flow kernel on an unrelated cut-over. The lease exists to move existing families without two writers; a new family has no second writer to guard against |
| A separate repository and MCP server for the kernel | Rejected by the owner on 2026-11-06. It would be a second server to install on every harness, while Maestro already serves Claude, Codex, Gemini CLI and Antigravity and holds the asks that human steps need |
| Keep flow state inside each agent's conversation | That is today's failure. Order lives in prose and gets skipped |
| Write flow state into the existing work ledger | Gives the work ledger a second writer before its cut-over, which is exactly the defect BRO-3186 forbids |

## Review history

Between 2026-11-06 and 2026-11-07 this ADR went through 11 P20 rounds, scoring 3, 3, 5, 5, 7
(failed on one MAJOR), 5, 5, 5, 6, 5 and 3, plus CodeRabbit reviews. The decisions above held
from round 3 onwards. Every later finding was about a mechanism written in prose: host binding,
claim tokens, resumption, incarnation versus a stable owner marker, lock location and epochs, and
ask filing and linking. Each fix to a prose mechanism produced the next round's defect. The owner
therefore accepted this decision-only form on 2026-11-06 (AskUserQuestion) and moved every
mechanism, with the findings that motivated it, into BRO-3624 as required tests. The review
ledgers are archived at `~/.config/broomva/cross-review-archive/maestro21-flow-ledger-adr/`.
