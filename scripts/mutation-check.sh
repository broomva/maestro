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
# Flow writes: only a verified agent session; neither unverified callers nor the owner principal.
mutate $auth '(Principal::Agent { .. }, Authority::AgentWrite) => true,' '(_, Authority::AgentWrite) => true,'
mutate $auth '(Principal::Agent { .. }, Authority::AgentWrite) => true,' '(Principal::Agent { .. } | Principal::Owner { .. }, Authority::AgentWrite) => true,'
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
mutate $mcp '    let caller = Principal::Unverified;' '    let caller = Principal::Owner { via: "mcp".into() };'
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
mutate $fl/ledger.rs '        log::fits(line.len() as u64, existing_len, log.seq())?;' ''
mutate $fl/at.rs '        if size > limit {' '        if size >= limit {'
mutate $fl/at.rs '        if size > limit {' '        if false {'
mutate $fl/log.rs '        if size > MAX_LINE {' '        if false {'
mutate $fl/log.rs '        if prefix.seq >= MAX_EVENTS {' '        if prefix.seq > MAX_EVENTS {'
mutate $fl/log.rs '        if prefix.seq >= MAX_EVENTS {' '        if false {'
mutate $fl/log.rs '    if events >= MAX_EVENTS {' '    if events > MAX_EVENTS {'
mutate $fl/log.rs '    if events >= MAX_EVENTS {' '    if false {'

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
mutate $fk/ir.rs '    if ir.ir_version != IR_VERSION {' '    if false {'
mutate $fk/ir.rs '    if ir.nodes.len() > MAX_NODES {' '    if false {'
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
# needed from outside its repeat) and chains deeper than MAX_DEPTH.
mutate $fk/ir.rs '    topology(ir)
}' '    Ok(())
}'
mutate $fk/ir.rs '    if finished == deps.len() {' '    if true {'
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
# start: params that do not fit are refused before anything is written; null is absent.
mutate $fk/replay.rs '.find(|(name, required)| **required && !supplied(params, name))' '.find(|_| false)'
mutate $fk/replay.rs '    if let Some(name) = given.keys().find(|k| !ir.params.contains_key(*k)) {' '    if let Some(name) = given.keys().find(|_| false) {'
mutate $fk/replay.rs '    params.get(name).is_some_and(|v| !v.is_null())' '    params.get(name).is_some()'
# The pin: a Started first, a pinned IR of this version that validates (without it a cyclic pin
# recurses until the stack overflows), matches its hash and is the flow it names.
mutate $fk/replay.rs '    if first.kind != Started::KIND {' '    if false {'
mutate $fk/replay.rs '    ir::validate(&started.ir)
        .map_err(|e| malformed(log, first.seq, format!("the pinned IR does not validate ({e})")))?;' '' 'stack overflow'
