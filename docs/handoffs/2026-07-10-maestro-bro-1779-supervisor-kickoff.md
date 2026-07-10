# BRO-1779 — Supervisor spawn/tail/reap (F2/F3) — kickoff (dep-chain + design)

**Status:** branch `feature/bro-1779-supervisor-spawn-tail-reap` cut from main tip `543ab51` (BRO-1746
merged). Dep-chain (P14) complete, design pinned. This is the durable checkpoint (rot-reseed boundary
per the loop driver: this session drove BRO-1767 + BRO-1746 through 4 P20 rounds; the Loop-1
integration ticket deserves a fresh context). A fresh iteration builds `apps/runtime/src/supervisor/`
from this doc.

**Ticket:** the heart of Loop 1. F2 dispatch (lease → session + run_budget insert → sandbox worktree →
contract snapshot → proxy token → child spawn → tee wire → node running) + reap over the HARNESS §4
exit-code matrix + crash containment (runtime survives any child crash). done.check:
`bun test apps/runtime --filter supervisor` — exit-code matrix fixture (0 / 10-per-reason / 20 / crash)
lands in the right states; SIGKILL child leaves the runtime serving.

## What 1779 is (and is NOT)

- **IS** the supervisor SIDE: it spawns a child PROCESS, tees its stdout via `superviseChildStdio`
  (BRO-1767), and reaps its exit code into session/node state transitions + events.
- **IS NOT** the child's iteration loop. FLOWS §F3 (budget-guard-in-path, model→tools, write memory,
  stop-condition) runs INSIDE the child. The real Agent-SDK child (`broomva-child`, HARNESS §1 argv
  `broomva-child --role agent --session <id>`) is spawned through an INJECTABLE spawn seam; the tests
  drive that seam with FIXTURE children (tiny bun scripts that emit `run.exiting {code,reason}` then
  exit with a code). The real `broomva-child` entrypoint + `ClaudeSdkRunner.start()` (deferred at
  BRO-1756) land when the child binary does — NOT this ticket. The injectable seam is what makes the
  exit-code matrix testable without the SDK.
- **IS NOT** F8 kill (BRO-1801) nor F9 crash-recovery/startup-replay (BRO-1814) — but 1779 MUST leave
  the seams those need (revoke token on reap, `run.failed` on crash, worktree preserved).

## Dep-chain (P14) — concrete upstream (all shipped)

**Sandbox (BRO-1746) — `apps/runtime/src/sandbox/{sandbox,worktree}.ts`:**
- `createWorktreeSandboxFactory({ workspace }): SandboxFactory`
- `factory.create(runId, { resources? }): Promise<Sandbox>` — worktree + runDir scaffold; idempotent
  (fresh-context respawn reuses the LIVE worktree). `Sandbox` exposes `workdir`, `runDir`, `branch`,
  `spawnContext(): { cwd, commandPrefix, env }` (ENTER — spawn the child with
  `[...commandPrefix, ...argv]`, cwd `spawnContext().cwd`), `exec()`, `teardown({ preserve })`
  (default preserve=true; false frees the worktree, keeps branch+runDir).

**Stdio tee (BRO-1767) — `apps/runtime/src/harness/stdio.ts`:**
- `superviseChildStdio(child: ChildStdioPort, deps): SupervisedChild` — `child` = narrow port
  `{ stdout, stderr, writeStdin, kill(signal), exited: Promise<number> }`. `fromBunSubprocess(proc)`
  adapts a real `Bun.Subprocess<"pipe","pipe","pipe">`. deps = `{ db, sessionId, runDir, now, config? }`.
  Returns `{ control (chat/stop/ping), liveness, tee, done, stop() }`. `done` resolves when stdout
  closes (after tee drain). The tee writes `session.jsonl` (FS-first) + the `event` table; SSE
  fan-out is automatic. On a tee write failure the child is REAPED (SIGKILL + `run.failed` + park
  blocked) — so the supervisor's own reap must be idempotent w.r.t. that.

