#!/usr/bin/env bash
# Acceptance gate for host-identity (BRO-3268):
#   bash scripts/acceptance/host-identity.sh --goldens --mutants --selftest
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
unset GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES

for dir in "${HOME}/.cargo/bin" "${HOME}/.bun/bin" "${HOME}/.local/bin"; do
  if [ -d "$dir" ]; then
    export PATH="$dir:${PATH}"
  fi
done
if [ -d /dev/shm ]; then
  export TMPDIR="${TMPDIR:-/dev/shm}"
fi

run_goldens=0
run_mutants=0
run_selftest=0

if [ "$#" -eq 0 ]; then
  echo "usage: $0 [--goldens] [--mutants] [--selftest]" >&2
  exit 2
fi

for arg in "$@"; do
  case "$arg" in
    --goldens) run_goldens=1 ;;
    --mutants) run_mutants=1 ;;
    --selftest) run_selftest=1 ;;
    *)
      echo "unknown argument: $arg" >&2
      exit 2
      ;;
  esac
done

for tool in cargo python3 git bun sha256sum timeout; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "missing required runner: $tool" >&2
    exit 1
  fi
done

if [ ! -s fixtures/hosts/callers-golden.json ] || [ ! -s fixtures/parity/plugin-rev ] || [ ! -s scripts/hosts-golden.ts ]; then
  echo "missing or empty host-identity golden fixtures or generator" >&2
  exit 1
fi

head_sha="$(git rev-parse HEAD)"
plugin_rev="$(tr -d '[:space:]' < fixtures/parity/plugin-rev)"
[[ "$plugin_rev" =~ ^[0-9a-f]{40}$ ]] || { echo "plugin-rev must be a 40-hex commit, got '$plugin_rev'" >&2; exit 1; }
golden_digest="$(sha256sum fixtures/hosts/callers-golden.json | awk '{print $1}')"
case_count="$(python3 -c 'import json; d=json.load(open("fixtures/hosts/callers-golden.json")); print(len(d["verify"]) + len(d["started"]) + len(d["permissions"]))')"
if [ "$case_count" -le 0 ]; then
  echo "golden corpus has zero cases" >&2
  exit 1
fi

verify_evidence_manifest() {
  local manifest="$1"
  local expected_sha="$2"
  local expected_rev="$3"
  local expected_cases="$4"
  local expected_digest="$5"
  python3 - "$manifest" "$expected_sha" "$expected_rev" "$expected_cases" "$expected_digest" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
expected_sha = sys.argv[2]
expected_rev = sys.argv[3]
expected_cases = int(sys.argv[4])
expected_digest = sys.argv[5]

if not path.is_file() or path.stat().st_size == 0:
    sys.exit(1)
try:
    data = json.loads(path.read_text())
except Exception:
    sys.exit(1)
if not isinstance(data, dict):
    sys.exit(1)
required = [
    "head_sha",
    "plugin_rev",
    "case_count",
    "golden_sha256",
    "tests_passed",
    "tests_failed",
    "mutants_killed",
    "mutants_total",
    "noop_survived",
    "fresh_seconds",
]
for key in required:
    if key not in data:
        sys.exit(1)
if data["head_sha"] != expected_sha:
    sys.exit(1)
if data["plugin_rev"] != expected_rev:
    sys.exit(1)
if data["case_count"] != expected_cases or data["case_count"] <= 0:
    sys.exit(1)
if data["golden_sha256"] != expected_digest:
    sys.exit(1)
if not isinstance(data["tests_passed"], int) or data["tests_passed"] <= 0:
    sys.exit(1)
if data["tests_failed"] != 0:
    sys.exit(1)
if not isinstance(data["mutants_total"], int) or data["mutants_total"] < 14:
    sys.exit(1)
if data["mutants_killed"] != data["mutants_total"]:
    sys.exit(1)
if data["noop_survived"] is not True:
    sys.exit(1)
if not isinstance(data["fresh_seconds"], (int, float)) or data["fresh_seconds"] < 0 or data["fresh_seconds"] > 3600:
    sys.exit(1)
PY
}

