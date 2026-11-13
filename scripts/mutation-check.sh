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
# A mutant in crates/flow runs maestro-flow's tests and, only if they all pass, the tests of every
# package that depends on maestro-flow (BRO-3633 and BRO-3634; the whole-workspace run per mutant
# no longer fit the CI shards). A mutant is killed when any test fails, so the `&&` gives the
# verdict of running both, and a mutant the flow tests kill (all of them, so far) costs no
# dependent build. That verdict holds only while every dependent is listed here: the run refuses
# to start when the workspace has a dependent this list lacks, or the list names a package that
# is no longer one.
flow_dependents="maestro-mcp maestro"
run_flow_tests() {
  local deps="" d
  for d in $flow_dependents; do deps+=" -p $d"; done
  timeout "$limit" sh -c "cargo test -q -p maestro-flow --locked && cargo test -q --locked$deps"
}
if ! dependents=$(cargo metadata -q --format-version 1 --no-deps --locked | python3 -c '
import json, sys
m = json.load(sys.stdin)
print(" ".join(sorted(p["name"] for p in m["packages"]
               if p["name"] != "maestro-flow" and any(d["name"] == "maestro-flow" for d in p["dependencies"]))))'); then
  echo "cannot read the workspace metadata"; exit 3
fi
listed=$(printf '%s\n' $flow_dependents | LC_ALL=C sort | tr '\n' ' ' | sed 's/ $//')
if [ "$dependents" != "$listed" ]; then
  echo "maestro-flow's dependents are ($dependents), but its mutants run ($listed): update flow_dependents"; exit 3
fi

# A mutant only counts as killed against a suite that passes unmutated.
if ! run_tests >log 2>&1; then
  echo "baseline is red: fix the suite before reading any mutant verdict"; cat log; exit 2
fi
if ! timeout "$limit" python3 scripts/conformance/test_phase1.py >log 2>&1; then
  echo "conformance baseline is red"; cat log; exit 2
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
  case $file in
    scripts/conformance/*) timeout "$limit" python3 scripts/conformance/test_phase1.py >log 2>&1 ;;
    crates/flow/*) run_flow_tests >log 2>&1 ;;
    *) run_tests >log 2>&1 ;;
  esac
  code=$?
  # Killed means a named test failed. A build error, a timeout, a crash or any other exit
  # proves nothing about the guard. `cargo test -q` lists failing tests, indented, after the
  # second `failures:` line.
  caught=$(awk '/^failures:$/ { n++; next } n >= 2 && /^    [^ ]/ { sub(/^ +/, ""); print; exit }' log)
  if [ "$code" = 0 ]; then
    echo "SURVIVED  $file: $old"; failed=1
  elif [[ $file == scripts/conformance/* ]] && [ "$code" = 1 ] && grep -qE '^FAILED \(failures=[0-9]+\)$' log; then
    echo "killed    $file: $old  [conformance guard test]"; killed=$((killed + 1))
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
# Flow writes: only a verified agent session; neither unverified callers nor the owner principal.
mutate $auth '(Principal::Agent { .. }, Authority::AgentWrite) => true,' '(_, Authority::AgentWrite) => true,'
mutate $auth '(Principal::Agent { .. }, Authority::AgentWrite) => true,' '(Principal::Agent { .. } | Principal::Owner { .. }, Authority::AgentWrite) => true,'
# Views: needs are open blocking asks; loop drift is only what is provable.
mutate $views 'if ask.state() != AskState::Open || !ask.blocking {' 'if ask.state() != AskState::Open {'
mutate $views 'TICK_CLOSE_SECS.max(2 * median(&mut lengths))' 'TICK_CLOSE_SECS'
mutate $views 'if secs(&t.started_at).is_some_and(|s| now.as_second() - s > limit) {' 'if false {'
# Reading: the plugin's ledger limit, bounded reads, UTF-8 only, regular files only.
mutate $store 'if t.len() > LEDGER_LIMIT {' 'if false {'
mutate $tree 'if bytes.len() > limit {' 'if false {'
mutate $tree 'if size > limit as u64 {' 'if false {'
mutate $store 'let text = self.tree.read_bounded(&rel, LEDGER_LIMIT)' 'let text = self.tree.read_bounded(&rel, tree::MAX_READ)'
mutate $store 'if let Some((i, reason)) = bad.first() {' 'if let Some((i, reason)) = bad.first().filter(|_| false) {'
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
# Text: unread prints where and why, never a count; partial names its partial read (BRO-3322); hosts claims no adapter.
mutate $cli '("unreadable", unavailable(&errors))' '("unreadable", line)'
mutate $cli '("partial", format!("{line}{}", partial(&errors)))' '("partial", line)'
mutate $cli '[e] => one(e),' '[e] => s(&e["reason"]).to_string(),'
mutate $cli '[e, rest @ ..] => format!("{}; and {} more, use --json", one(e), rest.len()),' '[e, ..] => one(e),'
mutate $cli 'println!("Needs you: {}", unavailable(&errors_of(&v)));' 'println!("Needs you: unknown");'
mutate $cli 'println!("asks: {}", unavailable(&errors_of(&v)));' 'println!("asks: unreadable");'
mutate $cli 'let from = partial(&errors_of(&v));' 'let from = String::new();'
mutate $cli 'println!("work: {}", unavailable(&errors_of(&v)));' ''
mutate $cli '    println!("These are what each surface and harness can do. Maestro launches and sends nothing yet (BRO-3276).");
' ''
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
# Web pages (BRO-3588): only the bound host, no cross-site request, the policy on every page.
web=crates/mcp/src/web.rs
page=crates/view/src/page.rs
mutate $web 'if !hosts.iter().any(|h| h.eq_ignore_ascii_case(name)) {' 'if false {'
mutate $web '        (Some(site), None) => matches!(site.to_str(), Ok("same-origin" | "none")),' '        (Some(_), None) => true,'
mutate $web '        (Some(_), Some(_)) => false,' '        (Some(_), Some(_)) => true,'
mutate $web "    if value.contains('@') || (auth.as_str() != auth.host() && auth.port_u16().is_none()) {" "    if false {"
mutate $web "let host = host.strip_suffix('.').unwrap_or(host);" "let host = host;"
mutate $web '    if uri.scheme().is_some() || uri.authority().is_some() {' '    if false {'
mutate $web '    let (Some(host), None) = (values.next(), values.next()) else { return false };' '    let Some(host) = values.next() else { return false };'
mutate $web '    section.data.is_some() && matches.is_empty() && unread.is_empty()' '    section.data.is_some() && matches.is_empty()'
mutate $web '            && absent(&store, ledger, id)' '            && false'
mutate $web '    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(page::WEB_CSP));' '    let _ = page::WEB_CSP;'
mutate $web '    if !admitted(uri, headers, &web.hosts) {
        return refused();
    }
    let spec' '    let spec'
# T3 answer handoff (BRO-3590): a safe thread, an encoded prompt, the thread reaching the render.
handoff=crates/view/src/handoff.rs
mutate $handoff "s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))" 's.bytes().all(|_| true)'
mutate $handoff "        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {" '        if true {'
mutate $handoff '        let ok = |s: &str| ok(s) && s != "." && s != "..";' '        let ok = |s: &str| ok(s);'
mutate $handoff '    prompt.encode_utf16().count() <= MAX_PROMPT' '    prompt.encode_utf16().count() <= usize::MAX'
mutate $handoff '    let json = |v: &serde_json::Value| ascii_json(&v.to_string());' '    let json = |v: &serde_json::Value| v.to_string();'
mutate $handoff '        if c.is_ascii() && !c.is_ascii_control() {' '        if true {'
mutate $handoff '        Some(uid) => serde_json::json!({ "uid": uid }),' '        Some(_) => serde_json::json!({ "id": a.id, "uid": null }),'
mutate $web '    AnswerThread::parse(value?)' '    AnswerThread::parse(value.unwrap_or("e/t"))'
decision=crates/view/src/screens/decision.rs
mutate $decision 'filter(|_| a.class != Some(AskClass::Idea) && !a.options.is_empty())' 'filter(|_| !a.options.is_empty())'
mutate $decision 'filter(|_| a.class != Some(AskClass::Idea) && !a.options.is_empty())' 'filter(|_| a.class != Some(AskClass::Idea))'
mutate $decision '        @if landing == Landing::Unknown {' '        @if false {'
mutate $decision '        } @else if state == AskState::Archived {' '        } @else if false {'
mutate $web 'render_with(&store, &validated, at, thread.as_deref())' 'render_with(&store, &validated, at, None)'
mutate $page 'aria-current=[(path == current).then_some("page")]' 'aria-current=[(path != current).then_some("page")]'
# SIGTERM is a clean stop. (The handler-before-readiness order is held by a probabilistic test,
# sigterm_right_after_the_readiness_line_is_a_clean_stop: under this harness's load the race closes.)
mutate $cli 'let mut term = signal(SignalKind::terminate())?;' 'let mut term = signal(SignalKind::user_defined1())?;'
mutate $mcp 'source: store.tree.source,' 'source: store.tree.commit.clone().map_or(store.tree.source, maestro_store::TreeSource::GitRev),'
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

# Agent requests (BRO-3267): the plugin's §5.5 lock as ported (the journal's), the filer's
# refusals, the writer of record called with Maestro's session, and every caller held.
locks=crates/store/src/locks.rs
requests=crates/store/src/requests.rs
request=crates/core/src/request.rs
mutate $locks 'if !out.status.success() || out.stdout.iter().all(u8::is_ascii_whitespace) {' 'if false {'
mutate $locks '(Ps::Running(Some(actual)), Some(s)) => (actual - s).abs() <= 2,' '(Ps::Running(Some(_)), Some(_)) => true,'
mutate $locks 'Some((!alive && age >= stale_after, words))' 'Some((!alive, words))'
mutate $locks '    if !still_ours(&lock) {' '    if false {'
mutate $locks '    hash.update(b"/");' '    hash.update(b"");'
mutate $locks 'Ok(hash.finalize().iter().take(8)' 'Ok(hash.finalize().iter().take(9)'
mutate $requests '    maestro_core::ask::OUTCOME_KEYS.contains(&key) || FILER_REFUSED.contains(&key)' '    FILER_REFUSED.contains(&key)'
mutate $requests $'    "answered_by",\n    "answer_option",' '    "answer_option",'
mutate $requests '.args(["--json", "-", &format!("--maestro-session={session}"), "--wait"])' '.args(["--json", "-", "--maestro-session=x", "--wait"])'
mutate $requests '        Ok(m) if m.is_file() => {}' '        Ok(_) => {}'
mutate $requests '    if id.is_empty() {' '    if false {'
mutate $request '            (Self::AskFile, _) => Disposition::Applied,' '            (Self::AskFile, _) => Disposition::Held,'
mutate $request '            (Self::WorkQueue | Self::ActionRequest, _) => Disposition::Held,' '            (Self::WorkQueue | Self::ActionRequest, _) => Disposition::Recorded,'
mutate $mcp '        None => (session.clone(), Principal::Unverified),' '        None => (session.clone(), Principal::Owner { via: "mcp".into() }),'
# A filed ask whose journal line fails is a failed request; a busy lock exits 75 to retry (P20 r1).
mutate $mcp '            if let Err(e) = requests::record(&disk.paths, &record) {' '            if let Some(e) = requests::record(&disk.paths, &record).err().filter(|_| false) {'
mutate $mcp '        Some(json!({ "filed": { "ledger": ledger, "id": id } })),' '        Some(json!({ "filed": { "ledger": ledger, "id": id }, "busy": true })),'
mutate $mcp '        Some(json!({ "filed": { "ledger": ledger, "id": id } })),' '        Some(json!({ "filed": { "ledger": ledger, "id": id }, "busy": is_busy(e) })),'
mutate $locks '        Some(text) => holder_alive(text),' '        Some(_text) => false,'
mutate $mcp 'McpError::internal_error(other.to_string(), busy.then(|| json!({ "busy": true })))' 'McpError::internal_error(other.to_string(), None)'
mutate $cli '            if e.downcast_ref::<Busy>().is_some() {' '            if false {'
mutate $mcp '                            .idempotent(spec.authority == Authority::Read)' '                            .idempotent(true)'
# Decisions (BRO-3269): tabs by class, open asks only, answers waiting to land and an unread
# working tree said, lower bounds said, the plugin's link rules, the queue order, ages, and the
# answer bar only on an open ask that has not been answered locally.
scr=crates/view/src/screens
mutate $scr/mod.rs 'Some(AskClass::Authority) => Tab::Authority,' ''
mutate $scr/mod.rs '.filter(|(_, ask)| ask.state() == AskState::Open)' '.filter(|_| true)'
mutate $scr/mod.rs 'landing: landing(cx, lf, ask),' 'landing: Landing::NotSeen,'
mutate crates/store/src/views.rs 'if unlanded.errors.iter().any(|e| may_hide(&e.at, &lf.file)) || ambiguous(lf, ask) {' 'if ambiguous(lf, ask) {'
mutate crates/store/src/views.rs 'if asks.errors.iter().any(|e| e.at != asks.source && may_hide(&e.at, &lf.file)) {' 'if false {'
mutate crates/store/src/views.rs 'self.ledger == ledger && self.key == key(ask)' 'self.key == key(ask)'
mutate crates/store/src/views.rs 'self.ledger == ledger && self.key == key(ask)' 'self.ledger == ledger'
mutate crates/store/src/views.rs 'Some((_, name)) if name.ends_with(".yaml") || name.ends_with(".yml") => name == ledger,' 'Some((_, name)) if name.ends_with(".yaml") => name == ledger,'
mutate crates/store/src/views.rs 'Some((_, name)) if name.ends_with(".yaml") || name.ends_with(".yml") => name == ledger,' 'Some((_, name)) if name.ends_with(".yaml") || name.ends_with(".yml") => true,'
mutate $scr/mod.rs 'ambiguous: ambiguous(lf, ask),' 'ambiguous: false,'
mutate crates/store/src/views.rs 'None => a.uid.is_none() && a.id == ask.id,' 'None => a.id == ask.id,'
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
# BRO-3269: origin's maestro/answers, as last fetched, is read with this clone's.
mutate crates/store/src/views.rs '("refs/remotes/origin/maestro/answers", "origin/maestro/answers")' '("refs/remotes/origin/none", "origin/maestro/answers")'
mutate crates/store/src/views.rs '(Some(union), Some(at_ref)) => union.extend(at_ref),' '(Some(_), Some(_)) => {}'
mutate crates/store/src/views.rs '            _ => union = None,' '            _ => {}'
mutate crates/store/src/views.rs '                (None, vec![SourceError { at: format!("{label}:{name}"), reason }])' '                (None, vec![SourceError { at: format!("{ANSWERS_BRANCH}:{name}"), reason }])'
mutate $scr/decision.rs '        @if landing == Landing::Unknown {' '        @if false {'
mutate $scr/decision.rs '@if a.blocking && !a.gates.is_empty() {' '@if !a.gates.is_empty() {'
mutate $scr/rich.rs 'let label = text(row.get("label"), LABEL_MAX);' 'let label = text(row.get("label"), usize::MAX);'
mutate $scr/rich.rs 'text(row.get(*axis), CELL_MAX)?' 'text(row.get(*axis), usize::MAX)?'
mutate $scr/sources.rs 'let label = super::rich::words(s.get("label"), 80);' 'let label = super::rich::words(s.get("label"), usize::MAX);'
mutate $scr/inbox.rs '@if e.ask.blocking { " · "' '@if false { " · "'
mutate $scr/sources.rs "path.is_none_or(|p| p.bytes().all(|b| matches!(b, b'!'..=b'~')))" "path.is_none_or(|p| p.bytes().all(|b| matches!(b, b' '..=b'~')))"
mutate $scr/sources.rs "|| c == '\\u{7f}'" ''
mutate $scr/decision.rs '} @else if state == AskState::Archived {' '} @else if false {'
mutate $scr/decision.rs '" shows whether it has been answered since, and is where to answer it."' '" (answer it there)"'
mutate $scr/inbox.rs '                    (elsewhere)
                }' '                }'
mutate $scr/inbox.rs '@if cx.store.tree.source == maestro_store::TreeSource::WorkingTree {' '@if false {'
mutate $scr/inbox.rs 'Some(_) if !worktree =>' 'Some(_) =>'
mutate $scr/decision.rs 'given.or(a.answer.as_deref()).or(a.answer_option.as_deref())' 'given.or(a.answer_option.as_deref())'
mutate $scr/inbox.rs 'Landing::NotSeen => @if let Some(r) = e.ask.recommended().filter(' 'Landing::NotSeen => @if let Some(r) = e.ask.options.first().filter('
mutate $scr/inbox.rs '@if count(tab) == 0 {' '@if false {'
mutate crates/view/src/render.rs 'self.ids.set(n + 1);' 'self.ids.set(n);'
mutate crates/store/src/views.rs 'closed_in(&d, |s| s == AskState::Answered)' 'closed_in(&d, |s| s != AskState::Open)'
mutate crates/store/src/views.rs 'closed_in(&asks, |s| s != AskState::Open)' 'closed_in(&asks, |s| s == AskState::Answered)'
mutate crates/store/src/views.rs 'return match branch.tree.has_ref(name) {' 'return match Ok::<bool, String>(false) {'
mutate crates/store/src/tree.rs '            Some(2) => Ok(false),' '            Some(1) | Some(2) => Ok(false),'
mutate crates/store/src/views.rs 'at: if e.at.starts_with("git:") { e.at.clone() } else { format!("{label}:{}", e.at) },' 'at: e.at.clone(),'
mutate crates/store/src/views.rs '.filter(|_| whole && !store.reads_worktree());' '.filter(|_| whole);'
mutate crates/mcp/src/lib.rs '"waiting_to_land": if worktree { None } else {' '"waiting_to_land": if false { None } else {'
mutate crates/store/src/views.rs 'Some(refs.iter().any(|r| r.is(&lf.file, ask)))' 'Some(refs.iter().any(|r| r.ledger == lf.file))'
mutate crates/store/src/views.rs '|| ambiguous(lf, ask) {' '{'
mutate crates/store/src/views.rs 'first.entry((lf.file.clone(), key(a))).or_insert_with(|| a.state());' 'first.insert((lf.file.clone(), key(a)), a.state());'
mutate crates/store/src/views.rs 'let whole = whole_tree && waiting.errors.is_empty();' 'let whole = waiting.errors.is_empty();'
mutate crates/store/src/views.rs 'let whole = whole_tree && waiting.errors.is_empty();' 'let whole = whole_tree;'
mutate crates/store/src/views.rs '.filter(|k| first.get(*k).is_none_or(|s| *s == AskState::Open))' '.filter(|k| first.get(*k) == Some(&AskState::Open))'
mutate $scr/inbox.rs 'Some(n) => (n, false),' 'Some(n) => (n, true),'
mutate $scr/inbox.rs '.filter(|e| e.landing == Landing::Waiting).count(), true),' '.filter(|e| e.landing == Landing::Waiting).count(), false),'
mutate $scr/inbox.rs 'let waiting = match cx.snap.asks.data.as_ref().and_then(|c| c.unlanded) {' 'let waiting = match None::<usize> {'
mutate $scr/inbox.rs '            @if p.project.is_none() {' '            @if true {'
mutate $scr/inbox.rs '@if filtered { "Counted for the whole checkout, not this project alone. " }' '@if false { "" }'
mutate $scr/inbox.rs '(banner(cx, waiting.0, waiting.1, p.project.is_some()))' '(banner(cx, waiting.0, waiting.1, true))'
mutate $scr/decision.rs '@if state == AskState::Open && landing == Landing::NotSeen && !ambiguous { "Choose one" }' '@if state == AskState::Open { "Choose one" }'
mutate crates/cli/src/main.rs '    } else if a["unlanded"].is_null() {' '    } else if false {'
mutate crates/cli/src/main.rs '    let landing = if v["tree"] == "worktree" {' '    let landing = if false {'
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
mutate $scr/mod.rs '    errors.extend(cx.unlanded().errors.iter().filter(|e| ledger.is_none_or(|l| views::may_hide(&e.at, l))).cloned());' ''
mutate $scr/decision.rs '            @if state == AskState::Open && landing == Landing::NotSeen {
                @if let Some(d) = &a.default {' '            @if state == AskState::Open {
                @if let Some(d) = &a.default {'
mutate $scr/decision.rs '            @if state == AskState::Open && landing == Landing::NotSeen {
                @if let Some(d) = &a.default {' '            @if landing == Landing::NotSeen {
                @if let Some(d) = &a.default {'
mutate $scr/decision.rs '            @if ambiguous {' '            @if false {'

# Today and the health rail (BRO-3270).
td=crates/view/src/screens/today.rs
hl=crates/view/src/screens/health.rs
mutate $td 'cx.reads.work.data.as_deref().map(|w| maestro_store::views::work_by_state(w, Some(WorkState::Running)).len())' 'cx.reads.work.data.as_deref().map(|w| maestro_store::views::work_by_state(w, None).len())'
mutate $td '        Count { n: None, .. } => "What needs you could not be read.".to_string(),' '        Count { n: None, .. } => "Nothing blocking needs you.".to_string(),'
mutate $td '                None => span class="mv-pill-value mv-unavailable" { "unavailable" },' '                None => span class="mv-pill-value" { "0" },'
mutate $td '.find(|e| e.landing != Landing::Waiting)' '.find(|_| true)'
mutate $hl '    t.data.is_some() && t.errors.is_empty()' '    t.data.is_some()'
mutate $hl '        && !d.whole()' '        && false'
mutate $hl '    let count = (section.data.is_some() && !partial).then_some(list.len());' '    let count = section.data.as_ref().map(|_| list.len());'
mutate $td '        Count { n, partial: n.is_some() && !section.errors.is_empty() }' '        Count { n, partial: false }'
mutate $td '    empty(if partial {' '    empty(if false {'
mutate $td '        Count { n: Some(n), partial: true } => format!("{n} found needing you {PART}."),' '        Count { n: Some(_), partial: true } if false => String::new(),'
mutate $hl '    if !row.drift.is_empty() {' '    if false {'
mutate $hl '    } else if known && has(NoticeKind::NoTickReceipts) {' '    } else if has(NoticeKind::NoTickReceipts) {'
mutate $hl '    } else if !known {' '    } else if false {'
mutate $hl '    } else if row.desired == Desired::Paused {' '    } else if false {'
mutate $hl '            None => Beat::NoTicks,' '            None => Beat::Ok,'
mutate $hl '=> Beat::LastFailed,' '=> Beat::Ok,'
mutate $hl '            Some(t) if t.is_closed() => Beat::Ended,' '            Some(_) => Beat::Ended,'
mutate $hl 'Some(t) if t.is_closed() && t.outcome == Some(TickOutcome::Ok) => Beat::Ok,' 'Some(t) if t.is_closed() => Beat::Ok,'
mutate $hl '    } else if row.desired == Desired::Retired {' '    } else if false {'
mutate $hl '(from..=cx.at.as_second())' '(i64::MIN..=cx.at.as_second())'
mutate $hl '(from..=cx.at.as_second())' '(from..=i64::MAX)'
mutate $hl '            @if !known {' '            @if false {'
mutate $hl '                .filter(|i| i.status.is_live())' ''
mutate $hl '        _ => None,
    };
    Section { source: plans.source.clone(), data, errors }' '        _ => Some(Vec::new()),
    };
    Section { source: plans.source.clone(), data, errors }'
mutate $hl '@if cap.get("known") == Some(&Value::Bool(false)) {' '@if false {'
mutate crates/view/src/render.rs '    let running = by(maestro_core::work::WorkState::Running);' '    let running = s.work.data.as_ref().map(|w| w.get("Running").copied().unwrap_or(0));'
mutate crates/store/src/lib.rs '            Ok(_) => Section::unavailable(label, "it is not a list of usage rows"),' '            Ok(_) => Section::ok(label, Vec::new()),'
mutate crates/store/src/views.rs '        Err(e) => Section::unavailable(usage.source.clone(), e.to_string()),' '        Err(_) => Section::ok(usage.source.clone(), serde_json::json!({})),'
mutate crates/store/src/tree.rs 'out.trim().parse::<jiff::Timestamp>().map(Some)' 'out.trim().parse::<jiff::Timestamp>().map(|_| None)'


# Host identity: Paseo, T3, Direct, symlinks, prefix boundary, cross-surface spoofs, bridge credentials, disconnect and permission needs (BRO-3268).
hp=crates/hosts/src/paseo.rs
ht=crates/hosts/src/t3.rs
hlb=crates/hosts/src/lib.rs
mutate $hp 'if parsed.id != expected_id {' 'if false {'
mutate $hp 'if self.disconnected || !is_safe_id(id) {' 'if false {'
mutate $hp 'if !liveness.is_live() {' 'if false {'
mutate $hp 'if !within(Path::new(cwd), Path::new(&agent.cwd)) {' 'if false {'
mutate $hp 'if let Ok(canon) = std::fs::canonicalize(path) {' 'if let Ok(canon) = std::fs::canonicalize(lexical_normalize(path)) {'
mutate $hp 'inside.starts_with(&outside)' 'inside.to_string_lossy().starts_with(&*outside.to_string_lossy())'
mutate $hp '&& cred.uid != want_uid' '&& false'
mutate $hlb 'if in_t3 && !in_paseo {' 'if false {'
mutate $hlb 'if in_paseo && !in_t3 {' 'if false {'
mutate $hlb 'Sha256::digest(t.as_bytes()) == Sha256::digest(s.token.as_bytes())' 'true'
mutate $hlb 'if cred.generation != current_gen {' 'if false {'
mutate $ht 'if self.disconnected {' 'if false {'
mutate $ht '        } else if self.settled_at.is_some() && self.status.as_deref() != Some("running") {' '        } else if false {'
mutate $ht '            .filter(|s| !s.trim().is_empty())
            .map(|created_at| format!("t3-owner:{}:{created_at}", thread.id));' '            .or(Some(""))
            .map(|created_at| format!("t3-owner:{}:{created_at}", thread.id));'
mutate $ht 'format!("t3-owner:{}:{created_at}", thread.id)' 'format!("t3-owner:{}:{}", thread.id, created_at.len() * 0)'
mutate $hp '            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|handle_id|' '            .as_deref()
            .or(Some(""))
            .map(|handle_id|'
mutate $hp 'format!("paseo-owner:{}:{handle_id}", agent.id)' 'format!("paseo-owner:{}:{handle_id}:{:?}", agent.id, agent.pid)'
mutate $hlb 'format!("direct-owner:{}", hex_sha256(session.token.as_bytes()))' 'format!("direct-owner:{}:{}", session.cwd, hex_sha256(session.token.as_bytes()))'
mutate $session '        self.pending_permissions' '        Vec::<PermissionPrompt>::new()'
mutate $mcp 'if !hosts.has_surface(&session.surface) && !has_claim_meta {' 'if true {'
mutate $mcp 'if arg_cred.is_none() && (session.surface != maestro_core::session::Surface::Direct || arg_token.is_none()) {' 'if false {'
mutate $mcp 'if packet.scope.project.is_none() && packet.scope.initiative.is_none() {' 'if true {'
mutate $hlb 'if effective_now_ms < cred.issued_at_ms || effective_now_ms >= cred.expires_at_ms {' 'if false {'
mutate $hlb 'if lock_dir_stale(&lock_dir, &lock_dir) == Some(true) {' 'if false {'
mutate $hlb 'msg.extend_from_slice(incarnation.as_bytes());' ''

# Flow ledger storage (BRO-3633, BRO-3624 items 1, 2, 3 and 8): one lock and one sequence rule,
# writes through the root directory's descriptor, locks never rewritten in place, genesis,
# archive and recovery, retire, the hash-chained log and the projection.
fl=crates/flow/src
# Item 1: one exclusive lock, readers shared, released on drop, one sequence rule.
mutate $fl/lock.rs 'Mode::Exclusive => file.try_lock(),' 'Mode::Exclusive => file.try_lock_shared(),'
mutate $fl/lock.rs 'Mode::Shared => file.try_lock_shared(),' 'Mode::Shared => Ok(()),'
mutate $fl/ledger.rs 'let _ = self.ledger.lock_file.unlock();' 'let _ = &self.ledger;'
mutate $fl/ledger.rs 'if expected_seq != log.seq() {' 'if false {'
# The root directory's own lock, held by every writer and by archive, and checked to be the
# directory the writer opened.
mutate $fl/lock.rs '    take(&dir.fd, mode, wait, root)?;' ''
mutate $fl/lock.rs '        && dir.id()? != want' '        && false'
mutate $fl/lock.rs '    if root_id(root)? != dir.id()? {' '    if false {'
# Item 2: host, epoch and lock identity when the lock is taken.
mutate $fl/ledger.rs 'if owner != host.as_str() {' 'if false {'
mutate $fl/ledger.rs '        && found != expected' '        && false'
mutate $fl/ledger.rs 'if !lock::same_file(&self.lock_file, &self.layout.lock)? {' 'if false {'
mutate $fl/ledger.rs '            check_identity(layout::inspect(&self.layout)?, &self.layout, &self.host, Some(&self.epoch))?;' ''
mutate $fl/ledger.rs '    if std::fs::symlink_metadata(&layout.root).is_ok() {
        return match open() {' '    if false {
        return match open() {'
mutate $fl/layout.rs '        None => Err(FlowError::LockLost {
            lock: layout.lock.clone(),
            detail: format!(
                "the lock is {} beside a live root",
                if lock.is_some() { "not an epoch" } else { "missing" }
            ),
        }),' '        None => Ok(State::Live { host, epoch }),'
mutate $fl/layout.rs '    text.len() == 32 && text.bytes()' '    text.bytes()'
# Genesis: a loser waits on the winner's lock; a crash after the link is refused, then finished.
mutate $fl/ledger.rs '        lock::take(&file, Mode::Shared, options.lock_wait, &layout.lock)?;' ''
# A genesis re-checks, holding its linked lock, that nothing was archived or put in place
# meanwhile, and takes its lock back if so.
mutate $fl/ledger.rs '    if archived || layout::exists(&layout.root)? {' '    if layout::exists(&layout.root)? {'
mutate $fl/ledger.rs '    if archived || layout::exists(&layout.root)? {' '    if archived {'
mutate $fl/ledger.rs '        if lock::same_file(&file, &layout.lock)? {
            let _ = std::fs::remove_file(&layout.lock);
        }' ''
mutate $fl/ledger.rs 'let archived = !after_archive && layout::exists(' 'let archived = layout::exists('
mutate $fl/layout.rs '            (_, Some(e)) if exists(&layout.stage(e))? => Err(FlowError::GenesisIncomplete {' '            (_, Some(e)) if false && exists(&layout.stage(e))? => Err(FlowError::GenesisIncomplete {'
mutate $fl/operator.rs '            std::fs::rename(layout.stage(epoch), &layout.root).map_err(FlowError::io(&layout.root))?;' ''
mutate $fl/operator.rs '            let _ = std::fs::remove_file(layout.archived_marker());
            sync_dir(&layout.parent);
            return Ok(Recovered::FinishedGenesis' '            sync_dir(&layout.parent);
            return Ok(Recovered::FinishedGenesis'
# Writes go through the held root's descriptor, never a path; the projection only while the
# path still names that root.
mutate $fl/ledger.rs '        let mut file = dir
            .file(EVENTS, OFlags::WRONLY | OFlags::APPEND | OFlags::CREATE)?
            .ok_or_else(|| FlowError::UnknownInstance { instance: id.to_string() })?;' '        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(self.ledger.layout.root.join(INSTANCES).join(id.as_str()).join(EVENTS))
            .map_err(FlowError::io(&dir.path))?;'
mutate $fl/ledger.rs '                instances.mkdir(id.as_str())?;' '                std::fs::create_dir_all(self.ledger.layout.root.join(INSTANCES).join(id.as_str())).map_err(FlowError::io(&instances.path))?;'
mutate $fl/projection.rs '    if lock::root_id(&layout.root)? != root_id {' '    if false {'
# No link under the root is followed: directories, logs, the root itself, the projection.
mutate $fl/at.rs '    if e == Errno::LOOP {' '    if false {'
mutate $fl/at.rs '            Err(_) if self.kind(name)? == Some(FileType::Symlink) => {' '            Err(_) if false => {'
mutate $fl/at.rs '            Err(_) if std::fs::symlink_metadata(root).is_ok_and(|m| m.file_type().is_symlink()) => {' '            Err(_) if false => {'
mutate $fl/at.rs 'flags | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode' 'flags | OFlags::CLOEXEC, Mode'
mutate $fl/at.rs '            if name != "." && name != ".." && self.is_dir(name)? {' '            if name != "." && name != ".." {'
mutate $fl/projection.rs 'Ok(Connection::open_with_flags(db_path(layout)?, flags | OpenFlags::SQLITE_OPEN_NOFOLLOW)?)' 'Ok(Connection::open_with_flags(db_path(layout)?, flags)?)'
mutate $fl/projection.rs 'match Connection::open_with_flags(&path, flags | OpenFlags::SQLITE_OPEN_NOFOLLOW) {' 'match Connection::open_with_flags(&path, flags) {'
# A lock is never rewritten in place: written whole, then renamed over.
mutate $fl/ledger.rs '    std::fs::rename(&private, &layout.lock).map_err(FlowError::io(&layout.lock))?;' '    std::fs::copy(&private, &layout.lock).map_err(FlowError::io(&layout.lock))?;'
# Identity checks while opening, for writers, readers and the operator.
mutate $fl/ledger.rs '            if !lock::same_file(&file, &layout.lock)? {
                return Err(FlowError::LockLost {
                    lock: layout.lock.clone(),
                    detail: "the lock file was replaced while this writer opened it".into(),' '            if false {
                return Err(FlowError::LockLost {
                    lock: layout.lock.clone(),
                    detail: "the lock file was replaced while this writer opened it".into(),'
mutate $fl/ledger.rs '            if !lock::same_file(&file, &layout.lock)? {
                return Err(FlowError::LockLost {
                    lock: layout.lock.clone(),
                    detail: "the lock file was replaced while this reader opened it".into(),' '            if false {
                return Err(FlowError::LockLost {
                    lock: layout.lock.clone(),
                    detail: "the lock file was replaced while this reader opened it".into(),'
mutate $fl/operator.rs '    let out = if lock::same_file(file, &layout.lock)? {' '    let out = if true {'
# recover: waits for a guard before restoring a missing lock, restores a broken one, removes
# only the exact debris shapes no process holds.
mutate $fl/operator.rs '        let held_root = lock::lock_root(&layout.root, None, Mode::Exclusive, options.lock_wait)?;' '        let held_root = std::fs::File::open(&layout.root).map_err(FlowError::io(&layout.root))?;'
mutate $fl/operator.rs '            if named.is_none() {' '            if false {'
mutate $fl/operator.rs '        if layout::exists(&layout.lock)? {
            return Err(refused("A lock appeared' '        if false {
            return Err(refused("A lock appeared'
mutate $fl/operator.rs '        clear_debris(&layout, None)?;' ''
mutate $fl/projection.rs 'Ok(has_meta && get("epoch")?.as_deref() == Some(epoch))' 'Ok(has_meta)'
mutate $fl/layout.rs ' && !b.is_ascii_uppercase()' ''
mutate $fl/operator.rs '        let debris = clear_debris(&layout, named)?;' '        let debris = 0;'
mutate $fl/operator.rs 'file.strip_prefix(&stage).filter(|e| is_epoch(e))' 'file.strip_prefix(&stage)'
mutate $fl/operator.rs '            if meta.is_dir() && keep != Some(epoch) {' '            if keep != Some(epoch) {'
mutate $fl/operator.rs '            if meta.is_dir() && keep != Some(epoch) {' '            if meta.is_dir() {'
mutate $fl/operator.rs 'file.strip_prefix(p.as_str()).is_some_and(is_epoch)' 'file.strip_prefix(p.as_str()).is_some()'
mutate $fl/operator.rs '            && meta.is_file()' ''
mutate $fl/operator.rs '(Some(file_id(&meta)) == live || File::open(&path).is_ok_and(|f| f.try_lock().is_ok()))' 'File::open(&path).is_ok_and(|f| f.try_lock().is_ok())'
mutate $fl/operator.rs '(Some(file_id(&meta)) == live || File::open(&path).is_ok_and(|f| f.try_lock().is_ok()))' 'true'
mutate $fl/operator.rs '        if debris > 0 {' '        if false {'
mutate $fl/operator.rs '        let debris = debris + usize::from(layout::exists(&marker)?);
        let _ = std::fs::remove_file(&marker);' ''
mutate $fl/operator.rs '        layout::inspect(&layout)?;
        // A crash after' '        // A crash after'
# A fresh ledger is refused beside a live root before it touches anything, and re-checks under
# the lock.
mutate $fl/operator.rs '    if in_place()? {
        return Err(still_there());
    }
    pause!("fresh_checked");' '    pause!("fresh_checked");'
mutate $fl/operator.rs '        if in_place()? {
            return Err(still_there());
        }
        let epoch = random_epoch()?;' '        let epoch = random_epoch()?;'
# archive: destination checked first (parent, absent, outside the root, same filesystem), the
# root directory's lock taken as a writer does, the destination made before the marker.
mutate $fl/operator.rs '    let Ok(parent) = std::fs::canonicalize(parent) else {' '    let Ok(parent) = Ok::<_, std::io::Error>(parent.to_path_buf()) else {'
mutate $fl/operator.rs 'if existing.is_some() && !reuse {' 'if false {'
mutate $fl/operator.rs '        let reuse = existing.as_ref().is_some_and(|m| m.file_type().is_dir())
            && std::fs::read_dir(dest).is_ok_and(|mut d| d.next().is_none());' '        let reuse = false;'
mutate $fl/operator.rs 'existing.as_ref().is_some_and(|m| m.file_type().is_dir())' 'existing.is_some()'
mutate $fl/operator.rs '            && std::fs::read_dir(dest).is_ok_and(|mut d| d.next().is_none());' ';'
mutate $fl/operator.rs '    lock::take(&file, Mode::Exclusive, options.lock_wait, &layout.lock)?;' ''
mutate $fl/operator.rs '        if !lock::same_file(&file, &layout.lock)? {
            return Err(FlowError::LockLost {
                lock: layout.lock.clone(),
                detail: "the lock file was replaced while the archive opened it".into(),' '        if false {
            return Err(FlowError::LockLost {
                lock: layout.lock.clone(),
                detail: "the lock file was replaced while the archive opened it".into(),'
mutate $fl/operator.rs '        let State::Live { epoch, .. } = layout::inspect(&layout)? else {' '        let State::Live { epoch, .. } = layout::inspect(&layout).unwrap_or(State::Live { host: String::new(), epoch: String::new() }) else {'
mutate $fl/operator.rs '        if lock::root_id(&layout.root)? != held.id()? {' '        if false {'
mutate $fl/operator.rs '    if parent.starts_with(' '    if false && parent.starts_with('
mutate $fl/operator.rs 'if other_device(if reuse { dest } else { &parent }, &layout.parent)? {' 'if false {'
mutate $fl/operator.rs 'if other_device(if reuse { dest } else { &parent }, &layout.parent)? {' 'if other_device(&parent, &layout.parent)? {'
mutate $fl/operator.rs '        let held = lock::lock_root(&layout.root, None, Mode::Exclusive, options.lock_wait)?;' '        let held = crate::at::Dir::open_root(&layout.root)?;'
mutate $fl/operator.rs '        if !reuse {
            std::fs::create_dir(dest).map_err(FlowError::io(dest))?;
        }
        let marker = layout.archived_marker();
        std::fs::write(&marker, format!("{}\n{epoch}\n", dest.display())).map_err(FlowError::io(&marker))?;' '        let marker = layout.archived_marker();
        std::fs::write(&marker, format!("{}\n{epoch}\n", dest.display())).map_err(FlowError::io(&marker))?;
        if !reuse {
            std::fs::create_dir(dest).map_err(FlowError::io(dest))?;
        }'
mutate $fl/ledger.rs '    if layout::exists(&layout.archived_marker())? {
        return Err(FlowError::RootMoved { root: layout.root.clone() });
    }
    Ok(None)' '    Ok(None)'
# Item 3: retire.
mutate $fl/ledger.rs 'Some(retired) if retired.exists(id.as_str())? =>' 'Some(retired) if false && retired.exists(id.as_str())? =>'
mutate $fl/operator.rs '    if !instances.is_dir(id.as_str())? {' '    if false {'
# Item 8: the chain, torn tail (no newline only) against broken chain, reads write nothing, the projection.
mutate $fl/log.rs 'prev_event: log.last_hash.clone(),' 'prev_event: None,'
mutate $fl/log.rs 'if event.prev_event.as_deref() != want_prev {' 'if false {'
mutate $fl/log.rs 'if event.seq != want_seq {' 'if false {'
mutate $fl/log.rs '(FlowError::ChainBroken { instance: instance.to_string(), line: prefix.seq + 1, detail }, prefix.clone())' '(FlowError::TornTail { instance: instance.to_string(), verified_seq: prefix.seq, detail }, prefix.clone())'
mutate $fl/operator.rs 'file.set_len(prefix.len)' 'file.set_len(bytes.len() as u64)'
mutate $fl/projection.rs 'let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;' 'let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE;'
mutate $fl/projection.rs 'if current(&conn, epoch)? { sync_with(&mut conn, root) }' 'if true { sync_with(&mut conn, root) }'
mutate $fl/projection.rs 'if cached.get(id.as_str()) != Some(&stat_key(root, id)) {' 'if false {'
mutate $fl/projection.rs '    for id in cached.keys().filter(|id| !seen.contains(*id)) {
        delete_row(&tx, id)?;
    }' ''
mutate $fl/projection.rs '    for id in &ids {
        write_row(&tx, &analyze(root, id))?;
    }' '    for id in &ids {
        let row = analyze(root, id);
        if row.state == "ok" {
            write_row(&tx, &row)?;
        }
    }'
mutate $fl/ledger.rs 'Ok(Summary { summary: from_log, cache })' 'Ok(Summary { summary: match &cache { CacheState::Disagrees(Some(c)) => c.clone(), _ => from_log }, cache })'
mutate $fl/ids.rs "} else if !value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {" '} else if false {'
mutate $fl/operator.rs '        if let Err(e) = std::fs::rename(&layout.root, dest.join(layout.name())) {
            let _ = std::fs::remove_file(&marker);' '        if let Err(e) = std::fs::rename(&layout.root, dest.join(layout.name())) {'
# Flow storage bounds (BRO-3633 PR 3b1): an event line, a log and an event count over their bounds
# are refused at append, and when an existing log is read or verified.
mutate $fl/log.rs '    if line_len > MAX_LINE {' '    if line_len >= MAX_LINE {'
mutate $fl/log.rs '    if line_len > MAX_LINE {' '    if false {'
mutate $fl/log.rs '    if total > MAX_LOG {' '    if total >= MAX_LOG {'
mutate $fl/log.rs '    if total > MAX_LOG {' '    if false {'
mutate $fl/log.rs '    if events >= MAX_EVENTS {' '    if events > MAX_EVENTS {'
mutate $fl/log.rs '    if events >= MAX_EVENTS {' '    if false {'
mutate $fl/ledger.rs '        log::fits(line.len() as u64, existing_len, log.seq())?;' ''
mutate $fl/at.rs '        if bytes.len() as u64 > limit {' '        if bytes.len() as u64 >= limit {'
mutate $fl/at.rs '        if bytes.len() as u64 > limit {' '        if false {'
mutate $fl/log.rs '    if lines > MAX_EVENTS {' '    if lines >= MAX_EVENTS {'
mutate $fl/log.rs '    if lines > MAX_EVENTS {' '    if false {'
mutate $fl/log.rs '        if size > MAX_LINE {' '        if false {'
# Nesting: an append refuses a body the reader could not read back; whatever it takes reads back.
mutate $fl/log.rs '    if depth > MAX_NESTING {' '    if depth >= MAX_NESTING {'
mutate $fl/log.rs '    if depth > MAX_NESTING {' '    if false {'
mutate $fl/log.rs '        deepest = deepest.max(depth);' ''
mutate $fl/log.rs '        todo.extend(inner.into_iter().map(|v| (v, depth + 1)));' '        todo.extend(inner.into_iter().map(|v| (v, depth)));'
mutate $fl/ledger.rs '        log::shallow(&event.body)?;' ''

# Flow kernel (BRO-3633 PR 3b1): definitions, the compiler, guard checks and the read-only scheduler.
fk=crates/flow/src/kernel
# Compile rejections: unknown references, loops without a bound, undeclared switch cases,
# effects without reconcile, repeated or malformed ids, phase 2 reuse, class rules, size.
mutate $fk/definition.rs '#[serde(deny_unknown_fields)]
pub(crate) struct NodeDef {' 'pub(crate) struct NodeDef {'
mutate $fk/compile.rs '        if !seen.insert(id) {' '        if false && !seen.insert(id) {'
mutate $fk/compile.rs '    unique("param", params.iter().map(|(name, _)| *name))?;' ''
mutate $fk/compile.rs '            unique("edge", edges.iter().map(|(e, _)| e.as_str()))?;' ''
mutate $fk/compile.rs '                unique("exit_map", codes.iter().map(String::as_str))?;' ''
mutate $fk/compile.rs '    if def.uses.is_some() {' '    if false {'
mutate $fk/compile.rs '    if def.templates.is_some() {' '    if false {'
mutate $fk/compile.rs '.find(|(_, n)| n.uses.is_some())' '.find(|_| false)'
mutate $fk/compile.rs "    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {" '    if id.is_empty() {'
mutate $fk/compile.rs '        plain("param", name)?;' ''
mutate $fk/compile.rs '        if f.all.is_empty() {' '        if false {'
mutate $fk/compile.rs '        if let Some(b) = f.all.iter().find(|b| !node_ids.contains(b.as_str())) {' '        if let Some(b) = f.all.iter().find(|_| false) {'
mutate $fk/compile.rs '.into_iter().find(|f| !allowed.contains(f))' '.into_iter().find(|_| false)'
mutate $fk/compile.rs '        Ok(s) if s > 0 => Ok(s),' '        Ok(s) => Ok(s),'
mutate $fk/compile.rs '            if !needs.contains(&x) {' '            if true {'
mutate $fk/compile.rs '    if yaml.len() > MAX_SOURCE {' '    if yaml.len() >= MAX_SOURCE {'
mutate $fk/compile.rs '    if yaml.len() > MAX_SOURCE {' '    if false {'
mutate $fk/compile.rs '    if bytes > ir::MAX_IR {' '    if bytes >= ir::MAX_IR {'
mutate $fk/compile.rs '    if bytes > ir::MAX_IR {' '    if false {'
mutate $fk/ir.rs '    if ir.ir_version != want {' '    if false {'
# An IR is written in the lowest version that expresses it: 3 with an effect that runs a
# command, 2 with a workdir, 1 with neither; version 3 needs a command on every effect.
mutate $fk/ir.rs '    } else if ir.workdir.is_some() {
        2' '    } else if false {
        2'
mutate $fk/ir.rs '    } else {
        1
    }
}' '    } else {
        2
    }
}'
mutate $fk/ir.rs '    } else if runs_effect {' '    } else if false {'
mutate $fk/ir.rs 'Class::Effect { run, .. } if !run.is_empty()));' 'Class::Effect { .. }));'
mutate $fk/ir.rs '                if ir.ir_version >= 3 {' '                if false {'
mutate $fk/ir.rs '    if ir.nodes.len() > MAX_NODES {' '    if ir.nodes.len() >= MAX_NODES {'
mutate $fk/ir.rs '    if ir.nodes.len() > MAX_NODES {' '    if false {'
mutate $fk/ir.rs '        if reads > MAX_FAN_IN {' '        if reads >= MAX_FAN_IN {'
mutate $fk/ir.rs '        if reads > MAX_FAN_IN {' '        if false {'
mutate $fk/ir.rs '                    read.insert(m);
                    read.extend(body(r));' '                    read.insert(m);'
mutate $fk/ir.rs '                _ if ir.repeats.contains_key(m) => read.extend(body(m)),' '                _ if ir.repeats.contains_key(m) => {}'
mutate $fk/ir.rs '        let reads = read.len();' '        let reads = n.needs.len();'
mutate $fk/ir.rs '        if r.max == 0 || r.max > MAX_REPEAT {' '        if r.max > MAX_REPEAT {'
mutate $fk/ir.rs '        if r.max == 0 || r.max > MAX_REPEAT {' '        if r.max == 0 {'
mutate $fk/ir.rs '        if r.body.is_empty() {' '        if false {'
mutate $fk/ir.rs '        for b in &r.body {
            node(&from, b)?;' '        for b in &r.body {'
mutate $fk/ir.rs 'if let Some(other) = membership.insert(b.as_str(), rid.as_str()) {' 'if let Some(other) = membership.insert(b.as_str(), rid.as_str()).filter(|_| false) {'
mutate $fk/ir.rs '        if let Some(target) = &r.on_exhausted {
            node(&from, target)?;' '        if let Some(target) = &r.on_exhausted {'
mutate $fk/ir.rs '            if r.body.contains(target) {' '            if false {'
mutate $fk/ir.rs '                Need::Node { node: m } => {
                    node(&from, m)?;' '                Need::Node { node: m } => {
                    let _ = m;'
mutate $fk/ir.rs '        if n.retry == Some(0) {' '        if false {'
mutate $fk/ir.rs '                Class::Choice { edges, .. } => edges.contains_key(&case.value),' '                Class::Choice { .. } => true,'
mutate $fk/ir.rs '                Class::Observe { exit_map, .. } => exit_map.iter().any(|e| e.value == case.value),' '                Class::Observe { .. } => true,'
mutate $fk/ir.rs '                _ => false,
            };' '                _ => true,
            };'
mutate $fk/ir.rs '            if argv.is_empty() {' '            if false {'
mutate $fk/ir.rs '                if fresh_for.is_none() && inputs.is_empty() {' '                if false {'
mutate $fk/ir.rs '                if exit_map.is_empty() {' '                if false {'
mutate $fk/ir.rs '                        Input::Param { name } => param(&from, name)?,' '                        Input::Param { .. } => {}'
mutate $fk/ir.rs '                        Input::Node { node: m } => {
                            node(&from, m)?;' '                        Input::Node { node: m } => {'
mutate $fk/ir.rs '                            if !n.needs.contains(&Need::Node { node: m.clone() }) {' '                            if false {'
mutate $fk/ir.rs '                if edges.is_empty() {' '                if false {'
mutate $fk/ir.rs '                if reconcile.is_empty() {' '                if false {'
mutate $fk/ir.rs '                if ask.trim().is_empty() {' '                if false {'
# The topology: one pass refuses cycles (through needs, switches, repeat bodies, and a body node
# needed from outside its repeat) and chains deeper than MAX_DEPTH. Without it a cyclic pin
# recurses until the stack overflows.
mutate $fk/ir.rs '    topology(ir)
}' '    Ok(())
}' 'stack overflow'
mutate $fk/ir.rs '    if finished == deps.len() {' '    if true {' 'stack overflow'
mutate $fk/ir.rs '        if d > MAX_DEPTH {' '        if false {'
mutate $fk/ir.rs '                set.insert(format!("r:{repeat}"));' ''
mutate $fk/ir.rs '                set.insert(format!("r:{r}"));' ''
mutate $fk/ir.rs 'let on: Vec<&str> = on.chain(n.switch.as_ref().map(|c| c.on.as_str())).collect();' 'let on: Vec<&str> = on.collect();'
mutate $fk/ir.rs '        deps.entry(format!("r:{rid}")).or_default().extend(r.body.iter().map(|b| format!("n:{b}")));' '        deps.entry(format!("r:{rid}")).or_default();'
# Guards: checked at compile time, refused when they could recurse, run unbounded generators,
# read the clock or environment, have an effect or repeat strings.
mutate $fk/compile.rs '    check_guards(&ir)?;' ''
mutate $fk/compile.rs '            check(format!("repeat {rid} until"), until)?;' '            let _ = until;'
mutate $fk/compile.rs '                    check(format!("node {id} edge {edge}"), g)?;' '                    let _ = (edge, g);'
mutate $fk/compile.rs '                    check(format!("node {id} precondition {i}"), g)?;' '                    let _ = (i, g);'
mutate $fk/jq.rs '        Tok::Word if BANNED_WORDS.contains(s) => Some(*s),' '        Tok::Word if false => Some(*s),'
mutate $fk/jq.rs "        Tok::Sym if s.starts_with('*') => Some(*s)," '        Tok::Sym if false => Some(*s),'
mutate $fk/jq.rs '        Tok::Block(inner) => banned(inner),' '        Tok::Block(_) => None,'
mutate $fk/jq.rs '            StrPart::Term(t) => banned(std::slice::from_ref(t)),' '            StrPart::Term(_) => None,'
# start: params that do not fit are refused before anything is written; null is absent; params
# have a byte bound so a flow that compiles can always start.
mutate $fk/replay.rs '.find(|(name, required)| **required && !supplied(params, name))' '.find(|_| false)'
mutate $fk/replay.rs '    if let Some(name) = given.keys().find(|k| !ir.params.contains_key(*k)) {' '    if let Some(name) = given.keys().find(|_| false) {'
mutate $fk/replay.rs '    params.get(name).is_some_and(|v| !v.is_null())' '    params.get(name).is_some()'
mutate $fk/replay.rs '    if bytes > ir::MAX_PARAMS {' '    if bytes >= ir::MAX_PARAMS {'
mutate $fk/replay.rs '    if bytes > ir::MAX_PARAMS {' '    if false {'
mutate $fk/replay.rs '    if depth >= crate::log::MAX_NESTING {' '    if depth > crate::log::MAX_NESTING {'
mutate $fk/replay.rs '    if depth >= crate::log::MAX_NESTING {' '    if false {'
# The pin: a Started first, a pinned IR of this version that validates (without it a cyclic pin
# recurses until the stack overflows), matches its hash and is the flow it names.
mutate $fk/replay.rs '    if first.kind != Started::KIND {' '    if false {'
mutate $fk/replay.rs '    ir::validate(&started.ir)
        .map_err(|e| malformed(log, first.seq, format!("the pinned IR does not validate ({e})")))?;' '' 'stack overflow'
mutate $fk/replay.rs '    if ir::content_hash(&started.ir)? != started.ir_hash {' '    if false {'
mutate $fk/replay.rs '    if started.flow != format!("{}@{}", started.ir.flow, started.ir.version) {' '    if false {'
# Acceptance, in log order: kernel kinds only, a declared node, its current iteration, the right
# class, a declared edge, an enabled node not already done at the event's own clock, verdicts
# for a settled current iteration of a repeat with an until, once per body.
mutate $fk/replay.rs '        if !kinds.contains(&e.kind.as_str()) {' '        if false {'
mutate $fk/replay.rs 'let Some((id, node)) = ir.nodes.get_key_value(&subject.node) else {' 'let Some((id, node)) = ir.nodes.get_key_value(&subject.node).or_else(|| ir.nodes.iter().next()) else {'
mutate $fk/replay.rs 'let Some(i) = self.current(id).filter(|i| self.iteration(id, *i) == subject.iteration) else {' 'let Some(i) = self.current(id) else {'
mutate $fk/replay.rs '                Phase::Exhausted(_) => None,' '                Phase::Exhausted(i) => Some(*i),'
mutate $fk/replay.rs '                    Class::Choice { .. } => return Ok(Err("wrong_class")),' '                    Class::Choice { .. } => (None, None),'
mutate $fk/replay.rs 'let Class::Choice { edges, .. } = &node.class else { return Ok(Err("wrong_class")) };' 'let Class::Choice { edges, .. } = &node.class else { return Ok(Ok(Change::Inert)) };'
mutate $fk/replay.rs '                if !edges.contains_key(&c.edge) {' '                if false {'
mutate $fk/replay.rs 'match exit.and_then(|x| exit_map.iter().find(|m| m.exit == x)) {' 'match exit.or(Some(0)).and_then(|x| exit_map.iter().find(|m| m.exit == x)) {'
mutate $fk/replay.rs 'exit_map.iter().find(|m| m.exit == x)' 'exit_map.iter().find(|_| true)'
mutate $fk/replay.rs '        if !core.enabled {' '        if false {'
mutate $fk/replay.rs '        if eval.seen(key, &core).state == State::Done && !matches!(node.class, Class::Observe { .. }) {' '        if false {'
mutate $fk/replay.rs '        if eval.seen(key, &core).state == State::Done && !matches!(node.class, Class::Observe { .. }) {' '        if eval.seen(key, &core).state == State::Done {'
mutate $fk/replay.rs '        let mut eval = Eval::new(self, at);
        let core = eval.node(id, i);' '        let mut eval = Eval::new(self, Timestamp::MAX);
        let core = eval.node(id, i);'
mutate $fk/replay.rs '        if r.until.is_none() {' '        if false {'
mutate $fk/replay.rs '        if self.phases.get(rid) != Some(&Phase::At(g.iteration)) {' '        if false {'
mutate $fk/replay.rs '        if !eval.settled(rid, g.iteration) {' '        if false {'
mutate $fk/replay.rs '        if self.verdicts.get(&(rid, g.iteration)) == Some(&judged) {' '        if false {'
mutate $fk/replay.rs 'e.at.parse().map_err(|_| malformed(log, e.seq, format!("{} is not a timestamp", e.at)))?;' 'e.at.parse().unwrap_or(Timestamp::UNIX_EPOCH);'
# Repeats: a true verdict holds only for the body it judged; a false one moves the repeat on; it
# exhausts at max; a body that never runs is unreachable; a node not taken settles its iteration;
# a verdict is a change that can settle another repeat.
mutate $fk/replay.rs '        if g.holds {' '        if true {'
mutate $fk/replay.rs '            return Ok(self.guarded(g, at).map(|()| Change::State));' '            return Ok(self.guarded(g, at).map(|()| Change::Inert));'
mutate $fk/replay.rs '                    && *judged == self.body_digest(rid, i)' '                    && true'
mutate $fk/replay.rs 'e.digest.as_deref().filter(|_| e.satisfies)' 'e.digest.as_deref()'
mutate $fk/replay.rs '        if i.saturating_add(1) >= max { Phase::Exhausted(i) } else { Phase::At(i + 1) }' '        Phase::At(i + 1)'
mutate $fk/replay.rs '        evals.iter().all(|e| e.dead || e.satisfies) && !evals.iter().all(|e| e.dead)' '        evals.iter().all(|e| e.satisfies) && !evals.iter().all(|e| e.dead)'
mutate $fk/replay.rs '        evals.iter().all(|e| e.dead || e.satisfies) && !evals.iter().all(|e| e.dead)' '        evals.iter().all(|e| e.dead || e.satisfies)'
mutate $fk/replay.rs 'if self.body(rid).iter().all(|b| self.node(b, i).dead) {' 'if false {'
mutate $fk/replay.rs '                let awaiting_verdict = state == RepeatState::Running && eval.settled(rid, iteration);' '                let awaiting_verdict = state == RepeatState::Running;'
mutate $fk/replay.rs 'NodeStatus { state: State::Exhausted, class, iteration, unmet, inputs: BTreeMap::new() };' 'NodeStatus { state: State::Blocked, class, iteration, unmet, inputs: BTreeMap::new() };'
mutate $fk/replay.rs '            Loop::Exhausted(_) => Dep::Dead(Reason::RepeatExhausted { repeat: rid.to_string() }),' '            Loop::Exhausted(_) => Dep::Waits(Reason::RepeatRunning { repeat: rid.to_string() }),'
mutate $fk/replay.rs '            Loop::Done(j) => Dep::Eval(self.node(m, j)),' '            Loop::Done(_) => Dep::Waits(Reason::RepeatRunning { repeat: rid.to_string() }),'
mutate $fk/compile.rs '            node.needs.push(Need::Exhausted { repeat: rid.clone() });' ''
mutate $fk/replay.rs '                    Loop::Exhausted(_) => kill(&mut out, Reason::RepeatExhausted { repeat: r.clone() }),' '                    Loop::Exhausted(_) => push(&mut out, Reason::RepeatExhausted { repeat: r.clone() }),'
mutate $fk/replay.rs '                    Loop::Done(_) => kill(&mut out, Reason::RepeatFinished { repeat: r.clone() }),' '                    Loop::Done(_) => push(&mut out, Reason::RepeatFinished { repeat: r.clone() }),'
# Needs, the fork join, switches, dead branches; receipts done only while what they consumed,
# transitively, is current and fresh.
mutate $fk/compile.rs '            branches.iter().map(|b| Need::Node { node: b.clone() }).collect()' '            Vec::new()'
mutate $fk/replay.rs '                } else if !e.satisfies {' '                } else if false {'
mutate $fk/replay.rs '                    out.dead = true;
                    out.reasons.push(Reason::Unreachable { node: m.to_string() });' '                    out.reasons.push(Reason::Unreachable { node: m.to_string() });'
mutate $fk/replay.rs '                out.observed.extend(e.observed.iter().copied());
                // A node' '                // A node'
mutate $fk/replay.rs '                            out.observed.extend(e.observed.iter().copied());' ''
mutate $fk/replay.rs '                        out.consumed.insert(r.as_str(), judged);' ''
mutate $fk/replay.rs '            && e.value.as_deref() != Some(case.value.as_str())' '            && false'
mutate $fk/replay.rs '        if let Some(failure) = &r.failure {' '        if let Some(failure) = r.failure.as_ref().filter(|_| false) {'
mutate $fk/replay.rs '            if r.inputs.get(*input).map(String::as_str) != Some(&**d) {' '            if false {'
mutate $fk/replay.rs '        out.satisfies = out.reasons.is_empty();' '        out.satisfies = true;'
mutate $fk/replay.rs '            out.observed.insert((id, i));' ''
mutate $fk/replay.rs '        (age > secs(*f) || age.is_negative()).then_some(age)' '        (age >= secs(*f) || age.is_negative()).then_some(age)'
mutate $fk/replay.rs '        (age > secs(*f) || age.is_negative()).then_some(age)' '        (age > secs(*f)).then_some(age)'
mutate $fk/replay.rs '        (age > secs(*f) || age.is_negative()).then_some(age)' '        age.is_negative().then_some(age)'
mutate $fk/replay.rs '                reasons.extend(stale.iter().filter(|s| *s != id).map(|s| Reason::Stale { node: s.clone() }));' ''
mutate $fk/replay.rs '            Form::Open if !stale.is_empty() => {' '            Form::Open if false => {'
mutate $fk/replay.rs '        let digest = Rc::from(receipt_digest(outputs, value.as_deref()));' '        let digest = Rc::from(receipt_digest(outputs, None));'
mutate $fk/replay.rs 'let rerun = s.state == State::Stale && stale.get(&id).is_some_and(|set| set.iter().all(|s| *s == id));' 'let rerun = s.state == State::Stale;'
mutate $fk/replay.rs 'let rerun = s.state == State::Stale && stale.get(&id).is_some_and(|set| set.iter().all(|s| *s == id));' 'let rerun = false;'
mutate $fk/replay.rs '            Some(Class::Human { ask }) => {' '            Some(Class::Human { ask }) if false => {'
mutate $fk/replay.rs '    SignedDuration::from_secs(i64::try_from(s).unwrap_or(i64::MAX))' '    SignedDuration::from_mins(i64::try_from(s / 60).unwrap_or(i64::MAX))'
# Read-path bounds: inert and rejected events cost no evaluation; cores (evaluations without the
# clock) are kept across events, dropped downstream of a receipt with new content, and all
# dropped when a repeat moves or is judged; a re-run with the same result drops nothing.
mutate $fk/replay.rs '                Ok(Change::Inert) => {}' '                Ok(Change::Inert) => fold.advance(at),'
mutate $fk/replay.rs '        if !same {
            self.invalidate(id);
        }' ''
mutate $fk/replay.rs '            old.digest == receipt.digest && old.failure == receipt.failure && old.inputs == receipt.inputs' '            old.digest == receipt.digest && old.failure == receipt.failure'
mutate $fk/replay.rs '                    digests.retain(|(rid, _), _| rid != r);' ''
mutate $fk/replay.rs '                    reads.extend(body(repeat).iter().map(String::as_str))' '                    let _ = repeat;'
mutate $fk/replay.rs '                Need::Repeat { repeat } | Need::Exhausted { repeat } => {
                    reads.extend(body(repeat).iter().map(String::as_str))
                }' '                Need::Repeat { repeat } => reads.extend(body(repeat).iter().map(String::as_str)),
                Need::Exhausted { .. } => {}'
mutate $fk/replay.rs '                reads.extend(body(r).iter().map(String::as_str));' ''
mutate $fk/replay.rs '        let mut reads: Vec<&str> = n.switch.iter().map(|c| c.on.as_str()).collect();' '        let mut reads: Vec<&str> = Vec::new();'
mutate $fk/replay.rs '            self.forget();
            self.verdicts.insert((rid, g.iteration), judged);' '            self.verdicts.insert((rid, g.iteration), judged);'
mutate $fk/replay.rs '            self.forget();
            self.phases.insert(rid, next);' '            self.phases.insert(rid, next);'
mutate $fk/replay.rs '            self.forget();
            self.phases.extend(moves);' '            self.phases.extend(moves);'
# Positive controls: a replay that ran a command, judged a repeat from its guard, or was not a
# function of the log and the clock fails the tests that say it never does.
mutate $fk/replay.rs '        let fold = self.fold;
        let ir: &'"'"'a Ir = fold.ir;
        let Some(n) = ir.nodes.get(id) else {' '        let fold = self.fold;
        let ir: &'"'"'a Ir = fold.ir;
        if let Some(Class::Pure { run }) = ir.nodes.get(id).map(|n| &n.class) {
            let _ = std::process::Command::new(&run[0]).args(&run[1..]).status();
        }
        let Some(n) = ir.nodes.get(id) else {'
mutate $fk/replay.rs '                if let Some(judged) = self.fold.verdicts.get(&(rid, i))
                    && *judged == self.body_digest(rid, i)
                {' '                if self.fold.ir.repeats.get(rid).and_then(|r| r.until.as_deref()).is_some_and(|g| super::jq::check(g).is_ok()) {'
mutate $fk/replay.rs '            seq: log.seq(),
            terminal,' '            seq: jiff::Timestamp::now().as_nanosecond() as u64,
            terminal,'
# Flow writers (BRO-3633 PR 3b2a): ownership, claims, tokens, leases, receipts.
# Ownership: a marker (non-empty) to write anything, start included; only the owner's session
# and marker together; an instance with no recorded owner takes no write.
mutate $fk/write.rs '    let marker = session.owner_marker().filter(|m| !m.is_empty()).ok_or(Refusal::NoOwnerMarker)?;' '    let marker = session.owner_marker().ok_or(Refusal::NoOwnerMarker)?;'
mutate $fk/replay.rs '    let owner = Some(write::owner(session)?);' '    let owner = write::owner(session).ok();'
mutate $fk/write.rs '    let Some(owner) = &owners.owner else {' '    let Some(owner) = owners.owner.as_ref().or(Some(&me)) else {'
mutate $fk/write.rs '    if *owner != me {' '    if false {'
mutate $fk/write.rs '    if *owner != me {' '    if owner.marker_sha256 != me.marker_sha256 {'
mutate $fk/write.rs '    if *owner != me {' '    if owner.session != me.session {'
mutate $fk/write.rs '    check_owner(&log, &started, instance, session)?;
    let now = wall(&log)?;
    let Some(n) = started.ir.nodes.get(node) else {' '    let now = wall(&log)?;
    let Some(n) = started.ir.nodes.get(node) else {'
mutate $fk/write.rs '    let owners = check_owner(&log, &started, instance, session)?;
    let claims = Claims::fold(&log)?;' '    let owners = super::adoption::owners(&log, &started)?;
    let claims = Claims::fold(&log)?;'
# The clock (P20 round 2): every write reads the wall clock itself, under the lock, right before
# its checks; a run is stamped with its start and judged after it at a fresh read. A clock ahead
# of or behind the wall clock, or a run's end taken as its start plus a monotonic duration (which
# stops while the machine sleeps), is killed.
# The clock itself (P20 round 3): `clock::now` is one code path in every build, the wall clock;
# only the crate's own unit-test binary passes it through a test clock (`cfg(test)`). A frozen
# clock, or one an hour ahead or behind, is killed by a test that reads it with no test clock.
mutate $fl/clock.rs '    let wall = Timestamp::now();' '    let wall = Timestamp::constant(1_762_560_000, 0);'
mutate $fl/clock.rs '    let wall = Timestamp::now();' '    let wall = Timestamp::now() + jiff::SignedDuration::from_hours(1);'
mutate $fl/clock.rs '    let wall = Timestamp::now();' '    let wall = Timestamp::now() - jiff::SignedDuration::from_hours(1);'
mutate $fk/write.rs '    let now = wall(&log)?;
    let Some(n) = started.ir.nodes.get(node) else {' '    let now = wall(&log)? - SignedDuration::from_secs(30);
    let Some(n) = started.ir.nodes.get(node) else {'
mutate $fk/write.rs '    let now = wall(&log)?;
    let Some(n) = started.ir.nodes.get(node) else {' '    let now = wall(&log)? + SignedDuration::from_secs(30);
    let Some(n) = started.ir.nodes.get(node) else {'
mutate $fk/write.rs '    let now = wall(&log)?;
    let held = hold(log, session, &instance, &sha, now)?;
    let lease_until' '    let now = wall(&log)? + SignedDuration::from_secs(30);
    let held = hold(log, session, &instance, &sha, now)?;
    let lease_until'
mutate $fk/write.rs '    let now = wall(&log)?;
    let mut held = hold(log, session, &instance, &sha, now)?;
    check_verb(&held, false)?;' '    let now = wall(&log)? - SignedDuration::from_secs(30);
    let mut held = hold(log, session, &instance, &sha, now)?;
    check_verb(&held, false)?;'
mutate $fk/write.rs '    let ran_at = clock::now();' '    let ran_at = clock::now() + SignedDuration::from_secs(30);'
mutate $fk/write.rs '    let after = wall(&log)?;' '    let after = wall(&log)? - SignedDuration::from_secs(30);'
mutate $fk/write.rs '    let ran_at = clock::now();
    let ran = match &plan.snapshot {
        Some(commit) => snapshot::run_in(&plan.cwd, commit, &plan.argv, limits),
        None => exec::run(&plan.argv, &plan.cwd, limits, feed),
    };
    pause!("command_ran");

    // Everything again, under the lock, against the log as it is now and the clock read again.
    let mut guard = ledger.lock()?;
    let log = guard.log(&instance)?;
    let after = wall(&log)?;' '    let ran_at = clock::now();
    let began = std::time::Instant::now();
    let ran = match &plan.snapshot {
        Some(commit) => snapshot::run_in(&plan.cwd, commit, &plan.argv, limits),
        None => exec::run(&plan.argv, &plan.cwd, limits, feed),
    };
    pause!("command_ran");

    // Everything again, under the lock, against the log as it is now and the clock read again.
    let mut guard = ledger.lock()?;
    let log = guard.log(&instance)?;
    let after = ran_at + SignedDuration::try_from(began.elapsed()).unwrap_or(SignedDuration::ZERO);'
# A wall clock that stepped back below the instance's last event is refused, in claim, in every
# token use, and after a run.
mutate $fk/write.rs '    if now < at {' '    if false {'
mutate $fk/write.rs '    let now = wall(&log)?;
    let Some(n) = started.ir.nodes.get(node) else {' '    let now = clock::now();
    let Some(n) = started.ir.nodes.get(node) else {'
mutate $fk/write.rs '    let now = wall(&log)?;
    let held = hold(log, session, &instance, &sha, now)?;
    let lease_until' '    let now = clock::now();
    let held = hold(log, session, &instance, &sha, now)?;
    let lease_until'
mutate $fk/write.rs '    let now = wall(&log)?;
    let mut held = hold(log, session, &instance, &sha, now)?;
    check_verb(&held, false)?;' '    let now = clock::now();
    let mut held = hold(log, session, &instance, &sha, now)?;
    check_verb(&held, false)?;'
mutate $fk/write.rs '    let after = wall(&log)?;' '    let after = clock::now();'
# Claims: every class but human, an enabled node only (out of order and write-time freshness),
# one live claim per node, attempts counted per node and iteration with checked arithmetic.
mutate $fk/write.rs '    if matches!(n.class, Class::Human { .. }) {' '    if false {'
mutate $fk/write.rs '    let Some(enabled) = next.enabled.iter().find(|e| e.node == node) else {' '    let Some(enabled) = next.enabled.iter().find(|e| e.node == node).or(next.enabled.first()) else {'
mutate $fk/write.rs '        && !p.spent
        && now < p.lease_until' '        && now < p.lease_until'
mutate $fk/write.rs '        && now < p.lease_until' '        && now <= p.lease_until'
mutate $fk/write.rs '        && p.inputs == enabled.inputs' ''
mutate $fk/write.rs '    let last = prior.map_or(0, |p| p.attempt);' '    let last: u32 = 0;'
mutate $fk/write.rs '    let attempt = last.checked_add(1).ok_or_else(' '    let attempt = Some(last.wrapping_add(1)).ok_or_else('
mutate $fk/write.rs '                    if c.attempt <= last {' '                    if false {'
mutate $fk/write.rs '        token_sha256: line_hash(token.as_bytes()),' '        token_sha256: token.clone(),'
# Leases: the node's heartbeat, bounded; a checked clock; a heartbeat moves the lease.
mutate $fk/write.rs '    secs(node.class.wait().map_or(DEFAULT_LEASE_SECS, |w| w.heartbeat).min(MAX_LEASE_SECS))' '    secs(node.class.wait().map_or(DEFAULT_LEASE_SECS, |w| w.heartbeat))'
mutate $fk/write.rs '    secs(node.class.wait().map_or(DEFAULT_LEASE_SECS, |w| w.heartbeat).min(MAX_LEASE_SECS))' '    secs(DEFAULT_LEASE_SECS)'
mutate $fk/write.rs '    now.checked_add(lease).map_err(|_| Refusal::ClockOutOfRange)' '    Ok(now.checked_add(lease).unwrap_or(now))'
mutate $fk/write.rs '                        rec.lease_until = lease_until;' ''
mutate $fk/write.rs '                    if let Some(rec) = claims.latest_mut(&(h.node, h.iteration))
                        && rec.attempt == h.attempt' '                    if let Some(rec) = claims.latest_mut(&(h.node, h.iteration))'
# Tokens: bounded; a spent token is refused; a lease ends at its time, or when a later claim
# took the node; the claim's iteration must be current (not moved on, not exhausted) and its
# inputs unchanged; a receipt of the claim's attempt spends it.
mutate $fk/write.rs '    if token.len() > MAX_TOKEN {' '    if token.len() >= MAX_TOKEN {'
mutate $fk/write.rs '    if token.len() > MAX_TOKEN {' '    if false {'
mutate $fk/write.rs '    if rec.spent {' '    if false {'
mutate $fk/write.rs '                    if let Some(rec) = claims.latest_mut(&key)
                        && rec.attempt == c.attempt
                    {
                        rec.spent = true;' '                    if let Some(rec) = claims.latest_mut(&key)
                        && rec.attempt == c.attempt
                    {'
mutate $fk/write.rs '                    if let Some(rec) = claims.latest_mut(&(c.node, c.iteration))
                        && rec.attempt == c.attempt
                    {
                        rec.spent = true;' '                    if let Some(rec) = claims.latest_mut(&(c.node, c.iteration))
                        && rec.attempt == c.attempt
                    {'
mutate $fk/write.rs '    if latest != Some(sha) || now >= rec.lease_until {' '    if now >= rec.lease_until {'
mutate $fk/write.rs '    if latest != Some(sha) || now >= rec.lease_until {' '    if latest != Some(sha) || now > rec.lease_until {'
mutate $fk/write.rs '    if latest != Some(sha) || now >= rec.lease_until {' '    if latest != Some(sha) {'
mutate $fk/write.rs '    if *iteration != rec.iteration || state == Some(State::Exhausted) {' '    if state == Some(State::Exhausted) {'
mutate $fk/write.rs '    if *iteration != rec.iteration || state == Some(State::Exhausted) {' '    if *iteration != rec.iteration {'
mutate $fk/write.rs '    if *inputs != rec.inputs {' '    if false {'
# Complete: limits and outputs bounded; enabled (fresh) before and after the run; the flows
# lock released while the command runs; everything checked again after it at the clock read
# again; only the command's own exit status makes a receipt.
mutate $fk/write.rs '    limits.check().map_err(|detail| Refusal::InvalidLimits { detail })?;' ''
mutate $fk/write.rs '    check_outputs(outputs)?;' ''
mutate $fk/write.rs '    if !(outputs.is_object() || outputs.is_null()) {' '    if false {'
mutate $fk/write.rs '    if bytes > MAX_OUTPUTS {' '    if bytes >= MAX_OUTPUTS {'
mutate $fk/write.rs '    if bytes > MAX_OUTPUTS {' '    if false {'
mutate $fk/write.rs '    if depth >= MAX_NESTING {' '    if depth > MAX_NESTING {'
mutate $fk/write.rs '    if depth >= MAX_NESTING {' '    if false {'
mutate $fk/write.rs '    if !held.next.enabled.iter().any(|e| e.node == held.rec.node) {' '    if false {'
mutate $fk/write.rs '    drop(guard);
    let ran_at = clock::now();
    let ran = match &plan.snapshot {
        Some(commit) => snapshot::run_in(&plan.cwd, commit, &plan.argv, limits),
        None => exec::run(&plan.argv, &plan.cwd, limits, feed),
    };' '    let ran_at = clock::now();
    let ran = match &plan.snapshot {
        Some(commit) => snapshot::run_in(&plan.cwd, commit, &plan.argv, limits),
        None => exec::run(&plan.argv, &plan.cwd, limits, feed),
    };
    drop(guard);'
mutate $fk/write.rs '    let held = hold(log, session, &instance, &sha, after)?;' '    let _ = (log, after);
    let held = held;'
mutate $fk/write.rs '    let after = wall(&log)?;' '    let after = now;'
mutate $fk/write.rs '    // The command is a function of the pinned IR and params, so only the rest can have changed.
    if let Err(unfit) = prepare(&held, &outputs) {' '    // The command is a function of the pinned IR and params, so only the rest can have changed.
    if let Err(unfit) = Ok::<(), Unfit>(()) {'
mutate $fk/write.rs '    let Some(exit) = code else {' '    let Some(exit) = code.or(Some(0)) else {'
mutate $fk/write.rs 'Class::Observe { exit_map, .. } => (!exit_map.iter().any(|m| m.exit == exit))' 'Class::Observe { exit_map, .. } => (!exit_map.iter().any(|_| true))'
mutate $fk/write.rs '        _ => (exit != 0).then(' '        _ => false.then('
# A receipt that could never fit an event line is refused before the command runs: the bound
# counts the widest exit status, the outputs and a 256-byte margin, accepted at the line's bound
# and refused one byte over.
mutate $fk/write.rs '    if size > MAX_LINE as usize {' '    if false {'
mutate $fk/write.rs '    if size > MAX_LINE as usize {' '    if size >= MAX_LINE as usize {'
mutate $fk/write.rs 'const RECEIPT_ROOM: usize = 256;' 'const RECEIPT_ROOM: usize = 255;'
mutate $fk/write.rs 'const RECEIPT_ROOM: usize = 256;' 'const RECEIPT_ROOM: usize = 257;'
mutate $fk/write.rs 'VerifyRecord { argv: argv.to_vec(), exit: i32::MIN,' 'VerifyRecord { argv: argv.to_vec(), exit: 0,'
mutate $fk/write.rs '    let body = serde_json::to_vec(&receipt(held, session, outputs.clone(), verify)).map(|b| b.len());' '    let body = serde_json::to_vec(&receipt(held, session, Value::Null, verify)).map(|b| b.len());'
# An observation's freshness counts from its run's start, never later than its append.
mutate $fk/replay.rs '                        ran.map_or(at, |ran| ran.min(at))' '                        at'
mutate $fk/replay.rs '                        ran.map_or(at, |ran| ran.min(at))' '                        ran.map_or(at, |ran| ran)'
# Flow writers (BRO-3633 PR 3b2b): choose, bounded guards, until verdicts, effects, retry and
# cancel. The guard evaluator: the compile-time denylist again, the input bound, a cleared
# environment, a timeout told from an error, a strict boolean verdict on its own line, an input
# file unlinked before it is written, a refusal detail cut to its bound.
fg=crates/flow/src/kernel/guard.rs
mutate $fg '    jq::check(code).map_err(|detail| ("guard_invalid", cut(detail)))?;' ''
mutate $fg '    if input.len() > MAX_GUARD_INPUT {' '    if input.len() >= MAX_GUARD_INPUT {'
mutate $fg '    if input.len() > MAX_GUARD_INPUT {' '    if false {'
mutate $fg 'cmd.args(ENTRY_ARGS).env_clear().env(' 'cmd.args(ENTRY_ARGS).env('
mutate $fg '        Ran::Failed { code: "verify_timeout", .. } => {' '        Ran::Failed { code: "never", .. } => {'
mutate $fg '        Some("false") => Ok(false),' '        Some("false") => Ok(true),'
mutate $fg '                std::fs::remove_file(&path)?;' ''
mutate $fg '    if detail.len() > MAX_DETAIL {' '    if false {'
# A child its kernel never kills (a kernel crash) ends itself at its own deadline, reported as a
# timeout; the deadline it gets is the kernel's timeout plus the grace.
mutate $fg '    std::thread::spawn(move || {
        std::thread::sleep(own);
        std::process::exit(DEADLINE_EXIT);
    });' ''
mutate $fg '        Ran::Exited { code: Some(DEADLINE_EXIT), .. } => {' '        Ran::Exited { code: Some(-999), .. } => {'
mutate $fg 'env(GUARD_ENV, own.as_millis().to_string())' 'env(GUARD_ENV, "1")'
mutate $fg '    let _ = writeln!(out, "\n{ANSWER}{answer}");' '    let _ = writeln!(out, "{ANSWER}{answer}");'
mutate $fk/jq.rs '            Some(Ok(Val::Bool(holds))) => Ok(holds),' '            Some(Ok(Val::Bool(_))) => Ok(true),'
mutate $fk/jq.rs '            Some(Ok(_)) => Err("its first output is not true or false".to_string()),' '            Some(Ok(_)) => Ok(true),'
# What a guard reads: the receipts a node consumes (a needed repeat through its body at the
# iteration judged), an until only its settled body; EffectStarted is a kernel event.
mutate $fk/replay.rs '        out.seq = Some(r.seq);' ''
mutate $fk/replay.rs '        if !eval.settled(rid, i) {
            continue;
        }' ''
mutate $fk/replay.rs '                read.push((m, c));' ''
mutate $fk/replay.rs '                read.push((b.as_str(), eval.node(b, j)));' ''
mutate $fk/replay.rs '            EffectStarted::KIND,
            AskPinned::KIND,' '            AskPinned::KIND,'
# Effects declare a command; the compiler refuses one without.
mutate $fk/compile.rs '                Some(run) if !run.is_empty() => run,' '                Some(run) => run,
                None => Vec::new(),'
mutate $fk/compile.rs '    (ClassDef::Effect, &["run", "preconditions",' '    (ClassDef::Effect, &["preconditions",'
# Choose: the choice verb only on a choice, a declared edge, its guard over the recorded state,
# checked again under the lock after it ran (the claim and freshness), a false guard refused,
# the digest recorded, the token spent, the verdicts it settles written.
mutate $fk/write.rs '    if (class == "choice") != choosing {' '    if false {'
mutate $fk/write.rs '    check_verb(&held, false)?;' ''
mutate $fk/write.rs '    check_verb(&held, true)?;' ''
mutate $fk/write.rs '        Class::Choice { edges, .. } => edges.get(edge).cloned(),' '        Class::Choice { edges, .. } => edges.values().next().cloned(),'
mutate $fk/write.rs '    let held = hold(log, session, &instance, &sha, judged_at)?;
    if let Err(unfit) = enabled(&held) {' '    let _ = (log, judged_at);
    if let Err(unfit) = enabled(&held) {'
mutate $fk/write.rs '    if let Err(unfit) = enabled(&held) {
        return reject(&mut guard, &held, unfit);
    }
    let Judged' '    let Judged'
mutate $fk/write.rs '    if !holds {
        return reject(' '    if false {
        return reject('
mutate $fk/write.rs '        guard_digest: Some(digest.clone()),' '        guard_digest: None,'
mutate $fk/write.rs '    let verdicts = settled(ledger, session, &instance);
    Ok(Choosing::Chosen' '    let verdicts = Vec::new();
    Ok(Choosing::Chosen'
# Until verdicts: written after a receipt, and by a claim first (a crash left one unwritten),
# only by the owner, at a clock not behind the log once the until ran, only while
# the repeat awaits it at that iteration over the input it judged, with that input's digest.
mutate $fk/write.rs '    settle(ledger, session, instance)?;
    let mut guard = ledger.lock()?;' '    let mut guard = ledger.lock()?;'
mutate $fk/write.rs '    let verdicts = settled(ledger, session, &instance);
    Ok(Completion::Completed' '    let verdicts = Vec::new();
    Ok(Completion::Completed'
mutate $fk/write.rs '        check_owner(&log, &started, instance, session)?;
        let pending' '        let pending'
mutate $fk/write.rs '        wall(&log)?;
        let verdict = match judged {' '        let verdict = match judged {'
mutate $fk/write.rs '.any(|a| a.repeat == awaited.repeat && a.iteration' '.any(|a| a.iteration'
mutate $fk/write.rs '&& a.iteration == awaited.iteration && digest' '&& digest'
mutate $fk/write.rs ' && digest(&a.input) == judged.digest);' ');'
mutate $fk/write.rs '    if !still {' '    if false {'
mutate $fk/write.rs '        digest: Some(judged.digest.clone()),' '        digest: None,'
mutate $fk/write.rs 'code: "verdict_stale", reason }' 'code: "guard_error", reason }'
# Effects: preconditions hold first, checked again with the claim and freshness under the lock;
# EffectStarted written before the run; never run twice in an iteration (a completion racing
# one that started it refuses); after a run with no receipt, the reconcile, whose failure is its
# own refusal; a receipt clears the start, so new inputs run it again; an earlier IR's effect
# with no command never runs.
mutate $fk/write.rs '    if let Step::Effect { preconditions } = &plan.step {' '    if let Step::Effect { preconditions } = &Step::Command {'
mutate $fk/write.rs '        if !holds {
            return Err(("precondition_false"' '        if false {
            return Err(("precondition_false"'
mutate $fk/write.rs '    Ok(Some(digest(input)))' '    Ok(None)'
mutate $fk/write.rs '        held = hold(log, session, &instance, &sha, judged_at)?;' '        let _ = (log, judged_at);'
mutate $fk/write.rs '        if held.effect_started {
            let reason' '        if false {
            let reason'
mutate $fk/write.rs '        if let Err(unfit) = prepare(&held, &outputs) {
            return refuse(&mut guard, &held, unfit, &[]);
        }
        let guard_digest' '        let guard_digest'
mutate $fk/write.rs '            Err(unfit) => return refuse(&mut guard, &held, unfit, &[]),
        };
        let intent' '            Err(_) => None,
        };
        let intent'
mutate $fk/write.rs '        guard.append(&instance, held.log.seq(), intent.to_event()?)?;' ''
mutate $fk/write.rs '                    claims.effects.insert((started.node, started.iteration, started.inputs), started.attempt);' ''
mutate $fk/write.rs 'let effect_started = claims.effects.contains_key(&(rec.node.clone(), rec.iteration.clone(), rec.inputs.clone()));' 'let effect_started = false;'
mutate $fk/write.rs '        Some(Class::Effect { reconcile, .. }) if held.effect_started =>' '        Some(Class::Effect { reconcile, .. }) if false =>'
mutate $fk/write.rs '        Some(Class::Effect { run, preconditions, .. }) if !run.is_empty() => {' '        Some(Class::Effect { run, preconditions, .. }) => {'
mutate $fk/write.rs '        Class::Effect { .. } if plan.step == Step::Reconcile =>' '        Class::Effect { .. } if false =>'
mutate $fk/write.rs '        Class::Effect { .. } => {
            (exit != 0)' '        Class::Effect { .. } => {
            (exit == i32::MIN)'
# Retry: no claim once the node's attempts have ended without a receipt (a receipt's claim does
# not count). Cancel: a deadline recorded in the claim, the lease never past it, the token
# refused cancelled from it on (a claim a later one took over has lost its lease instead).
mutate $fk/write.rs '    let lease_until = capped(until(now, lease(n))?, cancel_at);' '    let lease_until = until(now, lease(n))?;'
mutate $fk/write.rs '    let lease_until = capped(until(now, lease(held.node()?))?, held.rec.cancel_at);' '    let lease_until = until(now, lease(held.node()?))?;'
mutate $fk/write.rs '    cancel_at.map_or(lease_until, |at| lease_until.min(at))' '    cancel_at.map_or(lease_until, |_| lease_until)'
mutate $fk/write.rs '        cancel_at: cancel_at.map(|at| at.to_string()),' '        cancel_at: None,'
mutate $fk/write.rs '                    let cancel_at = c.cancel_at.as_deref().map(|at| stamp(log, e.seq, at)).transpose()?;' '                    let cancel_at: Option<Timestamp> = None;'
mutate $fk/write.rs 'rec.cancel_at.filter(|at| latest == Some(sha) && now >= *at)' 'rec.cancel_at.filter(|_| false)'
mutate $fk/write.rs 'rec.cancel_at.filter(|at| latest == Some(sha) && now >= *at)' 'rec.cancel_at.filter(|at| latest == Some(sha) && now > *at)'
mutate $fk/write.rs 'rec.cancel_at.filter(|at| latest == Some(sha) && now >= *at)' 'rec.cancel_at.filter(|at| now >= *at)'
# P20 round 1 (maestro#28). The guard host: a process that never called guard_entry evaluates
# nothing; guard_entry registers it; the variable alone does not make a binary the evaluator; an
# evaluator that crashes or exits non-zero is an error, never a verdict; the writers' default
# timeout and the child's default grace are bounded.
mutate $fg '    if !HOST.load(Ordering::SeqCst) {' '    if false {'
mutate $fg '        HOST.store(true, Ordering::SeqCst);' ''
mutate $fg 'std::env::var_os(GUARD_ENV).filter(|_| evaluator) else {' 'std::env::var_os(GUARD_ENV) else {'
mutate $fg '        Ran::Exited { code: None, .. } => Err(("guard_error", "the evaluator crashed".to_string())),' '        Ran::Exited { code: None, .. } => Ok(true),'
mutate $fg '        Ran::Exited { code: Some(code), .. } => Err(("guard_error", format!("the evaluator exited {code}"))),' '        Ran::Exited { code: Some(_), .. } => Ok(true),'
mutate $fg 'pub const GUARD_TIMEOUT: Duration = Duration::from_secs(1);' 'pub const GUARD_TIMEOUT: Duration = Duration::from_secs(3600);'
mutate $fg 'pub const CHILD_GRACE: Duration = Duration::from_millis(500);' 'pub const CHILD_GRACE: Duration = Duration::from_secs(3600);'
# Every precondition is judged, not only the first.
mutate $fk/write.rs '    for (i, code) in preconditions.iter().enumerate() {' '    for (i, code) in preconditions.iter().take(1).enumerate() {'
# Retry, projected by replay: an attempt ends by its lease (moved by its own heartbeat), by
# changed inputs or by a later claim, unless a receipt spent it; a node out of attempts is failed
# and claim says so; an effect started over its current inputs is spared, only its reconcile runs.
mutate $fk/replay.rs '            if let Some(attempts) = self.exhausted(id, now, &status.inputs)' '            if let Some(attempts) = self.exhausted(id, now, &status.inputs).filter(|_| false)'
mutate $fk/replay.rs '.filter(|(k, c)| !c.spent && (' '.filter(|(k, c)| (true) && ('
mutate $fk/replay.rs '(k + 1 < claims.len() || now >= c.lease_until' '(false || now >= c.lease_until'
mutate $fk/replay.rs '|| now >= c.lease_until || c.inputs != *inputs))' '|| now >= c.lease_until))'
mutate $fk/replay.rs '                    last.lease_until = at;' ''
mutate $fk/replay.rs '.insert(s.attempt, s.inputs);' '.entry(s.attempt);'
mutate $fk/replay.rs '                    started.remove(&r.attempt);' ''
mutate $fk/replay.rs 'is_some_and(|s| s.values().any(|started| started == inputs)) {' 'is_some_and(|s| !s.is_empty()) {'
mutate $fk/replay.rs 'is_some_and(|s| s.values().any(|started| started == inputs)) {' 'is_some_and(|_| false) {'
mutate $fk/write.rs '        if let Some(attempts) = exhausted {' '        if let Some(attempts) = exhausted.filter(|_| false) {'
# Effects: a command that never started withdraws its start; a start stands for its inputs only;
# a cancel deadline ends a running command.
mutate $fk/write.rs '                    if r.code == "spawn_failed" {' '                    if false {'
mutate $fk/write.rs 'let effect_started = claims.effects.contains_key(&(rec.node.clone(), rec.iteration.clone(), rec.inputs.clone()));' 'let effect_started = claims.effects.keys().any(|(n, i, _)| *n == rec.node && *i == rec.iteration);'
mutate $fk/write.rs '    let limits = &cancel_capped(limits, held.rec.cancel_at);' ''
mutate $fk/write.rs 'timeout: limits.timeout.min(left).max(' 'timeout: limits.timeout.max('
# P20 round 2 (maestro#28). Only the effect's own command failing to start withdraws its
# start, and only for its own attempt and iteration (a reconcile that cannot start has its own
# code); retry exhausts in any iteration, and a heartbeat moves its own attempt; the child's
# deadline as the writers wire it.
mutate $fk/write.rs 'if plan.step == Step::Reconcile && code == "spawn_failed" { "reconcile_spawn_failed" } else { code };' 'code;'
mutate $fk/write.rs '!(*node == r.node && *iteration == r.iteration && *attempt == r.attempt)' '!(*node == r.node && *iteration == r.iteration)'
mutate $fk/write.rs '!(*node == r.node && *iteration == r.iteration && *attempt == r.attempt)' '!(*node == r.node && *attempt == r.attempt)'
mutate $fk/replay.rs '                    started.remove(&r.attempt);' '                    started.clear();'
mutate $fk/replay.rs '        let i = self.current(id)?;' '        let i = 0;'
mutate $fk/replay.rs 'if let Some(last) = self.attempts.get_mut(&(id, i)).and_then(|c| c.last_mut())' 'if let Some(last) = self.attempts.get_mut(&(id, i)).and_then(|c| c.first_mut())'
mutate $fg '    evaluate_within(code, input, timeout, own_deadline(timeout))' '    evaluate_within(code, input, timeout, Duration::from_secs(3600))'
# The child's address space limit is enforced, and so tested, on Linux only: this mutant runs
# where CI's mutation shards run (ubuntu-latest), and is skipped elsewhere, where the code it
# removes is not compiled and so no test could kill it.
[ "$(uname -s)" = Linux ] && mutate $fg '        let _ = rustix::process::setrlimit(rustix::process::Resource::As, limit);' ''
# Commands: params fill placeholders within one byte budget, only declared params, only scalars;
# the workdir is declared, absolute and a directory.
mutate $fk/write.rs '        left -= filled.len();' ''
mutate $fk/write.rs '        if out.len() > budget {' '        if out.len() >= budget {'
mutate $fk/write.rs '        if out.len() > budget {' '        if false {'
mutate $fk/write.rs '                .filter(|name| ir.params.contains_key(*name))' '                .filter(|_| true)'
mutate $fk/write.rs '                Some(_) => return Err(format!("param {name} is not a string, number or boolean")),' '                Some(_) => {}'
mutate $fk/write.rs '    if !cwd.is_absolute() || !cwd.is_dir() {' '    if !cwd.is_dir() {'
mutate $fk/write.rs '    if !cwd.is_absolute() || !cwd.is_dir() {' '    if !cwd.is_absolute() {'
# Running: argv with no shell, a cleared environment but the allowed names, a process group
# killed when the command exits, times out or floods, a byte cap kept to the byte, limits bounded.
fx=crates/flow/src/kernel/exec.rs
mutate $fx '    cmd.args(args).current_dir(cwd).env_clear();' '    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c".to_string(), argv.join(" ")]).current_dir(cwd).env_clear();'
mutate $fx '    cmd.args(args).current_dir(cwd).env_clear();' '    cmd.args(args).current_dir(cwd);'
mutate $fx '            cmd.env(name, value);' ''
mutate $fx '    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);' ''
mutate $fx 'rustix::process::kill_process_group(' 'rustix::process::kill_process('
mutate $fx '            Ok(Some(status)) => {
                kill_group(child);' '            Ok(Some(status)) => {'
mutate $fx '        if Instant::now() >= deadline {' '        if false {'
mutate $fx '        if over {
            kill_group(child);' '        if false {
            kill_group(child);'
mutate $fx '    if over {
        let detail' '    if false {
        let detail'
mutate $fx '            Ok(n) if p.kept.len().saturating_add(n) > cap =>' '            Ok(n) if p.kept.len().saturating_add(n) >= cap =>'
# The environment allowlist is exactly its five names.
mutate $fx 'pub const ENV_ALLOW: &[&str] = &["PATH", "HOME", "LANG", "LC_ALL", "TMPDIR"];' 'pub const ENV_ALLOW: &[&str] = &["PATH", "HOME", "LANG", "LC_ALL", "TMPDIR", "CARGO_PKG_NAME"];'
# Pipes are read to their end, for at most the grace once the command ended; then they are
# closed, so a process that left the group holds nothing of the run. (Blocking pipes are not a
# mutant here: the runner then blocks reading the silent stream of a flood, and a hung suite is
# no verdict.)
mutate $fx '            Ok(0) => p.fd = None,' '            Ok(0) => {}'
mutate $fx 'pub const READ_GRACE: Duration = Duration::from_secs(1);' 'pub const READ_GRACE: Duration = Duration::from_secs(3600);'
mutate $fx '        if left.is_zero() {' '        if false {'
mutate $fx '            drop(pipes);' '            std::mem::forget(pipes);'
mutate $fx '        if self.timeout < Duration::from_millis(1) || self.timeout > MAX_TIMEOUT {' '        if self.timeout > MAX_TIMEOUT {'
mutate $fx '        if self.timeout < Duration::from_millis(1) || self.timeout > MAX_TIMEOUT {' '        if self.timeout < Duration::from_millis(1) {'
mutate $fx '        if self.max_output == 0 || self.max_output > MAX_OUTPUT {' '        if self.max_output > MAX_OUTPUT {'
mutate $fx '        if self.max_output == 0 || self.max_output > MAX_OUTPUT {' '        if self.max_output == 0 {'

# Flow surfaces (BRO-3634, BRO-3624 items 4 and 9): the kernel's verbs as MCP tools and `maestro
# flow`. Writes only for a verified session (an unverified or refused caller writes as nobody);
# the flows root only in a workspace's main checkout with .broomva, made only by start; names
# bounded; no argument outside a tool's schema; the write tools declared agent writes and the
# reads reads; the binary a guard host from the first line of main; a refusal exits 1.
mf=crates/mcp/src/flow.rs
mutate $mf '        Ok(Some(me)) => me,
        Ok(None) => {' '        Ok(Some(me)) => me,
        Ok(None) => VerifiedSession::new(session.clone(), "/", None, None, vec![]).with_owner_marker("forged"),
        #[allow(unreachable_patterns)]
        Ok(None) => {'
mutate $mf '        Err(e) => {
            let mut v = refusal("caller_refused"' '        Err(_) => VerifiedSession::new(session.clone(), "/", None, None, vec![]).with_owner_marker("forged"),
        #[allow(unreachable_patterns)]
        Err(e) => {
            let mut v = refusal("caller_refused"'
mutate $mf '    if !std::fs::symlink_metadata(&broomva).is_ok_and(|m| m.is_dir()) {' '    if false {'
mutate $mf '    if std::fs::symlink_metadata(workspace.join(".git")).is_ok_and(|m| !m.is_dir()) {' '    if false {'
mutate $mf '        if !self.may_create && std::fs::symlink_metadata(&self.root).is_err() {' '        if false {'
mutate $mf '                let flow = kernel::compile(&a.definition)?;
                let params = a.params.clone().unwrap_or_else(|| json!({}));
                let begun = kernel::start(&mut w.ledger()?, &w.me, &flow, params)?;' '                let mut ledger = w.ledger()?;
                let flow = kernel::compile(&a.definition)?;
                let params = a.params.clone().unwrap_or_else(|| json!({}));
                let begun = kernel::start(&mut ledger, &w.me, &flow, params)?;'
mutate $mf '    let root = flows_root(&paths.workspace)?;
    let run = || -> Result<Value, KernelError> {
        let instance = InstanceId::new(instance)?;
        let guard' '    let root = paths.workspace.join(FLOWS);
    let run = || -> Result<Value, KernelError> {
        let instance = InstanceId::new(instance)?;
        let guard'
# The spine reads the same ledger, or refuses as the other reads do (BRO-3636).
mutate $mf '    let root = flows_root(&paths.workspace)?;
    let run = || -> Result<Value, KernelError> {
        let instance = InstanceId::new(instance)?;
        let against' '    let root = paths.workspace.join(FLOWS);
    let run = || -> Result<Value, KernelError> {
        let instance = InstanceId::new(instance)?;
        let against'
mutate $mf '    if value.len() > MAX_NAME {' '    if false {'
mutate $mf '    if value.len() > MAX_NAME {' '    if value.len() >= MAX_NAME {'
mutate $mf '#[serde(deny_unknown_fields)]
pub struct FlowClaimArgs {' 'pub struct FlowClaimArgs {'
mutate $mf '#[serde(deny_unknown_fields)]
pub struct FlowInstanceArgs {' 'pub struct FlowInstanceArgs {'
mutate $mcp '        authority: Authority::AgentWrite,
        request: None,
        scope: flow::FLOWS,
        ui: Ui::None,
        schema: schema_of::<flow::FlowStartArgs>,' '        authority: Authority::Read,
        request: None,
        scope: "read",
        ui: Ui::None,
        schema: schema_of::<flow::FlowStartArgs>,'
mutate $mcp '        authority: Authority::Read,
        request: None,
        scope: "read",
        ui: Ui::None,
        schema: schema_of::<flow::FlowInstanceArgs>,
    },
    ToolSpec {
        name: flow::CLAIM,' '        authority: Authority::AgentWrite,
        request: None,
        scope: flow::FLOWS,
        ui: Ui::None,
        schema: schema_of::<flow::FlowInstanceArgs>,
    },
    ToolSpec {
        name: flow::CLAIM,'
# A shared daemon (serve --http) takes no caller claim from its own environment.
mutate $http '    let server = MaestroServer::new(store).shared();' '    let server = MaestroServer::new(store);'
mutate $mcp '        Ambient::Shared => None,' '        Ambient::Shared => std::env::var(key).ok().filter(|s| !s.is_empty()),'
mutate $mcp '        None => match var("MAESTRO_BRIDGE_CREDENTIAL") {' '        None => match std::env::var("MAESTRO_BRIDGE_CREDENTIAL").ok().filter(|s| !s.is_empty()) {'
mutate $mcp '            Some(kind) => agent_request_in(store, hosts, ambient, kind, args),' '            Some(kind) => agent_request_in(store, hosts, Ambient::Caller, kind, args),'
# Every write schema declares the caller's claim, or a schema-enforcing client cannot send it.
mutate $mf '    pub params: Option<Value>,
    /// Your session'"'"'s folder, an absolute path: with `token` or `credential`, your claim.
    pub cwd: Option<String>,' '    pub params: Option<Value>,'
# The JSON each verb returns, field by field; a refusal before a write is not recorded.
mutate $mf '    refusal(e.code(), e, false)' '    refusal(e.code(), e, true)'
mutate $mf '                    "node_id": c.node,' '                    "node_id": c.instance.as_str(),'
mutate $mf '                    "attempt": c.attempt,' '                    "attempt": c.iteration,'
mutate $mf 'beat.lease_until.to_string()' '"2000-01-01T00:00:00Z"'
mutate $mf '                        v["stderr"] = json!(stderr);' ''
mutate $mf 'let params = a.params.clone().unwrap_or_else(|| json!({}));' 'let params = a.params.clone().unwrap_or(Value::Null);'
mutate $mf '                    limits.timeout = std::time::Duration::from_secs(secs);' '                    let _ = secs;'
# The host id: the first DNS label, lower case, whatever network suffix the name carries.
mutate $mf "        .split('.')" "        .split(',')"
mutate $mf '{ c.to_ascii_lowercase() }' '{ c }'
# An oversized definition is the tool's size refusal, even where the read's cut splits a character.
mutate $cli '    if bytes.len() > limit {' '    if false {'
mutate $cli '    maestro_flow::kernel::guard_entry();
    drop_inherited_git_repo();' '    drop_inherited_git_repo();'
mutate $cli '    if v["ok"] == json!(true) {
        return Ok(());
    }
    if v["code"] == "definition_mismatch" {' '    if true {
        return Ok(());
    }
    if v["code"] == "definition_mismatch" {'

# Human steps (BRO-3635; BRO-3624 items 1 and 7): a flow's ask, pinned, filed, linked, answered.
asks=crates/flow/src/kernel/asks.rs
replay=crates/flow/src/kernel/replay.rs
# The minted id collides with no id or uid in the ledger.
mutate $asks '.flat_map(|a| [Some(a.id), a.uid])' '.flat_map(|a| [Some(a.id), None])'
mutate $asks 'if !taken.contains(&id) {' 'if true {'
# asked_at is the kernel's, pinned before hashing; the comparable body excludes origin.
mutate $asks '    body.insert("asked_at".into(), Value::String(clock::now().strftime("%Y-%m-%dT%H:%M:%SZ").to_string()));' ''
mutate $asks 'const KERNEL_KEYS: [&str; 4] = ["id", "uid", "ask", "asked_at"];' 'const KERNEL_KEYS: [&str; 4] = ["id", "uid", "ask", "ask"];'
mutate $asks 'map.retain(|k, _| !requests::refused_key(k));' 'map.retain(|_, _| true);'
# The working tree's row with that id and hash links, after a filing and after a duplicate.
mutate $asks 'let row = match find(&here, &rec.ledger, &rec.ask_id).map_err(unconfirmed)? {' 'let row = match (if outcome.is_ok() { find(&here, &rec.ledger, &rec.ask_id).map_err(unconfirmed)? } else { Found::None }) {'
# A hash miss is ask_refused.
mutate $asks '    if found != rec.body_hash {
        return Err(Refusal::AskRefused {' '    if false {
        return Err(Refusal::AskRefused {'
# Confirmation reads the working tree, not origin/main.
mutate $asks 'let here = Store::new(paths.clone(), TreeSource::WorkingTree);' 'let here = Store::new(paths.clone(), TreeSource::GitRev("refs/remotes/origin/main".into()));'
# A busy ask ledger is retried by the kernel against its own clock, until its deadline, then refused.
mutate $asks 'if clock::now() >= deadline {' 'if true {'
mutate $asks 'if clock::now() >= deadline {' 'if false {'
# The flow finds its ask by the id it minted, never by searching the asks.
mutate $asks '.filter(|a| a.id == id || a.uid.as_deref() == Some(id))' '.filter(|a| a.id == id || a.uid.as_deref() == Some(id) || a.state() == AskState::Answered)'
# The flows lock is never held while an ask is filed.
mutate $asks 'let outcome = file_pinned(&here.paths, &dir, &rec, body, session)?;' 'let outcome = { let _held = ledger.lock()?; file_pinned(&here.paths, &dir, &rec, body, session)? };'
# Only the owner path answers (owner decision 2026-11-09, "Landed main, remote-checked"): main at
# the commit the remote names, present here; an answered row; the linked ask. A local
# maestro/answers or refs/remotes/origin/main never answers, and unread is not unanswered.
mutate $asks 'Ok(Found::Row(row)) if row.state() == AskState::Answered => return Ok(Answer::Landed' 'Ok(Found::Row(row)) => return Ok(Answer::Landed'
mutate $asks '    let sha = maestro_store::tree::remote_ref(answers_ls_remote(&remote), MAIN, REMOTE_TIMEOUT)' '    let sha = Ok::<_, String>(Store::new(paths.clone(), TreeSource::GitRev("refs/remotes/origin/main".into())).tree.commit)'
mutate $asks '    let sha = maestro_store::tree::remote_ref(answers_ls_remote(&remote), MAIN, REMOTE_TIMEOUT)
        .map_err(unread)?' '    let sha = maestro_store::tree::remote_ref(answers_ls_remote(&remote), MAIN, REMOTE_TIMEOUT)
        .or_else(|_| Ok::<_, String>(Store::new(paths.clone(), TreeSource::GitRev("refs/remotes/origin/main".into())).tree.commit))
        .map_err(unread)?'
mutate $asks '    match local.ask_ledger_verified(&rec.ledger, &sha, READ_TIMEOUT).and_then(|l| rows(l, &rec.ask_id)) {' '    match find(&Store::new(paths.clone(), TreeSource::GitRev("refs/heads/maestro/answers".into())), &rec.ledger, &rec.ask_id) {'
mutate $asks '    if !maestro_store::tree::has_object(&paths.workspace, &sha, READ_TIMEOUT).map_err(unread)? {' '    if false {'
mutate $asks 'Ok(Found::Row(row)) if row.state() == AskState::Answered => Some(label.to_string()),' 'Ok(Found::Row(_)) if false => Some(label.to_string()),'
mutate $replay '                    Class::Human { .. } => return Ok(Err("wrong_class")),' ''
mutate $replay 'if !self.asks.get(&(id, i)).is_some_and(|r| r.linked && r.ask_id == a.ask_id) {' 'if false {'
# Every other code `ask` adds, by its guard: ask_body_invalid (ledger name, size, nesting),
# ask_ledger_unread, ask_id_exhausted, ask_not_filed, ask_unconfirmed, ask_ambiguous,
# answer_mismatch, answers_unread, answer_too_large and ask_changed.
mutate $asks 'if !requests::ledger_name_ok(&filing.ledger) ||' 'if false ||'
mutate $asks 'if size > MAX_ASK_BODY {' 'if false {'
mutate $asks 'if nesting(&given) + 2 >= MAX_NESTING {' 'if false {'
mutate $asks 'return Err(unread("there is no such ledger in the asks folder".into()).into());' 'return Ok(BTreeSet::new());'
mutate $asks 'for _ in 0..MINT_TRIES {' 'for _ in 0..MINT_TRIES + 1 {'
mutate $asks '            Err(FileError::Refused(reason)) => {' '            Err(FileError::Refused(reason)) if false => {'
mutate $asks '            Err(FileError::Refused(reason)) => {' '            _ => {
                let reason = String::from("withdrawn whatever the writer said");'
mutate $asks '        _ => Found::Ambiguous,' '        (Some(a), _) => Found::Row(Box::new(a)),'
mutate $asks '    if found != rec.body_hash {
        return Err(Refusal::AnswerMismatch {' '    if false {
        return Err(Refusal::AnswerMismatch {'
mutate $asks '        Err(e) => return Err(Refusal::AnswersUnread { at: source, detail: clip(&e) }.into()),' '        Err(_) => {}'
mutate $asks '        Ok(Found::Ambiguous) => {
            let at = format!("{source}:{}", rec.ledger);' '        Ok(Found::Ambiguous) if false => {
            let at = format!("{source}:{}", rec.ledger);'
mutate $asks 'if size > MAX_OUTPUTS {' 'if false {'
mutate $asks '        Some(now) if now == *expect => {}' '        _ => {}'
# The question is relayed toward a T3 owner's thread, and only once its ask is filed.
mutate $mf '.filter(|o| o.surface == Surface::T3)' '.filter(|_| true)'
mutate $mf '(&thread, ask_id, ledger, state != "unfiled")' '(&thread, ask_id, ledger, true)'
mutate $mf 'if plain && maestro_view::handoff::fits(&prompt) {' 'if maestro_view::handoff::fits(&prompt) {'
mutate $mf 'if plain && maestro_view::handoff::fits(&prompt) {' 'if plain {'
# P20 round 2: the answers remote is compiled in (owner decision 2026-11-09, "Compiled into
# maestro") and asked with nothing local applying; reads at its main run with replace objects off;
# only exactly refs/heads/main counts, and a remote without it is unread, never its HEAD.
mutate $tree '    cmd.arg("--no-replace-objects").arg("-C").arg(root);' '    cmd.arg("-C").arg(root);'
mutate $tree '    cmd.env_clear().env("PATH", git_path());
    if let Some(home) = std::env::var_os("HOME") {' '    cmd.env("PATH", git_path());
    if let Some(home) = std::env::var_os("HOME") {'
# The answers remote is private: its one credential helper is GitHub's, and gh needs HOME.
mutate $tree '    if let Some(home) = std::env::var_os("HOME") {
        cmd.env("HOME", home);
    }' ''
mutate $asks ', "credential.https://github.com.helper=!gh auth git-credential"]' ']'
# P20 round 3: git does not check an object's hash when it reads it; the answer read checks each
# object against its id, step by step. The system git by absolute path on a fixed PATH; no GIT_*
# environment redirects a store read; the ls-remote reader stops at its bound.
mutate $tree '    check(&format, "commit", commit, &body)?;' ''
mutate $tree '        check(&format, "tree", &id, &body)?;' ''
mutate $tree '    check(&format, "blob", &id, &blob)?;' ''
mutate $tree '    if hex != id {' '    if false {'
mutate $asks '    match local.ask_ledger_verified(&rec.ledger, &sha, READ_TIMEOUT).and_then(|l| rows(l, &rec.ask_id)) {' '    match find(&Store::new(paths.clone(), TreeSource::GitRev(sha.clone())), &rec.ledger, &rec.ask_id) {'
mutate $tree '        _ => "/usr/bin/git",' '        _ => "git",'
mutate $tree '        "macos" => "/Library/Developer/CommandLineTools/usr/bin/git",' '        "macos" => "/usr/bin/git",'
mutate $tree '    let mut cmd = Command::new(git());
    cmd.env_clear().env("PATH", git_path());
    if let Some(home)' '    let mut cmd = Command::new(git());
    cmd.env_clear().env("PATH", std::env::var_os("PATH").unwrap_or_default());
    if let Some(home)'
mutate $tree '    cmd.env_clear().env("PATH", git_path()).env("xcrun_nocache", "1");
    for key in GIT_ENV_KEPT {' '    cmd.env("PATH", git_path()).env("xcrun_nocache", "1");
    for key in GIT_ENV_KEPT {'
mutate $tree '                let _ = pipe.take(limit as u64 + 1).read_to_end(&mut buf);' '                let _ = { let mut pipe = pipe; pipe.read_to_end(&mut buf) };'
# The floor (owner decision 2026-11-09, "Set the floor, fix, re-check"): git root's alone, checked
# before each answer read; xcrun's cache off; each verified read bounded in bytes and in time.
mutate $tree '        if meta.uid != 0 {' '        if false {'
mutate $tree '        if meta.mode & 0o022 != 0 {' '        if false {'
# The narrow re-check (owner decision 2026-11-10): every folder above the binary as given, the
# group-write bit, no symbolic link on the path, and a deadline on the presence check.
mutate $tree '    for at in path.ancestors() {' '    for at in path.ancestors().take(1) {'
mutate $tree '    for at in path.ancestors() {' '    for at in path.ancestors().skip(1) {'
mutate $tree '    for at in path.ancestors() {' '    for at in path.ancestors().take_while(|p| p.parent().is_some()) {'
mutate $tree '        if meta.mode & 0o022 != 0 {' '        if meta.mode & 0o002 != 0 {'
mutate $tree '        if meta.is_symlink {' '        if false {'
mutate $tree '    let (status, _, _) = run_bounded(cmd, "git cat-file -e", 64, timeout)?;' '    let (status, _, _) = run_bounded(cmd, "git cat-file -e", 64, Duration::from_secs(3600))?;'
mutate $asks '    trusted_git().map_err(|e| unread(format!("the git Maestro runs is not root'"'"'s alone: {e}")))?;' ''
mutate $tree '    cmd.env("xcrun_nocache", "1");' ''
mutate $tree '    let body = git_bytes(root, &["cat-file", "commit", commit], MAX_OBJECT, timeout)?;' '    let body = git_bytes(root, &["cat-file", "commit", commit], 4 * MAX_OBJECT, timeout)?;'
mutate $tree '        let body = git_bytes(root, &["cat-file", "tree", &id], MAX_OBJECT, timeout)?;' '        let body = git_bytes(root, &["cat-file", "tree", &id], 4 * MAX_OBJECT, timeout)?;'
mutate $tree '    let blob = git_bytes(root, &["cat-file", "blob", &id], limit, timeout)?;' '    let blob = git_bytes(root, &["cat-file", "blob", &id], MAX_OBJECT, timeout)?;'
mutate $tree '    if out.len() > limit {
        return Err(format!("{what} said more than' '    if false {
        return Err(format!("{what} said more than'
mutate $tree '    let (status, out, _) = run_bounded(cmd, &what, limit, timeout)?;' '    let (status, out, _) = run_bounded(cmd, &what, limit, Duration::from_secs(3600))?;'
mutate $tree '    if out.len() > MAX_LS_REMOTE {
        // The reader stopped' '    if false {
        // The reader stopped'
mutate $tree 'false if std::time::Instant::now() >= deadline => break false,' 'false if false => break false,'
# The CodeRabbit minors (owner decision 2026-11-10, "Fix the 2 minors, re-check, merge"): ls-remote
# runs in `/`, never in the repository the process runs from; the group is killed on both paths, so
# a helper that outlives git cannot hold the readers.
mutate $tree '    cmd.current_dir("/");' ''
mutate $tree '    let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);' ''
mutate $tree '    if out.len() > MAX_LS_REMOTE {
        return Err(format!("it said more than' '    if false {
        return Err(format!("it said more than'
mutate $tree '        if name != refname {
            continue;' '        if false {
            continue;'
mutate $tree 'if !(hex && (oid.len() == 40 || oid.len() == 64)) || found.is_some() {' 'if found.is_some() {'
mutate $tree 'if !(hex && (oid.len() == 40 || oid.len() == 64)) || found.is_some() {' 'if !(hex && (oid.len() == 40 || oid.len() == 64)) {'
mutate $asks 'pub const MAIN: &str = "refs/heads/main";' 'pub const MAIN: &str = "HEAD";'
mutate $asks '        .ok_or_else(|| unread(format!("the answers remote has no {MAIN}")))?;' '        .or_else(|| {
            let head = maestro_store::tree::ls_remote_command(&remote, "HEAD", ANSWERS_GIT_CONFIG);
            maestro_store::tree::remote_ref(head, "HEAD", REMOTE_TIMEOUT).ok().flatten()
        })
        .ok_or_else(|| unread(format!("the answers remote has no {MAIN}")))?;'
mutate $asks 'Ok(Found::Row(row)) if row.state() == AskState::Answered => Some(label.to_string()),' 'Ok(Found::Row(_)) => Some(label.to_string()),'
mutate $asks '        out.push_str("...");' ''
mutate $asks 'Refusal::AskLedgerUnread { ledger: ledger.to_string(), detail: clip(&detail) }' 'Refusal::AskLedgerUnread { ledger: ledger.to_string(), detail }'
# P20 round 1: the guards no test killed. The owner check before anything is read or written; the
# busy deadline from the first try; only a waiting node files; a pin at the sequence it was minted
# at; the replay rules for pins and links; an ask is unfiled until linked; echoed text is bounded
# and a ledger name is printable ASCII; the CLI passes --ask on.
mutate $asks '    check_owner(&log, &started, instance, session)?;
    waiting(&log, &started, node)?;' '    waiting(&log, &started, node)?;'
mutate $asks '    let deadline = clock::now().checked_add(ASK_BUSY_DEADLINE).map_err(|_| Refusal::ClockOutOfRange)?;
    loop {' '    loop {
        let deadline = clock::now().checked_add(ASK_BUSY_DEADLINE).map_err(|_| Refusal::ClockOutOfRange)?;'
mutate $asks '        Some(s) if s.state == State::Waiting => Ok((s.iteration.clone(), s.inputs.clone())),' '        Some(s) => Ok((s.iteration.clone(), s.inputs.clone())),'
mutate $asks 'let at = if adoption::only_proposals_since(&log, seen.seq) { log.seq() } else { seen.seq };' 'let at = log.seq();'
mutate $replay '                if self.asks.contains_key(&(id, i)) {' '                if false {'
mutate $replay 'Some(r) if !r.linked && r.ask_id == l.ask_id && r.body_hash == l.body_hash => {' 'Some(r) if !r.linked && r.ask_id == l.ask_id => {'
mutate $replay 'if !self.asks.get(&(id, i)).is_some_and(|r| !r.linked && r.ask_id == w.ask_id) {' 'if !self.asks.get(&(id, i)).is_some_and(|r| r.ask_id == w.ask_id) {'
mutate $replay 'Some(r) if r.linked => {' 'Some(r) if true => {'
mutate $asks 'let mut out: String = text.chars().take(MAX_ECHO).collect();' 'let mut out: String = text.chars().collect();'
mutate $asks ' || !filing.ledger.bytes().all(|b| b.is_ascii_graphic())' ''
mutate $cli 'let ask = json_arg("--ask", ask.as_deref())?;' 'let ask: Option<Value> = None;'
# Owner-approved adoption and retirement (BRO-3635; ADR decisions 6 and 7): an agent proposes, the
# owner decides, the kernel applies only the owner's approve option, read from landed main.
adopt=crates/flow/src/kernel/adoption.rs
# Only the approve option exactly; anything else is declined and changes nothing.
mutate $adopt '    if answer_option.as_deref() != Some(proposal.approve_option()) {' '    if false {'
mutate $adopt '    if answer_option.as_deref() != Some(proposal.approve_option()) {' '    if answer_option.as_deref().map(str::to_lowercase).as_deref() != Some(proposal.approve_option()) {'
# The owner adopts nothing; an approval applies only while the owner it named still owns it.
mutate $adopt '        if proposal == Proposal::Adoption && current == me {' '        if false {'
mutate $adopt '    if owners.since != rec.epoch {' '    if false {'
# P20 round 1: a proposal belongs to the ownership epoch it was pinned under, so it is stale for
# good once the instance changes owner, even back to the same owner (A, C, A); only asking again
# proposes anew, and a declined proposal says declined on every call until then.
mutate $adopt '    let stale = seen.rec.as_ref().is_some_and(|r| r.epoch != seen.owners.since);' '    let stale = seen.rec.as_ref().is_some_and(|r| seen.owners.owner.as_ref() != Some(&r.owner));'
mutate $adopt '        Some(rec) if stale => {' '        Some(rec) if false => {'
mutate $adopt '        Some(_) if stale && again => pin(ledger, paths, instance, proposal, &me, &seen, filing)?,' ''
mutate $adopt '        if r.epoch != self.since {' '        if self.owner.as_ref() != Some(&r.owner) {'
mutate $adopt 'state: match (r.epoch == self.since, r.ask.linked) {' 'state: match (true, r.ask.linked) {'
mutate $adopt 'state: match (r.epoch == self.since, r.ask.linked) {' 'state: match (self.owner.as_ref() == Some(&r.owner), r.ask.linked) {'
mutate $adopt '        if again {' '        if true {'
# A verdict is recorded only while its writer still owns the instance (the until ran unlocked).
mutate $fk/write.rs '        // The instance may have changed owner while the until ran.
        check_owner(&log, &started, instance, session)?;' ''
# Proposals are bounded per session and instance, and their bookkeeping never fails the owner's
# ask pin, while an adoption landing meanwhile does.
mutate $adopt '    if seen.owners.pins.iter().filter(|by| *by == me).count() >= MAX_PROPOSALS {' '    if false {'
mutate $adopt '                self.pins.push(p.by.clone());' ''
mutate $asks 'let at = if adoption::only_proposals_since(&log, seen.seq) { log.seq() } else { seen.seq };' 'let at = seen.seq;'
mutate $adopt '.all(|e| BOOKKEEPING.contains(&e.kind.as_str()))' '.all(|e| KINDS.contains(&e.kind.as_str()))'
# The guards no test killed: a proposal that changed (replaced) records nothing; nothing counts
# after `Retired`; a pin names the current owner; an approval matches its kind; no owner, no proposal.
mutate $adopt '        Some(now) if now == expect => {}' '        Some(_) => {}'
mutate $adopt '        if self.retired.is_some() {
            return Ok(Err("retired"));' '        if false {
            return Ok(Err("retired"));'
mutate $adopt '                if self.owner.as_ref() != Some(&p.owner) {' '                if false {'
mutate $adopt 'r.ask.ask_id == approval.ask_id && r.proposal == kind) else {' 'r.ask.ask_id == approval.ask_id) else {'
mutate $adopt '        let Some(current) = &owners.owner else {' '        let Some(current) = owners.owner.as_ref().or(Some(me)) else {'
# P20 round 2: replay takes a pin only within the current ownership epoch (a forged future epoch
# never applies when the owner returns; a past one evicts nothing), and a pin in a new epoch
# replaces the session's stale unlinked one.
mutate $adopt '                if p.epoch != self.since {
                    return Ok(Err("not_this_epoch"));' '                if false {
                    return Ok(Err("not_this_epoch"));'
mutate $adopt 'if !self.proposals[i].ask.linked && self.proposals[i].epoch == p.epoch {' 'if !self.proposals[i].ask.linked {'
# The kernel's keys (the question, the options, the default) are never the agent's.
mutate $adopt '["id", "uid", "ask", "asked_at", "headline", "options", "default", "preanswerable"]' '["id", "uid", "ask", "asked_at", "headline", "uid", "uid", "uid"]'
# The flows lock is never held while a proposal is filed.
mutate $adopt '    asks::file_and_confirm(ledger, paths, session, rec.ask.clone(), |ledger, confirmed| {' '    let _held = crate::Reader::new(ledger.root(), crate::Options::default())?.lock()?;
    asks::file_and_confirm(ledger, paths, session, rec.ask.clone(), |ledger, confirmed| {'
# An approved retirement moves the instance, as operator::retire does; so does the next call after
# a crash between `Retired` and the move.
mutate $adopt '            crate::operator::retire_held(&guard, instance)?;' ''
mutate $adopt '        return Err(Refusal::ProposalChanged { ask_id }.into());
    }
    crate::operator::retire_held(&guard, instance)?;' '        return Err(Refusal::ProposalChanged { ask_id }.into());
    }'
# Replay takes an owner only from a standing linked proposal, by its proposer, of the owner it
# named, answered with the approve option; a link names its pin's hash.
mutate $adopt '        if !r.ask.linked {
            return Err("not_linked");' '        if false {
            return Err("not_linked");'
mutate $adopt '        if r.by != *by {' '        if false {'
mutate $adopt '        if r.epoch != self.since {' '        if false {'
mutate $adopt '        if approval.answer_option != kind.approve_option() {' '        if false {'
mutate $adopt '                if self.proposals[i].owner != a.from {' '                if false {'
mutate $adopt 'Some(r) if !r.ask.linked && r.ask.body_hash == l.body_hash => r.ask.linked = true,' 'Some(r) if !r.ask.linked => r.ask.linked = true,'
# After `Adopted`: the claims taken before it are the former owner's; the current owner writes; a
# log ending in `Retired` takes no write; status names the current owner; replay reads the kinds.
mutate $adopt '                self.since = e.seq;' ''
mutate $fk/write.rs '    if rec.seq < owners.since {' '    if false {'
mutate $fk/write.rs '    let Some(owner) = &owners.owner else {' '    let Some(owner) = &started.owner else {'
mutate $fk/write.rs '    if owners.retired.is_some() {' '    if false {'
mutate $replay 'owner: self.owners.owner.as_ref().map(|o| o.session.clone()),' 'owner: started.owner.as_ref().map(|o| o.session.clone()),'
mutate $replay '        if adoption::KINDS.contains(&e.kind.as_str()) {' '        if false {'
# The tools take no answer and no ask id, and each proposes its own change; the CLI passes --ask on.
mutate $mf '#[serde(deny_unknown_fields)]
pub struct FlowProposeArgs {' 'pub struct FlowProposeArgs {'
mutate $mf 'if name == PROPOSE_ADOPTION { kernel::propose_adoption } else { kernel::propose_retirement }' 'if name == PROPOSE_ADOPTION { kernel::propose_retirement } else { kernel::propose_adoption }'
mutate $cli 'let body = json_arg("--ask", ask.as_deref())?;' 'let body: Option<Value> = None;'
# The verifier pilot (BRO-3636). Commands an agent's PATH cannot stand in for: `maestro` is the
# kernel's own executable; `sh`, `git` and `test` the system's root-owned files at their real path,
# under the name they were given; every command searches the fixed system PATH but the project's
# own command in a snapshot, which searches the caller's for its toolchain.
mutate $fx '    if name == "maestro" {' '    if false {'
mutate $fx '        "git" => vec![maestro_store::tree::git()],' '        "never" => vec![maestro_store::tree::git()],'
mutate $fx '        let checked = std::fs::canonicalize(c)' '        let checked = Ok::<_, std::io::Error>(PathBuf::from(c))'
mutate $fx '            .and_then(|real| maestro_store::tree::check_root_owned(&real).map(|()| real));' '            .and_then(|real| Ok::<_, String>(real));'
mutate $fx '            std::os::unix::process::CommandExt::arg0(&mut cmd, name);' ''
mutate $fx '            cmd.env("PATH", maestro_store::tree::git_path());' '            cmd.env("PATH", std::env::var_os("PATH").unwrap_or_default());'
mutate $fk/write.rs '    let feed = Feed { search: Search::Trusted, stdin: plan.stdin.as_deref() };' '    let feed = Feed { search: Search::Caller, stdin: plan.stdin.as_deref() };'
mutate $fk/snapshot.rs '&left(limits, deadline), Feed { search: Search::Caller, stdin: None });' '&left(limits, deadline), Feed { search: Search::Trusted, stdin: None });'
# A work node's verify reads the outputs it judges as JSON on its standard input, from a file that
# never blocks the kernel; no other command reads them, and `{{outputs.*}}` in any command, a
# snapshot or the workdir is refused at compile time (an IR pinned before keeps its run-time
# refusal).
mutate $fx '        Some(Ok(file)) => cmd.stdin(Stdio::from(file)),' '        Some(Ok(_)) => cmd.stdin(Stdio::null()),'
mutate $fk/write.rs '    let stdin = judges.then(|| serde_json::to_vec(outputs).unwrap_or_default());' '    let stdin = false.then(|| serde_json::to_vec(outputs).unwrap_or_default());'
mutate $fk/write.rs '        Some(Class::Pure { run }) | Some(Class::Observe { run, .. }) => (run, Step::Command, false),' '        Some(Class::Pure { run }) | Some(Class::Observe { run, .. }) => (run, Step::Command, true),'
mutate $fk/write.rs '        Some(Class::Work { verify, .. }) => (verify, Step::Command, true),' '        Some(Class::Work { verify, .. }) => (verify, Step::Command, false),'
mutate $fk/compile.rs '    check_no_outputs_placeholders(&ir)?;' ''
mutate $fk/compile.rs '    let mut words: Vec<(String, &String)> = ir.workdir.iter().map(|w| ("the workdir".to_string(), w)).collect();' '    let mut words: Vec<(String, &String)> = Vec::new();'
mutate $fk/compile.rs '            Class::Effect { run, reconcile, .. } => vec![run, reconcile],' '            Class::Effect { run, .. } => vec![run],'
mutate $fk/compile.rs '.chain(&n.snapshot)' '.chain(&None::<String>)'
# The project's check runs in a snapshot of one commit (a full object id), never in the working
# tree: a shared clone of the repository (its history and tags, read in place) with that commit
# checked out detached, removed whatever the command came to, a read-only tree included. The
# history is there, so a check could check out another commit; the run is then `snapshot_moved`
# (below). Only a pure node runs in one, in IR version 4.
mutate $fk/write.rs '        Some(commit) => snapshot::run_in(&plan.cwd, commit, &plan.argv, limits),' '        Some(_) => exec::run(&plan.argv, &plan.cwd, limits, Feed { search: Search::Caller, stdin: None }),'
mutate $fk/write.rs '            if !snapshot::is_object_id(&commit) {' '            if false {'
mutate $fk/snapshot.rs '    matches!(commit.len(), 40 | 64) && ' '    true && '
mutate $fk/snapshot.rs '        remove(&self.root);' ''
# The run's timeout is one deadline that setup and the command share. A check that moved the
# snapshot's HEAD (off the commit, onto a branch, away and back, its log rewritten) is refused.
mutate $fk/snapshot.rs '            match exec::supervise(cmd, &left(limits, deadline)).0 {' '            match exec::supervise(cmd, limits).0 {'
mutate $fk/snapshot.rs 'exec::run(argv, &snapshot.root, &left(limits, deadline), Feed' 'exec::run(argv, &snapshot.root, limits, Feed'
mutate $fk/snapshot.rs '    let left = deadline.saturating_duration_since(Instant::now());' '    let left = limits.timeout;'
mutate $fk/snapshot.rs '    match snapshot.moved(commit) {' '    match None::<String> {'
mutate $fk/snapshot.rs '    (bytes.len() as u64 <= MAX_LOG).then_some(bytes)' '    Some(bytes)'
# The snapshot's HEAD and its log are read only from a regular file, never through a link (a link
# to a regular file is none) and never from a FIFO (none, not an empty file, and no hang: without
# NONBLOCK the open waits for a writer, and the test fails at its 5 s bound).
mutate $fk/snapshot.rs '    let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;' '    let flags = rustix::fs::OFlags::NONBLOCK;'
mutate $fk/snapshot.rs '    let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;' '    let flags = rustix::fs::OFlags::NOFOLLOW;'
mutate $fk/snapshot.rs '    if !file.metadata().ok()?.is_file() {' '    if false {'
mutate $fk/snapshot.rs '        if head.as_deref().map(|h| h.trim_ascii()) != Some(commit.as_bytes()) {' '        if false {'
mutate $fk/snapshot.rs "            .any(|line| line.split(|b| *b == b' ').nth(1) != Some(commit.as_bytes()));" '            .any(|_| false);'
mutate $fk/snapshot.rs "            return Some(\"the check rewrote the snapshot's HEAD log\".into());" '            return None;'
mutate $fk/snapshot.rs '        let _ = std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));' '        let _ = &dir;'
mutate $fk/ir.rs '        if n.snapshot.is_some() && !matches!(n.class, Class::Pure { .. }) {' '        if false {'
mutate $fk/ir.rs '    if ir.nodes.values().any(|n| n.snapshot.is_some()) {' '    if false {'
# The receipt verifier, dogfood-receipt@1: the outputs name the receipt and its digest, within
# their bound; each file is opened without following a link and without blocking, and read only
# when regular; the receipt hashes to the named digest (the log pins it), within its bound, well
# formed, for the head and surface, says pass; its evidence, from the receipt's folder when
# relative and within its bound, hashes to the receipt's digest. Each failure its own code.
dog=crates/flow/src/dogfood.rs
mutate $dog '    if input.len() > MAX_OUTPUTS {' '    if false {'
mutate $dog '    if input.len() > MAX_OUTPUTS {' '    if input.len() >= MAX_OUTPUTS {'
mutate $dog '    if named.receipt.is_empty() || named.receipt.len() > MAX_FIELD {' '    if false {'
mutate $dog '    if !is_digest(&named.receipt_sha256) {' '    if false {'
mutate $dog '    if receipt_sha256 != named.receipt_sha256 {' '    if false {'
mutate $dog '    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC;' '    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;'
mutate $dog '    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC;' '    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC;'
mutate $dog '    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC;' '    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW;'
mutate $dog '        Err(Errno::NOENT) => return Err(Unopened::Missing),' '        Err(Errno::NOENT) => return Err(Unopened::Unreadable("gone".to_string())),'
mutate $dog '        Err(Errno::LOOP) => return Err(Unopened::NotRegular("it is a symbolic link".to_string())),' '        Err(Errno::LOOP) => return Err(Unopened::Unreadable("a loop".to_string())),'
mutate $dog '    if kind != FileType::RegularFile {' '    if false {'
mutate $dog '    if bytes.len() > MAX_RECEIPT {' '    if false {'
mutate $dog '    if bytes.len() > MAX_RECEIPT {' '    if bytes.len() >= MAX_RECEIPT {'
mutate $dog '    if receipt.schema != SCHEMA {' '    if false {'
mutate $dog '        if value.trim().is_empty() {' '        if false {'
mutate $dog '        if value.len() > MAX_FIELD {' '        if false {'
mutate $dog '        if value.len() > MAX_FIELD {' '        if value.len() >= MAX_FIELD {'
mutate $dog '    if !is_digest(&receipt.evidence.sha256) {' '    if false {'
mutate $dog 'b.is_ascii_digit() || (b'"'"'a'"'"'..=b'"'"'f'"'"').contains(&b)' 'b.is_ascii_hexdigit()'
mutate $dog '    if receipt.at.parse::<jiff::Timestamp>().is_err() {' '    if false {'
mutate $dog '#[serde(deny_unknown_fields)]
pub struct Receipt {' 'pub struct Receipt {'
mutate $dog '#[serde(deny_unknown_fields)]
pub struct Evidence {' 'pub struct Evidence {'
mutate $dog '    if receipt.head != head {' '    if false {'
mutate $dog '    if receipt.surface != surface {' '    if false {'
mutate $dog '    if receipt.result != Outcome::Pass {' '    if false {'
mutate $dog '        Some(dir) if evidence.is_relative() => dir.join(evidence),' '        Some(dir) if false => dir.join(evidence),'
mutate $dog '    if read > limit {' '    if false {'
mutate $dog '    if got != receipt.evidence.sha256 {' '    if false {'
# The CLI exits with each failure's own status, which the kernel records in its refusal.
mutate $cli '            std::process::ExitCode::from(failure.exit())' '            std::process::ExitCode::FAILURE'
# The CLI reads its input to one byte past the verifier's bound, so outputs one byte too long are
# refused for their size, never cut to fit.
mutate $cli '    let limit = maestro_flow::dogfood::MAX_OUTPUTS as u64 + 1;' '    let limit = maestro_flow::dogfood::MAX_OUTPUTS as u64;'
# verify@1 itself (V1 to V4). V1: pin compares HEAD with params.head, maps a moved one to `moved`,
# and carries fresh_for. V2: check runs only on at_head, in a snapshot of params.head. V4: exercise
# only with a declared surface (the surface node's own `test -n`), its receipt bound to the params'
# head. The kernel guards these rest on are anchored above and reused here: freshness by `fresh_for`
# (`age > secs(*f)`, three mutants), a stale observation blocking its dependents (`Form::Open if
# !stale.is_empty()`), write-time freshness in complete (`if !held.next.enabled.iter().any(..)`), a
# switch on the recorded value (`e.value.as_deref() != Some(case.value.as_str())`), V3's exit status
# (`_ => (exit != 0).then(`) and the exit_map (`!exit_map.iter().any(|m| m.exit == exit)`).
vy=crates/flow/flows/verify.yaml
mutate $vy 'test "$(git -C "$1" rev-parse HEAD)" = "$2"' 'true'
mutate $vy 'exit_map: {0: at_head, 1: moved}' 'exit_map: {0: at_head, 1: at_head}'
mutate $vy 'fresh_for: 10m' 'fresh_for: 1000000h'
mutate $vy '    switch: {on: pin, case: at_head}
' ''
mutate $vy '    snapshot: "{{params.head}}"
' ''
mutate $vy '    switch: {on: surface, case: declared}
' ''
mutate $vy '    run: [test, -n, "{{params.surface}}"]' '    run: [test, -z, "{{params.surface}}"]'
mutate $vy '"--head={{params.head}}"' '"--head={{params.repo}}"'
# The accepting spine: receipts replay accepted only; one record per (node, iteration), at its
# first place with its latest result; choices and answers on it. Two runs are compared only as
# runs of one definition (name and IR hash), a precondition reported on its own; then a record
# missing on either side, skipped told from reordered, the edge, the exit.
spn=$fk/spine.rs
mutate $spn '    for e in log.events.iter().filter(|e| !set_aside.contains(&e.seq)) {' '    for e in log.events.iter().filter(|_| true) {'
mutate $spn '            Some(&i) => records[i] = record,' '            Some(_) => records.push(record),'
mutate $spn '            Some(&i) => records[i] = record,' '            Some(_) => {}'
mutate $spn '            Chosen::KIND => {' '            "never" => {'
mutate $spn '            Answered::KIND => {' '            "nor" => {'
mutate $spn '    (instance.flow != against.flow || instance.ir_hash != against.ir_hash).then(' '    (instance.flow != against.flow).then('
mutate $spn '    (instance.flow != against.flow || instance.ir_hash != against.ir_hash).then(' '    (instance.ir_hash != against.ir_hash).then('
mutate $spn '            (Some(x), None) => return Some(skipped(x, index, Side::Against)),' '            (Some(_), None) => break,'
mutate $spn '            (None, Some(y)) => return Some(skipped(y, index, Side::Instance)),' '            (None, Some(_)) => break,'
mutate $spn '        if x.key() != y.key() {' '        if false {'
mutate $spn '            let Some(against_index) = find(b, x) else {' '            let Some(against_index) = find(b, x).or(Some(0)) else {'
mutate $spn '            if find(a, y).is_none() {' '            if false {'
mutate $spn '        if x.edge != y.edge {' '        if false {'
mutate $spn '        if x.exit != y.exit {' '        if false {'
# The tool checks the definitions first, and a difference is a refusal the CLI exits 1 on, never
# an ok; another definition exits 3.
mutate $mf '        if let Some(mismatch) = kernel::definition_mismatch(&mine, &other) {' '        if let Some(mismatch) = None::<kernel::DefinitionMismatch> {'
mutate $mf '                let mut v = refusal("spine_differs", &difference, false);' '                let mut v = json!({ "ok": true, "code": "spine_differs" });'
mutate $cli '                return std::process::ExitCode::from(3);' '                return std::process::ExitCode::from(1);'
# Specialist agents (BRO-3636): the source parses strictly, field by field; drift between a source
# and its renders is named (a render missing, differing, longer by a byte, or left by a source
# that is gone, and only a render is ever taken for one); a root without sources is unread.
agt=$fl/agents.rs
mutate $agt '#[serde(deny_unknown_fields)]
struct Source {' 'struct Source {'
mutate $agt '#[serde(deny_unknown_fields)]
pub struct Models {' 'pub struct Models {'
mutate $agt '    if text.len() > MAX_SOURCE {' '    if false {'
mutate $agt '    if s.schema != SCHEMA {' '    if false {'
mutate $agt '    if !plain(&s.name) {' '    if false {'
mutate $agt '            .all(|run| !run.is_empty() && run.bytes()' '            .all(|run| run.bytes()'
mutate $agt '    if file != format!("{}{SUFFIX}", s.name) {' '    if false {'
mutate $agt '    if s.description.trim().is_empty() || s.description.len() > MAX_DESCRIPTION {' '    if s.description.len() > MAX_DESCRIPTION {'
mutate $agt '    if s.description.trim().is_empty() || s.description.len() > MAX_DESCRIPTION {' '    if s.description.trim().is_empty() {'
mutate $agt '    if s.role.trim().is_empty() {' '    if false {'
mutate $agt '        if model.is_empty() || model.len() > MAX_MODEL || !ok {' '        if model.is_empty() || model.len() > MAX_MODEL {'
mutate $agt '        if model.is_empty() || model.len() > MAX_MODEL || !ok {' '        if model.len() > MAX_MODEL || !ok {'
mutate $agt '        if !plain(skill) {' '        if false {'
mutate $agt '        if s.skills[..i].contains(skill) {' '        if false {'
mutate $agt '    if s.outputs.get("type") != Some(&Value::from("object")) {' '    if false {'
mutate $agt '            Some(folder) => match folder.kind(file)? {
                None => DriftKind::Missing,' '            Some(folder) => match folder.kind(file)? {
                None => continue,'
mutate $agt '            Some(bytes) if bytes != r.text.as_bytes() => DriftKind::Differs,' '            Some(_) if false => DriftKind::Differs,'
mutate $agt 'match folder.read(file, r.text.len())? {' 'match folder.read(file, r.text.len() - 1)? {'
mutate $agt '            if agents.iter().any(|a| a.name == stem) {' '            if false {'
mutate $agt '.is_some_and(|line| line.starts_with(MARKER.as_bytes()))' '.is_some()'
mutate $agt '            if generated {' '            if false {'
mutate $agt '    for entry in std::fs::read_dir(&dir).map_err(|e| io(&dir, e))? {' '    for entry in std::fs::read_dir(&dir).into_iter().flatten() {'
mutate $cli '        if let Some(drift) = agents::check(root)? {' '        if let Some(drift) = None::<agents::Drift> {'
# An unknown agent: only an `agent:` performer names one; a definition naming one with no source
# does not compile, at any call; `next` gives an agent performer its launch spec, and an agent a
# pinned IR names that this build lacks is an unknown_agent spec, never none.
mutate $agt '    performer.strip_prefix(PERFORMER)' '    Some(performer)'
mutate $fk/compile.rs '            && crate::agents::find(agent).is_none()' '            && false'
mutate $fk/compile.rs '    check_agents(&ir)?;
' ''
mutate $replay '                launch_spec: class.and_then(Class::launch).and_then(agents::launch_spec),' '                launch_spec: None,'
mutate $agt '    Some(match find(name) {' '    find(name)?;
    Some(match find(name) {'
# The driver skill's order is the IR's: a node after everything it waits on, its switch's subject
# and a needed repeat's body included; a skill name past the spec's length is refused; --check
# exits 1 on a skill that is not what the IR generates.
drv=$fl/driver.rs
mutate $drv '.map(|d| at(ir, d, memo) + 1).max().unwrap_or(0);' '.map(|d| at(ir, d, memo) * 0).max().unwrap_or(0);'
mutate $drv '    on.extend(node.switch.as_ref().map(|c| c.on.as_str()));
' ''
mutate $drv 'Need::Repeat { repeat } | Need::Exhausted { repeat } => on.extend(body(repeat)),' 'Need::Repeat { .. } | Need::Exhausted { .. } => {}'
mutate $drv '    if name.len() > agents::MAX_NAME {' '    if false {'
mutate $cli '            if on_disk.as_deref() != Some(skill.as_bytes()) {' '            if false {'
# Round 1 (BRO-3636). No link is followed under the render root: a folder that is a link is
# refused, a render goes in by rename (which replaces a link), --check reads a link at a render
# path as drift, and only a regular file is taken for an orphan. A repeated key in outputs is
# refused at any level.
mutate $agt 'const WALK: OFlags = OFlags::RDONLY.union(OFlags::DIRECTORY).union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);' 'const WALK: OFlags = OFlags::RDONLY.union(OFlags::DIRECTORY).union(OFlags::CLOEXEC);'
mutate $agt 'rustix::fs::renameat(&self.fd, tmp.as_str(), &self.fd, name).map_err(|e| self.io(name, e))' 'std::fs::rename(self.path.join(&tmp), self.path.join(name)).map_err(|e| self.io(name, e))'
mutate $agt '                Some(FileType::Symlink) => DriftKind::Link,' '                Some(FileType::Symlink) if false => DriftKind::Link,'
mutate $agt 'const READ: OFlags = OFlags::RDONLY.union(OFlags::NOFOLLOW).union(OFlags::NONBLOCK).union(OFlags::CLOEXEC);' 'const READ: OFlags = OFlags::RDONLY.union(OFlags::NONBLOCK).union(OFlags::CLOEXEC);'
mutate $agt '            if out.insert(key.clone(), value).is_some() {' '            if out.insert(key.clone(), value).is_some() && false {'
mutate $agt '    #[serde(deserialize_with = "strict_json")]
' ''
# The kernel's stop decision: terminal while no node is ready, stale or waiting, and next says
# what status says; the driver skill's loop stops on it.
mutate $replay 'matches!(n.state, State::Ready | State::Stale | State::Waiting)' 'matches!(n.state, State::Ready | State::Stale)'
mutate $replay 'matches!(n.state, State::Ready | State::Stale | State::Waiting)' 'matches!(n.state, State::Ready | State::Waiting)'
mutate $replay 'matches!(n.state, State::Ready | State::Stale | State::Waiting)' 'matches!(n.state, State::Stale | State::Waiting)'
mutate $replay '        terminal: status.terminal,' '        terminal: false,'
mutate $drv 'Then, if `status.terminal` is `true`, stop' 'Then, if every node is done, stop'
# The driver skill: a node outside a repeat waits on the whole body it reaches into; Waits on
# names a switch's subject; the description stays within the spec's limit; a control character
# in a flow's name never ends a line of the frontmatter.
mutate $drv '            Some(r) if repeat_of(id) != Some(r) => out.extend(body(r)),' '            Some(r) if false => out.extend(body(r)),'
mutate $drv '        waits.push(format!("{} (its switch reads it)", code(&case.on)));
' ''
mutate $drv 'if full.chars().count() > MAX_DESCRIPTION { say(skill) } else { full }' 'full'
mutate $drv 'if c.is_control() { '"' '"' } else { c }' 'c'
# --out naming the definition itself, by any path, is refused before anything is written.
mutate $cli '        && same_file(path, definition)' '        && false'
# Round 2 (BRO-3636). A repeat awaiting its until verdict keeps an instance from being terminal.
# The driver skill checks a resumed instance's ir_hash before anything else, takes a terminal
# next as done first, asks a filed ask again (only flow ask reads the answer from main), and
# retries a pending verdict through flow claim a bounded number of times. Gemini CLI launches an
# agent through invoke_agent. The name and model bounds are held.
mutate $replay '                && !repeats.values().any(|r| r.awaiting_verdict);' '                && true;'
mutate $drv 'First, `status.ir_hash` must be `{hash}`' 'First, `status.ir_hash` names the definition'
mutate $drv 'If `next.terminal` is `true`, the flow is done' 'If nothing is enabled, the flow is done'
mutate $drv 'An `owner` or `answer_waiting_to_land` entry needs nothing more' 'Skip an `owner` or `answer_waiting_to_land` entry; it needs nothing more'
mutate $drv 'Retry it with `flow claim --instance <I> --node-id <the first node of its body' 'Wait for it, never `flow claim --instance <I> --node-id <the first node of its body'
mutate $drv 'After {VERDICT_RETRIES} such claims' 'After many such claims'
mutate $agt '                tool: GEMINI_AGENT_TOOL.into(),' '                tool: self.name.clone(),'
mutate $agt '    name.len() <= MAX_NAME' '    name.len() <= MAX_NAME + 1'
mutate $agt 'model.len() > MAX_MODEL' 'model.len() > MAX_MODEL + 1'
# Round 3 (BRO-3636). The check and the write are bound to one open folder: a render's temporary
# file is made new, never through a link (O_EXCL, O_NOFOLLOW); an orphan is removed through the
# descriptor it was found by, and only while it is still a regular file.
mutate $agt 'let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;' 'let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::CLOEXEC;'
mutate $agt 'rustix::fs::unlinkat(&self.fd, name, AtFlags::empty()).map_err(|e| self.io(name, e))?;' 'std::fs::remove_file(self.path.join(name)).map_err(|e| self.io(name, e))?;'
mutate $agt '        if self.kind(name)? != Some(FileType::RegularFile) {' '        if false {'
# Round 3 nits (BRO-3636): a claim token in the verdict retry goes back to 1, never on to a stop
# bullet; an agent flow with no params says none.
mutate $drv 'do that node as step 2 says, then go back to 1. A refusal' 'do that node as step 2 says. A refusal'
mutate $drv 'if ir.params.is_empty() { "none".to_string() }' 'if false { "none".to_string() }'
# Slice 6b-2 (BRO-3636): the Stop-hook gate. The kernel's one predicate: an enabled node, a
# pending verdict, an unfiled ask and a landed answer are work; a former owner's live claim and an
# answer not landed are someone else's; only work refuses a stop. The open instances are the
# session's own and unfinished. StopUnblockable replays as a record that changes no node, is
# written only by the owner and not again until the instance moves. The loop bound refuses one
# position three times, then lets the stop through, and starts again when the instance moves.
stp=$fk/stop.rs
gt=crates/mcp/src/stop.rs
mutate $stp '        !self.work.is_empty()' '        true'
mutate $stp '            Some(c) if c.seq < since => waiting.push(Waiting::Lease {' '            Some(c) if false => waiting.push(Waiting::Lease {'
mutate $stp '        if let (true, Some(node)) = (r.awaiting_verdict, first) {' '        if let (true, Some(node)) = (false, first) {'
mutate $stp '                Reason::Unfiled { .. } => work.push(Work::Ask { node: id.clone(), iteration }),' '                Reason::Unfiled { .. } => {}'
mutate $stp '                        AnswerSeen::Landed => work.push(Work::Answer { node, iteration, ask_id, ledger }),' '                        AnswerSeen::Landed => waiting.push(Waiting::Owner { node, iteration, ask_id, ledger, landing: None }),'
mutate $stp '            if owners.owner.as_ref() != Some(&me) || owners.retired.is_some() {' '            if owners.retired.is_some() {'
mutate $stp '            Ok(Some(assess(&log, &started, now, seen)?).filter(|a| !a.terminal))' '            Ok(Some(assess(&log, &started, now, seen)?))'
mutate $stp '        if prior.is_some_and(|p| p.session == *session.session() && p.reason == reason) {' '        if false {'
mutate $stp '    check_owner(&log, &started, instance, session)?;' ''
mutate $stp '            let refusals = if prior >= MAX_REFUSALS {' '            let refusals = if prior > MAX_REFUSALS {'
mutate $stp 'self.counts.get(&a.instance).filter(|c| c.seq == a.seq)' 'self.counts.get(&a.instance).filter(|_| true)'
mutate $replay '        if e.kind == StopUnblockable::KIND {' '        if false {'
mutate $replay '            self.unblockable.push(record);' ''
mutate $asks '        return AnswerSeen::Landed;' '        return AnswerSeen::Open;'
# The driver skill stops on the kernel's work list, which next and status carry.
mutate $drv 'If `{work}` (beside `next` in step 2' 'If `next.enabled` (beside `next` in step 2'
mutate $mf '        out[kernel::stop::WORK] = work;' ''
# The gate: the harness's own block format; a turn-end program that cannot block records and
# alerts; the fourth stop goes through recorded; it fails open on stdin that is not a Stop hook's
# (and past its bound), on an instance that does not read and on counts it cannot keep, and
# alerts; no ledger is no flow; one alert per subject; the install check reads only a maestro
# Stop hook; a command line that does not parse exits 0.
mutate $mf '    let guard = Reader::new(&w.root, Options::default())?.lock()?;' '    if std::fs::symlink_metadata(&w.root).is_err() { return Err(FlowError::NoLedger { root: w.root.clone() }.into()); }
    let guard = Reader::new(&w.root, Options::default())?.lock()?;'
mutate $gt '    if code == "busy" { "refusals_busy" } else { code }' '    code'
mutate $gt '        let guard = Reader::new(root, Options { lock_wait: kstop::REFUSALS_WAIT })?.lock()?;' '        let guard = Reader::new(root, Options::default())?.lock()?;'

mutate $cli '    args.iter().any(|a| a == "--check-install" || a.starts_with("--check-install="))' '    args.iter().any(|a| a == "--check-install")'
mutate $cli '    args.iter().any(|a| a == "--check-install" || a.starts_with("--check-install="))' '    false'
mutate $cli '        Err(_) if hook && !check_install => return std::process::ExitCode::SUCCESS,' '        Err(_) if hook => return std::process::ExitCode::SUCCESS,'
mutate $cli '        Err(_) if hook && !check_install => return std::process::ExitCode::SUCCESS,' '        Err(_) if false => return std::process::ExitCode::SUCCESS,'
mutate $cli '    args.windows(2).any(|w| w[0] == "flow" && w[1] == "stop-gate") && !check_install' '    args.windows(2).any(|w| w[0] == "flow" && w[1] == "stop-gate")'
mutate $gt '            Harness::Claude => json!({ "decision": "block", "reason": reason }).to_string(),' '            Harness::Claude => json!({ "decision": "deny", "reason": reason }).to_string(),'
mutate $gt '            Harness::Codex => json!({ "decision": "block", "reason": reason }).to_string(),' '            Harness::Codex => json!({ "decision": "block", "reason": reason, "continue": true }).to_string(),'
mutate $gt '        if self.gate.cannot_block {' '        if false {'
mutate $gt '        if tally.refused.is_empty() {' '        if false {'
mutate $gt '        Some("Stop") => {}' '        Some(_) => {}'
mutate $gt '        if let Err(e) = payload(input) {' '        if let (true, Err(e)) = (false, payload(input)) {'
mutate $gt '    if input.len() > MAX_INPUT {' '    if false {'
mutate $gt '            if unread.is_some() {' '            if false {'
mutate $gt '            if let Err(e) = counts.save(&guard, &me) {' '            if let (true, Err(e)) = (false, counts.save(&guard, &me)) {'
mutate $gt '            Err(e) if e.code() == "no_ledger" => {' '            Err(_) if true => {'
mutate $gt '                self.alert(s, &key, UNCHECKED, &words);' ''
mutate $gt '        self.alert(&session, &key, LEFT_WORK, &words);' ''
mutate $gt '            Err(e) if e.message.contains("already has an ask") => {' '            Err(e) if false => {'
mutate $gt 'let maestro = words.next().is_some_and(|w| Path::new(w).file_name().is_some_and(|n| n == "maestro"));' 'let maestro = words.next().is_some();'
mutate $gt '    maestro && verb && flag && !words.iter().any(|w| *w == "--check-install" || w.starts_with("--check-install="))' '    maestro && verb && flag && !words.contains(&"--check-install")'
mutate $gt '    maestro && verb && flag && !words.iter().any(|w| *w == "--check-install" || w.starts_with("--check-install="))' '    maestro && verb && flag'
mutate $gt '            if hook.get("type").and_then(Value::as_str) == Some("command") && runs_gate(command, harness) {' '            if runs_gate(command, harness) {'
# Review round 1 (BRO-3636): a choice node's next step is choose, never complete; a record whose
# alert was not filed is alerted by the next stop that finds it; a record that failed is alerted.
mutate $stp '    if class == "choice" {' '    if false {'
mutate $gt '                    already.push((a.instance.clone(), seq));' ''
mutate $gt '        if made.is_empty() && already.is_empty() && failed.is_empty() {' '        if made.is_empty() && already.is_empty() {'
# Round 1, stratum B (BRO-3636): two stops of one session at once are both counted (the count
# lock spans load, tally and save; one held past its wait fails open); an instance that does not
# read is noted, alerted and named whatever else the gate decides; a live claim holds a node only
# over its current inputs.
mutate $stp '        let held = live.iter().find(|c| c.node == e.node && c.iteration == e.iteration && c.inputs == e.inputs);' '        let held = live.iter().find(|c| c.node == e.node && c.iteration == e.iteration);'
mutate $gt '            self.report_unread(&session, list);' ''
mutate $gt '            reason.push_str(&format!(" Maestro could not read {list}, so the gate could not check it for work."));' ''
mutate $gt '                let code = if e.code() == "busy" { "refusals_busy" } else { "refusals_unread" };' '                let code = "refusals_unread";'
# Round 1, stratum A (Gemini, BRO-3636): every alert carries a summary; the counts live under the
# ledger's own root, bound to its epoch, written only when there is something to write (the lock
# that spans load and save is the ledger's write lock, which `Refusals::load` takes as its
# argument, so removing it does not build); a retired instance has no work and is not open; a
# record checks the clock, names at most MAX_NAMED instances and is deduplicated only against the
# same session's record; a counts file past its bound is refused; a moved or archived ledger is
# not "no ledger"; an alert's id does not depend on the order of its records; a settings file past
# its bound is not read.
lay=$fl/layout.rs
mutate $gt '        "summary": SUMMARY,' ''
mutate $lay 'pub(crate) const STOP_GATE: &str = "stop-gate";' 'pub(crate) const STOP_GATE: &str = "../flows.stop-gate";'
mutate $stp '    Ok(if parsed.epoch == epoch { parsed.counts } else { BTreeMap::new() })' '    Ok(parsed.counts)'
mutate $stp '        if self.was_empty && self.counts.is_empty() {' '        if false {'
mutate $stp '    let retired = owners.retired.is_some();' '    let retired = false;'
mutate $stp '            if owners.owner.as_ref() != Some(&me) || owners.retired.is_some() {' '            if owners.owner.as_ref() != Some(&me) {'
mutate $stp '    wall(&log)?;' ''
mutate $stp '    let Some(bytes) = read_beside(root, STOP_GATE, name, MAX_REFUSALS_FILE, "refusal-count file")? else {' '    let Some(bytes) = read_beside(root, STOP_GATE, name, u64::MAX, "refusal-count file")? else {'
mutate $stp '        open_instances: open_instances.iter().take(MAX_NAMED).cloned().collect(),' '        open_instances: open_instances.to_vec(),'
mutate $stp '        if prior.is_some_and(|p| p.session == *session.session() && p.reason == reason) {' '        if prior.is_some_and(|p| p.reason == reason) {'
mutate $gt '    sorted.sort();' ''
mutate $gt '        Ok(bytes) if bytes.len() <= MAX_INPUT => bytes,' '        Ok(bytes) => bytes,'
# CodeRabbit at d57da10 (BRO-3636): the root is synced after `stop-gate/` is made in it, so the
# new folder's entry is durable (a crash cannot be tested; `at::seams::SYNCED`, a unit-test record
# of every directory synced, observes the sync); and a stop with no readable work clears the
# counts even beside an unread instance (the mutant is the bug: the clear skipped then).
mutate $fl/ledger.rs '        root.sync();' ''
mutate $gt '            if counted {' '            if counted && unread.is_none() {'
mutate $cli '        Err(e) if gate => {' '        Err(e) if false => {'
# The live runner must reject each invalid acceptance verdict.
mutate scripts/conformance/phase1.py "    if not result.get('ok') or result.get('equal') is not True:" '    if False:'
mutate scripts/conformance/phase1.py "    if any(e.get('kind') == 'Completed' and e.get('body', {}).get('node') == 'check' for e in events):" '    if False:'
mutate scripts/conformance/phase1.py '               for _, node in found if node is not None):' '               for _, node in found if node is not None) and False:'
mutate scripts/conformance/phase1.py "    if not any(d.get('decision')=='block' and instance in d.get('reason','') for d in decisions):" "    if not any(d.get('decision')=='block' for d in decisions):"
mutate scripts/conformance/phase1.py "            if needle in (proc/'environ').read_bytes().split(b'\\0'):" '            if False:'
mutate scripts/conformance/phase1.py "    if not {'maestro_flow_claim', 'maestro_flow_complete'}.issubset(set(calls)):" '    if False:'
mutate scripts/conformance/phase1.py "    if harness in ('claude', 'codex') and not available:" '    if False:'
mutate scripts/conformance/phase1.py '    if not valid:' '    if False:'
mutate scripts/conformance/phase1.py '            env.pop(key,None)' '            pass'
mutate scripts/conformance/phase1.py "            if entry.name=='maestro' or not entry.is_file() or not os.access(entry,os.X_OK): continue" "            if not entry.is_file() or not os.access(entry,os.X_OK): continue"
mutate scripts/conformance/phase1.py "    if item.get('type')!='command_execution' or item.get('status')!='completed' or item.get('exit_code')!=0:" "    if item.get('type')!='command_execution' or item.get('status')!='completed':"
mutate scripts/conformance/phase1.py "    if not words or Path(words[0]).name not in ('cat','sed','head','rg'): return False" '    if not words: return False'
mutate scripts/conformance/phase1.py "    return isinstance(output,str) and any(len(line.strip())>=24 and line in output for line in skill.read_text().splitlines())" '    return True'
mutate scripts/conformance/phase1.py "        raise Failure('capability harness has Direct credentials or maestro on PATH')" '        pass'
mutate scripts/conformance/phase1.py "        if harness=='gemini' and item.get('type')=='tool_use':" '        if False:'
mutate scripts/conformance/phase1.py "        elif harness=='agy' and item.get('event')=='step_update':" '        elif False:'
mutate scripts/conformance/phase1.py "        return isinstance(args,dict) and (args.get('instance')==instance if verb=='maestro_flow_claim'" "        return isinstance(args,dict) and (True if verb=='maestro_flow_claim'"
echo "mutation  $killed of $total killed (shard $shard_i/$shard_n of $seen mutants)"
[ "$total" -gt 0 ] || { echo "shard $shard_i/$shard_n ran no mutant"; failed=1; }
exit $failed
