#!/usr/bin/env bash
# The one gate: CI runs it, agents run it before a PR, and the `ci` node's done command
# runs it. Each step is a hard failure.
set -euo pipefail
cd "$(dirname "$0")/.."
step() { echo "== $*"; "$@"; }
step cargo fmt --all --check
step cargo clippy --workspace --all-targets --locked -- -D warnings
step cargo test --workspace --locked
step scripts/mutation-check.sh
step scripts/parity-check.sh
echo "== ci green"