mutate $fk/replay.rs '    if ir::content_hash(&started.ir)? != started.ir_hash {' '    if false {'
mutate $fk/replay.rs '    if started.flow != format!("{}@{}", started.ir.flow, started.ir.version) {' '    if false {'
# Acceptance, in log order: kernel kinds only, a declared node, its current iteration, the right
# class, a declared edge, an enabled node not already done, verdicts for a settled current
# iteration of a repeat with an until.
mutate $fk/replay.rs '        if ![Completed::KIND, Chosen::KIND, Claimed::KIND, Heartbeat::KIND, Refused::KIND].contains(&e.kind.as_str()) {' '        if false {'
mutate $fk/replay.rs 'let Some(node) = self.ir.nodes.get(&subject.node) else {' 'let Some(node) = self.ir.nodes.get(&subject.node).or_else(|| self.ir.nodes.values().next()) else {'
mutate $fk/replay.rs '        if self.current(&subject.node).as_ref() != Some(&subject.iteration) {' '        if false {'
mutate $fk/replay.rs '                Phase::Exhausted(_) => None,' '                Phase::Exhausted(i) => Some(Iteration::from([(rid.to_string(), *i)])),'
mutate $fk/replay.rs '                    Class::Choice { .. } => return Ok(Err("wrong_class")),' '                    Class::Choice { .. } => (None, None),'
mutate $fk/replay.rs 'let Class::Choice { edges, .. } = &node.class else { return Ok(Err("wrong_class")) };' 'let Class::Choice { edges, .. } = &node.class else { return Ok(Ok(())) };'
mutate $fk/replay.rs '                if !edges.contains_key(&c.edge) {' '                if false {'
mutate $fk/replay.rs 'match exit.and_then(|x| exit_map.iter().find(|m| m.exit == x)) {' 'match exit.or(Some(0)).and_then(|x| exit_map.iter().find(|m| m.exit == x)) {'
mutate $fk/replay.rs 'exit_map.iter().find(|m| m.exit == x)' 'exit_map.iter().find(|_| true)'
mutate $fk/replay.rs '        if !now.enabled {' '        if false {'
mutate $fk/replay.rs '        if now.state == State::Done && !matches!(node.class, Class::Observe { .. }) {' '        if false {'
mutate $fk/replay.rs '        if now.state == State::Done && !matches!(node.class, Class::Observe { .. }) {' '        if now.state == State::Done {'
mutate $fk/replay.rs '        if r.until.is_none() {' '        if false {'
mutate $fk/replay.rs '        if self.phases.get(g.repeat.as_str()) != Some(&Phase::At(g.iteration)) {' '        if false {'
mutate $fk/replay.rs '        if !eval.settled(&g.repeat, g.iteration) {' '        if false {'
# Repeats: a true verdict holds only for the body it judged; a false one moves the repeat on; it
# exhausts at max; a body that never runs is unreachable; a node not taken settles its iteration.
mutate $fk/replay.rs '        if g.holds {' '        if true {'
mutate $fk/replay.rs '                    && *judged == self.body_digest(rid, i)' '                    && true'
mutate $fk/replay.rs '                (b.as_str(), e.digest.filter(|_| e.satisfies))' '                (b.as_str(), e.digest)'
mutate $fk/replay.rs '        if i.saturating_add(1) >= max { Phase::Exhausted(i) } else { Phase::At(i + 1) }' '        Phase::At(i + 1)'
mutate $fk/replay.rs '        evals.iter().all(|e| e.dead || e.satisfies) && !evals.iter().all(|e| e.dead)' '        evals.iter().all(|e| e.satisfies) && !evals.iter().all(|e| e.dead)'
mutate $fk/replay.rs '        evals.iter().all(|e| e.dead || e.satisfies) && !evals.iter().all(|e| e.dead)' '        evals.iter().all(|e| e.dead || e.satisfies)'
mutate $fk/replay.rs 'if self.body(rid).iter().all(|b| self.node(b, &at).dead) {' 'if false {'
mutate $fk/replay.rs '                let awaiting_verdict = state == RepeatState::Running && eval.settled(rid, iteration);' '                let awaiting_verdict = state == RepeatState::Running;'
mutate $fk/replay.rs '                    NodeEval::new(State::Exhausted, vec![Reason::RepeatExhausted { repeat: rid.to_string() }]),' '                    self.node(id, &Iteration::from([(rid.to_string(), i)])),'
mutate $fk/replay.rs '            Loop::Exhausted(_) => Dep::Dead(Reason::RepeatExhausted { repeat: rid.to_string() }),' '            Loop::Exhausted(_) => Dep::Waits(Reason::RepeatRunning { repeat: rid.to_string() }),'
mutate $fk/replay.rs '            Loop::Done(i) => Dep::Eval(Box::new(self.node(m, &Iteration::from([(rid.to_string(), i)])))),' '            Loop::Done(_) => Dep::Waits(Reason::RepeatRunning { repeat: rid.to_string() }),'
mutate $fk/compile.rs '            node.needs.push(Need::Exhausted { repeat: rid.clone() });' ''
mutate $fk/replay.rs '                    Loop::Exhausted(_) => kill(&mut out, Reason::RepeatExhausted { repeat: r.clone() }),' '                    Loop::Exhausted(_) => push(&mut out, Reason::RepeatExhausted { repeat: r.clone() }),'
mutate $fk/replay.rs '                    Loop::Done(_) => kill(&mut out, Reason::RepeatFinished { repeat: r.clone() }),' '                    Loop::Done(_) => push(&mut out, Reason::RepeatFinished { repeat: r.clone() }),'
# Needs, the fork join, switches, dead branches; receipts done only while what they consumed,
# transitively, is current and fresh.
mutate $fk/compile.rs '            branches.iter().map(|b| Need::Node { node: b.clone() }).collect()' '            Vec::new()'
mutate $fk/replay.rs '                } else if !e.satisfies {' '                } else if false {'
mutate $fk/replay.rs '                    kill(out, Reason::Unreachable { node: m.to_string() });' '                    push(out, Reason::Unreachable { node: m.to_string() });'
mutate $fk/replay.rs '                out.stale.extend(e.stale.iter().cloned());' ''
mutate $fk/replay.rs '                            out.stale.extend(e.stale);' ''
mutate $fk/replay.rs '                        out.consumed.insert(r.clone(), judged);' ''
mutate $fk/replay.rs '            && e.value.as_deref() != Some(case.value.as_str())' '            && false'
mutate $fk/replay.rs '        if let Some(failure) = &r.failure {' '        if let Some(failure) = r.failure.as_ref().filter(|_| false) {'
mutate $fk/replay.rs '            if r.inputs.get(input) != Some(d) {' '            if false {'
mutate $fk/replay.rs '        out.satisfies = out.reasons.is_empty();' '        out.satisfies = true;'
mutate $fk/replay.rs '            if age > secs(*f) || age.is_negative() {' '            if age >= secs(*f) || age.is_negative() {'
mutate $fk/replay.rs '            if age > secs(*f) || age.is_negative() {' '            if age > secs(*f) {'
mutate $fk/replay.rs '            if age > secs(*f) || age.is_negative() {' '            if age.is_negative() {'
mutate $fk/replay.rs '                out.stale.insert(id.to_string());' ''
mutate $fk/replay.rs '        out.reasons.extend(out.stale.iter().filter(|s| *s != id).map(|s| Reason::Stale { node: s.clone() }));' ''
mutate $fk/replay.rs '        let digest = receipt_digest(outputs, value.as_deref());' '        let digest = receipt_digest(outputs, None);'
mutate $fk/replay.rs '        let rerun = s.state == State::Stale && eval.node(&id, &s.iteration).stale.iter().all(|s| *s == id);' '        let rerun = s.state == State::Stale;'
mutate $fk/replay.rs '        let rerun = s.state == State::Stale && eval.node(&id, &s.iteration).stale.iter().all(|s| *s == id);' '        let rerun = false;'
mutate $fk/replay.rs '        if let Class::Human { ask } = &n.class {' '        if let Some(ask) = None::<&String> {'
# Read-path bounds: inert and rejected events cost no evaluation; a node reads at most
# MAX_FAN_IN nodes; evaluations and body digests that do not read the clock are kept across
# events and dropped downstream of each change.
mutate $fk/replay.rs '                Ok(Change::Inert) => {}' '                Ok(Change::Inert) => fold.advance(at),'
mutate $fk/ir.rs '        if reads > MAX_FAN_IN {' '        if reads >= MAX_FAN_IN {'
mutate $fk/ir.rs '        if reads > MAX_FAN_IN {' '        if false {'
mutate $fk/ir.rs '                Some(r) if member.get(id.as_str()) != Some(r) => 1 + body(r),' '                Some(r) if member.get(id.as_str()) != Some(r) => 1,'
mutate $fk/ir.rs '                _ if ir.repeats.contains_key(m) => body(m),' '                _ if ir.repeats.contains_key(m) => 1,'
mutate $fk/replay.rs '        self.invalidate(&key.0);' ''
mutate $fk/replay.rs '            out.clocked = true;' ''
mutate $fk/replay.rs '                out.stale.extend(e.stale.iter().cloned());
                out.clocked |= e.clocked;' '                out.stale.extend(e.stale.iter().cloned());'
