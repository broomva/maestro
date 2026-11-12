# Maestro

Maestro is the governance and orchestration metalayer for Broomva's agent fleet. It types what the
fleet already runs on (asks, decisions, work, loops, ticks, initiatives, grants and fleet reports)
and exposes it the same way to every agent harness: a `maestro` CLI and an MCP server.

Maestro sits on top of the existing substrate. It does not replace any of it:

- the fleet skill keeps census, classification and agent-to-agent mail;
- bstack keeps the hooks, gates and primitives;
- the workspace's `.broomva/` (legacy `.control/`) stays the source of truth. Every path comes from
  bstack's path registry: `crates/store/src/broomva_paths.rs` is its generated reader, vendored
  byte for byte by `scripts/vendor-paths.sh` (BRO-3266).

Maestro reads those, computes the views every surface shares (needs, loop drift, context packets),
and injects that context into agent sessions.

This is the Rust successor to the Paseo plugin (`broomva/maestro-paseo`) and the earlier TypeScript
app (archived as `broomva/maestro-legacy`). Design and phases: [`docs/specs/2026-10-18-maestro-rust.html`](docs/specs/2026-10-18-maestro-rust.html).

## Install

```bash
cargo install --locked --path crates/cli --root ~/.local   # installs ~/.local/bin/maestro
```

## Use

```bash
maestro snapshot                     # counts across the control plane, with read health per section
maestro needs                        # holds on the owner: the "Needs you" list
maestro asks [--project P] [--blocking] [--state open|answered|archived|all]
maestro asks show <ledger.yaml> <id> # one ask in full, with its content hash
maestro asks validate <ledger.yaml>  # runs the workspace's ask_ledger.py, the validator of record
maestro work [--state "Needs you"]   # canonical states: Queued, Running, Stuck, Needs you, Done, Standing
maestro loops [--drifting]           # loop registry joined with tick receipts (the ledger's tail): drift and notices
maestro initiatives [--all]
maestro fleet
maestro context [--cwd D] [--format markdown|json|hook]
maestro authority                    # what each surface may do, and the standing invariants
maestro hosts                        # this node, surfaces and harnesses, each with its capabilities
maestro mcp                          # MCP server on stdio
maestro serve --http [--bind ADDR]   # MCP server over Streamable HTTP at /mcp, 127.0.0.1:7171 by default
maestro doctor [--bstack DIR]        # is the vendored path reader the one bstack generates (as of that checkout's last fetch)? exit 1 if stale or unread
maestro delegation assess|plan|brief|ladder   # tiers, assessments and agent-graph rules (the workspace's delegation.py, ported)
maestro economy capacity --usage F [--samples F] [--now T] [--state F]   # capacity forecast, cadence mode and launch gates; --state = provider_manager --json state (failover)
maestro economy tiers [--path F]     # Maestro's economy by tier, and what to re-plan
```

Agent requests (BRO-3267). Each names the session making it (`--session surface:env/id`, or
`MAESTRO_SESSION`), is journalled in `$BROOMVA_HOME/ledger/requests/requests.jsonl`, and starts nothing:

```bash
maestro ask file --ledger L.yaml --ask '<json>'   # file one ask (or JSON on stdin) into an existing ledger
maestro queue --title T --brief B                 # queue work for the owner to dispatch: held
maestro action-request --action A --reason R      # one exact action the owner must allow: held
maestro close --receipt R                         # this session's work is finished, with its evidence
maestro handback --summary S [--next N]
maestro escalate --reason R
```

- **Filing.** `ask file` files through the workspace's writer of record, `ask_ledger.py file` (or
  `MAESTRO_ASK_LEDGER`; BRO-3406), and writes no YAML itself. The writer holds the ledger's §5.5 lock
  (the Paseo plugin's `server/locks.ts`; root `MAESTRO_PASEO_ASK_LOCKS`, else
  `~/.local/state/control-asks/locks`, which Maestro passes on) and the flock, appends the ask in the
  line shape its own `stamp` and `archive` edit, records the origin `{via: maestro, session, verified:
  false}`, and replaces the ledger only when it reads back as the ledger's asks plus exactly that ask with
  no new validator error. It writes to the working tree. It refuses an answer, a status or an origin in
  the ask, an id or uid outside `^[A-Za-z0-9][A-Za-z0-9._-]*$` or one any existing ask is addressed by,
  values Maestro's reader would read back otherwise (integers beyond ±(2^53 − 1), `-0.0`), a result over
  the readers' 1 MiB limit, and a ledger that does not exist or is a symlink. A lock busy for the
  whole wait (the writer's exit 75, or the journal's) makes `maestro` exit 75 too, and MCP answers an
  error with `data.busy: true`: retry. An ask filed whose journal line cannot be written is a failed
  request whose error names the filed ask (filing it again is refused as a duplicate).
  `crates/store/src/locks.rs` (the same lock, ported) guards the requests journal.
- **Unverified callers.** Until sessions are verified against their surface (BRO-3268), every caller is
  unverified: an ask is filed with `origin: {via: maestro, session, verified: false}` and no `agent_id`,
  so no answer is routed into a session on a caller's word; every other request is held.
