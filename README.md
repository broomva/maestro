# Maestro

Maestro is the governance and orchestration metalayer for Broomva's agent fleet. It types what the
fleet already runs on (asks, decisions, work, loops, ticks, initiatives, grants and fleet reports)
and exposes it the same way to every agent harness: a `maestro` CLI and an MCP server.

Maestro sits on top of the existing substrate. It does not replace any of it:

- the fleet skill keeps census, classification and agent-to-agent mail;
- bstack keeps the hooks, gates and primitives;
- `.control/` and `.broomva/loops/` stay the source of truth.

Maestro reads those, computes the views every surface shares (needs, loop drift, context packets),
and injects that context into agent sessions.

This is the Rust successor to the Paseo plugin (`broomva/maestro-paseo`) and the earlier TypeScript
app (archived as `broomva/maestro-legacy`). Design and phases: [`docs/specs/2026-10-18-maestro-rust.html`](docs/specs/2026-10-18-maestro-rust.html).

## Install

```bash
cargo install --locked --path crates/maestro-cli --root ~/.local   # installs ~/.local/bin/maestro
```

## Use

```bash
maestro snapshot                     # counts across the control plane, with read health per section
maestro needs                        # holds on the owner: the "Needs you" list
maestro asks [--project P] [--blocking] [--state open|answered|archived|all]
maestro asks show <ledger.yaml> <id> # one ask in full, with its content hash
maestro asks validate <ledger.yaml>  # runs the workspace's ask_ledger.py, the validator of record
maestro work [--state "Needs you"]   # canonical states: Queued, Running, Stuck, Needs you, Done, Standing
maestro loops [--drifting]           # loop registry joined with tick receipts: drift and notices
maestro initiatives [--all]
maestro fleet
maestro context [--cwd D] [--format markdown|json|hook]
maestro authority                    # what each surface may do, and the standing invariants
maestro hosts                        # known harness capabilities
maestro mcp                          # MCP server on stdio
```

Every command takes `--json`. Agents should use `--json`: it is the same data the MCP tools return,
because the CLI and the MCP server share one dispatch.

By default Maestro reads the workspace at `origin/main`, because main's copy wins under the home rule.
`--tree worktree` reads the local checkout instead. An answer present locally but not on main is
reported as waiting to land, not as open and not as answered. If the local checkout itself cannot be
read, that count becomes a lower bound and the error is reported under `worktree:`.

- **Revisions.** A revision that does not resolve, or that starts with `-`, reads nothing. Every
  section is then unavailable, not empty. The MCP server resolves the revision again on each request,
  so a long-lived server follows `origin/main` as it moves.
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
        leaves them literal);
      - surrogate escapes: a pair decodes, and a lone half reads as text outside double quotes.

      Scalars resolve by the package's core-schema rules: `0004` is 4, `0b101` a string, big integers
      round, `.inf` is `Infinity`. Keys stringify as `toJS` makes them, and duplicates are an error.
    - **Outside the subset:**
      - tabs, lone CRs, NUL and other control characters, invalid UTF-8;
      - directives, `...`, a later `---`;
      - tags and explicit `?` keys. No writer emits a tag: both write JSON strings, and PyYAML
        writes `!!binary` only for `bytes`, which no ledger holds;
      - an implicit key whose `:` is over 1024 UTF-16 units past its start, as the package measures
        it (refused in a flow mapping too, which the package reads);
      - `+` chomping and indentation digits;
      - whitespace-only lines, and a missing final newline;
      - a lone surrogate escape in a double-quoted scalar;
      - collection keys;
      - nesting past 256 levels.

      The full list, in the rules' own words, is the module doc of `crates/maestro-store/src/yaml.rs`.
    - Every real ledger is inside it: `~/broomva/.control/asks` reads 237 asks with 0 errors.
  - Ledger entries are JavaScript values (`maestro-core/src/js.rs`), so a non-finite number keeps
    `text(Infinity)` as `Infinity` while it hashes as `null`. Entry hashes are `JSON.stringify` byte for
    byte, including JavaScript number printing and object key order.
  - Work flags come from the plugin's `parseEvents`, and lines are read as `JSON.parse` reads them.
    So a torn last line, a huge number or a lone surrogate never hides or resurrects a flag.
  - **Proof:**
    - `fixtures/parity/plugin-golden.json` is the plugin's own reading of a 107-ledger edge corpus,
      written by `scripts/plugin-golden.ts`.
    - The test `every_ledger_reads_as_the_plugin_reads_it` holds each ledger to one class: read to the
      byte, refused by both, or refused as outside the subset. The class sizes are pinned.
    - `scripts/parity-fuzz.py` is a differential fuzz against the plugin. Anything Maestro reads must
      match, and anything the plugin refuses Maestro must refuse. A ledger drawn from the subset
      grammar that the plugin reads must be read, so a reader refusing everything cannot pass.
    - Regenerate the golden file whenever the plugin's readers change.
- **Limits.** `maestro asks` returns `total` and `truncated` alongside the page (`--limit`, default
  50).

## Harness registration

| Harness | Registration |
|---|---|
| Claude Code (and T3 Code threads on the Claude provider) | `claude mcp add maestro -s local -- ~/.local/bin/maestro mcp` |
| Codex (and T3 Code threads on the Codex provider) | `[mcp_servers.maestro] command = "~/.local/bin/maestro"`, `args = ["mcp"]` in `~/.codex/config.toml` |
| Any MCP client | stdio: `maestro mcp` |

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
| `MAESTRO_WORKSPACE` | `~/broomva` |
| `MAESTRO_TREE` | `origin/main` |
| `MAESTRO_WORK_ROOT` | `workRoot` from `~/.config/maestro-paseo/config.json`, else `~/maestro` |
| `BROOMVA_HOME` | `~/.broomva` |
| `MAESTRO_FLEET_STATE` | `~/.local/state/fleet-reconcile` |

## Layout

| Crate | Role |
|---|---|
| `maestro-core` | Typed domain, no I/O: work states and transitions, asks, needs, loops and drift, ticks and due tiers, initiatives, grants, fleet, host-namespaced sessions, authority, context packets |
| `maestro-store` | Readers for the on-disk control plane (working tree or a git revision) and the shared views |
| `maestro-mcp` | MCP server: read tools, the `maestro_context` prompt, and `maestro://` resources |
| `maestro-cli` | The `maestro` binary |

## Develop

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Tests run against `fixtures/machine/`, a miniature machine layout, so they never touch real ledgers.

Design system: `DESIGN.md` and `design-system/broomva/` are the Broomva foundation profile. Work-state
labels in `maestro-core::work::WorkState` are the canonical agentic-work vocabulary, and they are tested.
