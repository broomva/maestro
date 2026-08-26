---
arc: maestro-1784-orchestrator
specs: [maestro]
ticket: BRO-1784
priority: 1
---

# Maestro P4 — the Loop-3 orchestrator (BRO-1784) DONE; governor loop halted

**TL;DR.** BRO-1784 (the P4 Loop-3 orchestrator: decision policy + versioned prompt + server-side intent subset + an acting tick) is closed DONE across three P20-gated slices, and the autonomous governor loop was halted at the user's request ("finish this arc properly then halt") — **first action: resume the governor on `BRO-1945` (nudge execution + `nudgedSessionIds`), the direct continuation that makes the tick's already-tested `afterNudge:true` branch live.**

## State of the world (P15 snapshot 2026-07-18)

- **`apps/maestro`** (repo `github.com/broomva/maestro`, root of the workspace) — branch `main`, **clean tree**, 0 ahead / 0 behind origin/main. Tip `fd925b9`. No open PRs. No worktree orphans (janitor run after every merge).
- **Preserved branch** — `feature/fidelity-s1-two-plane-home` (3 unpushed commits on an OLD main; the UI two-plane-home work — do NOT delete; rebase to compose when that arc resumes).
- **Running daemons** — none left running by this session (the P11 dogfood runtimes on :4321/:4322 were booted, observed, and killed; temp workspaces cleaned).
- **Linear** — project `Broomva Maestro — build` (`6c738b50`); BRO-1784 **Done**; follow-ups BRO-1944/1945/1946 filed (Backlog).

## What this arc delivered (so the next agent doesn't redo it)

The orchestrator went from *nonexistent* to *deciding and acting* in three slices, each through a full cross-model P20 gate (Strata-B, round-2 verify after every MAJOR fix, every fix mutation-proven):

| PR | Merge SHA | Files | What it gave |
|----|-----------|-------|--------------|
| #96 | `266f57c` | `packages/protocol/src/intents.ts`, `apps/runtime/src/api/intents.ts` | **s1 — server-side intent actor-gate (§4).** `approve/revise/block/escalate/grant/kill/set_state` rejected (403) for `agent:*` actors regardless of prompt; `MAESTRO_ACTOR_HEADER` + `HUMAN_ONLY_INTENT_TYPES` + `isAgentActor` (trim+lowercase). Gate fires before any handler → no lease/side-effect on a refused intent. |
| #97 | `0c09c4a` | `apps/runtime/src/orchestrator/{policy,orchestrator}.ts`, `apps/runtime/prompts/orchestrator.md`, `packages/protocol/src/work.ts`, `apps/runtime/src/tick/briefing.ts`, `apps/runtime/src/ledger/day-total.ts` | **s2 — decision policy made deterministic (the "mock-model tick").** `decidePolicy(briefing, opts)` = §3.1 budget-halt/stale-nudge/escalate · §3.2 surface · §3.3 dispatch-triggered-first-then-runnable-proposed-up-to-cap · §7 plain-voice wake log. `computeOrchestratorTick` = assembleBriefing + decidePolicy (read-only). Versioned §6 prompt. `contractRunnableReason` (§3.3). `deriveDayTotalUsdFromIndex` moved to a leaf module. |
| #98 | `fd925b9` | `apps/runtime/src/tick/{tick,wake-log}.ts`, `apps/runtime/src/orchestrator/policy.ts`, `apps/runtime/src/index.ts` | **s2b — the tick ACTS.** On each scheduler wake `runTick` runs the policy and DISPATCHES the runnable work via the supervisor (in-process, the runtime's own code — safe without BRO-1944), recording the honest §7 narrative of what it *actually did*. `wake-log.ts` leaf extracted to break the tick↔briefing cycle. |

