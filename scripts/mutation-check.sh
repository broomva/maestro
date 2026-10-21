#!/usr/bin/env bash
# Mutation check: each mutant breaks one guarded predicate, and the workspace tests must
# fail for every one. Run from anywhere: scripts/mutation-check.sh
# Prints one line per mutant, with the test that caught it, and exits non-zero if any mutant
# survives, does not build, ends without a failing test (timeout, crash, infra), or its anchor
# is missing. The list is the guards proven so far, not every guard in the crates.
# Mutants are applied to a throwaway copy of the git-visible tree, built into a target dir
# inside it; this checkout's files are not modified.
# Pattern: core/life crates/cuenca/cuenca-tree/scripts/mutation-check.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v timeout >/dev/null || { echo "mutation-check needs \`timeout\` (GNU coreutils)"; exit 3; }
limit=${MUTANT_TIMEOUT_SECS:-900}
[[ $limit =~ ^[1-9][0-9]*$ ]] || { echo "MUTANT_TIMEOUT_SECS must be a positive whole number of seconds"; exit 3; }

copy=$(mktemp -d)
trap 'rm -rf "$copy"' EXIT
trap 'exit 130' INT TERM
# A tracked file deleted in the working tree is deleted in the copy too.
git ls-files -z --cached --others --exclude-standard \
  | while IFS= read -r -d '' f; do [ -e "$f" ] && printf '%s\0' "$f"; done \
  | tar --null -T - -cf - | tar -xf - -C "$copy"
export CARGO_TARGET_DIR=$copy/.target CARGO_TERM_COLOR=never
git -C "$copy" init -q
echo /.target >"$copy/.git/info/exclude"
git -C "$copy" add -A
git -C "$copy" -c core.hooksPath=/dev/null -c user.name=mutation -c user.email=mutation@localhost commit -qm base
cd "$copy"
run_tests() { timeout "$limit" cargo test -q --workspace --locked; }

# A mutant only counts as killed against a suite that passes unmutated.
if ! run_tests >log 2>&1; then
  echo "baseline is red: fix the suite before reading any mutant verdict"; cat log; exit 2
fi
echo "baseline  green"

set +e
failed=0 killed=0 total=0
mutate() { # file, old, new[, crash]: the anchor must occur exactly once. `crash` names the one
  # abort that counts as a kill, for a guard whose whole purpose is to prevent it (`stack
  # overflow`): the runtime's own whole line plus the test process's SIGABRT, never a timeout.
  local file=$1 old=$2 new=$3 crash=${4:-} code caught
  total=$((total + 1))
  git checkout -q -- . && git clean -qfdx -e log -e .target || { echo "cannot reset the copy"; exit 3; }
  if ! python3 - "$file" "$old" "$new" <<'PY'
import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
if s.count(sys.argv[2]) != 1: sys.exit("anchor not unique: " + sys.argv[2])
p.write_text(s.replace(sys.argv[2], sys.argv[3]))
PY
  then echo "ANCHOR MISSING  $file: $old"; failed=1; return; fi
  run_tests >log 2>&1
  code=$?
  # Killed means a named test failed. A build error, a timeout, a crash or any other exit
  # proves nothing about the guard. `cargo test -q` lists failing tests, indented, after the
  # second `failures:` line.
  caught=$(awk '/^failures:$/ { n++; next } n >= 2 && /^    [^ ]/ { sub(/^ +/, ""); print; exit }' log)
  if [ "$code" = 0 ]; then
    echo "SURVIVED  $file: $old"; failed=1
  elif grep -qE '^error\[E[0-9]+\]|could not compile|failed to select a version' log; then
    echo "NO BUILD  $file: $old (a mutant that does not build proves nothing)"; failed=1
  elif [ "$code" != 124 ] && [ -n "$caught" ] && grep -q '^test result: FAILED' log; then
    echo "killed    $file: $old  [$caught]"; killed=$((killed + 1))
  # The runtime's line may follow `cargo test -q` progress dots another test thread printed first.
  elif [ "$code" != 124 ] && [ -n "$crash" ] && grep -qE "^[.i]*fatal runtime error: $crash, aborting$" log \
    && grep -qE '^  process didn.t exit successfully: .*\(signal: 6, SIGABRT: process abort signal\)$' log; then
    echo "killed    $file: $old  [crash: $crash, as declared]"; killed=$((killed + 1))
  else
    echo "UNCLEAR   $file: $old (exit $code, no failing test: timeout, crash or infra)"; tail -5 log; failed=1
  fi
}

