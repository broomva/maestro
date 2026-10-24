#!/usr/bin/env bash
# The MCP App end to end in WebKit: real ledgers (read-only, at origin/main) and a scratch
# copy of the fixture that the run edits to prove the App's poll. Needs bun and Playwright's
# WebKit (`cd crates/view/bridge && bun install && bunx playwright install webkit`); not part
# of ci.sh, which runs no browser. Screenshots go to $1 (default: a temp dir, printed).
set -euo pipefail
cd "$(dirname "$0")/.."
out=${1:-$(mktemp -d)}
mkdir -p "$out"
cargo build -q --locked -p maestro
bin=$PWD/target/debug/maestro
scratch=$(mktemp -d)
pids=()
trap 'kill "${pids[@]}" 2>/dev/null || true; rm -rf "$scratch"' EXIT
cp -R fixtures/machine/. "$scratch/"

serve() { # port, then maestro's global args
  local port=$1; shift
  "$bin" "$@" serve --http --bind "127.0.0.1:$port" >"$scratch/serve-$port.log" 2>&1 &
  pids+=($!)
  for _ in $(seq 50); do curl -s -o /dev/null "http://127.0.0.1:$port/mcp" && return; sleep 0.1; done
  echo "maestro did not listen on $port"; cat "$scratch/serve-$port.log"; exit 1
}

cat >"$scratch/spec.json" <<'JSON'
{ "root": "main", "blocks": {
  "main": { "type": "stack", "children": ["snap", "needs", "asks", "loops", "init"] },
  "snap": { "type": "snapshot" },
  "needs": { "type": "needs" },
  "asks": { "type": "asks", "props": { "blocking": true, "limit": 5 } },
  "loops": { "type": "loops", "props": { "drifting": true } },
  "init": { "type": "initiatives" } } }
JSON
cat >"$scratch/fixture-spec.json" <<'JSON'
{ "root": "main", "blocks": {
  "main": { "type": "stack", "children": ["snap", "ask", "work"] },
  "snap": { "type": "snapshot" },
  "ask": { "type": "ask", "props": { "ledger": "demo-arc.yaml", "id": "merge-now" } },
  "work": { "type": "work" } } }
JSON

cd crates/view/bridge
serve 7198
echo "== real ledgers (origin/main, read-only)"
mkdir -p "$out/real"
bun run e2e.ts --url http://127.0.0.1:7198/mcp --spec "$scratch/spec.json" --out "$out/real"

echo "== scratch fixture, live edit"
BROOMVA_HOME=$scratch/broomva-home MAESTRO_WORK_ROOT=$scratch/work serve 7199 --workspace "$scratch/workspace" --tree worktree
mkdir -p "$out/fixture"
bun run e2e.ts --url http://127.0.0.1:7199/mcp --spec "$scratch/fixture-spec.json" --out "$out/fixture" \
  --edit "$scratch/workspace/.control/asks/demo-arc.yaml" --from "label: Merge now" --to "label: Merge after the poll" --expect "Merge after the poll"
echo "screenshots: $out"
