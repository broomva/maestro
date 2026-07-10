# BRO-1767 — NDJSON stdio plumbing — kickoff (dep-chain + design)

**Status:** branch `feature/bro-1767-ndjson-stdio-plumbing` cut from main tip `009a022` (BRO-1788
merged). Dep-chain (P14) complete, design pinned. No code written yet — this is the durable checkpoint
so a fresh context builds the concurrency-sensitive tee at full quality (rot-reseed boundary per the
loop driver: BRO-1788's 8-round saga consumed the prior context; this ticket deserves a fresh one).

**Ticket:** supervisor tee (HARNESS §2 / FLOWS §F2 step 6). Child NDJSON stdout → `session.jsonl`
append FIRST (FS = truth) → index `event` table → SSE fan-out; raw stderr → `child.stderr.log`. Plus
stdin control (chat/stop/ping) and a liveness monitor. done.check: `bun test apps/runtime --filter stdio`
— append-first ordering asserted; hung-child fixture escalates SIGTERM→SIGKILL and parks blocked.

## HARNESS §2 (verbatim intent)

- **stdout = events, stdin = control.** stderr captured raw to `runs/run-<id>/child.stderr.log`
  (crash forensics only, never parsed).
- Child → supervisor: each stdout line is a `session.jsonl` event, DATA-MODEL §A.3 shape. Supervisor
  tees each line: **(1) append to `session.jsonl` FIRST**, (2) project into index `event` table,
  (3) SSE fan-out. **The child never writes `session.jsonl` — one writer, no interleaving.**
- Supervisor → child (stdin), NDJSON:
  - `{"type":"chat","message":{...UIMessage}}` — F10, route a user message into the live loop.
  - `{"type":"stop","reason":"user_stop"}` — graceful: finish the beat, write memory, exit 10.
  - `{"type":"ping"}` — liveness probe; child echoes `{"type":"pong"}`.
- Signals: `SIGTERM` = graceful stop (same as `stop`, 15 s grace, then escalate). `SIGKILL` = kill
  switch (F8), no cooperation assumed.
- Liveness: child must emit an event or `pong` at least every 60 s (emit `{"type":"heartbeat"}` when
  idle-waiting on a long tool). Silent > 5 min → supervisor `SIGTERM`, then `SIGKILL` after grace,
  marks the session `blocked` with a `run.hung` event.

## Dep-chain (P14) — concrete upstream / downstream

**Upstream (this depends on):**
- `apps/runtime/src/harness/runner.ts` — `ChildEmittedEvent { actor, type, payload? }` is the exact
  event shape the tee consumes/persists (BRO-1756 defined it and says "the supervisor's tee (BRO-1767)
  assigns seq + appends"). The subprocess child emits the same triple as NDJSON; the tee stamps `ts`
  and lets the event table assign `seq`.
- `apps/runtime/src/db/schema.ts` — `event` table: `{ seq (autoincrement PK), sessionId (nullable),
  ts (integer ms), actor, type, payload (payload_json TEXT) }`. `session` table: `status` is
  `SessionStatus` which INCLUDES `"blocked"` (`packages/protocol/src/work.ts:135`).
- `packages/protocol/src/events.ts` — `EVENT_TYPES` has the `run.*` namespace (RUN_STARTED/BEAT/
  EXITING/FINISHED/FAILED/KILLED/ORPHANED) but **NO `run.hung`**. `run.${string}` is namespace-legal,
  so ADD `RUN_HUNG: "run.hung"` (+ a one-line events.test.ts assertion). Small protocol edit, not
  governance-class (events.ts is normal code, not CLAUDE.md/AGENTS.md/policy.yaml — no L3 gate).
- `apps/runtime/src/config.ts` — `RuntimeConfig`. Add liveness constants (env-overridable, mirror the
  `streamPollMs` pattern): `childHeartbeatMs` (60000), `childHungMs` (300000), `childGraceMs` (15000).
  `positiveInt` helper already exists.
- `apps/runtime/src/db/client.ts` — `IndexDb` type for the tee's `db` dep.
- The on-disk A.3 `session.jsonl` line shape — CHECK `apps/runtime/src/api/event-projection.ts`
  `toEnvelope` for the canonical wire/on-disk shape (payload-nested `EventEnvelope`), and match it so
  the rebuild's future journal-replay (BRO-1808 deferred) reads back byte-identically.

**Downstream (consumers — do not break):**
- BRO-1779 supervisor spawn/tail/reap consumes `superviseChildStdio(child, deps)`. Keep the child a
  NARROW injectable port (not `Bun.Subprocess`) so 1779 and tests both satisfy it.
- SSE stream (BRO-1816, `api/stream.ts`) auto-fans-out from the `event` table by polling `seq>cursor`
  — so **step (3) "SSE fan-out" is AUTOMATIC once the tee inserts the event row**; do NOT build a
  second push path. The load-bearing single-writer invariant (seq order == commit order) is already
  documented in stream.ts; the tee being the sole event writer per session preserves it.
- `session.jsonl` is the FS truth the rebuild journal-replay (BRO-1808 deferred) will consume.
- `db/project.ts projectLiveNode` — not needed here (events aren't tombstoned), noted to avoid reinvention.

## Design (`apps/runtime/src/harness/stdio.ts`)

1. **Line splitter** — stateful `createNdjsonSplitter()` buffering partial lines across stdout chunks
   (Bun stdout is a `ReadableStream<Uint8Array>`; decode + split on `\n`, retain the trailing partial).
2. **Classify each stdout line:** parse JSON; a line with `{actor, type}` (namespaced type) is a
   SESSION EVENT → tee it; a bare `{"type":"pong"}` / `{"type":"heartbeat"}` is a LIVENESS-only signal
   → reset liveness, do NOT persist. A malformed line → drop + count (never crash the pump); ANY line
   received resets liveness (activity).
3. **`SessionTee({ db, sessionId, runDir, now })` `.append(ev)`** — the FS-FIRST invariant:
   `await appendFile(join(runDir,'session.jsonl'), line+'\n')` **then** `await db.insert(event)`. This
   ordering is the first anchor test (assert the FS write resolves before the DB insert — inject a
   spy/ordered-log fake for both). Stamp `ts = now()`; seq is the table's.
4. **stderr sink** — append raw bytes to `child.stderr.log` (never parsed).
5. **`ChildControl`** over stdin — `chat(msg)`, `stop(reason)`, `ping()` write NDJSON control lines.
6. **`LivenessMonitor({ pingIdleMs, hungMs, graceMs, now, onPing, onHung })`** — TICK-DRIVEN with an
   injectable clock + a `tick()` method tests call directly (NO real timers in tests): `activity()`
   resets `lastActivityAt`; on tick, `idle = now()-lastActivityAt`; `idle>hungMs` → escalate;
   else `idle>pingIdleMs` → `onPing()` (send a ping). Escalate = `onHung()`: `child.kill("SIGTERM")`,
   arm a graceMs timer, if `child.exited` hasn't resolved by grace → `child.kill("SIGKILL")`; then
   emit a `run.hung` event via the tee + update the session row `status="blocked"`. Second anchor test:
   a fake child that never exits → assert SIGTERM then (after grace) SIGKILL, a `run.hung` event
   appended, session row `blocked`.
7. **`superviseChildStdio(child, deps)`** — wire stdout→splitter→(activity + classify + tee),
   stderr→log, construct control + monitor, return `{ control, liveness, done }`. `child` is the narrow
   port `{ stdout, stderr, stdin/write, kill(signal), exited: Promise<number> }`.

## P20 risk surface (scale-to-risk: this is runtime concurrency → 4 heavy lenses)

- **Ordering/atomicity:** the append-first invariant must hold under a burst of stdout lines — serialize
  `append()` (a per-session write queue) so `session.jsonl` line order == `event.seq` order == commit
  order (the stream's single-writer guarantee). A naive `for await` that doesn't await each append in
  order, or parallel appends, breaks it. (Recall BRO-1804: a non-serialized reconcile collided; same
  class here.)
- **Liveness races:** a pong arriving DURING the grace window must not cancel an already-sent SIGKILL
  path incorrectly; the hung latch should be one-way. Clock injected; no wall-time sleeps in tests
  (the BRO-1816 abort-listener-leak + BRO-1804 flaky-timing lessons).
- **Partial-line / malformed / huge-line** stdout must never crash the pump or the runtime (wrap
  parse; cap line length defensively).
- **stdin backpressure / closed stdin** (child died) — writing control to a dead child must not throw
  unhandled.
- **Anti-vacuity** [[self-hosting-vacuous-pass]]: the append-first test must FAIL if the order is
  swapped (assert via an ordered event log, not just "both happened"); the hung test must FAIL if
  SIGKILL is never sent (assert the exact signal sequence, not a count).
- **Bun gotcha to verify by dogfood (P11):** `Bun.spawn` stdout as a `ReadableStream` — confirm the
  reader + `proc.exited` + `proc.kill(signal)` semantics on a REAL spawned fixture child (a tiny
  `bun` script that emits NDJSON then hangs), not just a fake — the BRO-1816 lesson was that Bun's
  stream/abort wiring differs from the docs.

## Loop bookkeeping
- BRO-1788 fully DONE + merged (`009a022`); Linear status-flip PENDING operator `/mcp` re-auth (token
  expired at close — code merged is the real completion). When Linear reconnects: `save_issue` id
  BRO-1788 state Done, and BRO-1767 → In Progress.
- After 1767: BRO-1779 supervisor (needs 1767 + 1746 sandbox) → unblocks the guardrails 1795/1801 +
  chat endpoint 1822. P2 exit = BRO-1827.
