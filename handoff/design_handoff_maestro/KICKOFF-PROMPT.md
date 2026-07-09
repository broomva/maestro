# Claude Code kickoff prompt

Paste the block below into a fresh Claude Code session opened at the **root of the new
Broomva repo** (with this design project's contents available — drop `design_handoff_maestro/`
and the design-system folders in, or point at them). It has Claude review the whole handoff
and produce the Linear project + tickets that the autonomy loops will later execute via
`/goal` and `/loop`.

> Tune the two lines marked **⟨edit⟩** to match how your `/goal` and `/loop` commands behave.

---

```
You are bootstrapping the Broomva Maestro build. Before writing any code, your job is to
READ the handoff, then PLAN it into Linear as a project and tickets that our autonomy loops
will execute. Do not scaffold the app yet — planning only, until I approve the plan.

## Step 1 — Read, in this order (do not skip; do not read everything at once)
1. design_handoff_maestro/START-HERE.md      — the map: canon ownership, the two lenses, the build spine
2. design_handoff_maestro/build-docs/CLAUDE.md — the always-on ruleset (put this at repo root)
3. design_handoff_maestro/build-docs/ROADMAP.md — phases P0–P6, each with an exit test
4. design_handoff_maestro/build-docs/BUILD-PLAN.md — the UI track M0–M6
5. design_handoff_maestro/build-docs/ARCHITECTURE.md + STACK.md + AUTONOMY.md + DATA-MODEL.md + API.md + FLOWS.md
6. design_handoff_maestro/build-docs/specs/*  — HARNESS, VERIFIER, ORCHESTRATOR, DECISIONS
7. design_handoff_maestro/docs/data-contract.md + canon-map.md + porting-notes.md
Skim, don't memorize. When you plan a ticket, cite the doc(s) that own its spec.

After reading, write me a ≤1-page brief: the product in your words, the build spine, the
seams (§5 of START-HERE), and anything in the docs that is ambiguous or conflicts. Ask me
your open questions before planning. Canon rule: where docs disagree, START-HERE §2 wins.

## Step 2 — Plan the Linear project
Create ONE Linear project "Broomva Maestro — build". Model the roadmap as it's written:

- One Linear **milestone/cycle per phase** P0…P6. Keep the phase names and their exit tests.
- The UI track M0…M6 rides inside the phases exactly as BUILD-PLAN maps it (M0→P0, M1–M2→P1,
  M3–M4→P2, M5→P3, M6→P4). Label UI-track tickets `track:ui`.
- Decompose each phase into tickets small enough to run as a single loop (roughly one PR /
  one `run/<id>` branch each). Prefer many small tickets over few big ones.
- Every ticket MUST carry:
    • an **acceptance test** — the concrete, runnable check that closes it. Where the phase
      has an exit test, the ticket that completes the phase inherits it verbatim.
    • **dependency links** (Linear "blocks"/"blocked by") — enforce the spine: foundations →
      shell → the vertical gate slice → the rest. And enforce the hard ordering rule:
      **guardrails before features** (P2's budget proxy, iteration cap, no-progress halt, kill
      switch block every unattended-run ticket).
    • **doc references** — the owning spec section(s), not a restatement of them.
    • labels: `phase:P#`, `track:ui|runtime|protocol|autonomy|infra`, `seam` on the four
      integration seams (ChatTransport, work-item store, gate queue, FS-as-truth/index).
- Flag the four **seams** as their own tickets that must be agreed (contract written) BEFORE
  their dependents start. Mark them `seam` and block dependents on them.
- Add a "Definition of done (global)" doc-ticket capturing the standing rules: every phase
  ships light+dark, reduced-motion safe, tokens-only (no raw values), holding CLAUDE.md.

## Step 3 — Make each ticket loop-executable
These tickets are what our agents run later, so shape them as work contracts:

- Write each ticket's body as a `_work.md`-style contract: goal, scope/out-of-scope,
  `done.check` (the acceptance test as a runnable command), and `gate: human` on everything.
  (Per ROADMAP P5, the backlog eventually moves into a Maestro workspace as these contracts —
  so author them that way now.)
- For each phase, note which tickets a `/loop` can pick up in parallel vs. which are gated by
  a `seam` or a prior phase's exit test.

## Step 4 — Kick off (after I approve the plan)
- ⟨edit⟩ `/goal` — seed the project objective: "Ship Broomva Maestro through P6 per ROADMAP,
  holding every rule in CLAUDE.md; the gate is human on every merge."
- ⟨edit⟩ `/loop` — start executing the ready P0 tickets in dependency order, one branch per
  ticket, verified against its `done.check`, parked at the gate for my approval. Never start a
  ticket whose blockers or seams aren't resolved. Never mark a ticket done without its check
  passing.

Deliverables from this session: (1) the brief + your questions, then after I answer —
(2) the created Linear project with milestones, tickets, dependencies, labels, and acceptance
tests. Stop after the plan and wait for my go before Step 4.
```