- **Not a trust boundary.** The CLI takes its workspace, home and validator from the caller's
  environment (`BROOMVA_WORKSPACE`, `MAESTRO_ASK_LEDGER`): it files for an agent that could write the
  file itself, and it records that agent as unverified. Verification is the surfaces' (BRO-3268).

Flows (BRO-3634). The flow kernel's verbs (`crates/flow`), each the CLI twin of a `maestro_flow_*`
MCP tool, printing the tool's JSON. A refusal is a result, `{"ok": false, "code", "reason"}`, and
the CLI exits 1 on it:

```bash
maestro flow start --definition F.yaml [--params '<json>']   # start an instance, owned by this session
maestro flow next --instance I                               # the nodes to claim now, and why not the rest
maestro flow status --instance I                             # every node's state
maestro flow claim --instance I --node-id N                  # take a node: a claim token and a lease
maestro flow heartbeat --claim-token T                       # extend the lease
maestro flow complete --claim-token T [--outputs '<json>']   # the kernel runs the node's command
maestro flow choose --claim-token T --edge E                 # take a choice node's edge if its guard holds
maestro flow ask --instance I --node-id N [--ledger L.yaml --ask '<json>']   # a human node: file its ask, then take the answer
maestro flow propose-adoption --instance I [--ledger L.yaml --ask '<json>']    # ask the owner to hand you an instance
maestro flow propose-retirement --instance I [--ledger L.yaml --ask '<json>']  # ask the owner to retire an instance
maestro flow spine --instance I [--against J]                # the accepting spine; exit 1 at the first difference from J's
maestro dogfood verify-receipt --head=H --surface=S < outputs.json   # check a dogfood receipt (verify@1's exercise verify)
maestro flow driver-skill --definition F.yaml [--out SKILL.md] [--check]   # the flow's driver skill, generated from its IR (no tool's twin)
maestro agents render [--root DIR] [--check]                 # agents/*.agent.yaml to each harness's agent file; --check exits 1 at the first drift
```