if [ "$run_goldens" -eq 1 ]; then
  echo "== acceptance: goldens (head=$head_sha plugin_rev=$plugin_rev cases=$case_count digest=$golden_digest)"
  work_golden="$(mktemp -d "${TMPDIR:-/tmp}/maestro-host-golden.XXXXXX")"
  driver=""
  cleanup_golden() {
    rm -rf "$work_golden" "${driver:-}"
  }
  trap cleanup_golden EXIT

  if [ -n "${MAESTRO_PASEO_DIR:-}" ]; then
    plugin="$MAESTRO_PASEO_DIR"
    if ! have_rev="$(git -C "$plugin" rev-parse HEAD 2>/dev/null)"; then
      echo "goldens failed: MAESTRO_PASEO_DIR ($plugin) is not a git repository" >&2
      exit 1
    fi
    if [ "$have_rev" != "$plugin_rev" ]; then
      echo "goldens failed: MAESTRO_PASEO_DIR ($plugin) is at $have_rev, expected $plugin_rev" >&2
      exit 1
    fi
    if [ ! -d "$plugin/node_modules" ]; then
      ( cd "$plugin" && bun install --frozen-lockfile >"$work_golden/install.log" 2>&1 ) || { cat "$work_golden/install.log" >&2; exit 1; }
    fi
    driver="$plugin/.hosts-golden-check-$$.ts"
  elif [ -e "${HOME}/broomva/apps/maestro-paseo/.git" ] && [ "$(git -C "${HOME}/broomva/apps/maestro-paseo" rev-parse HEAD 2>/dev/null || true)" = "$plugin_rev" ] && [ -d "${HOME}/broomva/apps/maestro-paseo/node_modules" ]; then
    plugin="${HOME}/broomva/apps/maestro-paseo"
    driver="$plugin/.hosts-golden-check-$$.ts"
  else
    url="${MAESTRO_PASEO_URL:-https://github.com/broomva/maestro-paseo}"
    plugin="$work_golden/plugin"
    git init -q "$plugin"
    git -C "$plugin" fetch -q --depth 1 "$url" "$plugin_rev"
    git -C "$plugin" checkout -q FETCH_HEAD
    ( cd "$plugin" && bun install --frozen-lockfile >"$work_golden/install.log" 2>&1 ) || { cat "$work_golden/install.log" >&2; exit 1; }
    driver="$plugin/.hosts-golden-check.ts"
  fi

  cp "$ROOT/scripts/hosts-golden.ts" "$driver"
  fresh_golden="$(cd "$plugin" && bun "$driver")"
  rm -f "$driver"
  diff -u <(printf '%s\n' "$fresh_golden") fixtures/hosts/callers-golden.json
  echo "golden    matches server/callers.ts + server/permissions.ts @ ${plugin_rev:0:12}"

  listed="$(cargo test -p maestro-hosts --test identity --locked -- --list 2>&1)"
  if ! printf '%s\n' "$listed" | grep -q ': test$'; then
    echo "ct guard: zero tests discovered in maestro-hosts --test identity" >&2
    exit 1
  fi
  test_code=0
  test_out="$(cargo test -p maestro-hosts --test identity --locked 2>&1)" || test_code=$?
  printf '%s\n' "$test_out"
  if [ "$test_code" -ne 0 ]; then
    echo "ct guard: maestro-hosts --test identity failed (exit $test_code)" >&2
    exit 1
  fi
  if ! printf '%s\n' "$test_out" | grep -q 'test result: ok[.][ ]*[1-9][0-9]* passed; 0 failed'; then
    echo "ct guard: no passing test summary in maestro-hosts --test identity" >&2
    exit 1
  fi
  rm -rf "$work_golden"
  echo "== goldens green"
fi

if [ "$run_mutants" -eq 1 ]; then
  echo "== acceptance: mutants"
  copy="$(mktemp -d "${TMPDIR:-/tmp}/maestro-host-mutants.XXXXXX")"
  trap 'rm -rf "$copy"' EXIT
  git ls-files -z --cached --others --exclude-standard | while IFS= read -r -d '' f; do [ -e "$f" ] && printf '%s\0' "$f"; done | tar --null -T - -cf - | tar -xf - -C "$copy"
  export CARGO_TARGET_DIR="$copy/.target" CARGO_TERM_COLOR=never
  git -C "$copy" init -q
  echo /.target > "$copy/.git/info/exclude"
  git -C "$copy" add -A
  git -C "$copy" -c core.hooksPath=/dev/null -c user.name=mutation -c user.email=mutation@localhost commit -qm base

  run_suite() {
    ( cd "$copy" && timeout 300 cargo test -q -p maestro-hosts --test identity -p maestro-core --locked )
  }

  if ! run_suite > "$copy/log" 2>&1; then
    echo "baseline is red in mutation copy" >&2
    cat "$copy/log" >&2
    exit 2
  fi
  echo "baseline  green"

  # No-op mutant control: must compile and survive
  python3 - "$copy/crates/hosts/src/lib.rs" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1])
