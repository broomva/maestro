# Autonomy

This is the part that *is* Broomva Maestro: the loop logic that decides when an agent keeps going and when it stops for you. `ARCHITECTURE.md` is the stage; this is the actor. Everything here is grounded in the loop-engineering deep dive (`uploads/2026-06-24-loop-engineering-deep-dive.html` in the design project) — that document is Broomva's own thesis, and this file maps it onto the product.

> **The frame:** autonomy is not "remove the human." It's **stretch the interval between human gates while keeping the gate meaningful.** Maestro's job is to maximize *unsupervised hours* — how long a run goes before it must surface a "Needs you." Autonomy is won in three places: **duration** (survive for hours), **judgment** (know when to stop and ask), and **recovery** (self-correct instead of drifting).

> **The principle that makes it possible** (Stable Agentic Control, arXiv:2605.03034): stability is a property of the **loop**, not the agent. A noisy model in a well-designed loop produced high action-level variance but **zero outcome variance across 40 runs.** So we engineer the loop, not the model.

---

## 1. The unit of work is the loop, not the prompt

Maestro does not prompt agents. It runs **loops that prompt agents** and decide what's done. Every loop is the same five beats: **trigger → find work → act → verify → log**, repeating until a stop condition. Two of those beats carry the whole system: **verify** (the gate on "done") and **log** (memory written to disk every beat). Remove either and you have an open loop — fine for exploration, dangerous unsupervised. **Maestro runs closed loops.**

## 2. The four nested loops, mapped to Broomva

Maestro is four loops at four cadences, each wrapping the one inside (cascade control: inner loop 3–5× faster than outer; tune inner first, change outer rarely). Where Broomva already has the concept, it's noted; where it's a **GAP**, that's net-new build.

### Loop 1 — Agent (per step) — *covered*
The Claude Agent SDK loop: context → tool calls → until step done. Runs inside a `run/<id>` worktree. This is the atomic unit; don't over-build it.

### Loop 2 — Verification (per attempt) — **THE bottleneck — now specified in `VERIFIER.md`**
The deep dive is blunt: the verifier is "the whole game," and "the writer never grades its own homework." Maestro's UI already promises a **"judge verdict" receipt** — this loop is what produces it. It is the highest-value thing to build. The full mechanism — `done:` schema, tamper guard, verdict/rubric formats, feedback wire — is `specs/VERIFIER.md`; the process seam it runs in is `specs/HARNESS.md`.

