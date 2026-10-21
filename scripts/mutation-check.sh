#!/usr/bin/env bash
# Mutation check: each mutant breaks one guarded predicate, and the workspace tests must
# fail for every one. Run from anywhere: scripts/mutation-check.sh
# Prints one line per mutant, with the test that caught it, and exits non-zero if any mutant
# survives, does not build, ends without a failing test (timeout, crash, infra), or its anchor
# is missing. The source is restored after each mutant, also on interrupt, and the tree must
# end exactly as it began. The list is the guards proven so far, not every guard in the crates.
# Pattern: core/life crates/cuenca/cuenca-tree/scripts/mutation-check.sh.
set -uo pipefail
cd "$(dirname "$0")/.."
die() { echo "$*"; exit 3; }
fingerprint() { { git diff HEAD; git ls-files --others --exclude-standard; } | git hash-object --stdin; }
before=$(fingerprint) || die "cannot fingerprint the tree"
backup=$(mktemp -d) || die "mktemp failed"
cp Cargo.lock "$backup/Cargo.lock" || die "cannot back up Cargo.lock"
restore() {
  if [ -n "${current:-}" ]; then cp "$backup/file" "$current" || die "RESTORE FAILED: $current still holds a mutant"; fi
  current=
  cp "$backup/Cargo.lock" Cargo.lock || die "RESTORE FAILED: Cargo.lock"
}
unchanged() {
  [ "$(fingerprint)" = "$before" ] && return
  echo "TREE CHANGED: the harness left the tree unlike it found it"; git status --short; return 1
}
trap 'restore; rm -rf "$backup"; unchanged || exit 3' EXIT
trap 'exit 130' INT TERM

# Each run is bounded, so a mutant that hangs is a verdict, not a stall.
limit=${MUTANT_TIMEOUT_SECS:-900}
run_tests() {
  if command -v timeout >/dev/null; then
    timeout "$limit" env CARGO_TERM_COLOR=never cargo test -q --workspace --locked
  else
    env CARGO_TERM_COLOR=never cargo test -q --workspace --locked
  fi
}

# A mutant only counts as killed against a suite that passes unmutated.
if ! run_tests >"$backup/log" 2>&1; then
  echo "baseline is red: fix the suite before reading any mutant verdict"; cat "$backup/log"; exit 2
fi
echo "baseline  green"

failed=0 killed=0 total=0
mutate() { # file, old, new[, crash]: the anchor must occur exactly once. `crash` names the
  # one process failure that counts as a kill, for a guard whose whole purpose is to prevent
  # it (`stack overflow`); any other crash is still UNCLEAR.
  local file=$1 old=$2 new=$3 crash=${4:-} code caught
  total=$((total + 1)); current=$file; cp "$file" "$backup/file" || die "cannot back up $file"
  if ! python3 - "$file" "$old" "$new" <<'PY'
import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
if s.count(sys.argv[2]) != 1: sys.exit("anchor not unique: " + sys.argv[2])
p.write_text(s.replace(sys.argv[2], sys.argv[3]))
PY
  then echo "ANCHOR MISSING  $file: $old"; failed=1; restore; return; fi
  run_tests >"$backup/log" 2>&1
  code=$?
  # Killed means a named test failed. A build error, a timeout, a crash or any other exit
  # proves nothing about the guard.
  # `cargo test -q` lists failing tests, indented, under a `failures:` line.
  caught=$(awk '/^failures:$/ { n++; next } n >= 2 && /^    [^ ]/ { sub(/^ +/, ""); print; exit }' "$backup/log")
  if [ "$code" = 0 ]; then
    echo "SURVIVED  $file: $old"; failed=1
  elif grep -qE '^error\[E[0-9]+\]|could not compile|failed to select a version' "$backup/log"; then
    echo "NO BUILD  $file: $old (a mutant that does not build proves nothing)"; failed=1
  elif [ -n "$caught" ] && grep -q '^test result: FAILED' "$backup/log"; then
    echo "killed    $file: $old  [$caught]"; killed=$((killed + 1))
  elif [ -n "$crash" ] && grep -q "$crash" "$backup/log"; then
    echo "killed    $file: $old  [crash: $crash, as declared]"; killed=$((killed + 1))
  else
    echo "UNCLEAR   $file: $old (exit $code, no failing test: timeout, crash or infra)"; tail -5 "$backup/log"; failed=1
  fi
  restore
}

