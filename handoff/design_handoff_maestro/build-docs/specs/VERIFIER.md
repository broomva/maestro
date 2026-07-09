# Verifier

Loop 2, fully specified. `AUTONOMY.md` says the verifier is "the whole game"; this file is the game. It turns the `done:` block of a work contract into a process, a file format, and a feedback wire. Everything here is enforced by the **runtime**, never by the agent.

> **The one rule everything serves:** the writer never grades its own homework. The verifier is a separate child process (see `HARNESS.md`), spawned by the supervisor, ideally on a different model/vendor for any judged work. The agent cannot spawn, configure, skip, or edit its own verifier.

---

## 1. The `done:` block, full schema

Extends `DATA-MODEL.md` A.2. `check` grows from a string into named checks; `protect` is new and is the anti-reward-hacking guard.

```yaml
done:
  check:                            # deterministic oracle — ordered, all must pass
    - name: tests
      run: "pnpm test"
      timeout_s: 600                # default 600; hard cap 1800
    - name: lint
      run: "pnpm lint"
      timeout_s: 120
    - name: types
      run: "pnpm typecheck"
      required: false               # advisory: reported in verdict, never gates
  judge: rubric.md                  # optional LLM judge — supplement, never sole gate for gate:auto
  protect:                          # paths the RUN may not modify — tamper guard
    - "**/*.test.*"
    - ".github/**"
    - "package.json"                # scripts the checks invoke
  diff:
    max_files: 30                   # exceed → verdict fail, reason "diff too large"
    max_lines: 2000
  stop_on: [cap, no_progress, budget]
```

- A bare string `check: "pnpm test"` is sugar for one check named `check`.
- `protect` defaults to `["**/*.test.*", "**/rubric.md", "**/_work.md"]` — the agent never edits its own success function. Contract authors extend it, never shrink it below the default (runtime enforces).
- **`gate: auto` is legal only when `check` is non-empty.** Judge-only contracts force `gate: human` — the runtime rejects the contract otherwise. This is the "weaker gate pairs with the human gate" rule made mechanical.

## 2. The pipeline (one verification attempt)

Spawned by the supervisor after `run.finished` (F4). Four stages, short-circuiting:

**Stage 0 — Tamper & diff guard** (cheap, no model, runs first)
`git diff --name-only <base>..run/<id>` against `protect` globs and `diff` limits.
- Protected path touched → verdict **fail**, reason `tampering`, the specific paths listed as evidence. This also fires when the agent edited the check commands' own dependencies (`package.json` in `protect`).
- Diff over limits → verdict **fail**, reason `diff_too_large`. The fix is scoping, not retrying — feedback says so.

**Stage 1 — Deterministic checks**
Each check runs in the worktree via the sandbox interface (`ARCHITECTURE.md` §5): non-interactive shell, `cwd` = worktree root, env allowlist only (PATH, HOME, language toolchain vars — **no runtime secrets, no model keys**), per-check `timeout_s`.
- exit 0 → check **pass**; nonzero → **fail** (exit code + last 200 lines of output captured as evidence).
- timeout → **fail**, reason `timeout`.
- command not found / spawn error → verdict **error** (infra problem, not the agent's) — the run parks at `blocked`, not send-back. Never burn an attempt on a broken harness.
- Ordered; first required failure stops the stage (advisory checks still run — they're cheap signal).

**Stage 2 — LLM judge** (only if `judge:` present and Stage 1 passed)
- Model: pinned in runtime config (`verifier.judge_model`), different from the writer's model where possible; temperature 0.
- Input: the rubric, the diff, the brief (`_work.md` body), and check outputs. Not the agent's chat transcript — the judge grades the *work*, not the narrative.
- Output: `judge.json` — per-criterion scores + one targeted paragraph per failing criterion.
- Pass = weighted score ≥ rubric threshold.

**Stage 3 — Verdict assembly**
Write `verdict.md` (§4), emit `check.verdict` event (**D-EVENTNAMES**), hand control back to the supervisor: pass → gate (F5) or auto-merge; fail → feedback (§5) and respawn, counting against `max_iterations`.

## 3. `rubric.md` format

Lives next to `_work.md`. Frontmatter + criteria; the body is the judge's instructions in plain language.

```yaml
---
threshold: 0.8              # weighted pass line, 0–1
scale: [0, 1, 2]            # per-criterion scoring scale
criteria:
  - id: coverage            # stable id — Loop 4 tracks these over time
    weight: 2
    ask: "Every meta tag listed in the brief is present and populated."
  - id: no-regressions
    weight: 1
    ask: "No unrelated files changed; diff is scoped to the brief."
---
Judge the diff against the brief. Score each criterion on the scale.
For any score below max, write one specific, actionable sentence.
```

## 4. `verdict.md` format — the receipt

The file the inspector renders (disclosure ladder rung 3) and the gate's "what it decided." Frontmatter is machine truth; the body is the plain-voice summary.

```yaml
---
verdict: fail                      # pass | fail | error
attempt: 2                        # which verification attempt on this run
base: 3f1c9e0                     # commit the diff was judged against
diffstat: { files: 4, plus: 122, minus: 8 }
tampering: []                      # protected paths touched, if any
checks:
  - { name: tests, ok: false, exit: 1, duration_s: 41, log: checks/tests.log }
  - { name: lint,  ok: true,  exit: 0, duration_s: 6,  log: checks/lint.log }
judge: { score: null }             # or { score: 0.85, model: "...", detail: judge.json }
---
2 of 3 checks passed. `pnpm test`: 3 failures in `head.test.tsx` —
all three assert `og:image` is absolute; the change writes a relative URL.
```

Everything referenced (`checks/*.log`, `judge.json`) lives in `runs/run-<id>/` beside it. The `check.verdict` event carries the frontmatter as payload; clients render the body via `GET /api/sessions/:id` receipts.

## 5. Feedback — closing the loop

On **fail**, the verifier appends to `fix_plan.md` (never rewrites — the history of attempts is signal for Loop 4):

```md
## Verifier — attempt 2 failed (2026-07-07T06:14Z)
- [ ] tests: 3 failures in head.test.tsx — og:image must be absolute (see runs/run-7f3a/checks/tests.log)
- [ ] judge/coverage (1/2): twitter:card missing on the blog template
```

Rules: one checkbox per failing check/criterion; each item names the evidence file; feedback is *targeted* (what failed and where), never prescriptive rewrites of the approach — that's the agent's job. The respawned agent (F3) reads this first.

**Attempt budget:** verification attempts count against `max_iterations`. Additionally `verifier.max_attempts` (default 5) caps consecutive fails → park at `blocked` with reason `verifier_exhausted`. Identical verdict twice in a row = the no-progress stop condition firing at Loop-2 level.

## 6. What the verifier is not

- Not a linter you can appease — Stage 0 means gaming the checks *is* the failure.
- Not the human gate — a pass with `gate: human` still parks at "Needs you." The verdict is evidence *for* the gate, never a bypass of it.
- Not extensible by the agent — the agent may propose `done:` changes in its diff, but `_work.md` is in the default `protect` set, so any such edit fails Stage 0. Contract changes go through the human.

## 7. Events

`verify.started` · `check.result` (per check, streamed live — the card's Undertow shows checks running) · `judge.result` · `verdict` (frontmatter payload) · `verify.error` (infra). All land in `session.jsonl` first, per the flows convention.