ask=crates/core/src/ask.rs
auth=crates/core/src/authority.rs
views=crates/store/src/views.rs
store=crates/store/src/lib.rs
tree=crates/store/src/tree.rs
yaml=crates/store/src/yaml.rs
session=crates/core/src/session.rs
cli=crates/cli/src/main.rs

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
# Sessions: a surface drives, a harness runs, a node is its stable ID; nothing unknown is promised.
mutate $session '"direct" => Self::Direct,' ''
mutate $session 'Surface::Direct => SurfaceCapabilities { launch: true, schedule: true, ..Default::default() },' 'Surface::Direct => SurfaceCapabilities { launch: true, send: true, schedule: true, ..Default::default() },'
mutate $session 'Surface::Other(_) => SurfaceCapabilities::default(),' 'Surface::Other(_) => SurfaceCapabilities { launch: true, ..Default::default() },'
mutate $session 'Harness::Other(_) => HarnessCapabilities::default(),' 'Harness::Other(_) => HarnessCapabilities { mcp: true, ..Default::default() },'
mutate $session 'let id = me.get("ID")?.as_str()?.trim();' 'let id = me.get("DNSName")?.as_str()?.trim();'
mutate $session 'if id.is_empty() {
            // A blank ID' 'if false {
            // A blank ID'
mutate $cli 'if std::time::Instant::now() >= deadline {' 'if false {'
mutate $cli 'status.success().then_some(buf)' 'Some(buf)'
mutate $cli 'let _ = child.kill();' ''
mutate $cli 'let buf = rx.recv_timeout(left).ok()?.ok()?;' 'let buf = rx.recv().ok()?.ok()?;'
mutate $cli '.map(|n| n.trim().to_string()).filter(|n| !n.is_empty())' '.map(|n| n.trim().to_string())'
# YAML: the subset admission, the package's alias limit, and the nesting and size bounds.
mutate $yaml 'if let Some(reason) = outside_subset(source) {' 'if let Some(reason) = outside_subset(source).filter(|_| false) {'
mutate $yaml 'if data.count.saturating_mul(data.alias_count) > MAX_ALIAS_COUNT {' 'if false {'
mutate $yaml '&& self.stack.len() >= MAX_DEPTH {' '&& false {'
mutate $yaml 'if depth > MAX_DEPTH {' 'if false {' 'stack overflow'
mutate $yaml '.checked_sub(n)' '.checked_sub(0)'
mutate $yaml 'Event::SequenceStart(..) | Event::MappingStart(..) => self.check_key(at, None, true),' 'Event::SequenceStart(..) | Event::MappingStart(..) => self.check_key(at, None, false),'
mutate $yaml 'if e.info() == "recursion limit exceeded" {' 'if false {'
# The ticks ledger is read by its tail, cut where the plugin's readTail cuts it.
mutate $store 'let (text, from) = match read_tail(&path, STREAM_TAIL) {' 'let (text, from) = match read_tail(&path, usize::MAX / 4) {'
mutate $store 'here >= target' 'here > target'
mutate $store 'let cut = at + nl + 1;' 'let cut = at + nl;'
mutate $store '.saturating_mul(3).saturating_add(3)' '.saturating_mul(3)'
mutate $store 'if start == 0 && units <= limit {' 'if units <= limit {'
mutate $store 'None => Ok((String::from_utf8_lossy(&std::fs::read(path)?).into_owned(), 0)),' 'None => Ok((text.into_owned(), start + skip as u64)),'
mutate $store 'let text = String::from_utf8_lossy(raw);' 'let text = std::borrow::Cow::Borrowed(std::str::from_utf8(raw).unwrap_or_default());'
mutate $store '.nth(breaks - 1)' '.nth(breaks)'
mutate $store '.take_while(|b| (**b & 0xC0) == 0x80).count().min(3)' '.take_while(|b| (**b & 0xC0) == 0x80).count()'
mutate $views 'if active && ticks.is_some_and(TickTail::whole) && names_receipts' 'if active && ticks.is_some() && names_receipts'

