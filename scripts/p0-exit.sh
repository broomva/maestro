#!/usr/bin/env bash
# P0 exit gate (BRO-1798) — runs the ROADMAP §P0 exit test verbatim and captures
# evidence. The exit test:
#   "the binary runs and serves a health route; the SPA renders tokens correctly
#    light + dark."
#
# Four checks, in order:
#   1. the runtime compiles to a single binary and serves /health with {"ok":true}
#   2. the SPA builds and its M0 token checks pass (bun static + playwright light/dark)
#   3. bstack governance is green (make control-audit = bstack doctor, non-strict)
#   4. evidence captured under dist/p0-exit/ (health.json + light/dark screenshots)
#
# A LOCAL gate: step 2's Playwright needs a browser (no browser in CI), so this is
# run once to close P0 and its evidence is attached to the PR — not a per-PR CI job.
#
# Usage: bash scripts/p0-exit.sh   (or: make p0-exit)
set -euo pipefail

cd "$(dirname "$0")/.."
PORT="${MAESTRO_PORT:-4319}"
EVIDENCE="dist/p0-exit"
mkdir -p "$EVIDENCE"

pass() { printf '\033[32m  ✓ %s\033[0m\n' "$1"; }
fail() { printf '\033[31m  ✗ %s\033[0m\n' "$1"; exit 1; }

SRV_PID=""
cleanup() { [ -n "$SRV_PID" ] && kill "$SRV_PID" 2>/dev/null || true; }
trap cleanup EXIT

echo "== P0 exit · 1/4 · runtime binary serves /health =="
bun build --compile apps/runtime/src/index.ts --outfile dist/maestro
MAESTRO_PORT="$PORT" ./dist/maestro &
SRV_PID=$!
# Wait for the socket to accept (compiled cold-start), up to ~9s.
for _ in $(seq 1 30); do
  curl -sf "localhost:$PORT/health" >/dev/null 2>&1 && break
  sleep 0.3
done
# Guard against a stray listener masking a bind failure: the /health we grade must
# be served by OUR freshly-compiled binary, not a leftover instance on the port.
kill -0 "$SRV_PID" 2>/dev/null || fail "runtime exited before serving (port $PORT already in use, or a crash)"
curl -sf "localhost:$PORT/health" >"$EVIDENCE/health.json" || fail "runtime did not serve /health on :$PORT"
grep -q '"ok":true' "$EVIDENCE/health.json" || fail "/health did not report ok:true"
pass "runtime serves /health → $(cat "$EVIDENCE/health.json")"

echo "== P0 exit · 2/4 · SPA renders tokens light + dark =="
bun run --filter @maestro/app test:m0 || fail "app M0 token checks failed (build + static + playwright)"
cp apps/app/test-results/m0-light.png "$EVIDENCE/" 2>/dev/null || fail "missing light screenshot"
cp apps/app/test-results/m0-dark.png "$EVIDENCE/" 2>/dev/null || fail "missing dark screenshot"
pass "SPA builds; M0 checks pass; light + dark screenshots captured"

echo "== P0 exit · 3/4 · governance green =="
make control-audit >"$EVIDENCE/doctor.txt" 2>&1 || fail "bstack doctor (make control-audit) failed"
pass "$(grep -oE '\[bstack doctor\][^—]*' "$EVIDENCE/doctor.txt" | tail -1 | sed 's/  */ /g')"

echo "== P0 exit · 4/4 · evidence =="
ls -1 "$EVIDENCE"
printf '\033[32m\nP0 EXIT: PASS — P0 is complete, P1 is unblocked.\033[0m\n'