**Spawn contract (BRO-1756) — `apps/runtime/src/harness/spawn-contract.ts`:**
- `serializeChildArgv({ role, session }): string[]` → `["--role", role, "--session", id]`.
- `buildChildEnv(hostEnv, spec: ChildEnvSpec): Record<string,string>` — deny-by-default allowlist +
  the BROOMVA_* contract vars. `ChildEnvSpec = { session, runDir, contractPath, modelProxyUrl,
  modelToken }`. NO host secrets ever reach the child.
- `contract-snapshot.ts`: `writeContractSnapshot(runDir, { session, node: WorkContract, dispatchedAt }):
  Promise<string>` → writes `runs/run-<id>/contract.json`, returns the path (= BROOMVA_CONTRACT).

**Proxy (BRO-1788) — `apps/runtime/src/proxy/{tokens,proxy}.ts`:**
- `SessionTokenRegistry.mint(ctx: SessionContext): string` (idempotent per session — re-mint revokes
  prior), `.revoke(session)`, `.resolve(token)`. `SessionContext = { session, runDir, role, budget }`.
- `createModelProxy(deps): Hono` + `serveProxy(app, { unixSocket? | port?, hostname? }): ProxyServer`
  ({ url, socketPath?, stop() }). The proxy is ONE listener for the whole runtime (mint per-run
  tokens against the shared registry) — NOT one-per-child. `modelProxyUrl` for the child env comes
  from `ProxyServer.url` (+ socketPath in unix mode).

**Index (BRO-1796) — `apps/runtime/src/db/schema.ts`:** tables `lease` (key, holder, acquiredAt,
expiresAt), `session` (id, nodeId, branch, status, startedAt, endedAt, diffstatJson, SyncFields),
`runBudget` (sessionId PK, spentUsd, iterations, lastCallAt), `node` (state ∈ OrchState). Events →
`event` table via the tee. `SessionStatus = running|blocked|review|done|canceled`. `IndexDb` from
`db/client.ts`. NOTE the schema gotchas (memory): libsql `:memory:` + `db.transaction()` is BROKEN
(use single-statement conditional UPDATE / `db.batch`); timestamps `integer{mode:number}` (Date.now()).

**Protocol events (`packages/protocol/src/events.ts`):** `EVENT_TYPES` has RUN_STARTED, RUN_BEAT,
RUN_EXITING, RUN_FINISHED, RUN_FAILED, RUN_KILLED, RUN_ORPHANED, RUN_HUNG, GATE_OPENED, GATE_DECIDED.
**MISSING and must be ADDED (namespace-legal, small protocol edit like BRO-1767's RUN_HUNG):**
`RUN_EXIT_MISMATCH: "run.exit_mismatch"` (HARNESS §4 — run.exiting code ≠ real exit code). Check
whether a `run.restart_requested` type is needed (HARNESS §5) or if fresh_context rides run.exiting
reason. The exit-10 `reason` enum is pinned (D-EVENTNAMES):
`budget | iteration_cap | no_progress | user_stop | fresh_context`.

## Downstream (consumers — do not break)

- BRO-1801 kill-switch (F8): needs the run registry (pid/session → kill handle) + token revoke on
  kill. 1779 owns the run registry; expose a `kill(session)` seam or the registry.
- BRO-1795 guardrails (iteration cap / no-progress / fresh-context): the child emits exit 10 + reason;
  1779 must route each reason correctly (esp. fresh_context → respawn).
- BRO-1822 chat endpoint (F10): routes a UIMessage into the live child via `control.chat()` — 1779
  must keep the `SupervisedChild.control` reachable per live session (the run registry holds it).
- BRO-1814 crash-recovery (F9): 1779's `run.failed` + preserved worktree + durable session.jsonl are
  what F9 replays. 1779 must NOT delete receipts on crash.

## Design (`apps/runtime/src/supervisor/`)

### F2 — `dispatch(nodeId)` (the happy path, in order)

1. **Lease** the node id (`lease` table, holder = runtime id, expiresAt = now + TTL). Held already →
   someone else is dispatching → drop silently (idempotency; NOT an error). Free-before-claim /
   conditional-insert (schema gotcha: no `db.transaction()` on :memory: — use a conditional INSERT or
   `db.batch`).
2. Read the node's resolved `WorkContract` (frontmatter + inherited defaults — via the scanner's
   `resolveWorkContract`, already in the index as `node` row; budget from `node.budgetJson`).
