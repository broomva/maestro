# Harness

The supervisor↔child contract — the seam every loop runs through. One contract, three tenants: the **agent** child (Loop 1), the **verifier** child (Loop 2, `VERIFIER.md`), and the **orchestrator** session (F6, `ORCHESTRATOR.md`). If it isn't specified here, a child can't rely on it.

> **Design stance:** the child is untrusted-by-default. It gets a worktree, a metered model proxy, and a pipe — never the host env, never the Anthropic key, never the index. Everything the guardrails promise (`AUTONOMY.md` §4) is enforced on the supervisor's side of this seam, so a misbehaving child can waste its own budget but nothing else.

---

## 1. Spawn contract

```
argv:  broomva-child --role agent|verifier|orchestrator --session <id>
cwd:   the run worktree (agent/verifier) · the workspace root, read-only bias (orchestrator)
env:   allowlist ONLY —
       PATH, HOME, LANG, toolchain vars (node/pnpm/bun caches)
       BROOMVA_SESSION=<id>
       BROOMVA_RUN_DIR=<abs path to runs/run-<id>/>
       BROOMVA_CONTRACT=<abs path to contract snapshot JSON>
       BROOMVA_MODEL_PROXY=<url, §3>  BROOMVA_MODEL_TOKEN=<per-session bearer>
```

- **Contract snapshot:** at dispatch the supervisor serializes the node's `_work.md` frontmatter + resolved defaults to `runs/run-<id>/contract.json`. The child reads the snapshot, never the live file — a mid-run contract edit takes effect on the *next* attempt, atomically.
- Host secrets (Anthropic key, runtime credential, relay key) are **never** in the child env. Phase-1 honesty: a worktree shares the host shell, so this is contamination-resistance, not containment — containment is the phase-2 sandbox (`ARCHITECTURE.md` §5). Same spawn contract on both sides of that swap.

## 2. Stdio protocol

NDJSON both ways. **stdout = events, stdin = control.** stderr is captured raw to `runs/run-<id>/child.stderr.log` (crash forensics only, never parsed).

**Child → supervisor (stdout):** each line is a `session.jsonl` event, exactly the `DATA-MODEL.md` A.3 shape. The supervisor tees each line: (1) append to `session.jsonl` — first, FS is truth; (2) project into the index `event` table; (3) SSE fan-out. The child never writes `session.jsonl` itself — one writer, no interleaving.

**Supervisor → child (stdin):** control messages the child must handle:

```jsonl
{"type":"chat","message":{...UIMessage}}        // F10 — route a user message into the live loop
{"type":"stop","reason":"user_stop"}            // graceful: finish the beat, write memory, exit 10
{"type":"ping"}                                  // liveness probe — child echoes {"type":"pong"}
```

**Signals:** `SIGTERM` = graceful stop (same as `stop`, 15 s grace, then escalate). `SIGKILL` = the kill switch (F8) — no cooperation assumed; memory-on-disk means nothing is lost that mattered.

**Liveness:** the child must emit an event or a `pong` at least every 60 s (emit `{"type":"heartbeat"}` when idle-waiting on a long tool). Silent > 5 min → supervisor sends `SIGTERM`, then `SIGKILL` after grace, marks the session `blocked` with `run.hung`.

## 3. The model proxy — budget-in-path, physically

The child's Agent SDK is configured with `baseURL = BROOMVA_MODEL_PROXY` and the per-session bearer token. The proxy is a supervisor-owned local HTTP listener (loopback or unix socket). Every model call flows through it:

1. **Before forwarding** — one transaction on the index: read `run_budget` + the node's budget contract + the day total; over any limit → respond `402 {code:"budget_exhausted"}` and emit `budget.refused`. The request never reaches Anthropic.
2. Forward to the real API with the runtime's key (attached here, never earlier).
3. **On response** — meter actual usage from the response headers/body into `run_budget.spent_usd`, increment `iterations`, emit `budget.metered { usd, tokens }`.
4. Streamed responses pass through unbuffered; metering happens on stream end.

Properties this buys: the guard is *in the request path, not the agent's goodwill* (F3.1 made physical); the key never touches a child; per-session tokens mean a leaked token is one revocable run; and killing a run invalidates its token immediately.

The token is scoped to messages/completions endpoints only. The proxy is also where model **pinning** lives (`AUTONOMY.md` §5 version drift): the child asks for a role ("writer", "judge"); the proxy resolves the pinned model id from runtime config.

## 4. Exit codes

| Code | Meaning | Supervisor action |
|---|---|---|
| `0` | claims complete | spawn verifier (F4) |
| `10` | stopped — budget / iteration cap / no-progress / graceful stop | park per reason (`blocked` or restart-fresh, §5) |
| `20` | needs input — a question only the human can answer | open a gate (`kind: "question"`), state → `review` |
| other / signal | crash | `session.status → blocked`, event `run.failed`, worktree preserved |

The exiting child's **last stdout event must be `run.exiting { code, reason }`** — the supervisor cross-checks it against the real exit code; mismatch is logged as `run.exit_mismatch` (Loop-4 signal for harness bugs).

## 5. Fresh-context restart

Long work never grows one context. The child self-monitors context size; approaching the configured ceiling it: rewrites `progress.md` (state of the world + what's left), ticks `fix_plan.md`, emits `run.restart_requested`, exits `10` with reason `fresh_context`. The supervisor respawns immediately — same session id, same worktree, same budget row (budgets span attempts, not processes). The new child's first act: read `contract.json`, `progress.md`, `fix_plan.md`; skip done work. This is `AUTONOMY.md` §3 as mechanism.

## 6. Agent SDK → `session.jsonl` mapping

The agent child wraps the Claude Agent SDK loop and translates SDK messages to the event vocabulary. Canonical mapping (extend, don't fork):

| SDK occurrence | event `type` | payload |
|---|---|---|
| assistant text/reasoning delta | `agent.said` | coalesced per turn — one event per completed text block, not per token (tokens go over the chat stream, F10; the log stores turns) |
| tool_use start | `tool.call` | `{ tool, input-summary, path? }` |
| tool result | `tool.result` | `{ tool, ok, summary }` |
| model call completed | *(none — proxy emits `budget.metered`)* | |
| loop beat completed | `run.beat` | `{ iteration, diffstat }` |
| child start/exit | `run.started` / `run.exiting` | |

Rule of thumb: `session.jsonl` is the **audit trail**, not the transcript — store decisions, actions, and costs; the full token stream is ephemeral chat plumbing.

## 7. What each role may touch

| | agent | verifier | orchestrator |
|---|---|---|---|
| FS scope | its worktree | its worktree (checks) + `runs/run-<id>/` (verdict) | workspace read; writes only via intents |
| Model proxy | writer role | judge role | writer role, small budget |
| Intents API | none | none | yes — the same `POST /api/intents` as a human (F6 invariant) |
| sh | yes, in worktree | check commands only | no |

The orchestrator having *no direct FS writes and no sh* is deliberate: everything it does must be an intent, so it is gated, audited, and idempotent exactly like a human action.