**Key lessons (P20-caught, all in `~/.claude/.../memory/maestro-build-arc.md` §GOVERNOR LOOP):**
- **GATE-INERT-HONESTY** (s1) — an auth-gate comment must not assert a fence ("model has no shell") the harness doesn't implement; the gate is "correct but inert" until BRO-1944. Default-absent header → "user" is *required* (the human SPA sends none), not fail-open.
- **EMPTY-STRING-CHECK VACUITY** (s2) — `hasCheck({check:""})` was `true`; an empty `done.check` would dispatch as runnable. Fix at the ROOT (`hasCheck` requires a non-blank run) so it tightens gate-pairing across all 3 call sites. `.some` not `.every` (a mixed `[blank, real]` array still has a real check).
- **FORWARD-HONESTY reconciliation** (s2b) — the wake log renders from what ACTUALLY happened (`dispatched` = seam-confirmed starts, `nudges` forced `[]`, un-nudged stale runs → `afterNudge:false`), never the plan. Keep technical deferral reasons on the decision object; translate to coffee-voice only at the render boundary (+ a coverage-guard test). Wake-log copy is user-facing → no em dashes / enum names / system terms (CLAUDE.md §Voice).

## E2E proof (re-runnable any time)

```bash
cd ~/broomva/apps/maestro && bun test apps/runtime --filter orchestrator
# Expected: ~766 pass, 0 fail. (Note: a timing-sensitive child-spawn integration test in the
# runtime sweep flakes ~1-in-3 — rerun; the orchestrator/tick/policy tests are deterministic.)
```

Dogfood (the booted binary's scheduler fires a tick through the new acting path): boot the runtime read-only, seed a due schedule + nodes into the index file, wait for a poll, read the last `tick.fired` narrative. The narrative is plain-voiced and reflects reality (empty board → honest "nothing").

## First action

Resume the governor loop (or work solo) on **BRO-1945** — *Orchestrator tick s2b-ii: nudge execution + `nudgedSessionIds` derivation + wake-log FS mirror* (High). It directly continues s2b: today stale runs are *surfaced* to the human ("quiet, worth a look"); s2b-ii wires the actual nudge (a chat into the stale session via the F10 child-stdin path) and derives `nudgedSessionIds` per the **2b contract already documented in `apps/runtime/src/orchestrator/policy.ts`** (a nudge must move `max(event.ts)`; the set is scoped to the CURRENT stale window). This makes the `afterNudge:true` render branch — unit-tested in `policy.test.ts` but inert in production — go live.

```bash
cd ~/broomva/apps/maestro && git checkout main && git pull --ff-only
# then branch FIRST (rule-of-three): git checkout -b feature/bro-1945-nudge-exec
```

If BRO-1945 is blocked or you prefer to close the phase first, the alternative is **BRO-1817** (P4-exit).

## Pickup state (what's open)

- [ ] **BRO-1945** (High) — nudge execution + `nudgedSessionIds` + wake-log FS mirror (the direct s2b continuation; makes `afterNudge:true` live).
- [ ] **BRO-1946** (Med) — orchestrator chrome presence: the bench chip that lights while the tick runs (`apps/app`, driven off the `tick.fired` stream).
- [ ] **BRO-1944** (High) — orchestrator harness confinement (host-pinned actor header + shell-less/network-confined session) + §5 grants; gates any LIVE *model* orchestrator issuing intents. The deterministic tick shipped this arc does not need it.
- [ ] **BRO-1817** — P4-exit (the phase gate).
- [ ] Data-spine 2/4 → 4/4 (runtime emits worker/verdict/reason/look + `run.started` reducer → WorkItem avatar/worktree → demo seed) — interleaveable; older but visible product gap.

## Related context

- Full per-iteration governor log + every P20 saga/lesson: `~/.claude/projects/-Users-broomva-broomva/memory/maestro-build-arc.md` §GOVERNOR LOOP (9 iterations it1–it9).
- Design canon: `apps/maestro/CLAUDE.md` + `handoff/design_handoff_maestro/build-docs/specs/ORCHESTRATOR.md` §2–§8.
- Linear: BRO-1784 (Done), BRO-1944/1945/1946 (follow-ups).
- Prior handoff: `docs/handoffs/2026-07-16-maestro-1914-DONE-1805-slice2bii-next.md`.
- P16 candidate (rule-of-two → three): a shared expiry-aware-lease helper (the scheduler + tick leases both needed expiry-takeover + holder-fencing) — extract at the next lease-holder.
