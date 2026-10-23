#!/usr/bin/env bash
# Lock parity: Maestro's port of the plugin's §5.5 lock against the plugin's own code
# (server/locks.ts and its answer path), at the revision pinned in fixtures/parity/plugin-rev.
# See scripts/lock-parity.ts for the four checks. Needs git, bun, cargo and network;
# MAESTRO_PASEO_DIR=<checkout at the pin> skips the fetch.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
rev=$(tr -d '[:space:]' <"$repo/fixtures/parity/plugin-rev")
[[ $rev =~ ^[0-9a-f]{40}$ ]] || { echo "lock-parity plugin-rev must be a 40-hex commit, got '$rev'"; exit 1; }
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
trap 'exit 130' INT TERM

if [ -n "${MAESTRO_PASEO_DIR:-}" ]; then
  plugin=$MAESTRO_PASEO_DIR
  [ "$(git -C "$plugin" rev-parse HEAD)" = "$rev" ] || { echo "lock-parity $plugin is not at $rev"; exit 1; }
  driver="$plugin/.lock-parity-$$.ts"
  trap 'rm -rf "$work" "$driver"' EXIT
else
  url=${MAESTRO_PASEO_URL:-https://github.com/broomva/maestro-paseo}
  plugin=$work/plugin
  git init -q "$plugin"
  git -C "$plugin" fetch -q --depth 1 "$url" "$rev"
  git -C "$plugin" checkout -q FETCH_HEAD
  (cd "$plugin" && bun install --frozen-lockfile >"$work/install.log" 2>&1) || { cat "$work/install.log"; exit 1; }
  driver="$plugin/.lock-parity.ts"
fi

(cd "$repo" && cargo build --locked -q -p maestro)
target=$(cd "$repo" && cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
cp "$repo/scripts/lock-parity.ts" "$driver"
mkdir -p "$work/scratch"
(cd "$plugin" && bun "$driver" "$target/debug/maestro" "$repo/fixtures/requests" "$work/scratch")