- **Human steps (BRO-3635).** A waiting `human` node is the owner's. `flow ask` (MCP
  `maestro_flow_ask`) files its ask on the first call: the kernel mints the id, takes the question
  from the definition and `asked_at` from its own clock, adds your `--ask` fields (what the
  ledger's validator requires), pins that body in the flow's log, and files it through the
  writer of record (`ask_ledger.py file`, as `ask file` does) with the flow ledger's lock
  released. It links the row it finds in the working tree by that id and hash. Call it again until
  it says `answered`: the answer counts only once it has landed on main of the answers remote,
  `https://github.com/broomva/workspace.git`, compiled into Maestro (owner decisions 2026-11-09,
  "Landed main, remote-checked" and "Compiled into maestro"; changing it is a code change). The
  kernel asks that URL for `refs/heads/main` with nothing local applying (git by absolute path,
  the Command Line Tools' on macOS and `/usr/bin/git` on Linux, checked root-owned and not group-
  or other-writable; a fixed `PATH`; outside the repository; no git configuration; none of your
  `GIT_*` environment; the one credential helper is `gh`'s, which only supplies credentials), and
  reads the ledger at that commit object by object, each checked against its id, so nothing planted
  in the object store reads as the answer. The floor (owner decision 2026-11-09, "Set the floor,
  fix, re-check"): the residual risk is anything needing the user-level write that can replace
  Maestro's own binary (`~/.local/bin/maestro`, the macOS `xcrun` cache, Homebrew's `gh`), a
  root-level change, or merging a pull request carrying an answer into that main. No local ref or remote counts: an answer on `maestro/answers` (this clone's
  or origin's) is reported as `answer_waiting_to_land`, and the flow resumes when it lands. A
  remote that cannot be asked (20 s per call), has no main, or names a commit this clone has not
  fetched is `answers_unread`. The answer is
  never read from the working tree or from you; it is recorded as the node's receipt, with the
  answer's fields as its outputs (`answer_option`, `answer`, ...). A busy asks ledger is retried
  until the kernel's deadline, then `ask_refused_busy`; a row with that id and another body is
  `ask_refused`. `flow next` and `flow status` return `relay`: each waiting node's question, its
  state (`unfiled`, `owner`, `answer_waiting_to_land`) and ask, and for a T3 owner, links that open
  its thread with a draft to take the answer.

- **Adoption and retirement (BRO-3635).** Who owns a flow is the owner's decision, so no verb
  adopts or retires an instance: `flow propose-adoption` and `flow propose-retirement` (MCP
  `maestro_flow_propose_adoption`, `maestro_flow_propose_retirement`) put it to the owner as a
  Maestro ask, filed, linked and answered exactly as a human node's ask is, and the kernel
  carries out only the owner's answer, read from main of the answers remote. The first call
  takes `--ledger` and `--ask` (the fields the validator requires; the kernel writes the
  question, the options and the default) and files an ask naming the instance, its owner, your
  session (each with its owner marker's digest, never the marker) and the progress so far.
  Call it again until it says where it ended. Only the option `adopt` (or `retire`) approves.
  Anything else, `keep` included, is `declined` on every call, and nothing changes; `--again`
  (with `--ledger` and `--ask`) files a new proposal. Unread is
  `answers_unread`. Neither verb takes an answer or an ask id. An adoption is proposed by a
  session that does not own the instance, often a replacement of its owner. On approval the
  kernel records `Adopted`: completed steps stand, the former owner is refused every write, its
  claims are not transferred (`former_owners_claim`) and run out on their leases, and the new
  owner claims the next attempt. Any verified session may propose a retirement, the owner
  included. On approval the kernel records `Retired` and moves the instance to `retired/`, as
  the operator's retire does: the log is kept, and every later read or write is refused
  `retired`. A proposal belongs to the ownership it was filed under: once the instance changes
  owner (even back to the same owner) it is stale for good, every call says `proposal_stale`,
  and `--again` proposes anew. One session pins at most eight proposals on an instance
  (`proposals_exhausted`). `flow next` and `flow status` list each standing
  proposal (`proposals`: kind, ask, proposer, state) and the current `owner`.
- **The verifier pilot (BRO-3636).** `crates/flow/flows/verify.yaml` is `verify@1`: `pin`
  observes that the repository is at `params.head` (fresh for 10 minutes); `check` runs
  `params.check` in a snapshot of `params.head`, never in the working tree; and `exercise`, only
  when `params.surface` is set, takes the dogfood receipt its performer names. The snapshot is a
  new repository in a private temporary folder, a `git clone --shared --no-checkout` of the
  repository (read only, its objects shared, its history and tags kept) with the head checked out
  detached, removed once the check ends, whatever it came to, so the result depends on the head's
  content alone. Setup and the check share the run's one timeout. A snapshot that cannot be made
  is `snapshot_failed`, a check that moved the snapshot's HEAD off the head (even and back)
  `snapshot_moved`, and a snapshot commit that is not a full object id `snapshot_invalid`, never
  an exit status of the project's. The performer completes `exercise` with `--outputs '{"receipt": "<path>",
  "receipt_sha256": "sha256:<hex>"}'`. The kernel hands those outputs to the verify as JSON on its
  standard input, never in its argv, and records them, so the log pins the receipt's content and
  the receipt pins its evidence. The verify is `maestro dogfood verify-receipt --head=H
  --surface=S`, which reads a `dogfood-receipt@1` (`schema`, `head`, `surface`, `entry`, `driver`,
  `evidence` `{path, sha256}`, `result`, `at`; see `maestro_flow::dogfood`). It opens the receipt
  and the evidence without following a link and without blocking, and takes regular files only.
  It exits 0 only for a receipt that hashes to `receipt_sha256` and passes for that head and
  surface, with evidence that hashes to the receipt's digest. Each failure has its own exit status:
  3 missing, 4 unreadable, 5 malformed, 6 wrong head, 7 wrong surface, 8 not pass, 9 evidence
  unreadable, 10 evidence mismatch, 11 outputs invalid, 12 receipt changed, 13 receipt not a
  regular file, 14 evidence not a regular file. `{{outputs.<name>}}` in any command or the
  workdir is refused when a definition compiles.
- **Specialist agents (BRO-3636).** One source per specialist, `agents/<name>.agent.yaml`
  (`agent@1`: `schema`, `name`, `description`, `role`, `posture` (`read_only` or `execute`),
  `model` per provider (`claude`, `codex`, `gemini`, `grok`), `skills`, and `outputs`, a JSON
  Schema of type object), parsed strictly: an unknown or repeated field at any level is refused,
  and so is a name its file does not have (see `maestro_flow::agents`). `agents/verifier.agent.yaml`
  performs verify@1's `exercise`. `maestro agents render` writes `.claude/agents/<name>.md`,
  `.gemini/agents/<name>.md` and `.grok/agents/<name>.md` from every source and removes the
  renders whose source is gone (a file whose second line is not the render marker is never
  touched); the Codex `spawn_agent` brief is not a file but part of the driver skill and of the
  launch spec. `--check` writes nothing and exits 1 naming the first render that is missing,
  differs, or has no source: what `bstack doctor` will call. Each harness gets only the fields
  its docs define: Claude Code `name`, `description`, `tools`, `model`, `skills`; Gemini CLI
  `name`, `description`, `kind: local`, `tools`, `model`; Grok `name`, `description`, `tools`,
  `model`. **Not confirmed:** that Claude Code's `subagent_type` is the agent's `name` (inferred
  from its docs, not stated); any Grok field beyond those (its built-in agents use
  `prompt_mode`, `permission_mode` and `agents_md`, which no page defines, so none is set), how
  Grok skills reach an agent, and which of two same-named agents Grok takes (it reads
  `.claude/agents/` too); and Codex's `spawn_agent` arguments, read from its source, not its
  docs (`task_name` is only in its second version, and a Codex subagent always takes its
  parent's sandbox, so the posture is a sentence in the brief). Agent files shape behaviour; they
  are not a security boundary: the kernel's verify is the check.
- **Launch specs (BRO-3636).** `flow next` (and `maestro_flow_next`) gives each enabled node whose
  performer is `agent:<name>` a `launch_spec`: `kind: agent`, the agent, the node's skill, its
  source and digest, its output schema, `claude`, `gemini` and `grok` (each `{file, tool,
  subagent_type?}`: the `Agent` tool, the tool named after the agent, `spawn_subagent`) and
  `codex` (`{tool: spawn_agent, task_name, model, message}`). Other nodes have none, so their
  shape is unchanged. The kernel knows the sources it was built with, so a definition naming an
  agent with no source does not compile (`unknown_agent`): it is a reference to nothing, refused
  where the compiler refuses unknown references, before an instance that could never be launched
  is started, and with no file read at compile time. An instance pinned by a build that had the
  agent replays as it did, and `next` gives that node `kind: unknown_agent`, never no spec.
- **Driver skills (BRO-3636).** `maestro flow driver-skill` generates a flow's `SKILL.md` (Agent
  Skills format, named `flow-<flow>-v<version>`) from its compiled IR: loop `flow next`, do each
  enabled node with its own skill (an agent performer through the harness's own subagent, or
  Codex's brief), `flow complete`, and stop when `flow status` has no node `ready`, `stale` or
  `waiting`. Its steps put each node after everything it waits on (needs, the node its switch
  reads, a needed repeat's body), by id within a step; the loop's text is fixed, and nothing in
  the order is written by hand. `skills/flow-verify-v1/SKILL.md` is verify@1's, regenerated with
  `maestro flow driver-skill --definition crates/flow/flows/verify.yaml --out
  skills/flow-verify-v1/SKILL.md`; tests hold it, and the committed renders, to a fresh
  generation.
- **Commands an agent's `PATH` cannot stand in for (BRO-3636).** The kernel runs a command's
  first word `maestro` as its own executable, and `sh`, `git` and `test` as the system's files at
  their real path, owned by root and writable by no one else, as is every folder above them.
  Every command searches a fixed system `PATH`. The one exception is the project's own command in
  a snapshot, which needs the project's toolchain (cargo, bun, ...) and so searches the caller's
  `PATH`. Faking that toolchain takes the user-level write that can already replace Maestro's own
  binary, the residual risk the owner accepted in BRO-3635.
- **The accepting spine (BRO-3636).** `flow spine` (MCP `maestro_flow_spine`) is one record per
  node and iteration the kernel accepted, with the edge chosen and the exit status of the verify it
  ran, in log order. Refusals, repairs, leases, re-claims, attempts, performers and timestamps are
  left out, so two runs of one flow compare equal on it however often their agents were refused.
  A node re-run in its iteration (a lapsed observation, say) is one record, at the place of its
  first receipt, holding its latest result; so receipts A, B, then A again read as A, B.
  `--against` compares two runs of one definition and exits 1 with `spine_differs` and the first
  difference: a node skipped or reordered, another edge, or another exit. Two instances of
  different definitions are not compared: `definition_mismatch`, exit 3. It writes nothing.
- **Verified writers only.** The writes act for a verified session. On the CLI the claim is a
  Direct launch token (`MAESTRO_DIRECT_TOKEN` with `MAESTRO_CALLER_CWD`) or a bridge credential
  (`MAESTRO_BRIDGE_CREDENTIAL`); over MCP it is the arguments `cwd`, `token`, `node` and
  `credential`, which every write tool's schema declares. The registries that check it
  (`MAESTRO_DIRECT_REGISTRY`, `MAESTRO_PASEO_AGENTS_DIR`, `MAESTRO_T3_HOME`,
  `MAESTRO_BRIDGE_SECRET`) are the server's configuration, never an argument. The HTTP
  daemon (`maestro serve --http`) serves every caller on the machine, so it reads a claim only from
  the call's arguments, never from its own environment, for every agent tool. An unverified
  caller is refused (`caller_unverified`), and a claim that does not verify too (`caller_refused`).
  `next`, `status` and `spine` read for anyone and write nothing.
- **Where.** The ledger is `.broomva/flows` in the workspace (the same root every reader uses). A
  workspace with no `.broomva` folder (`workspace_unready`) or that is a linked git worktree
  (`workspace_is_worktree`) is refused, and only `start` makes the ledger; the other verbs say
  `no_ledger`. It is bound to this machine's host name.

Every command takes `--json` (`delegation brief` prints its markdown block either way). Agents should use `--json`: it is the same data the MCP tools return,
because the CLI and the MCP server share one dispatch.

By default Maestro reads the workspace at `origin/main`, because main's copy wins under the home rule.
`--tree worktree` reads the local checkout instead. An answer present locally but not on main is
reported as waiting to land, not as open and not as answered. If the local checkout itself cannot be
read, that count becomes a lower bound and the error is reported under `worktree:`.

- **Revisions.** A revision that does not resolve, or that starts with `-`, reads nothing. Every
  section is then unavailable, not empty. The MCP server resolves the revision again on each request,
  so a long-lived server follows `origin/main` as it moves.
- **HTTP.** `maestro serve --http` is meant to run once per machine (as `maestrod`, installed by node
  `deploy`); nothing stops a second instance on another port. A client on MCP 2026-07-28 is served
  statelessly; a client that opens with `initialize` gets a legacy session. It listens on loopback
  unless `--bind` names an address, and a named address must be loopback or on the tailnet
  (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`); `0.0.0.0`, `::` and every other address are refused.
  Requests must name the bound address or a loopback name in `Host` (so a tailnet client uses the
  IP, not the MagicDNS name), and a request carrying a browser `Origin` is refused. SIGTERM or
  Ctrl-C stops it within `SHUTDOWN_GRACE` (3 s), so a stalled client cannot hold it. A request body has no read deadline yet (BRO-3302 tracks it). `crates/mcp/tests/transports.rs` holds stdio and HTTP to the same JSON.
- **Parity with the Paseo plugin.** Asks are read the way the plugin reads them:
  - Fields go through `text`, `textsOf` and `optionsOf`, and a flag counts only when it is `=== true`.
    So a numeric id, a string `blocking`, a single-string `gates` list or an option without a label
    never drops an entry.
  - Ledgers (`.yaml` or `.yml`, as the plugin's `LEDGER` names them) are read **exactly as the
    plugin's `yaml` package reads them, within a YAML subset**: the YAML that ledger writers produce
    (`ask_ledger.py`, PyYAML, the plugin) over the values they hold: strings, numbers, booleans,
    null, lists and maps. A document outside the subset is **refused**, with the
    feature named (`outside the YAML subset Maestro reads exactly: …`), and is never misread. The owner
    chose this scope on 2026-10-20: yaml-rust2 itself disagrees with the package at its edges (block
    scalars, NUL, byte-versus-char positions), so no rule over it reaches full parity. A Rust port of
    the package's parser is the follow-up that would.
    - **Inside the subset:**
      - block and flow collections, comments;
      - plain and quoted scalars, single- and multi-line;
      - block scalars `|`, `|-`, `>`, `>-`;
      - anchors and aliases, counted by the package's `maxAliasCount` and resolved to the anchor's value;
      - a leading BOM and `---`, CRLF, any Unicode, literal NEL/LS/PS included (`JSON.stringify`
        leaves them literal), and a literal U+FEFF inside a double-quoted scalar (likewise);
      - surrogate escapes: a pair decodes, and a lone half reads as text outside double quotes.

      Scalars resolve by the package's core-schema rules: `0004` is 4, `0b101` a string, big integers
      round, `.inf` is `Infinity`. Keys stringify as `toJS` makes them, and duplicates are an error.
    - **Outside the subset:**
      - tabs, lone CRs, NUL and other control characters, invalid UTF-8, and a U+FEFF past the start
        anywhere but inside a double-quoted scalar;
      - directives, `...`, a later `---`;
      - tags and explicit `?` keys. No writer emits a tag: both write JSON strings, and PyYAML
        writes `!!binary` only for `bytes`, which no ledger holds;
      - an implicit key whose `:` is over 1024 UTF-16 units past its start, as the package measures
        it (refused in a flow mapping too, which the package reads);
      - `+` chomping and indentation digits;
      - whitespace-only lines, and a missing final newline;
      - a lone surrogate escape in a double-quoted scalar;
      - collection keys, named as such inside a flow sequence pair too;
      - nesting past 256 levels, or past 255 in flow style (yaml-rust2's own limit, refused as
        nesting, not as a parser error);
      - more than 500,000 values (`MAX_NODES`). An aliased anchor's value is counted three times:
        for itself, for its cached copy and for each alias's copy. So an aliased value holds about a
        third of the cap: a 300,000-value anchor with one alias is refused, though the plugin reads it.

      The full list, in the rules' own words, is the module doc of `crates/store/src/yaml.rs`.
    - Every real ledger is inside it: `~/broomva/.control/asks` reads 237 asks with 0 errors.
  - Ledger entries are JavaScript values (`crates/core/src/js.rs`), so a non-finite number keeps
    `text(Infinity)` as `Infinity` while it hashes as `null`. Entry hashes are `JSON.stringify` byte for
    byte, including JavaScript number printing and object key order.
  - The ticks ledger is read by its tail, cut where the plugin's `readTail` cuts it: the last
    `STREAM_TAIL` (2,097,152) UTF-16 units, from the first whole line. A tick that started before
    the tail is not read, by either, so `no_tick_receipts` is raised only when the tail is the
    whole ledger. `fixtures/parity/tail-golden.json` is the plugin's cut expression run under bun on
    seeded byte cases (`scripts/tail-golden.ts`); `scripts/parity-check.sh` fails if the pinned
    plugin's line, or the generator's copy of it, changes. `scripts/tail-bench.sh` holds the hook
    under 200 ms on a 100 MB ledger with the release binary.
  - Work flags come from the plugin's `parseEvents`, and lines are read as `JSON.parse` reads them.
    So a torn last line, a huge number or a lone surrogate never hides or resurrects a flag.
  - **Proof:**
    - `fixtures/parity/plugin-golden.json` is the plugin's own reading of a 136-ledger edge corpus,
      written by `scripts/plugin-golden.ts`.
    - The test `every_ledger_reads_as_the_plugin_reads_it` holds each ledger to one class: read to the
      byte, refused by both, or refused as outside the subset. The class sizes are pinned.
    - `scripts/parity-fuzz.py` is a differential fuzz against the plugin. Anything Maestro reads must
      match, and anything the plugin refuses Maestro must refuse. A ledger drawn from the subset
      grammar that the plugin reads must be read, so a reader refusing everything cannot pass.
    - `scripts/lock-parity.sh` runs the plugin's own lock and answer code (bun, same pin) against
      the built binary. On the requests journal it meets Maestro's own lock port: each waits for the
      other's lock, a dead holder's or a reused pid's old lock is reclaimed, and 20 writers beside 20
      plugin holds write 20 whole lines. On the ledger it meets the writer of record, which holds that
      lock: `LOCK_PARITY_WRITER=<ask_ledger.py>` runs the real one; CI runs the stub, which proves
      Maestro's wiring only.
    - `scripts/parity-check.sh` regenerates the golden from the plugin at the revision pinned in
      `fixtures/parity/plugin-rev` and fails on any difference; CI runs it. To move the pin, edit
      `plugin-rev`, run `scripts/parity-check.sh --write`, and review the diff.
- **Limits.** `maestro asks` returns `total` and `truncated` alongside the page (`--limit`, default
  50).
- **Delegation and economy.** `maestro_core::delegation` and `maestro_core::economy` port the
  workspace's `scripts/delegation.py` (assessment, tier ladder, plan-graph rules, briefs) and the pure
  part of `scripts/fleet_economy.py` (capacity forecast, cadence modes, launch and resume gates, the
  economy by tier, and BRO-3223's spendable-account readiness, balancer bounds and resume gate), as of
  workspace `3395842d0` (ws#1112). The Python is the reference until the tick moves here (node `loops-port`).
  **Parity is claimed inside a typed schema** (`maestro_core::schema`), the shapes the real inputs hold:
  graph ids, kinds, repos, `done`, tiers and every list item are strings; signal counts are JSON integers
  within 64 bits (not bools, not floats, not digit strings), and so are tier counts; flags are bools;
  usage, sample and balancer numbers are numbers; timestamps are UTC strings
  `YYYY-MM-DDTHH:MM:SS[.ffffff]` ending `Z` or `+00:00` (the form the usage endpoint and the samples
  write), or epoch numbers, inside `datetime`'s years 1..9999. Inside it, messages, rounding, exit
  codes and uncaught-error kinds are the Python's for the same arguments (a runway past `datetime`'s
  years raises the same kind and decides nothing). **Outside it Maestro refuses** before any
  rule runs, naming the field (`graph.nodes[1].signals.est_loc: an integer within 64 bits, not a string
  (outside the schema Maestro reads exactly)`), where the Python would go on with Python's semantics on
  arbitrary input (`1` and `True` one dict key, `str.strip`'s separators, `int()` of any digit string):
  `plan` and `brief` exit 1, `economy` exits 1, a policy outside it is unreadable (classification fails
  closed). **The readers are held to Python's by measurement.** A JSON input (a `.json` graph, usage,
  state, samples, the tier file) is refused where serde_json and `json.loads` build different values: an
  integer no 64-bit integer holds, a float past f64's range, `NaN`/`Infinity`/`-Infinity`, and the
  integer `-0` (serde_json reads -0.0). A graph or policy is read within a YAML subset, the rule
  `maestro_store::yaml` follows for asks. These are refused with the reason and line named: a document
  the subset check cannot scan; a tab; a CR outside CRLF; a control character; U+0085, U+2028 and U+2029;
  a byte-order mark; anchors and aliases, tags and merge keys; a plain scalar holding `?`;
  a plain scalar PyYAML (YAML 1.1) and serde_yaml (YAML 1.2) read differently (`yes`, `off`, `0250`,
  `0o17`, `1e3`, `1:20`, `2026-10-23`, `.inf`); and a mapping key that is not a string. Quote the value
  to keep it. Each rule is there because a differential fuzz (`yaml_differential`, `json_differential` in
  `scripts/parity-delegation.py`) found documents the readers misread without it. The claim is that every
  document the CLI reads, PyYAML or `json.loads` reads to the same value, types strict. It is measured on
  each run (about 75,000 fuzzed YAML and 53,000 fuzzed JSON documents near the real inputs) and held by
  `cargo test` on goldens of PyYAML's and Python's readings. A refusal is always allowed: the CLI also
  refuses some documents Python reads. Known differences inside both: the CLI flags follow the
  subcommand (`maestro delegation plan G --repo N=P`, not `--repo N=P plan G`); JSON is printed with sorted
  keys and two-space indent; a duplicate YAML key is refused where PyYAML keeps the last value; `repr`
  escapes what the reference Python's `str.isprintable` refuses, from its Unicode tables (16.0, Python
  3.14; `crates/core/src/py_printable.rs`, generated and checked by the harness); a document serde_yaml cannot parse
  (a `%` directive among them) stops with its `YAMLError`; `maestro economy capacity` refuses a `--usage` that is not a list; a flag value outside its choices, or one `int()` refuses, prints argparse's error line, not its usage block (a repeated flag takes its last value, every occurrence checked against its choices, and `int()`'s Unicode digits are read, as in argparse). Argv argparse accepts that clap does not is refused with clap's error (exit 2), never read differently: an abbreviated flag (`--est` for `--est-loc`), a value starting with `-` (`--ticket -1`, `--files -1_000`) except an ASCII negative integer given to an int flag, a global flag given to a subcommand that does not take it (`plan --no-policy`), and an integer past 64 bits. A graph repo path in `~user/` form is refused by name (exit 1); only `~` and `~/` expand. The sample cache, transcript reading
  and the economy ledger stay in the Python. The tier ladder equals the plugin's `DEFAULT_TIER_LADDER`
  (maestro-paseo `shared/tiers.ts`).
  - `fixtures/delegation/golden.json` is the Python's output on deterministic cases, graph validation
    with files on disk among them; the test `crates/core/tests/parity_delegation.rs` holds the Rust to
    it, and `scripts/mutation-check.sh` breaks the guards listed under "Ports of delegation.py". Its
    type cases give every graph, usage-row, sample, balancer-state and tier field a value of each type;
    a case outside the schema holds the field the harness's own reading of the schema refuses
    (`refusal` in `scripts/parity-delegation.py`), and the Rust must refuse that same field.
  - `fixtures/delegation/yaml11-golden.json` is PyYAML's implicit resolver on 6,733 plain scalars; the
    CLI's YAML 1.1 reading (`yaml11` in `crates/cli/src/port.rs`) is held to it by its unit test.
  - `fixtures/delegation/yaml-golden.json` (6,000 fuzzed documents with PyYAML's reading) and
    `json-golden.json` (about 4,000 with `json.loads`'s) are replayed by `cargo test`
    (`the_cli_reads_yaml_as_pyyaml_or_refuses`, `the_cli_reads_json_as_python_or_refuses`): every
    document the CLI reads must read to Python's value.
  - `fixtures/delegation/cli-golden.json` is the Python CLIs' output on scratch repos (bindings, nested,
    absent and same-directory repos, policies); `crates/cli/tests/delegation.rs` rebuilds the
    layout and replays every case through the binary.
  - `python3 scripts/parity-delegation.py --workspace ~/broomva` fails unless both goldens are the
    Python's output at the workspace's `origin/main`, the same cases agree through `maestro
    parity-eval`, both CLIs agree on every real `docs/plans/*.graph.yaml` and on the scratch-repo
    scenarios, both forecasts agree on the live usage snapshot, 100,000 random runways agree to the
    microsecond, and the plugin's `tiers.ts` at `fixtures/delegation/tiers-rev` agrees. `--write`
    regenerates both goldens: review their diff.

## The owner surface (MCP App)

`crates/view` renders Maestro for people. An agent composes a screen from catalog blocks, and
Maestro decides every value on it:

- `maestro_ui_catalog` lists the block types (`stack`, `snapshot`, `needs`, `asks`, `ask`, `work`,
  `loops`, `initiatives`, `fleet`, and the Decisions screens `decisions` and `decision`) and their
  props. The Decisions screens read only: Paseo's Maestro plugin still writes every answer. Props are handles and filters only (a ledger
  name, an ask id, a project, a state, a limit); no block carries text, numbers or markup.
- `maestro_ui_render { root, blocks }` validates the spec, places what it can and reports every drop
  with its reason (an unknown type, a prop the type does not take, a cycle, a missing or unreachable
  block, more than 64 blocks or 6 levels). It reads every value from a Store built for that call and
  renders HTML with maud. A source that could not be read renders as unavailable, never as 0 (an ask
  card on a ledger that exists but did not read says so, not "no ledger"). Lists without a `limit`
  (loops, initiatives, fleet) stop at 50 rows and say how many more there are. In an MCP
  Apps host the result opens `ui://maestro/view.html`, which shows it and polls the app-only
  `maestro_ui_poll` with its revision every 15 s while visible; a poll returns html only when the
  revision (sha256 of the html) changed. `format: "snapshot"` returns a standalone HTML document with
  no script as the text result, for a host without MCP Apps (T3's `html_render` in Claude threads).
- The App page is the foundation CSS (`design-system/broomva`), `crates/view/assets/view.css` and one
  script, `assets/bridge.js`: the official MCP Apps client (`@modelcontextprotocol/ext-apps` 2.0.3),
  built with bun from `crates/view/bridge/src`. The build is checked in; `scripts/bridge-check.sh`
  (in `ci.sh`) fails unless it is what bun 1.4.3 builds from the source, and the crate's test pins its
  sha256. The page's Content-Security-Policy allows no fetch and no script but that bridge, by its
  hash, so markup in a forged tool result runs nothing (the e2e checks it). There is no WebAssembly:
  T3's CSP blocks it.
- `today` is the Today screen (design page 10): a headline composed from one snapshot that names the
  commit read and its age, count pills (needs you, running, open asks, silent loops, landing), the
  top decision in queue order, work running now with the tier, model, thinking, harness, node and
  budget its frontmatter carries, and Nodes (unavailable until host adapters read a node census).
  Beside it, `health` (also a block on its own): the plan graphs (`docs/plans/*.graph.yaml`) whose
  ticket a live initiative links, with node state marked unavailable (it lives in Linear, which
  Maestro does not read yet); each loop's heartbeat (ok, drift, review overdue, no receipts, or not
  known when the ticks ledger did not read clean) with its last 24 ticks; the economy from core's
  capacity over the usage rows in `MAESTRO_USAGE`; and the last hour's tick receipts.
- `scripts/e2e.sh [view|decisions|today] [DIR]` runs the App in Playwright WebKit through a real MCP Apps host
  (the ext-apps `AppBridge`) against `maestro serve --http`: on the real ledgers at `origin/main`,
  and on a scratch copy of the fixture that it edits to prove the poll; it also sends a forged result
  whose inline handler must not run. It writes screenshots at 390
  and 1440 px, light and dark. It is not in `ci.sh` (it needs WebKit: `cd crates/view/bridge && bun
  install && bunx playwright install webkit`).

The catalog is a draft (`0-draft`) until node `catalog-freeze` publishes it as a versioned schema.

## Harness registration

| Harness | Registration |
|---|---|
| Claude Code (and T3 Code threads on the Claude provider) | `claude mcp add maestro -s local -- ~/.local/bin/maestro mcp` |
| Codex (and T3 Code threads on the Codex provider) | `[mcp_servers.maestro] command = "~/.local/bin/maestro"`, `args = ["mcp"]` in `~/.codex/config.toml` |
| Any MCP client | stdio: `maestro mcp`; Streamable HTTP: `http://127.0.0.1:7171/mcp` while `maestro serve --http` runs |

Context injection through a hook, for example Claude Code `SessionStart`:

```json
{ "type": "command", "command": "~/.local/bin/maestro context --format hook --stdin --event SessionStart" }
```

The hook path never fails a session. On any error it prints nothing and exits 0.

Before 2026-10-18 both registrations pointed at the T3 MCP proof of concept,
`bun apps/maestro-paseo-t3-mcp-ui/mcp/server.ts`. The Claude Code entry also passed `--demo`.

## Environment

| Variable | Default |
|---|---|
| `BROOMVA_WORKSPACE`, then `MAESTRO_WORKSPACE`, then `BSTACK_WORKSPACE` | `~/broomva` (the registry's workspace root; `--workspace` overrides) |
| `MAESTRO_TREE` | `origin/main` |
| `MAESTRO_WORK_ROOT` | `workRoot` from `$BROOMVA_HOME/config/maestro/config.json`, then from `~/.config/maestro-paseo/config.json`, else the registry's `machine.work` (`~/.broomva/work`, or the legacy `~/maestro` while only it exists) |
| `BROOMVA_HOME` | `~/.broomva` |
| `MAESTRO_FLEET_STATE` | `~/.local/state/fleet-reconcile` |
| `MAESTRO_PASEO_ASK_LOCKS` | `~/.local/state/control-asks/locks` (the plugin's ask-ledger lock root) |
| `MAESTRO_ASK_LEDGER` | `<workspace>/scripts/ask_ledger.py` (the writer `ask file` runs: `ask_ledger.py file`) |
| `MAESTRO_REQUESTS_WAIT_MS` | `30000`: how long a request waits for the journal lock (1–60000 ms) |
| `MAESTRO_SESSION` | none: the session agent requests name |
| `MAESTRO_USAGE` | unset: a file holding `provider_manager.py --json usage` output, which the economy reads; unset, the economy is unavailable |

Inside the workspace, each name is read from `.broomva/<name>` when the tree has it and from
`.control/<name>` otherwise, on disk and at a git revision alike; a tree holding both is a read
error for that name. Work under `$BROOMVA_HOME/work` is filed by kind,
`<initiative>/<slug>/_work.md` (`_unfiled` for none); any other work root is flat, `<slug>/_work.md`.

## Layout

| Directory | Package | Role |
|---|---|---|
| `crates/core` | `maestro-core` | Typed domain, no I/O: work states and transitions, asks, needs, loops and drift, ticks and due tiers, initiatives, grants, fleet, sessions namespaced by surface, surfaces, harnesses and nodes, authority, context packets, delegation tiers and plan graphs, the capacity economy |
| `crates/store` | `maestro-store` | Readers for the on-disk control plane (working tree or a git revision) and the shared views |
| `crates/flow` | `maestro-flow` | The flow ledger and its kernel: the one writer of `.broomva/flows` |
| `crates/mcp` | `maestro-mcp` | MCP server: read tools, agent request tools, the flow kernel's tools, the `maestro_context` prompt, and `maestro://` resources |
| `crates/cli` | `maestro` | The `maestro` binary |

## Develop

```bash
bash scripts/ci.sh             # the whole gate, as CI runs it (needs bun and network for parity)
cargo test                     # the suite alone
scripts/mutation-check.sh      # each guarded predicate's mutant must fail the suite (MUTANT_SHARD=i/N: every Nth)
python3 scripts/parity-delegation.py --workspace ~/broomva   # the delegation and economy ports against the workspace's Python
```

Tests run against `fixtures/machine/`, a miniature machine layout, so they never touch real ledgers.

Design system: `DESIGN.md` and `design-system/broomva/` are the Broomva foundation profile. Work-state
labels in `maestro-core::work::WorkState` are the canonical agentic-work vocabulary, and they are tested.
