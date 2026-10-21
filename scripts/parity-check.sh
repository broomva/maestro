#!/usr/bin/env bash
# Plugin parity: regenerate fixtures/parity/plugin-golden.json with the Paseo plugin's own
# code, at the revision pinned in fixtures/parity/plugin-rev, and fail if it differs from the
# committed golden. The Rust side is held to that golden by `every_ledger_reads_as_the_plugin_reads_it`:
# a ledger both read must read to the byte, and one the plugin refuses Maestro must refuse; a ledger
# outside Maestro's subset is only proven refused, its plugin reading is not compared.
# Moving the pin is deliberate: bump plugin-rev, rerun with --write, review the diff.
# Needs git, bun and network. Usage: scripts/parity-check.sh [--write]
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
rev=$(tr -d '[:space:]' <"$repo/fixtures/parity/plugin-rev")
# A commit, never a ref: a branch name would move under the pin and still pass.
[[ $rev =~ ^[0-9a-f]{40}$ ]] || { echo "parity    plugin-rev must be a 40-hex commit, got '$rev'"; exit 1; }
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

# The ticks tail: the plugin's readTail line and STREAM_TAIL, verbatim, and its cut on the seeded cases.
tail_line='return limit !== null && text.length > limit ? text.slice(text.indexOf("\n", text.length - limit) + 1) : text;'
for want in "$tail_line" 'const STREAM_TAIL = 2 * 1024 * 1024;'; do
  grep -qF -- "$want" "$work/plugin/server/loops.ts" \
    || { echo "parity    server/loops.ts no longer has: $want (port the change to read_tail)"; exit 1; }
done
bun "$repo/scripts/tail-golden.ts" >"$work/tail-golden.json"

if [ "${1:-}" = "--write" ]; then
  mv "$work/tail-golden.json" "$repo/fixtures/parity/tail-golden.json"
  mv "$work/golden.json" "$repo/fixtures/parity/plugin-golden.json"
  echo "parity    golden rewritten from plugin ${rev:0:12}"
elif ! cmp -s "$work/tail-golden.json" "$repo/fixtures/parity/tail-golden.json"; then
  echo "parity    tail golden DIFFERS from scripts/tail-golden.ts:"
  diff -u "$repo/fixtures/parity/tail-golden.json" "$work/tail-golden.json" | head -20
  exit 1
elif cmp -s "$work/golden.json" "$repo/fixtures/parity/plugin-golden.json"; then
  echo "parity    golden matches plugin ${rev:0:12}, tail golden matches its readTail"
else
  echo "parity    golden DIFFERS from plugin ${rev:0:12}:"
  diff -u "$repo/fixtures/parity/plugin-golden.json" "$work/golden.json" | head -60
  exit 1
fi
