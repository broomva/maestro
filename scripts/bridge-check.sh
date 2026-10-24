#!/usr/bin/env bash
# The MCP App bridge pin: crates/view/assets/bridge.js must be exactly what bun builds from
# crates/view/bridge/src at the locked dependencies (the view crate's test pins its sha256).
# The build depends on bun's bundler, so the check runs with the bun CI pins (.github/workflows/ci.yml).
# Needs bun and network. Rebuild after a change: cd crates/view/bridge && bun install && bun run build
set -euo pipefail
cd "$(dirname "$0")/../crates/view/bridge"
want=$(sed -n 's/^ *bun-version: *//p' ../../../.github/workflows/ci.yml)
have=$(bun --version)
[ "$have" = "$want" ] || { echo "bridge    bun $have, but the bridge is built with bun $want"; exit 1; }
bun install --frozen-lockfile --production >/dev/null
bun run build.ts --check
