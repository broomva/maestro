#!/usr/bin/env bash
# Vendor the path registry's generated Rust reader from a bstack checkout (BRO-3266).
# Copies scripts/lib/broomva_paths.rs and references/paths.toml from bstack's origin/main
# (or REV), byte for byte. `maestro doctor` and crates/store/tests/paths.rs tell a stale copy.
# Usage: scripts/vendor-paths.sh [bstack checkout, default ~/broomva/bstack] [rev, default origin/main]
set -euo pipefail
cd "$(dirname "$0")/.."
bstack=${1:-$HOME/broomva/bstack}
rev=${2:-origin/main}
git -C "$bstack" rev-parse --verify --quiet "$rev^{commit}" >/dev/null || { echo "no commit $rev in $bstack"; exit 2; }
git -C "$bstack" show "$rev:scripts/lib/broomva_paths.rs" >crates/store/src/broomva_paths.rs
git -C "$bstack" show "$rev:references/paths.toml" >fixtures/registry/paths.toml
echo "vendored bstack $(git -C "$bstack" rev-parse --short "$rev"): run bash scripts/ci.sh"
