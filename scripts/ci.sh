#!/usr/bin/env bash
# The one gate: CI runs it, agents run it before a PR, and the `ci` node's done command
# runs it. Each step is a hard failure.
set -euo pipefail
cd "$(dirname "$0")/.."
# Run from a git hook, these would point the tests' fixture repos (and their commits) at the
# hook's repository.
unset GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES
step() { echo "== $*"; "$@"; }
step cargo fmt --all --check
step cargo clippy --workspace --all-targets --locked -- -D warnings
step cargo test --workspace --locked
# The workflow runs the mutants in shards before this job (BRO-3446) and sets
# MUTANTS_SHARDED=1 here only after every shard passed; run alone, this runs them all.
if [ "${MUTANTS_SHARDED:-}" = 1 ]; then echo "== mutation: ran in the workflow's shards"; else step scripts/mutation-check.sh; fi
step scripts/parity-check.sh
step scripts/lock-parity.sh
step scripts/tail-bench.sh
step scripts/bridge-check.sh
step scripts/acceptance/host-identity.sh --goldens --mutants --selftest
echo "== ci green"