ask=crates/maestro-core/src/ask.rs
auth=crates/maestro-core/src/authority.rs
views=crates/maestro-store/src/views.rs
store=crates/maestro-store/src/lib.rs
tree=crates/maestro-store/src/tree.rs
yaml=crates/maestro-store/src/yaml.rs

# Ask state and entry hash: the plugin's stateOf and entryHash.
mutate $ask 'if text(row.get("archived_at")).is_some() || status.starts_with("archived") {' 'if status.starts_with("archived") {'
mutate $ask 'if OUTCOME_KEYS.iter().any(|k| text(row.get(k)).is_some())' 'if false'
mutate $ask '|| TERMINAL_STATUS_PREFIXES.iter().any(|p| status.starts_with(p))' ''
mutate $ask 'digest.iter().take(8)' 'digest.iter().take(7)'
mutate $ask '(key.len() > 1 && key.starts_with('"'"'0'"'"'))' 'false'
mutate $ask '(None, None) => a.encode_utf16().cmp(b.encode_utf16()),' '(None, None) => a.cmp(b),'
# Authority: only the owner takes owner actions; unverified callers only read.
mutate $auth '(Principal::Owner { .. }, Authority::OwnerAction) => true,' '(_, Authority::OwnerAction) => true,'
mutate $auth '(Principal::Agent { .. } | Principal::Owner { .. }, Authority::AgentRequest) => true,' '(_, Authority::AgentRequest) => true,'
# Views: needs are open blocking asks; loop drift is only what is provable.
mutate $views 'if ask.state() != AskState::Open || !ask.blocking {' 'if ask.state() != AskState::Open {'
mutate $views 'TICK_CLOSE_SECS.max(2 * median(&mut lengths))' 'TICK_CLOSE_SECS'
mutate $views 'if secs(&t.started_at).is_some_and(|s| now.as_second() - s > limit) {' 'if false {'
# Reading: the plugin's ledger limit, bounded reads, UTF-8 only, regular files only.
mutate $store 'if t.len() > LEDGER_LIMIT {' 'if false {'
mutate $tree 'if bytes.len() > MAX_READ {' 'if false {'
mutate $tree 'if size > MAX_READ {' 'if false {'
mutate $tree 'if entry.file_type()?.is_file() {' 'if true {'
mutate $tree '(Some("100644" | "100755"), Some("blob"))' '(Some(_), Some(_))'
mutate $tree 'String::from_utf8(bytes).map_err(|_| TreeError::NotUtf8 { rel: rel.to_string() })' 'Ok(String::from_utf8_lossy(&bytes).into_owned())'
# Git: a revision is never read as an option.
mutate $tree "if rev.is_empty() || rev.starts_with('-') {" 'if rev.is_empty() {'
mutate $tree "if rev.starts_with('-') {
                tree.unresolved" "if false {
                tree.unresolved"
# YAML: the subset admission, the package's alias limit, and the nesting and size bounds.
mutate $yaml 'if let Some(reason) = outside_subset(source) {' 'if let Some(reason) = outside_subset(source).filter(|_| false) {'
mutate $yaml 'if data.count.saturating_mul(data.alias_count) > MAX_ALIAS_COUNT {' 'if false {'
mutate $yaml '&& self.stack.len() >= MAX_DEPTH {' '&& false {'
mutate $yaml 'if depth > MAX_DEPTH {' 'if false {' 'stack overflow'
mutate $yaml '.checked_sub(n)' '.checked_sub(0)'

echo "mutation  $killed of $total killed"
exit $failed