3. Mint a run id (short — `randomBytes`→hex or a counter; must satisfy sandbox `SAFE_RUN_ID`: no `..`,
   no `.lock`, `[A-Za-z0-9._-]` alnum-bounded).
4. Insert `session` (status running, branch `run/<id>`, startedAt) + `runBudget` (zeroed) rows.
5. `sandbox = await factory.create(runId, { resources })` → worktree + runDir.
6. `writeContractSnapshot(sandbox.runDir, { session: runId, node: contract, dispatchedAt: iso })`.
7. `token = tokens.mint({ session: runId, runDir: sandbox.runDir, role: "agent", budget })`.
8. `env = buildChildEnv(process.env, { session: runId, runDir: sandbox.runDir, contractPath,
   modelProxyUrl: proxy.url, modelToken: token })`.
9. **Spawn the child** via the injectable seam: `spawnChild({ argv: serializeChildArgv({role:"agent",
   session:runId}), env, cwd: sandbox.spawnContext().cwd, commandPrefix: sandbox.spawnContext()
   .commandPrefix })`. Default impl = `Bun.spawn([...commandPrefix, "broomva-child", ...argv], { cwd,
   env, stdout:"pipe", stderr:"pipe", stdin:"pipe" })` → `fromBunSubprocess`. Tests inject a
   fixture-child spawner. **A spawn THROW (ENOENT: broomva-child missing, etc.) = a spawn crash →
   step "crash" below (node blocked + run.failed, worktree preserved).**
10. `supervised = superviseChildStdio(child, { db, sessionId: runId, runDir, now, config })`.
11. Register the run: `registry.set(runId, { sandbox, child, supervised, token })` (for kill/chat/reap).
12. `node.state → running` (+ emit `run.started` — or does the child emit it? HARNESS §6 has the child
    emit run.started; the supervisor emits run.finished after reap. CHECK: avoid double run.started).
13. Start the reap (below) — do NOT await it in dispatch (dispatch returns once the child is live).

### Reap — `reap(runId)` (await child.exited, map the exit code)