mutate $fk/replay.rs '                            out.stale.extend(e.stale);
                            out.clocked |= e.clocked;' '                            out.stale.extend(e.stale);'
mutate $fk/replay.rs '                    digests.retain(|(rid, _), _| rid != r);' ''
mutate $fk/replay.rs '                    reads.extend(body(repeat).iter().map(String::as_str))' '                    let _ = repeat;'
mutate $fk/replay.rs '                reads.extend(body(r).iter().map(String::as_str));' ''
mutate $fk/replay.rs '        self.cache.borrow_mut().clear();
        self.digests.borrow_mut().clear();' ''
# A body's stale observations anywhere in it; a second verdict on a body already judged; event
# timestamps; freshness to the second; acceptance at the event's own clock.
mutate $fk/replay.rs '                        for b in self.body(r) {' '                        for b in self.body(r).iter().take(1) {'
mutate $fk/replay.rs '        if self.verdicts.get(&(g.repeat.clone(), g.iteration)) == Some(&judged) {' '        if false {'
mutate $fk/replay.rs 'e.at.parse().map_err(|_| malformed(log, e.seq, format!("{} is not a timestamp", e.at)))?;' 'e.at.parse().unwrap_or(Timestamp::UNIX_EPOCH);'
mutate $fk/replay.rs '    SignedDuration::from_secs(i64::try_from(s).unwrap_or(i64::MAX))' '    SignedDuration::from_mins(i64::try_from(s / 60).unwrap_or(i64::MAX))'
mutate $fk/replay.rs '        let now = Eval::new(self, at).node(&key.0, &key.1);' '        let now = Eval::new(self, Timestamp::MAX).node(&key.0, &key.1);'
# Positive controls: a replay that ran a command, judged a repeat from its guard, or was not a
# function of the log and the clock fails the tests that say it never does.
mutate $fk/replay.rs '        let fold = self.fold;
        let Some(n) = fold.ir.nodes.get(id) else {' '        let fold = self.fold;
        if let Some(Class::Pure { run }) = fold.ir.nodes.get(id).map(|n| &n.class) {
            let _ = std::process::Command::new(&run[0]).args(&run[1..]).status();
        }
        let Some(n) = fold.ir.nodes.get(id) else {'
mutate $fk/replay.rs '                if let Some(judged) = self.fold.verdicts.get(&(rid.to_string(), i))
                    && *judged == self.body_digest(rid, i)
                {' '                if self.fold.ir.repeats.get(rid).and_then(|r| r.until.as_deref()).is_some_and(|g| super::jq::check(g).is_ok()) {'
mutate $fk/replay.rs '            seq: log.seq(),
            nodes,' '            seq: jiff::Timestamp::now().as_nanosecond() as u64,
            nodes,'
echo "mutation  $killed of $total killed (shard $shard_i/$shard_n of $seen mutants)"
[ "$total" -gt 0 ] || { echo "shard $shard_i/$shard_n ran no mutant"; failed=1; }
exit $failed
