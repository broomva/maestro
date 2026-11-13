# Maestro: agent rules

## What this repo is

Typed governance metalayer: the `maestro` CLI and MCP server over the workspace control plane. See
`README.md` for commands and `docs/specs/2026-10-18-maestro-rust.html` for design and phases.

## Boundaries

- **One writer per ledger.** Maestro is read-only in phase 0. Do not add a write path to
  the asks or initiatives folders (`.broomva/`, legacy `.control/`), `.broomva/loops`, the work root or the loops ledger
  without the phase that transfers ownership. A second writer is the defect the BRO-3186 contract
  forbids. The one exception is the flow ledger (`.broomva/flows/` and the coordination files
  the ADR allows beside it), a new family with no prior
  writer. `crates/flow` is its only writer, and nothing else writes there
  (`docs/adrs/2026-11-06-flow-ledger.md`).
- **No owner verb on a model-visible surface** until an authenticated owner path exists.
  `crates/mcp/tests/dispatch.rs` enforces this, so keep it green and do not weaken it.
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
bash scripts/ci.sh   # fmt, clippy, tests, mutation check, plugin parity; CI runs it as the `ci` check,
                     # after sixteen `mutants` jobs split the mutation check (MUTANT_SHARD=i/N)
```

A behaviour change ships with a test. A guard, such as an authority check, a drift rule or a
degraded-source rule, ships with a test that fails when the guard is removed, and a line in
`scripts/mutation-check.sh` that proves it.

A PR touching one of the workspace policy's eight governance paths (here `CLAUDE.md`, `AGENTS.md`
and `.control/rcs-parameters.toml`; also `METALAYER.md`, `.control/policy.yaml` and three
`docs/references/` files, should a PR add one) runs the `stability-check` job
(`.github/workflows/l3-stability.yml`, BRO-3702). It runs bstack's scripts, pinned to one commit,
in a cleared environment bound to this checkout: `compute-lambda.sh` recomputes the RCS lambda
from this checkout's `.control/rcs-parameters.toml`, passed by path, and fails on drift, and
`l3-rate-gate.sh` reports the L3 rate. It runs `scripts/governance-check.py`: `CLAUDE.md` points
to this file, the shard count above equals the `ci.yml` mutants matrix, all eight paths trigger
the job and count toward the rate, and the files `CLAUDE.md` names trigger it too. The workspace
policy's governance tier needs this check to pass before p9 merges such a PR. The checker reads
YAML with mikefarah yq v4 and checks that the pinned `compute-lambda.sh` reads exactly the level
parameters it holds (locally, set `BSTACK` to a bstack checkout at the pin). Every refusal is a
registered guard, except the input readers' (a file or document the checker cannot read). Each
guard, disabled alone, fails its own case built from the live values. A completeness scan fails on
any refusal outside the registry. Its data tables (the governance paths, the refused characters and
byte classes, the mutants job's keys, the step order) are part of the reviewed contract: deleting
an entry removes its guard and its case together. A guard's predicate logic is verified by review,
not proven correct for all inputs. After changing the checker, run
`python3 scripts/governance-check.py --selftest` (the scan, then every case) and `--mutation-proof`
(the scan, then each guard disabled alone).

## Copy

Sentence case. No emoji, no em dashes, no title case. Follow `DESIGN.md` voice rules for any
user-facing text, including CLI output.