- **Writer ≠ judge.** The agent that did the work never approves it. Use a separate verifier process, ideally a different model/vendor (shared blind spots otherwise).
- **Deterministic oracle first.** Prefer tests / build / types / lint / a ground-truth compare as the exit signal — "the ground truth is the ground truth." Reserve an **LLM judge** for genuinely subjective work, and never let it be the *sole* gate.
- **Pattern:** Anthropic's evaluator-optimizer — generate → evaluate vs. a rubric → on shortfall send *targeted* feedback back to Loop 1 → repeat. The verdict + evidence (test output, a recorded Playwright run, a diff review) become the receipt the inspector shows.
- **A verifiable success function is mandatory** before a loop runs unattended. "Brief is complete" fails (the agent can't evaluate it). "0 failing tests, 0 lint errors, diff < N files" works.

> **Open decision for v1:** deterministic-first is the recommendation. If a lot of Maestro's target work isn't test-checkable, you'll lean on the LLM-judge path earlier — but treat that as a weaker gate, pair it with the human gate (Loop 3) more aggressively, and never let a model grade its own output.

### Loop 3 — Event-driven / governance (per event) — *partially covered*
Two halves:

- **Trigger** (front of the loop) — Broomva's routines / "Standing" work / schedules-in-a-sentence. The trigger taxonomy to design against: **heartbeat** (sec–min, monitoring/drift), **cron** (fixed times), **hook** (PR pushed, CI fails, message), **goal** (iterate until a success condition, then stop — the hardest, where tokens burn). Triggers need **idempotency** (a key/lease in the control-plane index) or a hook storm fires the same run concurrently and incinerates budget.
- **Governance gate** (end of the loop) — this is **Broomva's human gate, and it maps exactly** onto the Organizational Control Layer (arXiv:2606.04306), which intercepts every agent proposal *after generation, before execution* and returns one of four verdicts. **Those four verdicts are Broomva's gate verbs:**

  | Org-Control-Layer verdict | Broomva verb | Meaning |
  |---|---|---|
  | Approve | **approve** | proceed; the human gate clears |
  | Revise | **send back** | retry with feedback |
  | Block | (hold / cancel) | do not execute |
  | Escalate | **point** / **grant** | route to a human / grant a capability |

  Separating "what the agent suggests" from "what the platform executes" took unsafe executions **88% → 0%.** This is why no loop auto-completes and why "Needs you" is the designed stopping place — render it accent-blue, never red (`CLAUDE.md`).

### Loop 4 — Hill-climbing / self-improvement (per N runs) — **GAP, "arguably the most important"**
An analysis agent reads the **traces of many runs** (the event log + receipts) and *rewrites the inner loops* — prompts, tool choices, the verifier's rubric, the budgets. This is the loop that actually makes Maestro get better at autonomy over time, and it's currently unspecced. Build it **last and change it rarely** (cascade rule: a fast-changing outer loop destabilizes everything inside). v1 can ship with this as a human-driven review of run traces; v2 automates the rewrite.

## 3. Memory — on disk, fresh context per restart

State lives in the **workspace files**, not the context window (context rot is measured across 18 models; filesystem memory beats specialized graph memory, 74% vs 68.5%). Every loop iteration **writes progress to disk**; on restart the loop **re-reads the checkpoint and skips done work**. Long-horizon work uses **fresh-context restart loops**, not one ever-growing context — because the 80%-reliability time-horizon is far shorter than the 50% one (METR), so durable work must cross that cliff via restarts. (This is also why `ARCHITECTURE.md` makes the FS the system of record.)

## 4. Guardrails — hard runtime requirements, not features

Loops don't get tired. Without these, a loop on a vague goal is a token furnace (Uber burned its annual AI-tools budget in ~4 months; the common incident is retry-retry-retry, not a wrong answer). **Every unattended loop in Maestro must enforce all of these:**

- **Three independent stop conditions** — halt on *any* of: (1) **iteration cap** (start 20–50; on hit, stop and report — don't continue), (2) **no-progress / diff-stabilization** (exit after N consecutive empty diffs or identical errors — "the agent agreeing with itself"), (3) **budget exhausted**.
- **Budget guard in the request path** — checked transactionally **before each model call**, per-run and per-day, in the control-plane index (`ARCHITECTURE.md` §3b). On the invoice is too late.
- **Human-in-the-loop on irreversible actions** — deploys, DB writes, payments, credentials go through the gate (Loop 3) before execution. Never unattended.
- **Kill switch** — an out-of-band immediate stop for any running loop. Surface it in the UI.
- **Work isolation** — every run in its own worktree/branch (later: container). Contain blast radius.
- **Observability** — beyond "tests passed": error rates, traces, an audit trail of what was attempted, what failed, and *why it stopped*, posted to a human-visible surface. This is the inspector's receipts (`CLAUDE.md` disclosure ladder rung 3).

## 5. Failure modes to design against

From the deep dive's catalog — each needs a defense in the runtime:

- **Runaway / cost blowup** → iteration cap + in-path budget + no-progress halt.
- **Context rot / snowball** → fresh context per iteration; summarize every 10–20 steps; state on disk.
- **Task drift ("the loop becomes the task")** → restate the goal each step; require ≥1 fresh observation per iteration; cross-model check. Crons run *stale* instructions.
- **Reward hacking the verifier** (agents have deleted tests, used discovered API keys) → independent gold-standard verification; the agent never controls its own checker.
- **Webhook / heartbeat storms** → idempotency keys + locks + rate-limit/circuit-breaker in the index.
- **Version drift** → pin models; run an eval suite; canary new versions.

And the three *human-side* failures Maestro's UX should actively resist: **unverified automation** (the disclosure ladder pushes receipts at you), **comprehension debt** (show the diff and the why, not just "done"), **cognitive surrender** (the design's calm should aid attention, not replace it — "stay the engineer").

## 6. Build order for autonomy

Cascade discipline — make each loop stable before automating the one outside it:

1. **Loop 1** solid (agent in a worktree, writing memory to disk).
2. **Loop 2** — the verifier, deterministic-first. *This is where to spend the most time.* Nothing runs unattended until "done" is a hard check.
3. **Guardrails** — all three stop conditions + budget-in-path + kill switch, before any trigger is automated.
4. **Loop 3** — triggers (manual → cron → hook) + the governance gate wired to Broomva's verbs. Idempotency from the first hook.
5. **Loop 4** — human-reviewed trace analysis first; automate the harness-rewrite only once everything below is stable, and change it rarely.

## 7. Definition of "autonomous" for Maestro

A run is safely autonomous only when **all** hold: it persists across restarts from disk memory; "done" is a verifiable check graded by something other than the writer; it halts on any of the three stop conditions; budget is guarded before each call; irreversible actions route to the human gate; and every run leaves a receipt explaining what it did and why it stopped. Until all six hold, it's a longer leash, not autonomy.
