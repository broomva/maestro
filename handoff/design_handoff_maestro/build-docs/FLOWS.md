# Flows

Sequence specs for the core flows. Actors: **client** (any projection), **relay**, **runtime** (the supervisor), **agent** (Loop 1 child process), **verifier** (Loop 2 child process), **index** (control-plane store), **FS** (workspace + git). Data shapes in `DATA-MODEL.md`; loop rationale in `AUTONOMY.md`.

Conventions every flow obeys:
- Every state change lands in `session.jsonl` first (FS = truth), then the index projects it, then the event stream fans it out. Never the reverse.
- Anything transactional (budgets, leases) happens in the index **before** the side effect.
- No flow transitions to `done` without a gate verdict when `gate: human`.

---

## F1 — New mission

1. client → runtime: intent `new_mission { parentPath, title, brief, kind }`.
2. runtime → FS: create folder + `_work.md` (frontmatter: new UUID, `state: proposed`, defaults from parent's contract).
3. runtime → FS: git commit ("new work: <title>").
4. indexer: picks up the file change → upserts `node` row → emits `node.created` on the stream.
5. All clients see the new card via the stream. No client-local optimistic state is authoritative.

**Failure:** FS write fails → intent returns an error; nothing was half-created (the commit is the transaction).

## F2 — Dispatch (proposed/triggered → running)

1. Trigger: an intent (`dispatch`), the orchestrator's decision (F6), or a schedule fire (F7).
2. runtime → index: acquire `lease` on the node id. Held → someone else is dispatching; drop silently (idempotency).
3. runtime → index: insert `session` row (`status: running`), insert `run_budget` row (zeroed).
4. runtime → FS: `git worktree add ../run-<id> -b run/<id>`; create `runs/run-<id>/` (progress.md, fix_plan.md).
5. runtime: spawn **agent child process** in the worktree — args: node path, contract snapshot, session id. Update `node.state → running`.
6. Events flow: agent appends to `session.jsonl` → runtime tails it → index `event` table → SSE fan-out. The card wears the Undertow.

**Failure:** child crashes at spawn → `session.status → blocked`, `node.state → blocked`, event `run.failed` with the reason. Lease expires on its own.

## F3 — The agent iteration (one Loop 1 beat)

Runs inside the agent child, every beat:

1. **Budget guard (in-path):** transactional read-modify-write on `run_budget` — refuse if `spent_usd`, day total, or `iterations` exceed the contract. On refusal: write `budget.exhausted` event, exit with status `blocked`. This check is in the runtime's request path, not the agent's goodwill — the child calls the model **through** a runtime-provided proxy handle.
2. Model call → tool calls (FS edits, sh, git — all inside the worktree).
3. Write memory to disk: rewrite `progress.md`, tick `fix_plan.md`.
4. Append events to `session.jsonl`.
5. **Stop-condition check:** iteration cap · no-progress (N consecutive empty diffs / identical errors) · budget. Any hit → write the reason, exit cleanly.

**Fresh-context restart:** long work exits and respawns rather than growing one context; the new child reads `progress.md` + `fix_plan.md` and skips done work.

## F4 — Verification (Loop 2)

1. Agent child exits claiming complete: the child emits `run.exiting {code, reason}` (HARNESS owns this seam); the supervisor derives `run.finished` after reap (**D-EVENTNAMES**) — the verifier is spawned on the supervisor-derived `run.finished`.
2. runtime: spawn **verifier child** — a different process, ideally a different model/vendor. The writer never grades its own homework.
3. verifier: run `done.check` (deterministic oracle) in the worktree. If the contract has `done.judge`, run the LLM judge against the rubric — as a supplement, never the sole gate.
4. verifier → FS: write `verdict.md` (verdict + evidence: test output, diff review). Emit `check.verdict` event (**D-EVENTNAMES**).
5. **Pass + `gate: human`** → F5. **Pass + `gate: auto`** → runtime merges `run/<id>`, `node.state → done` (allowed only because the contract explicitly waived the gate).
6. **Fail** → targeted feedback appended to `fix_plan.md`, respawn agent child (back to F3) — counts against `max_iterations`.

## F5 — The gate (the one human verb)

1. runtime → index: insert `gate` row (`verdict: null`), `session.status → review`, `node.state → review` ("Needs you", accent-blue).
2. Stream carries a `data-gate` part with a stable id — every projection shows the look: *what changed · what it decided · what it asks*.
3. client → runtime: intent with the verdict — the four Org-Control-Layer values:
   - **approve** → merge `run/<id>` into the workspace branch, `node.state → done`, event `gate.approved`. The autonomy ledger notes one human look.
   - **revise** (send back) → feedback written to `fix_plan.md`, `node.state → triggered`, redispatch (F2 skipping folder setup).
   - **block** → `node.state → canceled` or parked; worktree kept for autopsy.
   - **escalate** (point/grant) → reassign `owner` or attach a capability grant; stays at review.
4. Gate row updated (`decidedBy`, `decidedAt`); `data-gate` part reconciled by id across every open client.

**Invariant:** approve is the *only* path from `review` to `done` when `gate: human`. No timer, no retry count, no verifier verdict overrides it.

## F6 — The tick (orchestrator wake)

1. Cause arrives: interval timer (from `schedule`) · a worker returning (F4/F5 settled) · your message · a self-set routine.
2. runtime → index: lease on `tick` (one tick at a time).
3. runtime: spawn the **orchestrator session** — an agent child whose workspace scope is the whole tree and whose tools are dispatch/schedule/summarize. It is work like any other work: it has a session, events, receipts.
4. Orchestrator reads the board (reactive queries, B.5), decides: dispatch queued work (F2), nudge stuck runs, adjust schedules, or nothing.
5. Its narrative streams as a `data-tick` part with stable id `tick-log` — the wake log renders *why it woke + what it did*. Re-sends update the card in place.

**Invariant:** the tick is a prompt, not a cron job with side effects — everything the orchestrator does goes through the same intents/flows as a human, and is equally gated.

## F7 — Trigger fire (routines, hooks)

1. Scheduler notices `schedule.next_fire_at <= now` (or a hook arrives).
2. runtime → index: acquire lease on the trigger's **idempotency key** (`nightly-triage-2026-07-07`). Held or recently released → drop. This is the storm killer.
3. Dispatch (F2). For `kind: routine`, the node returns to **Standing** after the run settles — routines never close.
4. `goal` triggers additionally re-check the success condition before every fire and disable themselves on satisfaction.

## F8 — Kill switch

1. client → runtime: intent `kill { sessionId }` — surfaced in the UI on every running card.
2. runtime: `SIGKILL` the agent/verifier child. No cooperation required — this is why runs are processes.
3. `session.status → canceled`, `node.state → blocked` (a human should look), event `run.killed`. Worktree and branch preserved as the receipt of the partial work.

## F9 — Startup / crash recovery

1. Runtime starts → scan workspace: parse every `_work.md` → rebuild `node`; replay `session.jsonl` files newer than the index high-water mark → rebuild `session`/`event`/`gate`/`schedule`.
2. Reconcile authoritative tables: re-derive `run_budget.spent_usd` from `budget.*` events where stale; expire dead `lease` rows.
3. Orphaned runs (session `running`, no live process): park at `blocked` with event `run.orphaned` — the human decides resume vs discard. Never silently respawn.
4. Open the API + stream only after reconcile completes.

## F10 — Chat message (a session projection)

1. client → runtime: `POST` to the session's chat endpoint (`API.md`), UIMessage in.
2. runtime routes it into the live agent child's stdin protocol (or spawns a session if the target is idle work).
3. Response streams back per the **UI Message Stream Protocol** — text/reasoning/tool parts, folded client-side by the pure reducer (`bvApplyChunk`).
4. The same events land in `session.jsonl` — chat never owns the work; closing the tab loses nothing.
