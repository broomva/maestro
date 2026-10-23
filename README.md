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
  the readers' 1 MiB limit, and a ledger that does not exist or is a symlink. Exit 75 (the lock was busy)
  is reported as busy, to retry. `crates/store/src/locks.rs` (the same lock, ported) guards the
  requests journal.
- **Unverified callers.** Until sessions are verified against their surface (BRO-3268), every caller is
  unverified: an ask is filed with `origin: {via: maestro, session, verified: false}` and no `agent_id`,
  so no answer is routed into a session on a caller's word; every other request is held.
- **Not a trust boundary.** The CLI takes its workspace, home and validator from the caller's
  environment (`BROOMVA_WORKSPACE`, `MAESTRO_ASK_LEDGER`): it files for an agent that could write the
  file itself, and it records that agent as unverified. Verification is the surfaces' (BRO-3268).

Every command takes `--json`. Agents should use `--json`: it is the same data the MCP tools return,
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
      the built binary: each waits for the other's lock, a dead holder's old lock is reclaimed, and
      20 parallel filings beside 5 plugin answers lose nothing.
    - `scripts/parity-check.sh` regenerates the golden from the plugin at the revision pinned in
      `fixtures/parity/plugin-rev` and fails on any difference; CI runs it. To move the pin, edit
      `plugin-rev`, run `scripts/parity-check.sh --write`, and review the diff.
- **Limits.** `maestro asks` returns `total` and `truncated` alongside the page (`--limit`, default
  50).

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
| `MAESTRO_SESSION` | none: the session agent requests name |

Inside the workspace, each name is read from `.broomva/<name>` when the tree has it and from
`.control/<name>` otherwise, on disk and at a git revision alike; a tree holding both is a read
error for that name. Work under `$BROOMVA_HOME/work` is filed by kind,
`<initiative>/<slug>/_work.md` (`_unfiled` for none); any other work root is flat, `<slug>/_work.md`.

## Layout

| Directory | Package | Role |
|---|---|---|
| `crates/core` | `maestro-core` | Typed domain, no I/O: work states and transitions, asks, needs, loops and drift, ticks and due tiers, initiatives, grants, fleet, sessions namespaced by surface, surfaces, harnesses and nodes, authority, context packets |
| `crates/store` | `maestro-store` | Readers for the on-disk control plane (working tree or a git revision) and the shared views |
| `crates/mcp` | `maestro-mcp` | MCP server: read tools, agent request tools, the `maestro_context` prompt, and `maestro://` resources |
| `crates/cli` | `maestro` | The `maestro` binary |

## Develop

```bash
bash scripts/ci.sh             # the whole gate, as CI runs it (needs bun and network for parity)
cargo test                     # the suite alone
scripts/mutation-check.sh      # each guarded predicate's mutant must fail the suite
```

Tests run against `fixtures/machine/`, a miniature machine layout, so they never touch real ledgers.

Design system: `DESIGN.md` and `design-system/broomva/` are the Broomva foundation profile. Work-state
labels in `maestro-core::work::WorkState` are the canonical agentic-work vocabulary, and they are tested.
