# ADR: The flow ledger, a new ledger family with one writer from birth (BRO-3624)

Date: 2026-11-06
Status: accepted (owner, 2026-11-06 and 2026-11-07, as a decision-only ADR; see Review history
and Owner input)
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
   validated by `scripts/ask_ledger.py`) and resolves by reading the owner's answer. The same
   question is also relayed into the T3 thread driving the flow, so the owner can answer where
   they are working. Either way, the answer is recorded only through the owner path (Maestro
   decisions, or the T3 answer handoff that writes through it), never by an agent.
5. **Who may write.** A flow write takes effect, unlike a held agent request (BRO-3186). So every
   write verb (`start`, `claim`, `complete`, `choose`, `heartbeat`) requires a verified agent
   session (`VerifiedSession`, via `resolve_caller`), and an unverified caller may only read
   (`next`, `status`). An instance is owned by the verified session that started it. A claim is
   bound to the session that made it.
6. **Ownership survives turns; replacements need your approval.** Ownership is keyed on the
   `SessionRef` plus an owner marker that stays fixed for the life of the session and changes
   when the session is replaced. The registries' existing `incarnation` does not qualify: it
   changes on every T3 turn (provider, latest `run_id`) and every Paseo restart (`pid`). Phase 1
   adds the marker in `crates/hosts`. A session with no marker can read but not write.
   A replacement session cannot write an instance it does not own. It may continue that
   instance only through **owner-approved adoption**:
   - **The request:** the new session files a Maestro ask naming the instance, the old and new
     owner, and the progress so far.
   - **On your approval:** the kernel appends an `Adopted` event, after reading the answer from
     the answers ledger, never from the agent. Completed steps stand, in-flight claims expire
     through their leases, and the new owner continues from the next ready step.
   - **Without approval:** nothing changes.

   **Retiring** an abandoned instance works the same way. An agent may propose it through a
   Maestro ask, and the kernel retires the instance (log preserved, never reopened) only after
   your approval. Adoption and retirement change who owns a flow, so they are owner decisions,
   applied by the kernel. They are never agent verbs.
7. **No owner verb.** The flow verbs act on an agent's own instances. None answers, approves,
   dispatches, reviews or decides for the owner, and their names pass the owner-word checks in
   `crates/mcp/tests/authority.rs` and `crates/cli/tests/requests.rs`. Hence `choose`, not
   `decide`. The agent-callable operations that carry out an approved adoption or retirement only
   present the approved ask's ID, and the kernel checks the answer before acting. Their names must
   pass the same owner-word checks (`archive` is on the lists, so retirement needs another name).
   Flow writes are a new agent-surface class, so the authority suites gain explicit
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

## Owner input (2026-11-07, AskUserQuestion)

| Decision | Owner's answer |
|---|---|
| Where the ledger lives | Workspace `.broomva/flows/` |
| Who may write | Verified sessions only; unverified callers read only |
| Ownership after a replacement | Owner-approved adoption through a Maestro ask, resuming at the last completed step |
| Storage | The append-only log is truth; SQLite is a rebuildable cache |
| Machines | Single host for phase 1 |
| Human steps | Maestro asks, also relayed into the driving T3 thread; answers only through the owner path |
| Retiring an abandoned instance | Through an owner-approved Maestro ask; the log is kept |


## Amendment: pinned specialist agents (BRO-3826)

This amends BRO-3636 round 1: a new instance embeds each resolved specialist's launch data and source SHA-256 in its IR. Replay uses those pinned fields, so changing a source or Maestro build does not change the Codex brief, the source digest, or the native file references of a running instance. The native agent files under `.claude/agents/`, `.gemini/agents/` and `.grok/agents/` are not pinned: the harness reads them at launch, so re-rendering a file changes what a later native launch runs (follow-up BRO-3882). Earlier IR retains the builtin lookup behavior. A definition may declare `agents: <relative path>` from its own directory. The canonical root and source files must remain inside the definition's Git repository; inaccessible roots are errors. A root agent whose name matches a builtin is refused with `agent_name_conflict`. CLI start and driver-skill use the definition path; MCP start requires an absolute `definition_path` when the field is declared.

New `verify@1` instances pin the builtin `verifier`, making their IR version 8 and changing its hash. Comparing the spine of an old `verify@1` instance with a new one using `spine --against` returns `definition_mismatch`.

## Terminal instances (BRO-3870)

The owner decision, "latch terminal", makes the first log position whose replayed state is terminal at that event's recorded time permanent. Replay derives that position from the log; it writes no event and does not consult the subject. Stale observations remain visible in status, but cannot enable work or reopen the instance. A flow that needs periodic verification starts a new instance.