# HTTP (BRO-3264): where the daemon may listen, which requests it takes, and a store per request.
http=crates/mcp/src/http.rs
mcp=crates/mcp/src/lib.rs
mutate $http 'if addr.ip().is_loopback() || is_tailnet(addr.ip()) {' 'if !addr.ip().is_unspecified() {'
mutate $http 'a == 100 && (b & 0xc0) == 64' 'a == 100'
mutate $http "v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0]" "v6.segments()[..2] == [0xfd7a, 0x115c]"
mutate $http 'v6.to_ipv4_mapped().is_some_and(|v4| is_tailnet(IpAddr::V4(v4)))' 'v6.to_ipv4_mapped().is_some()'
mutate $http '        .with_allowed_hosts(hosts)' '        .disable_allowed_hosts()'
mutate $http '        .enforce_origin_validation()' '        .disable_allowed_origins()'
mutate $http 'tokio::time::sleep(SHUTDOWN_GRACE).await;' 'std::future::pending::<()>().await;'
mutate $http '.with_json_response(true)' '.with_json_response(false)'
mutate $http ', local.ip().to_string()];' '];'
# SIGTERM is a clean stop. (The handler-before-readiness order is held by a probabilistic test,
# sigterm_right_after_the_readiness_line_is_a_clean_stop: under this harness's load the race closes.)
mutate $cli 'let mut term = signal(SignalKind::terminate())?;' 'let mut term = signal(SignalKind::user_defined1())?;'
mutate $mcp 'Self { paths: Arc::new(store.paths), source: store.tree.source }' 'Self { paths: Arc::new(store.paths), source: store.tree.commit.clone().map_or(store.tree.source, maestro_store::TreeSource::GitRev) }'
mutate $http 'None => Ok(Self::loopback()),' 'None => Self::named("100.64.0.1:7171".parse().unwrap()),'
mutate $http 'SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_PORT)' 'SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DEFAULT_PORT)'

# Path registry (BRO-3266): .broomva before .control, both refused, the git rule equal to the
# registry's, $BROOMVA_HOME by kind, and the stale-reader check.
paths=crates/store/src/paths.rs
doctor=crates/store/src/doctor.rs
mutate $store '        if both {' '        if false && both {'
mutate $store 'if !["040000", "100644", "100755"].contains(&mode.as_str()) {' 'if false {'
mutate $store '            if path == new || path == old {' '            if false {'
mutate $store 'Ok(if rels.len() > 1 && control && !moved {' 'Ok(if false {'
mutate $store 'Ok(if rels.len() > 1 && control && !moved {' 'Ok(if rels.len() > 1 && control {'
mutate $store 'if std::fs::symlink_metadata(bucket.join("_work.md")).is_ok() {' 'if false {'
mutate $paths '            Some(w) => w,' '            Some(_) => registry::workspace_root()?,'
mutate $store '            paths.workspace = views::lexical(&abs);' '            paths.workspace = abs;'
mutate $store 'let initiative = (name != UNFILED).then_some(name);' 'let initiative = Some(name);'
mutate $store "                    if name.starts_with('.') {" '                    if false {'
mutate $store 'Err(reason) => errors.push(SourceError { at: bucket.display().to_string(), reason }),' 'Err(_) => {}'
mutate $paths 'if same_dir(&self.work_root, &self.broomva_home.join(machine_rel("work"))) {' 'if false {'
mutate $paths "let plain = !stream.is_empty() && !stream.starts_with('.') && " 'let plain = !stream.is_empty() && '
mutate $paths '.or_else(|| configured_work_root(&config.join("config.json")))' ''
mutate $doctor '            if sha == REGISTRY_SHA256 {' '            if true {'
mutate $doctor 'Ok(text) if text == VENDORED_READER => {' 'Ok(_) => {'
mutate $cli 'if checks.iter().any(|c| c.status != Status::Ok) {' 'if checks.iter().any(|c| c.status == Status::Stale) {'

echo "mutation  $killed of $total killed"
exit $failed
