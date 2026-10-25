#!/usr/bin/env bash
# The MCP App end to end in WebKit, one screen at a time: `e2e.sh [view|decisions|today] [DIR]`.
#   view       every block on the real ledgers (read-only, at origin/main), then a scratch copy
#              of the fixture that the run edits to prove the App's poll.
#   decisions  the Decisions inbox on the real ledgers and on the scratch fixture with
#              crates/view/tests/fixtures/decisions.yaml: every tab pressed, a row opened, in the
#              App and in the scriptless snapshot, at 390 px and 1440 px.
#   today      the Today screen and its health rail on the real ledgers and on the scratch
#              fixture with crates/view/tests/fixtures/today/ (a linked plan, usage rows,
#              running work, recent ticks), then the Fleet and Loops screens; the scratch run
#              edits a work item to prove the poll.
# Needs bun and Playwright's WebKit (`cd crates/view/bridge && bun install && bunx playwright
# install webkit`); not part of ci.sh, which runs no browser. Screenshots go to DIR (default:
# a temp dir, printed).
set -euo pipefail
cd "$(dirname "$0")/.."
screen=${1:-view}
case $screen in view | decisions | today) ;; *) echo "usage: e2e.sh [view|decisions|today] [DIR]" >&2; exit 2 ;; esac
out=${2:-$(mktemp -d)}
mkdir -p "$out"
cargo build -q --locked -p maestro
bin=${CARGO_TARGET_DIR:-$PWD/target}/debug/maestro
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

cat >"$scratch/decision-spec.json" <<'JSON'
{ "root": "d", "blocks": { "d": { "type": "decision", "props": { "ledger": "decisions.yaml", "id": "fleet-cost-1" } } } }
JSON
cat >"$scratch/decisions-spec.json" <<'JSON'
{ "root": "inbox", "blocks": { "inbox": { "type": "decisions" } } }
JSON

cat >"$scratch/today-spec.json" <<'JSON'
{ "root": "t", "blocks": { "t": { "type": "today" } } }
JSON
cat >"$scratch/screens-spec.json" <<'JSON'
{ "root": "main", "blocks": {
  "main": { "type": "stack", "children": ["fleet", "loops"] },
  "fleet": { "type": "fleet" },
  "loops": { "type": "loops" } } }
JSON

today=$PWD/crates/view/tests/fixtures/today
cd crates/view/bridge
if [ "$screen" = today ]; then
  mkdir -p "$scratch/workspace/docs/plans"
  cp "$today/plan.graph.yaml" "$scratch/workspace/docs/plans/2026-10-20-demo.graph.yaml"
  cp "$today/initiative.yaml" "$scratch/workspace/.control/initiatives/today-demo.yaml"
  cp -R "$today/work/." "$scratch/work/"
  cat "$today/ticks.jsonl" >>"$scratch/broomva-home/ledger/loops/ticks.jsonl"
  serve 7198
  echo "== today, real ledgers (origin/main, read-only)"
  mkdir -p "$out/real"
  bun run e2e.ts --url http://127.0.0.1:7198/mcp --spec "$scratch/today-spec.json" --out "$out/real" --today
  echo "== fleet and loops, real ledgers"
  mkdir -p "$out/real-screens"
  bun run e2e.ts --url http://127.0.0.1:7198/mcp --spec "$scratch/screens-spec.json" --out "$out/real-screens"
  echo "== today, scratch fixture with usage, live edit"
  MAESTRO_USAGE=$today/usage.json MAESTRO_FLEET_STATE=$scratch/fleet-reconcile \
    BROOMVA_HOME=$scratch/broomva-home MAESTRO_WORK_ROOT=$scratch/work \
    serve 7199 --workspace "$scratch/workspace" --tree worktree
  mkdir -p "$out/fixture"
  bun run e2e.ts --url http://127.0.0.1:7199/mcp --spec "$scratch/today-spec.json" --out "$out/fixture" --today \
    --edit "$scratch/work/today-run/_work.md" --from "# Round-3 fixes" --to "# Round-4 fixes" --expect "Round-4 fixes"
  echo "== fleet and loops, scratch fixture"
  mkdir -p "$out/fixture-screens"
  bun run e2e.ts --url http://127.0.0.1:7199/mcp --spec "$scratch/screens-spec.json" --out "$out/fixture-screens"
  echo "screenshots: $out"
  exit 0
fi
if [ "$screen" = decisions ]; then
  cp ../tests/fixtures/decisions.yaml "$scratch/workspace/.control/asks/decisions.yaml"
  serve 7198
  echo "== decisions, real ledgers (origin/main, read-only)"
  mkdir -p "$out/real"
  bun run e2e.ts --url http://127.0.0.1:7198/mcp --spec "$scratch/decisions-spec.json" --out "$out/real" --decisions
  echo "== decisions, scratch fixture"
  BROOMVA_HOME=$scratch/broomva-home MAESTRO_WORK_ROOT=$scratch/work serve 7199 --workspace "$scratch/workspace" --tree worktree
  mkdir -p "$out/fixture"
  bun run e2e.ts --url http://127.0.0.1:7199/mcp --spec "$scratch/decisions-spec.json" --out "$out/fixture" --decisions \
    --edit "$scratch/workspace/.control/asks/decisions.yaml" --from "label: \"A person merges them\"" \
    --to "label: \"A person merges each batch\"" --expect "A person merges each batch"
  echo "== decision, standalone, scratch fixture"
  mkdir -p "$out/decision"
  bun run e2e.ts --url http://127.0.0.1:7199/mcp --spec "$scratch/decision-spec.json" --out "$out/decision" --decision
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