t = p.read_text()
assert "pub mod paseo;" in t
p.write_text(t.replace("pub mod paseo;", "pub mod paseo; // noop mutant", 1))
PY
  if ! run_suite > "$copy/log" 2>&1; then
    echo "NOOP FAILED: no-op mutant did not survive" >&2
    cat "$copy/log" >&2
    exit 1
  fi
  git -C "$copy" checkout -q -- .
  echo "noop      survived [control]"

  failed=0
  killed=0
  total=0
  check_mutant() {
    local file="$1" old="$2" new="$3"
    total=$((total + 1))
    git -C "$copy" checkout -q -- .
    if ! python3 - "$copy/$file" "$old" "$new" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1])
s = p.read_text()
if s.count(sys.argv[2]) != 1:
    sys.exit(f"anchor not unique ({s.count(sys.argv[2])}): {sys.argv[2]}")
p.write_text(s.replace(sys.argv[2], sys.argv[3], 1))
PY
    then
      echo "ANCHOR MISSING  $file: $old" >&2
      failed=1
      return
    fi
    set +e
    run_suite > "$copy/log" 2>&1
    local code=$?
    set -e
    local caught
    caught="$(awk '/^failures:$/ { n++; next } n >= 2 && /^    [^ ]/ { sub(/^ +/, ""); print; exit }' "$copy/log")"
    if [ "$code" -eq 0 ]; then
      echo "SURVIVED  $file: $old" >&2
      failed=1
    elif grep -qE '^error\[E[0-9]+\]|could not compile|failed to select a version' "$copy/log"; then
      echo "NO BUILD  $file: $old" >&2
      failed=1
    elif [ "$code" -ne 124 ] && [ -n "$caught" ] && grep -q '^test result: FAILED' "$copy/log"; then
      echo "killed    $file: $old  [$caught]"
      killed=$((killed + 1))
    else
      echo "UNCLEAR   $file: $old (exit $code)" >&2
      tail -n 10 "$copy/log" >&2
      failed=1
    fi
  }

  hp=crates/hosts/src/paseo.rs
  ht=crates/hosts/src/t3.rs
  hlb=crates/hosts/src/lib.rs
  session=crates/core/src/session.rs

  check_mutant "$hp" 'if parsed.id != expected_id {' 'if false {'
  check_mutant "$hp" 'if self.disconnected || !is_safe_id(id) {' 'if false {'
  check_mutant "$hp" 'if !liveness.is_live() {' 'if false {'
  check_mutant "$hp" 'if !within(Path::new(cwd), Path::new(&agent.cwd)) {' 'if false {'
  check_mutant "$hp" 'if let Ok(canon) = std::fs::canonicalize(path) {' 'if let Ok(canon) = std::fs::canonicalize(lexical_normalize(path)) {'
  check_mutant "$hp" 'inside.starts_with(&outside)' 'inside.to_string_lossy().starts_with(&*outside.to_string_lossy())'
  check_mutant "$hp" '&& cred.uid != want_uid' '&& false'
  check_mutant "$hlb" 'if in_t3 && !in_paseo {' 'if false {'
  check_mutant "$hlb" 'if in_paseo && !in_t3 {' 'if false {'
  check_mutant "$hlb" 'Sha256::digest(t.as_bytes()) == Sha256::digest(s.token.as_bytes())' 'true'
  check_mutant "$hlb" 'if cred.generation != current_gen {' 'if false {'
  check_mutant "$ht" 'if self.disconnected {' 'if false {'
  check_mutant "$ht" '        } else if self.settled_at.is_some() && self.status.as_deref() != Some("running") {' '        } else if false {'
  check_mutant "$session" '        self.pending_permissions' '        Vec::<PermissionPrompt>::new()'
  check_mutant crates/mcp/src/lib.rs 'if !hosts.has_surface(&session.surface) && !has_claim_meta {' 'if true {'
  check_mutant crates/mcp/src/lib.rs 'if arg_cred.is_none() && (session.surface != maestro_core::session::Surface::Direct || arg_token.is_none()) {' 'if false {'
  check_mutant crates/mcp/src/lib.rs 'if packet.scope.project.is_none() && packet.scope.initiative.is_none() {' 'if true {'
  check_mutant "$hlb" 'if effective_now_ms < cred.issued_at_ms || effective_now_ms >= cred.expires_at_ms {' 'if false {'
  check_mutant "$hlb" 'if lock_dir_stale(&lock_dir, &lock_dir) == Some(true) {' 'if false {'
  check_mutant "$hlb" 'msg.extend_from_slice(incarnation.as_bytes());' ''

  rm -rf "$copy"
  echo "mutation  $killed/$total killed"
  if [ "$failed" -ne 0 ] || [ "$killed" -ne "$total" ]; then
    exit 1
  fi
  echo "== mutants green"
fi

