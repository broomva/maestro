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
# MUTANT_SHARD=i/N runs only the mutants whose position (0-based, in file order) is i mod N, so
# N jobs with i = 0..N-1 together run every mutant once (BRO-3446). Unset, it runs them all.
shard=${MUTANT_SHARD:-0/1}
[[ $shard =~ ^(0|[1-9][0-9]*)/([1-9][0-9]*)$ ]] && ((BASH_REMATCH[1] < BASH_REMATCH[2])) \
  || { echo "MUTANT_SHARD must be i/N with 0 <= i < N"; exit 3; }
shard_i=${BASH_REMATCH[1]} shard_n=${BASH_REMATCH[2]}

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
failed=0 killed=0 total=0 seen=0
mutate() { # file, old, new[, crash]: the anchor must occur exactly once. `crash` names the one
  # abort that counts as a kill, for a guard whose whole purpose is to prevent it (`stack
  # overflow`): the runtime's own whole line plus the test process's SIGABRT, never a timeout.
  local file=$1 old=$2 new=$3 crash=${4:-} code caught
  seen=$((seen + 1))
  (((seen - 1) % shard_n == shard_i)) || return 0
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
mutate $store 'if std::fs::symlink_metadata(bucket.join("_work.md")).is_ok() {' 'if false {'
mutate $store '        if layout.shadowed.iter().any(|n| n == name) {' '        if false {'
mutate $store '                .unwrap_or_else(|| rels[0].clone())),' '                .unwrap_or_else(|| rels[rels.len() - 1].clone())),'
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
# git reads the repo at the root alone: no inherited GIT_DIR, and the root is a top level.
mutate $cli '    let _ = cmd.exec();' '    let _ = cmd;'
mutate $cli 'if !GIT_REPO_ENV.iter().any(|k| std::env::var_os(k).is_some()) {' 'if std::env::var_os("GIT_DIR").is_none() {'
mutate $tree '                    if same_path(Path::new(top), &tree.root) {' '                    if true {'

# Ports of delegation.py and fleet_economy.py (BRO-3204), held to the Python's golden
# (crates/core/tests/parity_delegation.rs): each guarded predicate, broken once.
deleg=crates/core/src/delegation.rs
econ=crates/core/src/economy.rs
mutate $deleg 'let up = (tier as i64).saturating_add(steps.max(0)).min(top as i64);' 'let up = (tier as i64).saturating_add(steps.max(0));'
mutate $deleg "Some(('?', rest)) => text.first().is_some_and(|c| *c != '/') && glob(rest, &text[1..])," "Some(('?', rest)) => text.first().is_some() && glob(rest, &text[1..]),"
mutate $deleg "if i == text.len() || text[i] == '/' {" 'if i == text.len() {'
mutate $deleg 'if GOVERNANCE_ROOT.contains(&p.as_str()) {' 'if false {'
mutate $deleg ' && CODE_EXT.iter().any(|e| base.ends_with(e)))' ')'
mutate $deleg 'let big = loc > SUBSTANTIVE_LOC;' 'let big = loc >= SUBSTANTIVE_LOC;'
mutate $deleg 'if !spec && (big || public_api || code_files > SPEC_FILES) {' 'if !spec && (big || public_api) {'
mutate $deleg '        && seen > executor' '        && seen < executor'
mutate $deleg '        && complexity != "governance"
' '
'
mutate $deleg 'if kind == "review" && tier < Tier::Frontier {' 'if false {'
mutate $deleg 'if let Some(&other) = produced_by.get(p) {' 'if let Some(&other) = None::<&usize> {'
mutate $deleg '1 => return true,' '1 => return false,'
mutate $deleg 'Some(s) if s < strength(&json!(a.verify))' 'Some(s) if s > strength(&json!(a.verify))'
mutate $deleg 'Some(&p) => ups.contains(&p) && node(p).get("kind") == Some(&json!("design")),' 'Some(&p) => node(p).get("kind") == Some(&json!("design")),'
mutate $deleg 'if norm.starts_with('"'"'/'"'"') || norm == ".." || norm.starts_with("../") {' 'if norm.starts_with('"'"'/'"'"') {'
mutate $econ 'a5.filter(|a| *a >= READ_ONLY_5H)' 'a5.filter(|_| false)'
mutate $econ '} else if crosses && runway < CRITICAL_RUNWAY_H {' '} else if false {'
mutate $econ '} else if !py::truthy(fc.get("known")) {' '} else if false {'
mutate $econ 'if let (Some(r), Some(_)) = (r, ok) {' 'if let (Some(r), _) = (r, ok) {'
mutate $econ '&& (p.1 < last.1 || !same_window(p.2.as_ref(), last.2.as_ref())?)' '&& (p.1 < last.1)'
mutate $econ 'let crosses = relief.is_none_or(|rl| ex < rl);' 'let crosses = relief.is_some_and(|rl| ex < rl);'
mutate $econ '(ctx > HANDOFF_K, driven && ctx > SUCCESSOR_K)' '(ctx > HANDOFF_K, ctx > SUCCESSOR_K)'
mutate $econ '(MIN_SPAN_H..=(7 * 24) as f64).contains(&open_h)' '(0.0..=f64::MAX).contains(&open_h)'
mutate $econ 'count(&raised) / count(&runs) >= REPLAN_RAISED' 'count(&raised) / count(&runs) > REPLAN_RAISED'
# The disk-aware guards: a stated spec must exist; a spec input is found on disk; a path inside
# another bound repo is refused.
mutate $deleg '} else if disk.is_some_and(|d| !(d.exists)(s)) {' '} else if false {'
mutate $deleg 'None => disk.is_some_and(|d| (d.exists)(x)),' 'None => false,'
mutate $deleg '&& d.starts_with(&format!("{mine}/"))' '&& false'
# Tier floors, reasons, decomposition, the thinking floor, the mechanical rung and the review exemption.
mutate $deleg '} else if tier < a.executor {' '} else if false {'
mutate $deleg 'if tier > a.executor && !has_reason {' 'if false {'
mutate $deleg 'if tier == Tier::Apex && !has_reason {' 'if false {'
mutate $deleg 'if a.needs.contains(&"decompose") {' 'if false {'
mutate $deleg 'Some(t) if (a.complexity == "governance" || kind == "review") && t < a.thinking =>' 'Some(t) if false && t < a.thinking =>'
mutate $deleg 'if MECHANICAL_KINDS.contains(&kind) && paths.is_empty() && loc == 0 && gov.is_empty() {' 'if MECHANICAL_KINDS.contains(&kind) && loc == 0 && gov.is_empty() {'
mutate $deleg '        && kind != "review"
' '
'
# Samples from the future, too short a span, unusable accounts, past resets, too few approvals.
mutate $econ '|| at > now {' '{'
mutate $econ 'if span < MIN_SPAN_H {' 'if false {'
mutate $econ 'py::num(v.get("7d")).filter(|s| !py::truthy(v.get("limited")) && *s < 100.0)' 'py::num(v.get("7d")).filter(|s| *s < 100.0)'
mutate $econ '.filter(|r| *r > now)' '.filter(|_| true)'
mutate $econ 'if count(&approved) >= 3.0 && fp < REPLAN_FIRST_PASS {' 'if count(&approved) >= 0.0 && fp < REPLAN_FIRST_PASS {'
mutate $econ 'let sum = at + whole as i64 * HOUR_US + round_half_even((hours - whole) * HOUR_US as f64);' 'let sum = at + (hours * 3600.0).trunc() as i64 * US + round_half_even((hours * 3600.0).fract() * 1e6);'

# The capacity gate (BRO-3223): who decides it, when a stale active account counts, who covers it by
# failover, the pool headroom and relief come from, and the balancer state's bounds.
mutate $econ 'if py::truthy(v.get("needs_login")) || py::truthy(v.get("limited")) {' 'if py::truthy(v.get("limited")) {'
mutate $econ 'let limited = py::num(v.get("7d")).is_some_and(|s| s >= LIMIT_7D) && reset.is_none_or(|r| r > now);' 'let limited = py::num(v.get("7d")).is_some_and(|s| s > LIMIT_7D) && reset.is_none_or(|r| r > now);'
mutate $econ '&& reset.is_none_or(|r| r > now);' ';'
mutate $econ 'let young = py::num(v.get("age_s")).is_some_and(|a| a < gate.stale_ok_min * 60.0);' 'let young = py::num(v.get("age_s")).is_some_and(|a| a <= gate.stale_ok_min * 60.0);'
mutate $econ 'if five.is_some() && (!stale || young || reset5.is_some_and(|r| r > now)) {' 'if five.is_some() && (!stale || young) {'
mutate $econ 'if stale || five.is_none() || !spendable(v, now)? {' 'if stale || five.is_none() {'
mutate $econ 'if f <= gate.standby_max' 'if f < gate.standby_max'
mutate $econ '&& !seven.is_some_and(|s| s >= gate.standby_weekly_max)' '&& true'
mutate $econ '&& !gate.held.contains(name)' '&& true'
mutate $econ 'standbys.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(b.2)));' 'standbys.sort_by(|a, b| a.2.cmp(b.2));'
mutate $econ '!py::truthy(v.get("needs_login")) && (py::truthy(v.get("active")) || !py::truthy(v.get("stale")))' 'py::truthy(v.get("active")) || !py::truthy(v.get("stale"))'
mutate $econ 'let ok = usable(v).filter(|_| pool.contains(name));' 'let ok = usable(v);'
mutate $econ 'for (_, v) in current.iter().filter(|(n, _)| pool.contains(n)) {' 'for (_, v) in current.iter() {'
mutate $econ 'let usable7: Vec<f64> = current.iter().filter(|(n, _)| in_pool(n)).filter_map(|(_, v)| usable(v)).collect();' 'let usable7: Vec<f64> = current.iter().filter_map(|(_, v)| usable(v)).collect();'
mutate $econ '.any(|p| now_s - at_seconds(p) < SIGNAL_TTL_S);' '.any(|p| now_s - at_seconds(p) <= SIGNAL_TTL_S);'
mutate $econ '&& !hold.is_some_and(|h| h > now)' '&& true'
mutate $econ '&& !pending;' ';'
mutate $econ 'if p.get("result") == Some(&json!("limited")) && now_s - at_seconds(p) < PROBE_HOLD_S {' 'if p.get("result") == Some(&json!("limited")) {'
# Not `py::truthy`: the schema holds autoBalance to a bool, where truthiness is the same test.
mutate $econ '&& cfg.get("autoBalance") == Some(&Value::Bool(true))' ''
mutate $econ '} else if let Some((name, _, max)) = top.filter(|t| t.2 >= RESUME_5H) {' '} else if let Some((name, _, max)) = top.filter(|t| t.2 > RESUME_5H) {'
mutate $econ '} else if r["known"] != json!(true) {' '} else if false {'
mutate $econ 'Some((_, _, b)) if f <= b => best,' 'Some((_, _, b)) if f < b => best,'
mutate $econ 'let needs_login = py::truthy(a.get("needsLogin")) || a.get("hasStoredCredentials") == Some(&Value::Bool(false));' 'let needs_login = py::truthy(a.get("needsLogin"));'

# Exact boundaries (P20 r1): resets and holds at now, a duplicate id's last email, a bool bound, the rank tie.
mutate $econ 'let limited = py::num(v.get("7d")).is_some_and(|s| s >= LIMIT_7D) && reset.is_none_or(|r| r > now);' 'let limited = py::num(v.get("7d")).is_some_and(|s| s >= LIMIT_7D) && reset.is_none_or(|r| r >= now);'
mutate $econ 'if five.is_some() && (!stale || young || reset5.is_some_and(|r| r > now)) {' 'if five.is_some() && (!stale || young || reset5.is_some_and(|r| r >= now)) {'
mutate $econ '&& !hold.is_some_and(|h| h > now)' '&& !hold.is_some_and(|h| h >= now)'
mutate $econ 'Some(slot) => slot.1 = name,' 'Some(_) => {}'
mutate $econ 'standbys.push((f, seven.unwrap_or(0.0), name));' 'standbys.push((f, seven.unwrap_or(100.0), name));'

# Skipped nodes are checked for structure only; the plan CLI's binding guards (held by the CLI
# golden in crates/cli/tests/delegation.rs).
port=crates/cli/src/port.rs
mutate $deleg 'if skip.contains(nid) {' 'if false {'
mutate $port 'if !absent.is_empty() && !bind.skip_absent_repos {' 'if false {'
mutate $port 'return true; // unknown here: not checked out, so never reported missing' 'return false;'
mutate $port 'if let Some(first) = seen.get(r.dir.as_str()) {' 'if let Some(first) = None::<&&str> {'
mutate $port '            if has_file {
                return Err(Fault::Value(format!(' '            if false {
                return Err(Fault::Value(format!('
mutate $port '    if !v.is_object() {' '    if false {'
mutate $port 'bindings.iter().rev().find(|(n, _)| *n == name)' 'bindings.iter().find(|(n, _)| *n == name)'
# YAML outside the subset (anchors, tags, merge keys) is refused, never misread.
mutate $port '            Event::Scalar(_, _, _, Some(_)) | Event::SequenceStart(_, Some(_)) | Event::MappingStart(_, Some(_)) => {
                "a tag".to_string()
            }' '            Event::Scalar(_, _, _, Some(_)) | Event::SequenceStart(_, Some(_)) | Event::MappingStart(_, Some(_)) => {
                continue
            }'
mutate $port '                if *anchor > 0 =>' '                if false =>'
# Port continuation (Codex): argparse's repeat, Unicode-digit and type-error readings of assess flags.
mutate $port '    #[arg(long, num_args = 0.., action = clap::ArgAction::Set)]' '    #[arg(long, num_args = 0..)]'
mutate $port "    let ascii: Option<String> = digits.chars().map(|c| if c == '_' { Some('_') } else { py::decimal(c) }).collect();" "    let ascii: Option<String> = digits.chars().map(|c| if c == '_' || c.is_ascii_digit() { Some(c) } else { None }).collect();"
mutate $port "    let t = s.trim_matches(|c: char| py::is_space(c) && !('\\u{1c}'..='\\u{1f}').contains(&c));" '    let t = py::strip(s);'
mutate $port '    let root = a.root.clone().filter(|r| !r.is_empty()).unwrap_or_else(cwd); // `o.root or os.getcwd()`' '    let root = a.root.clone().unwrap_or_else(cwd);'
mutate $port '    let root = bind.root.clone().filter(|r| !r.is_empty()).unwrap_or_else(cwd); // `o.root or os.getcwd()`' '    let root = bind.root.clone().unwrap_or_else(cwd);'
mutate $port '    Some(fail(format!("error: argument {flag}: {why}"), 2))' '    None'
mutate crates/core/src/py.rs '    (z <= n).then(|| char::from(b'"'"'0'"'"' + (n - z) as u8))' '    (z < n).then(|| char::from(b'"'"'0'"'"' + (n - z) as u8))'

# The schema (crates/core/src/schema.rs): every input is checked before any rule runs, and each
# field type holds. Held by the golden's refusals (the harness's own reading of the schema) and
# the CLI goldens. Then the dates past datetime's years, which stay inside the schema.
schema=crates/core/src/schema.rs
mutate $deleg '    crate::schema::graph(graph)?;' ''
mutate $deleg '    crate::schema::signals("signals", sig)?;' ''
mutate $deleg '    crate::schema::policy(policy)?;' ''
mutate $econ '    crate::schema::usage(rows)?;
    let mut accounts' '    let mut accounts'
mutate $econ '    crate::schema::state(state)?;' ''
mutate $econ '    crate::schema::samples(samples)?;' ''
mutate $econ '    crate::schema::tiers(data)?;' ''
mutate $schema '        (Ty::Int, Value::Number(n)) => n.is_i64(),' '        (Ty::Int, Value::Number(_)) => true,'
mutate $schema '        (Ty::NumOnly, Value::Null) => false,' '        (Ty::NumOnly, Value::Null) => true,'
mutate $schema '                if !x.is_string() {' '                if false {'
mutate $schema '        (Ty::Bool, Value::Bool(_)) => true,' '        (Ty::Bool, Value::Bool(_) | Value::Number(_)) => true,'
mutate $schema '        (Ty::Str, Value::String(_)) => true,' '        (Ty::Str, Value::String(_) | Value::Number(_)) => true,'
mutate $schema '        let Value::Object(n) = n else { return Err(refuse(&at, "a mapping", n)) };' '        let Value::Object(n) = n else { continue };'
mutate $schema '        Some(other) => Err(refuse(field, "a mapping", other)),' '        Some(_) => Ok(None),'
mutate $schema '        Some(other) => Err(refuse(field, "a list", other)),' '        Some(_) => Ok(&[]),'
mutate $schema '                if lit.parse::<i64>().is_err() && lit.parse::<u64>().is_err() {' '                if false {'
mutate $schema '                if lit.parse::<f64>().is_ok_and(|f| !f.is_finite()) {' '                if false {'
mutate $schema '            if matches!(digits, "NaN" | "Infinity") {' '            if false {'
mutate $schema "                if lit.starts_with('-') && digits.bytes().all(|d| d == b'0') {" '                if false {'
# YAML 1.1 and 1.2 read these plain scalars differently; a key that is not a string.
mutate $port '                    TScalarStyle::Plain => plain_scalar_outside(v, is_key),' '                    TScalarStyle::Plain => None,'
mutate $port '    if is_key && py != Yaml11::Str {' '    if false {'
# What the two readers scan differently; what the check cannot scan (held by the YAML golden:
# PyYAML's reading of 6,000 fuzzed documents, replayed by port.rs's unit test).
mutate $port '            Err(e) => return Some(format!("YAML the subset check cannot scan: {e}")),' '            Err(_) => return None,'
mutate $port "    if v.contains('?') {" '    if false {'
mutate $port "            '\\t' => \"a tab\"," "            '\\t' => continue,"
mutate $port "            '\\r' => \"a carriage return outside a CRLF line ending\"," "            '\\r' => continue,"
mutate $port "            '\\u{85}' | '\\u{2028}' | '\\u{2029}' => \"a Unicode line or paragraph separator\"," "            '\\u{85}' | '\\u{2028}' | '\\u{2029}' => continue,"
mutate $port "            '\\u{feff}' if i > 0 => \"a byte-order mark past the start\"," "            '\\u{feff}' if false => \"a byte-order mark past the start\","
mutate $port '            c if c.is_control() => "a control character",' '            c if false && c.is_control() => "a control character",'
mutate $port '        Yaml11::Bool if matches!(v, "true" | "True" | "TRUE" | "false" | "False" | "FALSE") => None,' '        Yaml11::Bool => None,'
mutate $port '            if plain && same { None } else { differ() }' '            if same { None } else { differ() }'
mutate $port '            Some(serde_yaml::Value::String(x)) if x == v => None,' '            _ if true => None,'
mutate $port '    if date || stamp {' '    if date {'
mutate $port '        bin || oct || dec || hex || sexa' '        bin || dec || hex'
mutate $port '        plain || dot || sexa || inf || nan' '        plain || inf || nan'
mutate crates/core/src/py.rs "    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)" '    c.is_whitespace()'
mutate crates/core/src/py.rs '    crate::py_printable::NONPRINTABLE.get(i).is_none_or(|&(start, _)| n < start)' '    crate::py_printable::NONPRINTABLE.get(i).is_none_or(|&(start, _)| n <= start)'
mutate $econ '    (DT_MIN_US..=DT_MAX_US).contains(&us).then_some(us)' '    Some(us)'
mutate $econ '    if !matches!(&s[i..], "Z" | "+00:00") {' '    if false {'
mutate $econ '    if hh > 23 || mm > 59 || ss > 59 {' '    if hh > 24 || mm > 59 || ss > 59 {'
mutate $econ '        if !(1..=6).contains(&n) {' '        if !(0..=9).contains(&n) {'
mutate $econ '    if !(1..=9999).contains(&y) || !(1..=12).contains(&m) || !(1..=dim[m as usize - 1]).contains(&d) {' '    if !(1..=9999).contains(&y) || !(1..=12).contains(&m) || !(1..=31).contains(&d) {'
mutate $econ '        Some(Value::String(s)) if s.is_empty() => Ok(None),' ''
mutate $schema '        (Ty::Time, Value::String(s)) => s.is_empty() || crate::economy::timestamp_us(s).is_some(),' '        (Ty::Time, Value::String(_)) => true,'
mutate $schema '        (Ty::Time, Value::Number(n)) => crate::economy::epoch_us(n.as_f64().unwrap_or(f64::NAN)).is_some(),' '        (Ty::Time, Value::Number(_)) => true,'
mutate $econ '    if (DT_MIN_US..=DT_MAX_US).contains(&sum) { Ok(sum) } else { Err(overflow()) }' '    Ok(sum)'
mutate $econ '    if opened < DT_MIN_US {' '    if false {'
# P20 port r5: documents Python reads that serde_json refuses are refused, never skipped as not
# JSON; an empty string is falsy where the Python tests it; a duplicate `repos` key is the last
# one; a flag outside its choices exits 2 (held by the CLI goldens and crates/cli/tests/delegation.rs).
mutate $schema 'if PYTHON_READS.iter().any(|p| msg.starts_with(p))' 'if false'
mutate $econ '.as_str().filter(|c| !c.is_empty()) {' '.as_str() {'
mutate $port 'let spec = a.spec.as_deref().filter(|s| !s.is_empty()).map(py::normpath);' 'let spec = a.spec.as_deref().map(py::normpath);'
mutate $port 'a.ticket.as_deref().filter(|t| !t.is_empty())' 'a.ticket.as_deref()'
mutate $port '    if choices.contains(&v.as_str()) {' '    if true {'
mutate $port "        if path.starts_with('~') && !(path == \"~\" || path.starts_with(\"~/\")) {" '        if false {'
mutate $port '    choice(d::norm_mode(s), &["normal", "conserve", "critical", "read_only"])' '    choice(s.to_string(), &["normal", "conserve", "critical", "read_only"])'
mutate $port '    #[arg(long, value_parser = tier_choice)]' '    #[arg(long)]'
mutate $port '    if py::truthy(graph.get("repos")) && bind.root.as_deref().is_some_and(|r| !r.is_empty()) {' '    if py::truthy(graph.get("repos")) && bind.root.is_some() {'
mutate $port '                            repos = m.next_value::<Option<Keys>>()?;' '                            repos = repos.or(m.next_value::<Option<Keys>>()?);'

# View (BRO-3203): the catalog validator drops what it cannot place and records each drop; an
# unread source renders unavailable, never 0; an ask card carries only its own ledger's errors;
# the page fetches nothing; an unchanged poll sends no html; the poll is offered to the App only.
cat=crates/view/src/catalog.rs
render=crates/view/src/render.rs
page=crates/view/src/page.rs
mutate $cat 'if let Some(extra) = raw.keys().find(|k| !matches!(k.as_str(), "type" | "props" | "children")) {' 'if let Some(extra) = None::<&String> {'
mutate $cat '        if self.placed.contains(id) {' '        if false {'
mutate $cat '        if depth > MAX_DEPTH {' '        if false {'
mutate $cat '        if self.placed.len() >= MAX_BLOCKS {' '        if false {'
mutate $cat '            if !kind.children {' '            if false {'
mutate $cat 'if !walk.placed.contains(id.as_str()) && !walk.dropped.contains(id.as_str()) {' 'if false {'
mutate $cat '    if value.chars().count() > MAX_HANDLE {' '    if false {'
mutate $cat '    if value.chars().any(char::is_control) {' '    if false {'
mutate $cat '    if value.chars().any(is_format) {' '    if false {'
mutate $cat '        let Some(kind) = KINDS.iter().find(|k| k.name == kind_name) else {' '        let Some(kind) = KINDS.iter().find(|k| k.name == kind_name).or(KINDS.first()) else {'
mutate $cat '        Some(n) if n == 0 || n > MAX_LIMIT =>' '        Some(n) if n > MAX_LIMIT =>'
mutate $render '        (None, _) => "unavailable",' '        (None, _) => "ok",'
mutate $render '                None => span class="mv-tile-value mv-unavailable" { "unavailable" },' '                None => span class="mv-tile-value" { "0" },'
mutate $render '            || file_at.is_err()' '            || true'
mutate $render '        [] if unread => html! {' '        [] if false => html! {'
mutate $render '                @for i in list.iter().take(MAX_ROWS) {' '                @for i in list.iter() {'
mutate $cat '    if blocks.len() > MAX_SPEC_BLOCKS {' '    if false {'
mutate $cat "        | '\\u{E0001}' | '\\u{E0020}'..='\\u{E007F}')" "        )"
mutate $cat '        if self.drops.len() < MAX_DROPS {' '        if true {'
mutate $cat '    handle("root", root)?;' '    let _ = root;'
mutate $cat '        handle("block id", id)?;' '        let _ = id;'
mutate $cat '            handle("block key", key)?;' '            let _ = key;'
mutate $cat '            handle("type", kind)?;' '            let _ = kind;'
mutate $cat '            handle("child id", child)?;' '            let _ = child;'
mutate $cat 'format!("invalid props: {}", clip(&e))' 'format!("invalid props: {e}")'
mutate $cat '            return Err("the spec takes only `root` and `blocks`".into());' '            return Err(format!("the spec takes only `root` and `blocks`, not `{key}`"));'
mutate $cat '        .find(|(_, n)| *n > MAX_BLOCKS)' '        .find(|(_, n)| *n > usize::MAX - 1)'
mutate $cat '    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 50))]
    pub limit: Option<u8>,
}

#[derive(Debug, Clone, Copy, Default' '    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u8>,
}

#[derive(Debug, Clone, Copy, Default'
mutate $render '                @for r in list.iter().take(MAX_ROWS) {' '                @for r in list.iter() {'
mutate $render '                @for f in list.iter().take(MAX_ROWS) {' '                @for f in list.iter() {'
mutate $page "script-src 'sha256-{hash}'\")" "script-src 'unsafe-inline'\")"
mutate $page '            out.push(if i <= chunk.len() {' '            out.push(if i < chunk.len() {'
mutate $mcp '#[serde(deny_unknown_fields)]
pub struct UiRenderArgs {' 'pub struct UiRenderArgs {'
mutate $page '    while let Some(at) = rest.find("@font-face") {' '    while let Some(at) = None::<usize> {'
mutate $mcp '            if a.revision.as_deref() == Some(r.revision.as_str()) {' '            if false {'
mutate $mcp '        Ui::AppOnly => json!(["app"]),' '        Ui::AppOnly => json!(["model", "app"]),'

# Decisions (BRO-3269): tabs by class, open asks only, answers waiting to land and an unread
# working tree said, lower bounds said, the plugin's link rules, the queue order, ages, and the
# answer bar only on an open ask that has not been answered locally.
scr=crates/view/src/screens
mutate $scr/mod.rs 'Some(AskClass::Authority) => Tab::Authority,' ''
mutate $scr/mod.rs '.filter(|(_, ask)| ask.state() == AskState::Open)' '.filter(|_| true)'
mutate $scr/mod.rs 'landing: landing(cx, lf, ask),' 'landing: Landing::NotSeen,'
mutate $scr/mod.rs 'if unlanded.errors.iter().any(|e| covers(&e.at, &lf.file)) {' 'if false {'
mutate $scr/mod.rs 'if cx.reads.asks.errors.iter().any(|e| e.at != cx.reads.asks.source && covers(&e.at, &lf.file)) {' 'if false {'
mutate $scr/mod.rs 'if !refs.iter().any(|r| r.ledger == lf.file && r.id == ask.handle()) {' 'if !refs.iter().any(|r| r.id == ask.handle()) {'
mutate $scr/mod.rs 'Some((_, name)) if name.ends_with(".yaml") || name.ends_with(".yml") => name == file,' 'Some((_, name)) if name.ends_with(".yaml") => name == file,'
mutate $scr/mod.rs '    if lf.ledger.asks.iter().filter(|a| a.handle() == ask.handle()).count() > 1 {
        Landing::Unknown' '    if false {
        Landing::Unknown'
mutate $scr/mod.rs 'Some((_, name)) if name.ends_with(".yaml") || name.ends_with(".yml") => name == file,' 'Some((_, name)) if name.ends_with(".yaml") || name.ends_with(".yml") => true,'
mutate $scr/mod.rs 'ambiguous: ambiguous(lf, ask),' 'ambiguous: false,'
mutate $scr/mod.rs 'None => a.uid.is_none() && a.id == ask.id,' 'None => a.id == ask.id,'
mutate $scr/mod.rs '(!a.ask.blocking, a.asked()' '(a.ask.blocking, a.asked()'
mutate $scr/mod.rs '0..86_400 =>' '0..1 =>'
mutate $scr/mod.rs '(!g.rows.iter().any(|r| r.ask.blocking), g.oldest()' '(false, g.oldest()'
mutate $scr/mod.rs '.map(|id| if uuid(&id) { id[..8].to_string() } else { id })' '.map(|id| id)'
mutate $scr/inbox.rs '} @else if cx.unlanded().data.is_none() {' '} @else if false {'
mutate $scr/inbox.rs 'let partial = !section.errors.is_empty() || rows().any(|e| e.landing == Landing::Unknown);' 'let partial = !section.errors.is_empty();'
mutate $scr/inbox.rs 'let partial = !section.errors.is_empty() || rows().any(|e| e.landing == Landing::Unknown);' 'let partial = rows().any(|e| e.landing == Landing::Unknown);'
mutate $scr/inbox.rs '.filter(|_| e.tab() != Tab::Ideas)' ''
mutate $scr/decision.rs 'if a.class == Some(AskClass::Idea) {' 'if false {'
mutate crates/store/src/views.rs '(Some(all), Some(here)) => all.extend(here),' '(Some(_), Some(_)) => {}'
mutate crates/store/src/views.rs '            (None, errors)' '            (Some(Answered::new()), errors)'
mutate crates/store/src/views.rs 'Some(uid) => format!("u:{uid}"),' 'Some(uid) => format!("i:{uid}"),'
mutate crates/store/src/views.rs '        _ => answered = None,' '        _ => {}'
mutate crates/store/src/views.rs 'unlanded: waiting.data.as_ref().map(Vec::len),' 'unlanded: Some(waiting.data.as_ref().map_or(0, Vec::len)),'
mutate $scr/decision.rs '        @if landing == Landing::Unknown {' '        @if false {'
mutate $scr/decision.rs '@if a.blocking && !a.gates.is_empty() {' '@if !a.gates.is_empty() {'
mutate $scr/rich.rs 'let label = text(row.get("label"), LABEL_MAX);' 'let label = text(row.get("label"), usize::MAX);'
mutate $scr/sources.rs "path.is_none_or(|p| p.bytes().all(|b| matches!(b, b'!'..=b'~')))" "path.is_none_or(|p| p.bytes().all(|b| matches!(b, b' '..=b'~')))"
mutate $scr/sources.rs "|| c == '\\u{7f}'" ''
mutate $scr/decision.rs '} @else if state == AskState::Archived {' '} @else if false {'
mutate $scr/decision.rs '" shows whether it has been answered since, and is where to answer it."' '" (answer it there)"'
mutate $scr/inbox.rs 'p class="bv-meta" { (elsewhere) }' 'p class="bv-meta" {}'
mutate $scr/inbox.rs '@if cx.store.tree.source == maestro_store::TreeSource::WorkingTree {' '@if false {'
mutate $scr/inbox.rs 'Some(_) if !worktree =>' 'Some(_) =>'
mutate $scr/decision.rs 'given.or(a.answer.as_deref()).or(a.answer_option.as_deref())' 'given.or(a.answer_option.as_deref())'
mutate $scr/inbox.rs 'Landing::NotSeen => @if let Some(r) = e.ask.recommended().filter(' 'Landing::NotSeen => @if let Some(r) = e.ask.options.first().filter('
mutate $scr/inbox.rs '@if count(tab) == 0 {' '@if false {'
mutate crates/view/src/render.rs 'self.ids.set(n + 1);' 'self.ids.set(n);'
mutate crates/store/src/views.rs '.filter(|(_, a)| a.state() == AskState::Answered)' '.filter(|(_, a)| a.state() != AskState::Open)'
mutate $scr/sources.rs '"url" if https(r) => Some(r.to_string()),' '"url" => Some(r.to_string()),'
mutate $scr/sources.rs "host.bytes().all(|b| matches!(b, b'!'..=b'.' | b'0'..=b'~'))" 'true'
mutate $scr/sources.rs '"ticket" if ticket(r) =>' '"ticket" =>'
mutate $scr/sources.rs 'if !KINDS.contains(&kind) || !shape(kind, r) {' 'if false {'
mutate $scr/sources.rs 'part != "." && part != ".."' 'part != "."'
mutate $scr/sources.rs '.take(MAX)' '.take(usize::MAX)'
mutate $scr/rich.rs '(!t.is_empty() && flat_length(t) <= max)' '(!t.is_empty())'
mutate $scr/rich.rs 'if option.is_none() == label.is_none() {' 'if option.is_none() && label.is_none() {'
mutate $scr/rich.rs '.take(TRADEOFF_ROWS_MAX)' '.take(usize::MAX)'
mutate $scr/rich.rs 'summary: words(get("summary"), SUMMARY_MAX),' 'summary: words(get("summary"), usize::MAX),'
mutate $scr/rich.rs '.map(|d| d.chars().take(DETAILS_MAX).collect())' '.map(|d| d.to_string())'
mutate $scr/rich.rs 'if option.as_ref().is_some_and(|o| !ask.options.iter().any(|x| &x.id == o)) {' 'if false {'
mutate $scr/decision.rs '[] if unread =>' '[] if false =>'
mutate $scr/decision.rs '} @else if state == AskState::Answered {' '} @else if false {'
mutate $scr/decision.rs '} @else if state == AskState::Open && landing == Landing::Waiting {' '} @else if false {'
mutate $scr/decision.rs 'let mut matches: Vec<_> = all().filter(|(_, a)| a.uid.as_deref() == Some(ask_id)).collect();' 'let mut matches: Vec<_> = all().filter(|(_, a)| a.uid.as_deref() == Some(ask_id) || a.id == ask_id).collect();'
mutate $scr/rich.rs 'Some(JsValue::Num(x)) => Some(js_string(*x)),' 'Some(JsValue::Num(x)) => Some(x.to_string()),'
mutate $scr/mod.rs '    errors.extend(cx.unlanded().errors.iter().filter(|e| ledger.is_none_or(|l| covers(&e.at, l))).cloned());' ''
mutate $scr/decision.rs '            @if state == AskState::Open && landing == Landing::NotSeen {
                @if let Some(d) = &a.default {' '            @if state == AskState::Open {
                @if let Some(d) = &a.default {'
mutate $scr/decision.rs '            @if state == AskState::Open && landing == Landing::NotSeen {
                @if let Some(d) = &a.default {' '            @if landing == Landing::NotSeen {
                @if let Some(d) = &a.default {'
mutate $scr/decision.rs '            @if ambiguous {' '            @if false {'

echo "mutation  $killed of $total killed (shard $shard_i/$shard_n of $seen mutants)"
[ "$total" -gt 0 ] || { echo "shard $shard_i/$shard_n ran no mutant"; failed=1; }
exit $failed
