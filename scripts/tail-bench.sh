#!/usr/bin/env bash
# The hook's budget on a long ticks ledger (BRO-3215): a 100 MB ledger whose last lines are the
# fixture's must give the fixture's context, in under 200 ms, from the release binary the
# harnesses run. The best of three warm runs is judged, as a hook after the ledger's last write
# finds it in the page cache. Usage: scripts/tail-bench.sh
set -euo pipefail
cd "$(dirname "$0")/.."
budget_ms=200
cargo build -q --release --locked -p maestro
bin=$PWD/target/release/maestro
fx=$PWD/fixtures/machine
home=$(mktemp -d)
trap 'rm -rf "$home"' EXIT
mkdir -p "$home/ledger/loops"
python3 - "$home/ledger/loops/ticks.jsonl" "$fx/broomva-home/ledger/loops/ticks.jsonl" <<'PY'
import sys
filler = ('{"actor":"agent:aaaa1111","cause":null,"data":{},"id":"F","prev":"sha256:0","refs":{},'
          '"stream":"loops/ticks","subject":"loop:filler","ts":"2026-09-01T00:00:00Z","type":"loop.tick.started","v":1}\n')
with open(sys.argv[1], "w") as out:
    out.write(filler * (100 * 1024 * 1024 // len(filler) + 1))
    out.write(open(sys.argv[2]).read())
PY
hook() {
  MAESTRO_WORKSPACE=$fx/workspace MAESTRO_WORK_ROOT=$fx/work MAESTRO_FLEET_STATE=$fx/fleet-reconcile \
    MAESTRO_TREE=worktree BROOMVA_HOME=$1 "$bin" context --format hook --cwd "$fx/workspace/apps/demo"
}
# A cut tail claims no missing receipts, so that one line is the only difference allowed.
receipts='\n- `silent`: the contract names tick receipts and the ledger holds none for this loop'
# The hook prints nothing on any failure, so an empty context must not pass as equal: the
# fixture's own context must hold its loop and the receipts line before that line is removed.
fixture=$(hook "$fx/broomva-home")
for want in '`demo-tick`' "$receipts"; do
  python3 -c 'import sys; sys.exit(sys.argv[2] not in sys.argv[1])' "$fixture" "$want" \
    || { echo "tail-bench fixture context lacks: $want"; exit 1; }
done
want=$(python3 -c 'import sys; print(sys.argv[1].replace(sys.argv[2], "", 1), end="")' "$fixture" "$receipts")
best=
for _ in 1 2 3; do
  start=$(python3 -c 'import time; print(time.monotonic_ns())')
  got=$(hook "$home")
  end=$(python3 -c 'import time; print(time.monotonic_ns())')
  [ "$got" = "$want" ] || { echo "tail-bench context differs from the fixture's"; diff <(echo "$want") <(echo "$got") | head; exit 1; }
  ms=$(( (end - start) / 1000000 ))
  [ -z "$best" ] || [ "$ms" -lt "$best" ] && best=$ms
done
size=$(wc -c <"$home/ledger/loops/ticks.jsonl" | tr -d ' ')
if [ "$best" -lt "$budget_ms" ]; then
  echo "tail-bench $size-byte ledger: hook in $best ms (budget $budget_ms ms), context matches"
else
  echo "tail-bench $size-byte ledger: hook took $best ms, over the $budget_ms ms budget"; exit 1
fi
