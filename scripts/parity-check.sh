#!/usr/bin/env bash
# Plugin parity: regenerate fixtures/parity/plugin-golden.json with the Paseo plugin's own
# code, at the revision pinned in fixtures/parity/plugin-rev, and fail if it differs from the
# committed golden. The Rust side is held to that golden by `every_ledger_reads_as_the_plugin_reads_it`,
# so together they prove Maestro reads every parity ledger as the plugin does.
# Moving the pin is deliberate: bump plugin-rev, rerun with --write, review the diff.
# Needs git, bun and network. Usage: scripts/parity-check.sh [--write]
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
rev=$(tr -d '[:space:]' <"$repo/fixtures/parity/plugin-rev")
url=${MAESTRO_PASEO_URL:-https://github.com/broomva/maestro-paseo}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
trap 'exit 130' INT TERM

git init -q "$work/plugin"
git -C "$work/plugin" fetch -q --depth 1 "$url" "$rev"
git -C "$work/plugin" checkout -q FETCH_HEAD
(cd "$work/plugin" && bun install --frozen-lockfile >"$work/install.log" 2>&1) || { cat "$work/install.log"; exit 1; }
cp "$repo/scripts/plugin-golden.ts" "$work/plugin/.golden.ts"
# The plugin's yaml package warns on stderr for collection keys; the golden is stdout only.
(cd "$work/plugin" && bun .golden.ts "$repo/fixtures/parity/workspace/.control/asks") >"$work/golden.json" 2>"$work/stderr.log" \
  || { cat "$work/stderr.log"; exit 1; }

if [ "${1:-}" = "--write" ]; then
  mv "$work/golden.json" "$repo/fixtures/parity/plugin-golden.json"
  echo "parity    golden rewritten from plugin ${rev:0:12}"
elif cmp -s "$work/golden.json" "$repo/fixtures/parity/plugin-golden.json"; then
  echo "parity    golden matches plugin ${rev:0:12}"
else
  echo "parity    golden DIFFERS from plugin ${rev:0:12}:"
  diff -u "$repo/fixtures/parity/plugin-golden.json" "$work/golden.json" | head -60
  exit 1
fi
