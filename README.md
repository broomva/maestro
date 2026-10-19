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
  - Ledgers (`.yaml` or `.yml`, as the plugin's `LEDGER` names them) are parsed as the plugin's
    `yaml` package parses them. This uses the YAML 1.2 core schema, with scalars resolved by the
    package's own rules (`crates/maestro-store/src/yaml.rs`):
    - `0004` is 4, `0b101` is a string and big integers round;
    - `!!float 1` is `"1"`;
    - keys stringify as `toJS` makes them, and duplicate keys are an error.
  - Entry hashes are `JSON.stringify` byte for byte, including JavaScript number printing and
    object key order.
  - Work flags come from the plugin's `parseEvents`, and lines are read as `JSON.parse` reads them.
    So a torn last line, a huge number or a lone surrogate never hides or resurrects a flag.
  - The proof is `fixtures/parity/plugin-golden.json`: the plugin's own reading of an edge corpus,
    written by `scripts/plugin-golden.ts`. The test `every_ledger_reads_as_the_plugin_reads_it`
    holds it to the byte. Regenerate the file whenever the plugin's readers change.
  - Known gaps. Each is refused or reported, and each is pinned by that test:
    - A `%YAML 1.1` document is refused, because the plugin would read it with YAML 1.1 rules.
    - A non-finite number (`.inf`, `1e400`) hashes the same, but reads as no text. So
      `answer: .inf` is answered in the plugin and open here.
    - A lone surrogate escape is read as U+FFFD and reported, so that entry's hash differs.
    - A collection map key that needs quoting in flow form is refused.
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