`const code = await supervised.done.then(() => child.exited)` — wait for stdout drain THEN the exit
code (so the last `run.exiting` line is tee'd before we act). Cross-check the tee'd `run.exiting`
event's `code` against the real exit code; mismatch → emit `run.exit_mismatch` (Loop-4 signal).

Map per HARNESS §4:
- **0** (claims complete) → spawn verifier (F4). STUB until P3: park session `review`, node `review`.
- **10** (stopped) → read the `reason` from the child's `run.exiting`/`budget.exhausted`. Park per
  reason: `fresh_context` → **immediate respawn** (same runId, same worktree [sandbox.create is
  idempotent], same run_budget row — do NOT re-insert; re-mint token); all others (`budget`,
  `iteration_cap`, `no_progress`, `user_stop`) → session/node `blocked`.
- **20** (needs input) → open a `gate` (kind `question`), session/node → `review`.
- **other / signal (crash)** → session `blocked`, node `blocked`, emit `run.failed` with the reason;
  **worktree PRESERVED** (`teardown` NOT called, or `teardown({preserve:true})`). The runtime MUST
  survive — a child crash is caught, never propagates.
- Always on reap (except fresh_context respawn): `tokens.revoke(runId)`, `registry.delete(runId)`,
  `supervised.stop()`. Derive `run.finished` after reap (D-EVENTNAMES). Set `session.endedAt`.

### Crash containment (the SIGKILL test)

A child killed with SIGKILL (or that segfaults) → `child.exited` resolves with the signal code →
reap maps to crash → session blocked + run.failed, worktree preserved, **the Hono runtime keeps
serving** (`/health` still 200, other sessions unaffected). This is the done.check's
"SIGKILL child leaves runtime serving." Test: spawn a fixture child that `process.exit`s on SIGKILL
(or a hang + supervisor SIGKILL), assert the runtime's other endpoints still respond.

## Test plan (P11) — `apps/runtime/src/supervisor/supervisor.test.ts`

Inject a **fixture-child spawner**: given argv/env/cwd, returns a `ChildStdioPort` backed by a tiny
bun script (or an in-test fake) that emits a scripted `run.exiting {code, reason}` on stdout then
exits with `code`. The exit-code matrix fixture:
- exit 0 → session/node `review` (verifier stub).
- exit 10 reason `budget|iteration_cap|no_progress|user_stop` → `blocked` (one case each).
- exit 10 reason `fresh_context` → respawn: same runId, same worktree, same run_budget row (assert NOT
  re-zeroed), a new token minted.
- exit 20 → `gate` row (kind question) + `review`.
- crash (non-zero signal / spawn throw) → `blocked` + `run.failed` + worktree still on disk.
- `run.exiting` code ≠ real exit → `run.exit_mismatch` emitted.
- SIGKILL containment: runtime keeps serving.
- lease already held → dispatch is a silent no-op.
Real-git sandbox (no mocks for the worktree); fixture only for the child process. Anti-vacuity: each
state-transition assertion must FAIL if the mapping is wrong (assert the exact status + the exact
event, not just "something happened"). Dogfood: one real `Bun.spawn` of a fixture bun script through
`fromBunSubprocess` to confirm the real exited/kill/stdout wiring (the BRO-1767 lesson: Bun stream
semantics differ from docs — and `process.stdout.write` block-buffers a pipe, so a fixture child must
`Bun.write(Bun.stdout, …)` to flush before exit).

## P20 risk surface (scale-to-risk: integration + concurrency + state machine → 4 heavy lenses)

- **Exit-code state machine:** every code → exactly one correct (session, node, event) triple; no
  double run.started/run.finished; fresh_context respawn must NOT re-zero the budget or re-insert the
  session (budgets span attempts, not processes).
- **Crash containment / resource leak:** a spawn throw or child crash must revoke the token, drop the
  registry entry, stop the liveness tick, and NOT leak a worktree or a live proxy token; the runtime
  survives. A partial dispatch failure (session inserted, spawn throws) must not orphan a running-state
  node with no process.
- **Idempotency / races:** lease double-dispatch; reap racing a kill (F8 seam); fresh_context respawn
  racing a reap; tee's own write-failure reap (BRO-1767) racing the supervisor reap.
- **Anti-vacuity** [[self-hosting-vacuous-pass]]: the fixture child must genuinely exercise each exit
  path; assert exact states + events; the SIGKILL test must prove the runtime STILL SERVES (hit a real
  endpoint), not just that the child died.

## Loop bookkeeping
- BRO-1746 DONE + merged (`543ab51`); P2 loop-1: 1756✓ 1788✓ 1767✓ 1746✓.
- After 1779: guardrails BRO-1795 (stop-conditions) + BRO-1801 (kill switch) + chat BRO-1822, then
  **P2 exit BRO-1827** (dispatch from the app, watch events, kill mid-run, restart — nothing lost).
- Protocol edit needed first: add `RUN_EXIT_MISMATCH` (+ maybe `run.restart_requested`) to events.ts
  + a one-line events.test.ts assertion (the BRO-1767 RUN_HUNG pattern).
