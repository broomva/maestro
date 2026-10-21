# Maestro: agent rules

## What this repo is

Typed governance metalayer: the `maestro` CLI and MCP server over the workspace control plane. See
`README.md` for commands and `docs/specs/2026-10-18-maestro-rust.html` for design and phases.

## Boundaries

- **One writer per ledger.** Maestro is read-only in phase 0. Do not add a write path to
  `.control/asks`, `.control/initiatives`, `.broomva/loops`, the work root or the loops ledger
  without the phase that transfers ownership. A second writer is the defect the BRO-3186 contract
  forbids.
- **No owner verb on a model-visible surface** until an authenticated owner path exists.
  `maestro-mcp/tests/dispatch.rs` enforces this, so keep it green and do not weaken it.
- **Unread is not empty.** Every reader returns a `Section` with its source and errors. Never collapse
  a failed read into an empty list.
- **Validator of record.** `scripts/ask_ledger.py` in the workspace validates asks. Do not
  reimplement its rules here. `maestro asks validate` delegates to it.
- **Vocabulary.** Work-state labels are exact: `Queued`, `Running`, `Stuck`, `Needs you`, `Done`,
  `Standing`. `Needs you` is used only for a concrete hold on the owner.
- **The fleet skill acts, Maestro reads.** Do not send agent mail or spawn sessions from here. That
  stays in the fleet skill until a host adapter phase lands.

## Gates

```bash
bash scripts/ci.sh   # fmt, clippy, tests, mutation check, plugin parity; CI runs it as the `ci` check
```

A behaviour change ships with a test. A guard, such as an authority check, a drift rule or a
degraded-source rule, ships with a test that fails when the guard is removed, and a line in
`scripts/mutation-check.sh` that proves it.

## Copy

Sentence case. No emoji, no em dashes, no title case. Follow `DESIGN.md` voice rules for any
user-facing text, including CLI output.