Once latched, instance writes are refused with `instance_terminal` and nothing is recorded. This applies to claims, completions, choices, heartbeats, human asks, adoption and retirement proposals, and stop records.

## Human re-asks when inputs change (BRO-3655)

- **Re-ask, bound to inputs.** An answer (and the ask it answers) is bound to the human node's input digests at the time of the ask. When an upstream input's receipt digest changes before the instance latches terminal, that earlier answer no longer satisfies the node, and the node re-enters `waiting` (never `enabled`) so it is re-askable. Calling `ask` then mints and files a new ask with a new ask id, recording that it `supersedes` the previous ask id with reason `inputs_changed`, without deleting or editing the old ask record in either ledger. Replay accepts a second `AskPinned` for the same node and iteration when its inputs differ from the standing ask's inputs, and still refuses a duplicate with `already_pinned` when the inputs are unchanged.
- **Revalidation.** Because replay is deterministic over the append-only log and binds each accepted human answer to the input digests it consumed, if the upstream receipt digests return to the exact values an earlier answer was bound to, that earlier answer satisfies the human node again without another ask.
- **Relay.** While a re-ask is pending (unfiled, waiting on the owner, or waiting to land on `origin/main`), `status`, `next` and the stop gate's `open` show the human node as waiting on the owner the same as a first ask, and include the reason `inputs_changed` (with `supersedes` naming the prior ask id), so neither relay hides it.
- **Terminal latch.** The terminal latch (`BRO-3870`) still holds: re-asking applies only before the instance latches terminal, and any `ask` on a latched instance is refused with `instance_terminal`.


## Enforced routing with attested performers (BRO-3828)

Until IR version 9, `routing.provider: different_from(writer)` was only a hint that `next` returned. A reviewer's provider was whatever the reviewer reported. Only the instance's owner claims and completes, so the "claimer" is always the driving session, never the reviewer it delegates to. The owner decided (2026-11-19, AskUserQuestion) to keep that rule, and to let the owner name the session that did the work.

- **Compile.** `different_from(a, params.b)` is parsed into the IR as `Launch.different_from`, which makes the IR version 9. Only a work node may route this way. Each argument is either a declared param or a work node that this node directly needs, so the node's receipt fixes the performer it compares against. Any other argument is a compile error. An older pinned IR has no parsed field, so it keeps its version and its routing stays a hint.
- **Attestation.** `claim` and `complete` take an optional performer session. The surface registry resolves it without that session's token. For a T3 owner, it must be a thread on this host whose parent is the owner's thread and which is not closed. A settled child still counts, since a delegated child has usually finished before its owner completes. For a direct owner, it must be a direct session whose `parent_id` is the owner. Anything else is refused `performer_unverified`, including a child on another host.
- **The rule.** Each provider is mapped from its harness: `claude-code` to claude, and `codex`, `gemini` and `grok` to themselves. `agy` and other harnesses have no attested provider, because they can run several vendors' models. A param must name one of claude, codex, gemini or grok. The performer's provider must be attested and must differ from every provider the route names. Otherwise the write is refused `routing_violated`. A claim that names a performer is checked then; a claim that names none only checks that the arguments resolve. A completion is always checked, against the performer it names, else the completing session. A performer named at claim is an early check and an audit record only: its attestation was of that moment, so it is not carried into the completion (P20 round 3: a child that switched to an excluded provider after the claim must not complete under its old one).
- **Binding (version 9).** A receipt that records a performer binds that performer into its digest, exactly as its event records it, which is the value the guards and the verify are shown (its harness, model and session). The guard document and the verify's input show every consumed receipt's performer, so a re-run by another performer with the same outputs changes its consumers' inputs, and a claim or completion judged against the old performer is refused `inputs_changed`.
- **Exposure (version 9).** The guard document carries `.nodes.<id>.performer`, the performer each consumed receipt records. With `receipt_stdin`, the verify's envelope carries `performer`, `{harness, provider, session}`, for the completing performer. Below version 9 neither carries anything new.

A T3 session's harness, for the owner as for a named child, is the one provider its provider-thread rows name; a thread that switched providers has none, so a routed node refuses it. A registry that reports no provider rows (a JSON snapshot, an older schema) leaves the thread's default provider for the owner's own session, and attests no named child at all.

Limit, stated: the registry attests that a child exists and what it runs. It cannot attest that the child, rather than the owner, wrote the outputs. An owner that lies about which child did the work is out of scope; this rule removes the case where a reviewer names its own provider.
