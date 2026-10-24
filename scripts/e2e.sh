#!/usr/bin/env bash
# The MCP App end to end in WebKit, one screen at a time: `e2e.sh [view|decisions] [DIR]`.
#   view       every block on the real ledgers (read-only, at origin/main), then a scratch copy
#              of the fixture that the run edits to prove the App's poll.
#   decisions  the Decisions inbox on the real ledgers and on the scratch fixture with
#              crates/view/tests/fixtures/decisions.yaml: every tab pressed, a row opened, in the
#              App and in the scriptless snapshot, at 390 px and 1440 px.
# Needs bun and Playwright's WebKit (`cd crates/view/bridge && bun install && bunx playwright
# install webkit`); not part of ci.sh, which runs no browser. Screenshots go to DIR (default:
# a temp dir, printed).
set -euo pipefail
cd "$(dirname "$0")/.."
screen=${1:-view}
case $screen in view | decisions) ;; *) echo "usage: e2e.sh [view|decisions] [DIR]" >&2; exit 2 ;; esac
out=${2:-$(mktemp -d)}
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

cat >"$scratch/decisions-spec.json" <<'JSON'
{ "root": "inbox", "blocks": { "inbox": { "type": "decisions" } } }
JSON

cd crates/view/bridge
if [ "$screen" = decisions ]; then
  cp ../tests/fixtures/decisions.yaml "$scratch/workspace/.control/asks/decisions.yaml"
  serve 7198
  echo "== decisions, real ledgers (origin/main, read-only)"
  mkdir -p "$out/real"
  bun run e2e.ts --url http://127.0.0.1:7198/mcp --spec "$scratch/decisions-spec.json" --out "$out/real" --decisions
  echo "== decisions, scratch fixture"
  BROOMVA_HOME=$scratch/broomva-home MAESTRO_WORK_ROOT=$scratch/work serve 7199 --workspace "$scratch/workspace" --tree worktree
  mkdir -p "$out/fixture"
  bun run e2e.ts --url http://127.0.0.1:7199/mcp --spec "$scratch/decisions-spec.json" --out "$out/fixture" --decisions
  echo "screenshots: $out"
  exit 0
fi
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