if [ "$run_selftest" -eq 1 ]; then
  echo "== acceptance: selftest"
  tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/maestro-host-selftest.XXXXXX")"
  trap 'rm -rf "$tmp_dir"' EXIT

  # 1. Empty evidence must fail
  : > "$tmp_dir/empty.json"
  if verify_evidence_manifest "$tmp_dir/empty.json" "$head_sha" "$plugin_rev" "$case_count" "$golden_digest"; then
    echo "selftest failed: empty evidence was accepted" >&2
    exit 1
  fi

  # 2. Malformed evidence must fail
  printf '{"head_sha": "%s"}\n' "$head_sha" > "$tmp_dir/malformed.json"
  if verify_evidence_manifest "$tmp_dir/malformed.json" "$head_sha" "$plugin_rev" "$case_count" "$golden_digest"; then
    echo "selftest failed: malformed evidence was accepted" >&2
    exit 1
  fi

  # 3. Wrong-SHA evidence must fail
  cat > "$tmp_dir/wrong_sha.json" <<EOF
{
  "head_sha": "0000000000000000000000000000000000000000",
  "plugin_rev": "$plugin_rev",
  "case_count": $case_count,
  "golden_sha256": "$golden_digest",
  "tests_passed": 6,
  "tests_failed": 0,
  "mutants_killed": 20,
  "mutants_total": 20,
  "noop_survived": true,
  "fresh_seconds": 5
}
EOF
  if verify_evidence_manifest "$tmp_dir/wrong_sha.json" "$head_sha" "$plugin_rev" "$case_count" "$golden_digest"; then
    echo "selftest failed: wrong-SHA evidence was accepted" >&2
    exit 1
  fi

  # 4. Stale evidence must fail
  cat > "$tmp_dir/stale.json" <<EOF
{
  "head_sha": "$head_sha",
  "plugin_rev": "$plugin_rev",
  "case_count": $case_count,
  "golden_sha256": "$golden_digest",
  "tests_passed": 6,
  "tests_failed": 0,
  "mutants_killed": 20,
  "mutants_total": 20,
  "noop_survived": true,
  "fresh_seconds": 7200
}
EOF
  if verify_evidence_manifest "$tmp_dir/stale.json" "$head_sha" "$plugin_rev" "$case_count" "$golden_digest"; then
    echo "selftest failed: stale evidence was accepted" >&2
    exit 1
  fi

  # 5. Surviving mutant evidence must fail
  cat > "$tmp_dir/surviving.json" <<EOF
{
  "head_sha": "$head_sha",
  "plugin_rev": "$plugin_rev",
  "case_count": $case_count,
  "golden_sha256": "$golden_digest",
  "tests_passed": 6,
  "tests_failed": 0,
  "mutants_killed": 13,
  "mutants_total": 14,
  "noop_survived": true,
  "fresh_seconds": 5
}
EOF
  if verify_evidence_manifest "$tmp_dir/surviving.json" "$head_sha" "$plugin_rev" "$case_count" "$golden_digest"; then
    echo "selftest failed: surviving mutant evidence was accepted" >&2
    exit 1
  fi

  # 6. Missing MAESTRO_PASEO_DIR on --goldens must fail closed
  if MAESTRO_PASEO_DIR="$tmp_dir/missing-plugin" bash "$0" --goldens >/dev/null 2>&1; then
    echo "selftest failed: --goldens accepted missing MAESTRO_PASEO_DIR" >&2
    exit 1
  fi

  # 7. Wrong-SHA MAESTRO_PASEO_DIR on --goldens must fail closed
  git init -q "$tmp_dir/wrong-rev-plugin"
  git -C "$tmp_dir/wrong-rev-plugin" -c user.name=test -c user.email=test@localhost commit --allow-empty -qm init
  if MAESTRO_PASEO_DIR="$tmp_dir/wrong-rev-plugin" bash "$0" --goldens >/dev/null 2>&1; then
    echo "selftest failed: --goldens accepted wrong-SHA MAESTRO_PASEO_DIR" >&2
    exit 1
  fi

  # 8. Complete valid control must pass
  cat > "$tmp_dir/valid.json" <<EOF
{
  "head_sha": "$head_sha",
  "plugin_rev": "$plugin_rev",
  "case_count": $case_count,
  "golden_sha256": "$golden_digest",
  "tests_passed": 6,
  "tests_failed": 0,
  "mutants_killed": 20,
  "mutants_total": 20,
  "noop_survived": true,
  "fresh_seconds": 5
}
EOF
  verify_evidence_manifest "$tmp_dir/valid.json" "$head_sha" "$plugin_rev" "$case_count" "$golden_digest"
  rm -rf "$tmp_dir"
  echo "== selftest green"
fi
