# Orchestrator

The maestro itself — the session F6 spawns on every tick. It is the product's namesake and deliberately its *least* powerful agent: it reads the whole board but can act only through the same intents a human sends. This file specifies its briefing, its policy, its tools, and its hard limits.

> **The invariant, restated from F6:** the tick is a prompt, not a cron job with side effects. The orchestrator is work like any other work — a session, events, receipts, budget — and everything it does is gated exactly as if you had done it.

---

## 1. What it is, mechanically

A standing node `routines/maestro/_work.md` (`kind: routine`, `owner: agent:maestro`, `gate: human`), dispatched via the harness (`HARNESS.md`) with `--role orchestrator`. Per §7 there: **no shell, no direct FS writes** — reads via the runtime API, writes via intents only. Its budget contract is small and non-negotiable by itself:

```yaml
budget: { per_run_usd: 0.50, per_day_usd: 5, max_iterations: 10 }
```

A tick that can't decide in 10 beats shouldn't decide; it should say so in the wake log and stop.

## 2. The briefing — what a tick reads

The supervisor assembles a briefing (not the raw board — curated, bounded, fresh) into the tick's context:

1. **Cause** — why it woke: `interval | worker_returned | user_message | hook`, with the triggering payload.
2. **Attention list** — every node in `blocked | review`, with age. These are *for the human*; the orchestrator's job is to make them visible, never to clear them.
3. **Active runs** — running sessions: iteration count, spend vs budget, last-event age (staleness signal).
4. **Queue** — `proposed | triggered` nodes, attention-ordered.
5. **Bench** — enabled schedules + next fires.
6. **Ledger** — day spend vs day budget across all runs; concurrency in use vs cap.
7. **Last wake log** — its own previous `data-tick` narrative (continuity without a growing context; the tick is a fresh-context loop like every other).

## 3. Decision policy — an ordered checklist, not a vibe

Evaluated top to bottom; earlier rules preempt later ones. **"Nothing" is a first-class, common outcome.**

1. **Safety first.** Day budget ≥ 90% spent → dispatch nothing; note it. A running session stale > 30 min → nudge it (one chat message restating its goal — the task-drift defense); stale after a prior nudge → recommend the human look (it cannot kill without a grant, §5).
2. **Surface, don't clear.** Anything in the attention list gets a line in the wake log with age and a one-sentence "what it asks." The orchestrator never approves, revises, blocks, or escalates a gate.
3. **Dispatch queued work** — while `running < concurrency cap` (default **3**, runtime config): dispatch `triggered` nodes first, then `proposed` nodes **only if** the contract is runnable — non-empty `done.check` (or judge + `gate: human`), a budget block, a brief. Not runnable → leave queued, say why in one line.
4. **Tidy the bench.** Fire-due routines are the scheduler's job (F7), not the tick's; the tick may *propose* schedule changes it justifies (a routine that found nothing 7 fires straight → propose halving cadence) via `set_routine` — which lands as a pending gate, not an applied change.
5. **Propose new work sparingly.** It may create `new_mission` nodes from patterns it sees (recurring failure across runs, a stale dependency). Anything it creates is born `proposed` and **it may not dispatch its own proposals** in the same or later ticks until a human has moved them — writer ≠ judge, applied to planning.

## 4. Tools (the intent subset)

`dispatch` · `new_mission` · `set_routine`(gated) · `tick`(self-reschedule, bounded to ≥ its configured interval) · `nudge`(a chat message into a running session, F10) — plus read-only board/node/event queries. **Not in the set:** `approve`, `revise`, `block`, `escalate`, `grant`, `kill`, `set_state`. The four gate verbs and the kill switch are human verbs, enforced server-side: the runtime rejects them from `agent:*` actors regardless of what the model asks for. Defense sits in the API, not the prompt.

## 5. Grants — how it earns more, one capability at a time

The `grant` verb (F5) can attach a capability to `routines/maestro/_work.md`:

```yaml
capabilities:
  - kill_stale_runs          # may kill a session silent > 60 min (still emits run.killed + wake-log line)
  - dispatch_own_proposals   # relaxes §3.5
  - raise_concurrency: 5
```

Each grant is a frontmatter line — visible, diffable, revocable by deleting it. Autonomy is *granted in increments you can read*, never accumulated silently. Default install: zero capabilities.

## 6. The system prompt — canon skeleton

Versioned at `runtime/prompts/orchestrator.md`; Loop 4 proposes edits to it via gated missions, never live. The load-bearing paragraphs:

- **Role:** "You are the orchestrator of this workspace. You keep work moving between human looks. You are not the worker and not the judge — you dispatch, observe, and report."
- **Priorities, in order:** budget safety → surfacing what needs the human → dispatching runnable work → tidying schedules → proposing improvements.
- **Hard lines:** never act on gates; never edit files; never restate or reinterpret a node's `done:` contract; when uncertain, write one plain sentence in the wake log and stop.
- **Voice:** the wake log is read by a person having coffee. Plain voice, no enum names, one line per decision, lead with anything that needs them.

## 7. The wake log

Streams as the `data-tick` part, stable id `tick-log`, updating in place (API §1). Shape: *why I woke → what needs you (if anything) → what I did → what I left alone (and why)*. Every dispatch/nudge line names the node and links its card. A tick that did nothing still writes two lines — silence reads as breakage.

## 8. Failure containment

- Tick crash / budget-out → exit per harness codes; the **scheduler is unaffected** (F7 fires from the index, not from ticks) — a broken maestro degrades to "runs still fire, nobody narrates," never to stopped work.
- Tick lease (F6.2) means overlapping causes coalesce; a hook storm produces one tick.
- `tick.skipped { cause, reason }` events when the lease is held — Loop 4 watches for chronic skips (interval too tight).
- The orchestrator's own node shows on the board like any routine: Standing between ticks, its sessions inspectable, its spend on the ledger. It gets no invisible privileges — that's the point.
